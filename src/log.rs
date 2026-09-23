//! Everything a command says on stderr goes through here: Nix's plain text,
//! or with `--log-format internal-json` one `@nix {...}` line per message in
//! the format of logging.cc:JSONLogger, plus `code` and `hint` on an error
//! (docs/cli.md). The `json-log-path` file gets every record in either
//! format.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use serde_json::{Value, json};

static JSON: AtomicBool = AtomicBool::new(false);
static SHOW_TRACE: AtomicBool = AtomicBool::new(false);
static JSON_LOG_PATH: OnceLock<String> = OnceLock::new();

/// `--log-format F`, loggers.cc:parseLogFormat. Every format but
/// `internal-json` is plain text here.
pub fn set_format(f: &str) -> Result<(), String> {
    match f {
        "raw" | "raw-with-logs" | "bar" | "bar-with-logs" => JSON.store(false, Ordering::Relaxed),
        "internal-json" => JSON.store(true, Ordering::Relaxed),
        _ => return Err(format!("option 'log-format' has an invalid value '{f}'")),
    }
    Ok(())
}

pub fn json() -> bool {
    JSON.load(Ordering::Relaxed)
}

/// `--show-trace`: the text always has the trace; a record carries it only
/// then, as in Nix.
pub fn set_show_trace() {
    SHOW_TRACE.store(true, Ordering::Relaxed);
}

/// `json-log-path` from the command line; it wins over nix.conf. Set it
/// before the first line goes out.
pub fn set_json_log_path(path: String) {
    let _ = JSON_LOG_PATH.set(path);
}

/// logging.cc:applyJSONLogger: a copy of `v`, without the `@nix ` prefix,
/// to the `json-log-path` file or unix socket. A path that does not open is
/// reported once and ignored, as Nix does.
fn tee(v: &Value) {
    if let Some(w) = sink() {
        let mut w = w.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(w, "{v}");
    }
}

fn sink() -> Option<&'static Mutex<Box<dyn Write + Send>>> {
    static SINK: OnceLock<Option<Mutex<Box<dyn Write + Send>>>> = OnceLock::new();
    // Config::load must not log: that would re-enter this get_or_init
    let sink = SINK.get_or_init(|| {
        let path = JSON_LOG_PATH.get().cloned().or_else(|| {
            let c = crate::config::Config::load(&[]).ok()?;
            c.str("json-log-path").map(str::to_string)
        })?;
        match open_sink(std::path::Path::new(&path)) {
            Ok(w) => Some(Mutex::new(w)),
            Err(e) => {
                let e = crate::error::io_text(&e);
                eprintln!("error (ignored): opening log file \"{path}\": {e}");
                None
            }
        }
    });
    sink.as_ref()
}

fn open_sink(path: &std::path::Path) -> std::io::Result<Box<dyn Write + Send>> {
    use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
    if std::fs::metadata(path).is_ok_and(|m| m.file_type().is_socket()) {
        return Ok(Box::new(std::os::unix::net::UnixStream::connect(path)?));
    }
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o644)
        .open(path)?;
    Ok(Box::new(f))
}

/// One record: `v` under internal-json, else `text` if any.
fn out(v: &Value, text: Option<&str>) {
    if json() {
        eprintln!("@nix {v}");
    } else if let Some(t) = text {
        eprintln!("{t}");
    }
    tee(v);
}

/// error.hh:Verbosity, and logging.hh:ActivityType and ResultType, the ones
/// ietsp emits itself.
pub const LVL_ERROR: u64 = 0;
pub const LVL_INFO: u64 = 3;
pub const ACT_COPY_PATH: u64 = 100;
pub const ACT_REALISE: u64 = 102;
pub const ACT_COPY_PATHS: u64 = 103;
pub const ACT_BUILDS: u64 = 104;
pub const ACT_BUILD: u64 = 105;
pub const RES_BUILD_LOG_LINE: u64 = 101;
pub const RES_SET_PHASE: u64 = 104;
pub const RES_PROGRESS: u64 = 105;
pub const RES_SET_EXPECTED: u64 = 106;

