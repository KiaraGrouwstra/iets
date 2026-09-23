//! Everything a command says on stderr goes through here: Nix's plain text,
//! or with `--log-format internal-json` one `@nix {...}` line per message in
//! the format of logging.cc:JSONLogger, plus `code` and `hint` on an error
//! (docs/cli.md).

use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

static JSON: AtomicBool = AtomicBool::new(false);
static SHOW_TRACE: AtomicBool = AtomicBool::new(false);

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

/// One `@nix` line. The activity records will go out through this too.
pub fn write(v: &Value) {
    eprintln!("@nix {v}");
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
        if json() {
            write(&self.record(&text));
        } else {
            eprintln!("{text}");
        }
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
}
