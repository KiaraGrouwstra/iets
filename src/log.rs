//! Everything a command says on stderr goes through here: Nix's plain text,
//! or with `--log-format internal-json` one `@nix {...}` line per message in
//! the format of logging.cc:JSONLogger, plus `code` and `hint` on an error
//! (docs/reference/errors.md). The `json-log-path` file gets every record in either
//! format.
//!
//! The state is a [`Log`], which a [`crate::session::Session`] owns: the
//! functions here act on the one this thread entered.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{Value, json};

use crate::session::Session;

/// Where one session's messages go and what it saw on the way.
#[derive(Default)]
pub struct Log {
    json: AtomicBool,
    show_trace: AtomicBool,
    /// Under one of Nix's names stderr holds what Nix prints and nothing else.
    classic: AtomicBool,
    json_log_path: OnceLock<String>,
    sink: OnceLock<Option<Mutex<Box<dyn Write + Send>>>>,
    sid: OnceLock<String>,
    next_activity: AtomicU64,
    mismatches: Mutex<Vec<(String, String)>>,
    want_mismatch: Mutex<Option<String>>,
    /// Where messages go. `None` writes nowhere: a library says nothing
    /// unless the host asked for it. `ietsp` points it at stderr.
    err: Mutex<Option<Box<dyn Write + Send>>>,
}

impl Log {
    /// Sends every message to `w`.
    pub fn write_to(&self, w: Box<dyn Write + Send>) {
        *self.err.lock().unwrap_or_else(|e| e.into_inner()) = Some(w);
    }

    /// What `ietsp` itself does, and what a host that wants Nix's own
    /// behaviour does.
    pub fn write_to_stderr(&self) {
        self.write_to(Box::new(std::io::stderr()));
    }

    /// `bytes` as they are, for output a caller already formatted.
    pub fn err(&self, bytes: &[u8]) {
        if let Some(w) = self.err.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            let _ = w.write_all(bytes);
        }
    }

    /// One line, in place of `eprintln!`.
    pub fn errln(&self, text: &str) {
        if let Some(w) = self.err.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            let _ = writeln!(w, "{text}");
        }
    }
}

fn cur() -> Arc<Session> {
    Session::current()
}

/// A `Write` onto this thread's session log, for code that must own its
/// destination: the build renderer takes a `Box<dyn Write + Send>`.
pub struct SessionOut(Arc<Session>);

impl Default for SessionOut {
    fn default() -> SessionOut {
        SessionOut(cur())
    }
}