/// Whether activity records go anywhere, so nobody builds one for nothing.
pub fn activities() -> bool {
    json() || sink().is_some()
}

/// logging.cc:Activity: the pid in the high half, a counter below.
pub fn activity_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed) + (u64::from(std::process::id()) << 32)
}

fn fields(mut v: Value, f: Vec<Value>) -> Value {
    if !f.is_empty() {
        v["fields"] = f.into();
    }
    v
}

fn start_record(id: u64, level: u64, ty: u64, text: &str, f: Vec<Value>, parent: u64) -> Value {
    let v = json!({"action": "start", "id": id, "level": level, "parent": parent, "text": text, "type": ty});
    fields(v, f)
}

fn result_record(id: u64, ty: u64, f: Vec<Value>) -> Value {
    fields(json!({"action": "result", "id": id, "type": ty}), f)
}

/// logging.cc:JSONLogger::startActivity.
pub fn start(id: u64, level: u64, ty: u64, text: &str, f: Vec<Value>, parent: u64) {
    if activities() {
        out(&start_record(id, level, ty, text, f, parent), None);
    }
}

/// logging.cc:JSONLogger::stopActivity.
pub fn stop(id: u64) {
    if activities() {
        out(&json!({"action": "stop", "id": id}), None);
    }
}

/// logging.cc:JSONLogger::result.
pub fn result(id: u64, ty: u64, f: Vec<Value>) {
    if activities() {
        out(&result_record(id, ty, f), None);
    }
}

/// An activity of ietsp's own, without a parent; it stops when dropped.
pub struct Activity(pub u64);

impl Activity {
    pub fn new(level: u64, ty: u64, text: &str, f: Vec<Value>) -> Activity {
        let id = activity_id();
        start(id, level, ty, text, f, 0);
        Activity(id)
    }

    pub fn result(&self, ty: u64, f: Vec<Value>) {
        result(self.0, ty, f);
    }
}

impl Drop for Activity {
    fn drop(&mut self) {
        stop(self.0);
    }
}

/// `text` as it goes to stderr, without the newline: itself, or a `msg`
/// record at the verbosity its prefix implies.
pub fn format(text: &str) -> String {
    let level = if text.starts_with("error:") || text.starts_with("trace:") {
        0
    } else if text.starts_with("warning:") || text.starts_with("evaluation warning:") {
        1
    } else {
        3
    };
    let v = json!({"action": "msg", "level": level, "msg": text});
    tee(&v);
    if json() {
        format!("@nix {v}")
    } else {
        text.to_string()
    }
}

/// logging.cc:JSONLogger::log: `text` at `level`, whatever its prefix.
pub fn msg(level: u8, text: &str) {
    out(
        &json!({"action": "msg", "level": level, "msg": text}),
        Some(text),
    );
}

/// A line that is not an error record.
pub fn line(text: &str) {
    eprintln!("{}", format(text));
}

/// A lint warning, `warning: [NAME] MSG` and maybe `\n  at POS`, as it goes
/// to stderr.
pub fn lint(text: &str) -> String {
    let v = lint_record(text);
    tee(&v);
    if json() {
        format!("@nix {v}")
    } else {
        text.to_string()
    }
}

fn lint_record(text: &str) -> Value {
    let rest = text.strip_prefix("warning: ").unwrap_or(text);
    let (raw, pos) = match rest.split_once("\n  at ") {
        Some((raw, pos)) => (raw, Some(pos.to_string())),
        None => (rest, None),
    };
    let r = Report {
        level: 1,
        pos,
        ..Report::new("lint", raw)
    };
    r.record(text)
}

/// The warnings a cache entry recorded, as `lint` puts them out.
pub fn replay_lints(bytes: &[u8]) -> Vec<u8> {
    let mut warnings: Vec<String> = Vec::new();
    for l in String::from_utf8_lossy(bytes).lines() {
        match warnings.last_mut() {
            Some(w) if l.starts_with("  at ") => {
                w.push('\n');
                w.push_str(l);
            }
            _ => warnings.push(l.to_string()),
        }
    }
    let records: Vec<Value> = warnings.iter().map(|w| lint_record(w)).collect();
    records.iter().for_each(tee);
    if !json() {
        return bytes.to_vec();
    }
    records
        .iter()
        .map(|v| format!("@nix {v}\n"))
        .collect::<String>()
        .into_bytes()
}

/// One error (or, at level 1, warning) for stderr.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// The stable code of docs/cli.md.
    pub code: &'static str,
    pub level: u8,
    /// The message, without `error: `.
    pub raw: String,
    /// `file:line:col`, as Nix prints a position.
    pub pos: Option<String>,
    /// Innermost first, each with its position.
    pub trace: Vec<(String, Option<String>)>,
    /// What to do about it. The text shows it for a usage error only.
    pub hint: Option<String>,
}

impl Report {
    pub fn new(code: &'static str, raw: impl Into<String>) -> Report {
        Report {
            code,
            level: 0,
            raw: raw.into(),
            pos: None,
            trace: Vec::new(),
            hint: None,
        }
    }

    /// An evaluation error; `at` names a position, `msg` is the message
    /// and `rewrite` maps a trace message as the caller maps `msg`.
    pub fn eval(
        e: &crate::eval::Error,
        msg: &str,
        at: &dyn Fn(crate::eval::Pos) -> String,
        rewrite: &dyn Fn(&str) -> String,
    ) -> Report {
        let at = |p: crate::eval::Pos| (!p.is_none()).then(|| at(p));
        Report {
            pos: at(e.pos),
            trace: e.trace.iter().map(|(p, m)| (rewrite(m), at(*p))).collect(),
            hint: e.hint.clone(),
            ..Report::new(e.kind.code(), msg)
        }
    }

    /// The text Nix-style commands print, without the last newline.
    pub fn text(&self) -> String {
        let mut s = format!("error: {}", self.raw);
        if let Some(p) = &self.pos {
            s.push_str(&format!("\n       at {p}"));
        }
        for (m, p) in &self.trace {
            match p {
                Some(p) => s.push_str(&format!("\n       … {m} at {p}")),
                None => s.push_str(&format!("\n       … {m}")),
            }
        }
        if let Some(h) = self.hint.as_ref().filter(|_| self.code == "usage") {
            s.push('\n');
            s.push_str(h);
        }
        s
    }

    /// logging.cc:JSONLogger::logEI; `text` is the `msg` field.
    pub fn record(&self, text: &str) -> Value {
        let mut v = json!({
            "action": "msg",
            "code": self.code,
            "level": self.level,
            "msg": text,
            "raw_msg": self.raw,
        });
        let o = v.as_object_mut().expect("an object");
        o.extend(pos_fields(self.pos.as_deref()));
        if let Some(h) = &self.hint {
            o.insert("hint".into(), h.as_str().into());
        }
        if SHOW_TRACE.load(Ordering::Relaxed) && !self.trace.is_empty() {
            let t = self.trace.iter().map(|(m, p)| {
                let mut f = serde_json::Map::from_iter(pos_fields(p.as_deref()));
                f.insert("raw_msg".into(), m.as_str().into());
                Value::Object(f)
            });
            o.insert("trace".into(), t.collect());
        }
        v
    }

    /// Prints the report and returns the exit code: 2 for a usage error of
    /// a native command, 1 for the rest.
    pub fn emit(&self) -> i32 {
        let text = self.text();
        out(&self.record(&text), Some(&text));
        if self.code == "usage" && crate::cli::help::command().is_some() {
            2
        } else {
            1
        }
    }
}

/// logging.cc:to_json(Pos): the line and column come off the end of `pos`.
fn pos_fields(pos: Option<&str>) -> [(String, Value); 3] {
    let lc = pos.and_then(|p| {
        let (rest, col) = p.rsplit_once(':')?;
        let (_, line) = rest.rsplit_once(':')?;
        Some((line.parse::<u32>().ok()?, col.parse::<u32>().ok()?))
    });
    [
        ("line".into(), lc.map(|l| l.0).into()),
        ("column".into(), lc.map(|l| l.1).into()),
        ("file".into(), pos.into()),
    ]
}