impl Write for SessionOut {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.log.err(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `--log-format F`, loggers.cc:parseLogFormat. Every format but
/// `internal-json` is plain text here.
pub fn set_format(f: &str) -> Result<(), String> {
    let s = cur();
    match f {
        "raw" | "raw-with-logs" | "bar" | "bar-with-logs" => {
            s.log.json.store(false, Ordering::Relaxed);
        }
        "internal-json" => s.log.json.store(true, Ordering::Relaxed),
        _ => return Err(format!("option 'log-format' has an invalid value '{f}'")),
    }
    Ok(())
}

pub fn json() -> bool {
    cur().log.json.load(Ordering::Relaxed)
}

/// `--show-trace`: the text always has the trace; a record carries it only
/// then, as in Nix.
pub fn set_show_trace() {
    cur().log.show_trace.store(true, Ordering::Relaxed);
}

/// This process answers to one of Nix's names (`cli::classic::dispatch`).
pub fn set_classic() {
    cur().log.classic.store(true, Ordering::Relaxed);
}

/// `json-log-path` from the command line; it wins over nix.conf. Set it
/// before the first line goes out.
pub fn set_json_log_path(path: String) {
    let _ = cur().log.json_log_path.set(path);
}

/// `sid`, the session of every record: `IETSP_SESSION_ID` when a parent
/// ietsp or the user set it, else a UUIDv7 made for this one. The
/// `eval-jobs` workers get their parent's, so one run's records share one
/// id (docs/reference/errors.md).
pub fn session_id() -> String {
    cur().log.sid().to_string()
}

impl Log {
    fn sid(&self) -> &str {
        self.sid.get_or_init(|| {
            std::env::var("IETSP_SESSION_ID")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| uuid_v7(std::time::SystemTime::now()))
        })
    }
}

/// RFC 9562 version 7: 48 bits of Unix milliseconds, then random bits.
fn uuid_v7(now: std::time::SystemTime) -> String {
    let ms = now
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let mut b = [0u8; 16];
    b[..6].copy_from_slice(&ms.to_be_bytes()[2..]);
    unsafe { libc::getrandom(b[6..].as_mut_ptr().cast(), 10, 0) };
    b[6] = 0x70 | (b[6] & 0x0f);
    b[8] = 0x80 | (b[8] & 0x3f);
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

impl Log {
    /// A record as it goes out, with its `sid`.
    fn wire(&self, v: &Value) -> String {
        let mut v = v.clone();
        if let Some(o) = v.as_object_mut() {
            o.insert("sid".into(), self.sid().into());
        }
        v.to_string()
    }

    /// logging.cc:applyJSONLogger: a copy of `v`, without the `@nix ` prefix,
    /// to the `json-log-path` file or unix socket. A path that does not open
    /// is reported once and ignored, as Nix does.
    fn tee(&self, v: &Value) {
        if let Some(w) = self.sink() {
            let mut w = w.lock().unwrap_or_else(|e| e.into_inner());
            let _ = writeln!(w, "{}", self.wire(v));
        }
    }

    fn sink(&self) -> Option<&Mutex<Box<dyn Write + Send>>> {
        // Config::read, not load: a warning would re-enter this get_or_init
        self.sink
            .get_or_init(|| {
                let path = self.json_log_path.get().cloned().or_else(|| {
                    let c = crate::config::Config::read(&[]).ok()?;
                    c.str("json-log-path").map(str::to_string)
                })?;
                match open_sink(std::path::Path::new(&path)) {
                    Ok(w) => Some(Mutex::new(w)),
                    Err(e) => {
                        let e = crate::error::io_text(&e);
                        self.errln(&format!(
                            "error (ignored): opening log file \"{path}\": {e}"
                        ));
                        None
                    }
                }
            })
            .as_ref()
    }
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

impl Log {
    /// One record: `v` under internal-json, else `text` if any.
    fn out(&self, v: &Value, text: Option<&str>) {
        if self.json.load(Ordering::Relaxed) {
            self.errln(&format!("@nix {}", self.wire(v)));
        } else if let Some(t) = text {
            self.errln(t);
        }
        self.tee(v);
    }
}

fn out(v: &Value, text: Option<&str>) {
    cur().log.out(v, text);
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
    let s = cur();
    s.log.json.load(Ordering::Relaxed) || s.log.sink().is_some()
}

/// logging.cc:Activity: the pid in the high half, a counter below.
pub fn activity_id() -> u64 {
    cur().log.next_activity.fetch_add(1, Ordering::Relaxed) + (u64::from(std::process::id()) << 32)
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
    note_mismatch(text);
    let level = if text.starts_with("error:") || text.starts_with("trace:") {
        0
    } else if text.starts_with("warning:") || text.starts_with("evaluation warning:") {
        1
    } else {
        3
    };
    let v = json!({"action": "msg", "level": level, "msg": text});
    cur().log.render(&v, text)
}

impl Log {
    /// `v` as an `@nix` line under internal-json, else `text`; either way
    /// the `json-log-path` file gets the record.
    fn render(&self, v: &Value, text: &str) -> String {
        self.tee(v);
        if self.json.load(Ordering::Relaxed) {
            format!("@nix {}", self.wire(v))
        } else {
            text.to_string()
        }
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
    cur().log.errln(&format(text));
}

/// A lint warning, `warning: [NAME] MSG` and maybe `\n  at POS`, as it goes
/// to stderr.
pub fn lint(text: &str) -> String {
    cur().log.render(&lint_record(text), text)
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
    let s = cur();
    records.iter().for_each(|v| s.log.tee(v));
    if !s.log.json.load(Ordering::Relaxed) {
        return bytes.to_vec();
    }
    records
        .iter()
        .map(|v| format!("@nix {}\n", s.log.wire(v)))
        .collect::<String>()
        .into_bytes()
}

/// One error (or, at level 1, warning) for stderr.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// The stable code of docs/reference/errors.md.
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
        if cur().log.show_trace.load(Ordering::Relaxed) && !self.trace.is_empty() {
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
        5 if msg.contains(MISMATCH) => 102,
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

/// What every fixed-output hash mismatch message starts with.
pub const MISMATCH: &str = "hash mismatch in fixed-output derivation";

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

/// `ietsp prefetch` builds a derivation whose hash is wrong on purpose: a
/// mismatch of a derivation called `name` is its answer, not a failure.
pub fn want_mismatch(name: &str) {
    *cur()
        .log
        .want_mismatch
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(format!("-{name}.drv"));
}

/// Whether `text` is about the derivation `want_mismatch` named.
fn wanted(log: &Log, text: &str) -> bool {
    let want = log.want_mismatch.lock().unwrap_or_else(|e| e.into_inner());
    want.as_ref().is_some_and(|n| text.contains(n.as_str()))
}

/// The daemon reports a resolved derivation's mismatch as a line of its own,
/// not as the build's result, so the line is read here too. The resolved copy
/// has a drv path of its own, so only the pair of hashes carries over; that
/// is all `fix-hashes` and `prefetch` need.
pub fn note_mismatch(text: &str) {
    if text.contains(MISMATCH)
        && let Some(m) = mismatch(&crate::build::results::strip_ansi(text))
    {
        push_mismatch(&cur().log, m);
    }
}

/// One `(specified, got)` pair, skipping one that is already there: a
/// mismatch can reach both paths that note one.
fn push_mismatch(log: &Log, m: (String, String)) {
    let mut v = log.mismatches.lock().unwrap_or_else(|e| e.into_inner());
    if !v.contains(&m) {
        v.push(m);
    }
}

/// The `(specified, got)` pairs of the hash mismatches reported since the
/// last call.
pub fn take_mismatches() -> Vec<(String, String)> {
    std::mem::take(
        &mut cur()
            .log
            .mismatches
            .lock()
            .unwrap_or_else(|e| e.into_inner()),
    )
}

/// `warning: RAW`, a level-1 record with its own code.
pub fn warning(code: &'static str, raw: String) {
    let text = format!("warning: {raw}");
    let r = Report {
        level: 1,
        ..Report::new(code, raw)
    };
    out(&r.record(&text), Some(&text));
}

/// What made a tracing-cache entry stale, as `re-evaluating: RAW`. Level 3:
/// nothing is wrong, and an edit invalidates an entry on every normal run
/// (docs/reference/errors.md). Under a classic name only `json-log-path`
/// gets it: Nix invalidates its own evaluation cache without a word
/// (docs/reference/divergences.md).
pub fn invalidated(raw: String) {
    let text = format!("re-evaluating: {raw}");
    let r = Report {
        level: 3,
        ..Report::new("cache-invalidated", raw)
    };
    let s = cur();
    if s.log.classic.load(Ordering::Relaxed) {
        s.log.tee(&r.record(&text));
    } else {
        out(&r.record(&text), Some(&text));
    }
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
    let s = cur();
    if r.code == "hash-mismatch"
        && let Some(m) = mismatch(&r.raw)
    {
        push_mismatch(&s.log, m);
        if wanted(&s.log, &r.raw) {
            return;
        }
    }
    s.log.out(&r.record(&r.text()), text);
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
        let s = Session::new();
        let _entered = s.enter();
        s.log.show_trace.store(true, Ordering::Relaxed);
        let v = r.record(&text);
        let t = r#"[{"column":9,"file":"«string»:1:9","line":1,"raw_msg":"while calling 'f'"}]"#;
        assert_eq!(v["trace"].to_string(), t);
    }

    #[test]
    fn a_session_id_is_a_uuid_v7() {
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_millis(0x019e_037b_f603);
        let (a, b) = (uuid_v7(t), uuid_v7(t));
        assert!(a.starts_with("019e037b-f603-7"), "{a}");
        assert!(matches!(&a[19..20], "8" | "9" | "a" | "b"), "{a}");
        assert_eq!(
            a.split('-').map(str::len).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12]
        );
        assert_ne!(a, b);
        let s = Session::new();
        let _entered = s.enter();
        let v: Value = serde_json::from_str(&s.log.wire(&json!({"action": "msg"}))).unwrap();
        assert_eq!(v["sid"], session_id());
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
    fn a_resolved_copy_reports_its_mismatch_as_a_line() {
        let s = Session::new();
        let _entered = s.enter();
        let a = "sha256-AoAoAoAoAo=";
        let line = format!(
            "error: hash mismatch in fixed-output derivation '/nix/store/r-x.drv':\n         specified: {a}\n            got:    sha256-BoBoBoBoBo="
        );
        note_mismatch(&line);
        note_mismatch(&line);
        assert_eq!(
            take_mismatches(),
            vec![(a.to_string(), "sha256-BoBoBoBoBo=".to_string())]
        );
        note_mismatch("error: build of resolved derivation '/nix/store/r-x.drv' failed");
        assert_eq!(take_mismatches(), Vec::new());
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