/// build-result.cc:failingExitStatus of one failed build, from its
/// worker-protocol status. The daemon sends a hash mismatch as
/// OutputRejected, so its message tells it apart.
pub fn build_exit(status: u8, msg: &str) -> i32 {
    match status {
        3 | 4 => 100,
        8 => 101,
        5 if msg.contains("hash mismatch in fixed-output derivation") => 102,
        12 => 104,
        _ => 1,
    }
}

/// failingExitStatus over several builds: the flags of the special codes
/// combine, any other failure is 1.
pub fn combine_exit(a: i32, b: i32) -> i32 {
    if a >= 100 && b >= 100 {
        a | b
    } else {
        a.max(b)
    }
}

/// A failed build as a record; `reason` is `status N: MSG` or a plain
/// message, `pos` where the derivation was defined.
/// The `specified` and `got` hashes of a fixed-output hash mismatch
/// message.
pub fn mismatch(raw: &str) -> Option<(String, String)> {
    let field = |k: &str| {
        raw.lines()
            .find_map(|l| l.trim().strip_prefix(k))
            .map(|h| h.trim().to_string())
    };
    Some((field("specified:")?, field("got:")?))
}

static MISMATCHES: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// The `(specified, got)` pairs of the hash mismatches reported since the
/// last call.
pub fn take_mismatches() -> Vec<(String, String)> {
    std::mem::take(&mut MISMATCHES.lock().unwrap_or_else(|e| e.into_inner()))
}

/// A hash that `fix-hashes` rewrote at `pos`, `file:line:col`.
pub fn fixed(pos: &str, old: &str, new: &str) {
    let raw = format!("'{old}' is now '{new}'");
    let text = format!("fixed: {pos}: {raw}");
    let r = Report {
        level: 3,
        pos: Some(pos.to_string()),
        ..Report::new("hash-fixed", raw)
    };
    out(&r.record(&text), Some(&text));
}

fn build_report(pos: Option<String>, reason: &str, exit: i32) -> Report {
    let msg = match reason.split_once(": ") {
        Some((s, m)) if s.starts_with("status ") => m,
        _ => reason,
    };
    let raw = crate::build::results::strip_ansi(msg);
    let (code, hint) = match mismatch(&raw) {
        Some((_, got)) if exit == 102 => (
            "hash-mismatch",
            Some(format!("set the expected hash to '{got}'")),
        ),
        _ => ("build-failed", None),
    };
    Report {
        pos,
        hint,
        ..Report::new(code, raw.trim_end())
    }
}

/// A failed build: its record under internal-json, else `text`, if any;
/// the renderer prints its own.
pub fn build_failed(pos: Option<String>, reason: &str, exit: i32, text: Option<&str>) {
    let r = build_report(pos, reason, exit);
    if r.code == "hash-mismatch"
        && let Some(m) = mismatch(&r.raw)
    {
        MISMATCHES.lock().unwrap_or_else(|e| e.into_inner()).push(m);
    }
    out(&r.record(&r.text()), text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_is_nix_log_ei_plus_code_and_hint() {
        let r = Report {
            pos: Some("/a/b.nix:3:7".into()),
            trace: vec![("while calling 'f'".into(), Some("«string»:1:9".into()))],
            hint: Some("did you mean 'abc'?".into()),
            ..Report::new("missing-attr", "attribute 'abd' missing")
        };
        let text = r.text();
        assert_eq!(
            text,
            "error: attribute 'abd' missing\n       at /a/b.nix:3:7\n       … while calling 'f' at «string»:1:9"
        );
        let want = r#"{"action":"msg","code":"missing-attr","column":7,"file":"/a/b.nix:3:7","hint":"did you mean 'abc'?","level":0,"line":3,"msg":"error: attribute 'abd' missing\n       at /a/b.nix:3:7\n       … while calling 'f' at «string»:1:9","raw_msg":"attribute 'abd' missing"}"#;
        assert_eq!(r.record(&text).to_string(), want);
        SHOW_TRACE.store(true, Ordering::Relaxed);
        let v = r.record(&text);
        SHOW_TRACE.store(false, Ordering::Relaxed);
        let t = r#"[{"column":9,"file":"«string»:1:9","line":1,"raw_msg":"while calling 'f'"}]"#;
        assert_eq!(v["trace"].to_string(), t);
    }

    #[test]
    fn record_without_position() {
        let r = Report::new("usage", "unknown flag '--x'");
        let want = r#"{"action":"msg","code":"usage","column":null,"file":null,"level":0,"line":null,"msg":"m","raw_msg":"unknown flag '--x'"}"#;
        assert_eq!(r.record("m").to_string(), want);
    }

    #[test]
    fn lint_warning_is_a_level_1_record() {
        let text = "warning: [url-literal] URL literal\n  at /a.nix:2:5";
        let want = r#"@nix {"action":"msg","code":"lint","column":5,"file":"/a.nix:2:5","level":1,"line":2,"msg":"warning: [url-literal] URL literal\n  at /a.nix:2:5","raw_msg":"[url-literal] URL literal"}"#;
        assert_eq!(format!("@nix {}", lint_record(text)), want);
    }

    #[test]
    fn activity_records_are_nix_json_logger_records() {
        let f = vec!["/nix/store/x.drv".into(), "".into(), 1.into(), 1.into()];
        let want = r#"{"action":"start","fields":["/nix/store/x.drv","",1,1],"id":7,"level":3,"parent":0,"text":"building '/nix/store/x.drv'","type":105}"#;
        let v = start_record(7, 3, ACT_BUILD, "building '/nix/store/x.drv'", f, 0);
        assert_eq!(v.to_string(), want);
        let want = r#"{"action":"start","id":8,"level":0,"parent":7,"text":"","type":102}"#;
        assert_eq!(
            start_record(8, 0, ACT_REALISE, "", vec![], 7).to_string(),
            want
        );
        let p = vec![1.into(), 2.into(), 1.into(), 0.into()];
        let want = r#"{"action":"result","fields":[1,2,1,0],"id":8,"type":105}"#;
        assert_eq!(result_record(8, RES_PROGRESS, p).to_string(), want);
        assert_eq!(activity_id() >> 32, u64::from(std::process::id()));
        assert_ne!(activity_id(), activity_id());
    }

    #[test]
    fn exit_status_flags_combine_like_nix() {
        assert_eq!(build_exit(3, ""), 100);
        assert_eq!(build_exit(8, ""), 101);
        assert_eq!(
            build_exit(5, "hash mismatch in fixed-output derivation 'x':"),
            102
        );
        assert_eq!(build_exit(5, "output rejected"), 1);
        assert_eq!(build_exit(12, ""), 104);
        assert_eq!(build_exit(10, ""), 1);
        assert_eq!(combine_exit(100, 101), 101);
        assert_eq!(combine_exit(100, 104), 108);
        assert_eq!(combine_exit(1, 102), 102);
        assert_eq!(combine_exit(0, 1), 1);
        assert_eq!(combine_exit(0, 0), 0);
    }

    #[test]
    fn a_hash_mismatch_names_the_hash_it_got() {
        let reason = "status 5: hash mismatch in fixed-output derivation '/nix/store/x.drv':\n  specified: sha256-AAAA=\n     got:    sha256-BBBB=";
        let r = build_report(Some("/a.nix:4:3".into()), reason, 102);
        assert_eq!(r.code, "hash-mismatch");
        assert_eq!(
            r.hint.as_deref(),
            Some("set the expected hash to 'sha256-BBBB='")
        );
        assert!(
            r.raw
                .starts_with("hash mismatch in fixed-output derivation")
        );
        assert_eq!(
            build_report(None, "status 3: Cannot build", 100).code,
            "build-failed"
        );
    }
}
