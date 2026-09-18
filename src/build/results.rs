//! `--result-file`, `--stream-json-lines` and the CI step summary, in
//! nix-fast-build's formats (results.py:28-43, report.py:35-101, 225-282).

use std::collections::BTreeMap;
use std::io::{self, Write};

/// results.py:ResultType, the two kinds this dispatcher produces. There is
/// no `UPLOAD`/`DOWNLOAD`/`CACHIX`: ietsp pushes to no cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Eval,
    Build,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Eval => "EVAL",
            Kind::Build => "BUILD",
        }
    }

    /// report.py:capitalize_first_letter, the JUnit `classname`.
    pub fn classname(self) -> &'static str {
        match self {
            Kind::Eval => "Eval",
            Kind::Build => "Build",
        }
    }
}

/// results.py:Result.
#[derive(Debug, Clone)]
pub struct Record {
    pub kind: Kind,
    pub attr: String,
    pub success: bool,
    pub duration: f64,
    pub error: Option<String>,
    pub log_output: Option<String>,
    pub outputs: Option<BTreeMap<String, String>>,
    pub drv_path: Option<String>,
    pub cache_status: Option<String>,
    pub skipped: bool,
}

impl Record {
    pub fn eval(attr: &str) -> Record {
        Record {
            kind: Kind::Eval,
            attr: attr.to_string(),
            success: true,
            duration: 0.0,
            error: None,
            log_output: None,
            outputs: None,
            drv_path: None,
            cache_status: None,
            skipped: false,
        }
    }

    pub fn build(attr: &str) -> Record {
        Record {
            kind: Kind::Build,
            ..Record::eval(attr)
        }
    }

    /// results.py:Result.as_dict, keys in the order `sort_keys=True` gives.
    pub fn as_json(&self) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        m.insert("attr".into(), self.attr.clone().into());
        if let Some(c) = &self.cache_status {
            m.insert("cacheStatus".into(), c.clone().into());
        }
        if let Some(d) = &self.drv_path {
            m.insert("drvPath".into(), d.clone().into());
        }
        m.insert(
            "duration".into(),
            serde_json::Number::from_f64(self.duration)
                .map_or(serde_json::Value::from(0.0), serde_json::Value::Number),
        );
        m.insert(
            "error".into(),
            match &self.error {
                Some(e) => e.clone().into(),
                None => serde_json::Value::Null,
            },
        );
        if let Some(o) = &self.outputs {
            m.insert(
                "outputs".into(),
                serde_json::Value::Object(
                    o.iter()
                        .map(|(k, v)| (k.clone(), serde_json::Value::from(v.clone())))
                        .collect(),
                ),
            );
        }
        if self.skipped {
            m.insert("skipped".into(), true.into());
        }
        m.insert("success".into(), self.success.into());
        m.insert("type".into(), self.kind.name().into());
        serde_json::Value::Object(m)
    }
}

/// One JSON object per line on stdout, as `--stream-json-lines` does.
pub fn write_json_line(w: &mut dyn Write, r: &Record) -> io::Result<()> {
    let line = serde_json::to_string(&r.as_json()).unwrap_or_default();
    w.write_all(line.as_bytes())?;
    w.write_all(b"\n")?;
    w.flush()
}

/// report.py:dump_json: `{"results": [...]}`, two-space indent, sorted keys.
pub fn dump_json(w: &mut dyn Write, records: &[Record]) -> io::Result<()> {
    let mut root = serde_json::Map::new();
    root.insert(
        "results".into(),
        serde_json::Value::Array(records.iter().map(Record::as_json).collect()),
    );
    let text = serde_json::to_string_pretty(&serde_json::Value::Object(root))
        .unwrap_or_else(|_| "{}".into());
    w.write_all(text.as_bytes())
}

/// ECMA-048 escape sequences, as report.py:strip_ansi removes them.
pub fn strip_ansi(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != 0x1b {
            let start = i;
            while i < b.len() && b[i] != 0x1b {
                i += 1;
            }
            out.push_str(&String::from_utf8_lossy(&b[start..i]));
            continue;
        }
        i += 1;
        match b.get(i) {
            // CSI: parameter bytes, intermediate bytes, one final byte
            Some(b'[') => {
                i += 1;
                while b.get(i).is_some_and(|c| (0x30..=0x3f).contains(c)) {
                    i += 1;
                }
                while b.get(i).is_some_and(|c| (0x20..=0x2f).contains(c)) {
                    i += 1;
                }
                if b.get(i).is_some_and(|c| (0x40..=0x7e).contains(c)) {
                    i += 1;
                }
            }
            // 7-bit C1 Fe other than CSI
            Some(c) if (0x40..=0x5a).contains(c) || (0x5c..=0x5f).contains(c) => i += 1,
            _ => {}
        }
    }
    out
}

/// XML rejects the C0 controls other than tab, newline and carriage return.
fn xml_text(s: &str) -> String {
    strip_ansi(s)
        .chars()
        .filter(|c| !matches!(*c as u32, 0x00..=0x08 | 0x0b | 0x0c | 0x0e..=0x1f))
        .collect()
}

fn xml_escape(s: &str, attr: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attr => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// `1.234` the way Python's `str(float)` prints it: never bare `1`.
fn py_float(v: f64) -> String {
    let s = format!("{v}");
    if s.contains('.') || s.contains('e') || s.contains("inf") || s.contains("NaN") {
        s
    } else {
        format!("{s}.0")
    }
}

/// report.py:dump_junit_xml: one `testcase` per attribute, the log tail as
/// the `failure` body.
pub fn dump_junit(w: &mut dyn Write, suite: &str, records: &[Record]) -> io::Result<()> {
    let failures = records.iter().filter(|r| !r.success).count();
    let skipped = records.iter().filter(|r| r.skipped).count();
    write!(
        w,
        "<testsuites><testsuite name=\"{}\" tests=\"{}\" failures=\"{failures}\" skipped=\"{skipped}\">",
        xml_escape(suite, true),
        records.len()
    )?;
    for r in records {
        write!(
            w,
            "<testcase classname=\"{}\" name=\"{}\" time=\"{}\"",
            r.kind.classname(),
            xml_escape(&r.attr, true),
            py_float(r.duration)
        )?;
        if r.skipped {
            write!(
                w,
                "><skipped message=\"{}\" /></testcase>",
                xml_escape(r.error.as_deref().unwrap_or(""), true)
            )?;
        } else if !r.success {
            let body = xml_text(r.log_output.as_deref().or(r.error.as_deref()).unwrap_or(""));
            write!(
                w,
                "><failure message=\"{}\" type=\"BuildFailure\">{}</failure></testcase>",
                xml_escape(r.error.as_deref().unwrap_or("<no message>"), true),
                xml_escape(&body, false)
            )?;
        } else {
            write!(w, " />")?;
        }
    }
    write!(w, "</testsuite></testsuites>")
}

/// `$GITHUB_STEP_SUMMARY` and the Gitea and Forgejo spellings
/// (report.py:get_ci_summary_file).
pub fn step_summary_file() -> Option<std::path::PathBuf> {
    for v in [
        "GITHUB_STEP_SUMMARY",
        "GITEA_STEP_SUMMARY",
        "FORGEJO_STEP_SUMMARY",
    ] {
        if let Some(p) = std::env::var_os(v)
            && !p.is_empty()
        {
            return Some(std::path::PathBuf::from(p));
        }
    }
    None
}

/// report.py:write_ci_summary. Failures first with the last 100 log lines in
/// a `<details>`, then the successes, collapsed.
pub fn step_summary(w: &mut dyn Write, records: &[Record], code: i32) -> io::Result<()> {
    let mut lines: Vec<String> = vec!["# nix-fast-build Results\n".into()];
    let ok: Vec<&Record> = records.iter().filter(|r| r.success).collect();
    let bad: Vec<&Record> = records.iter().filter(|r| !r.success).collect();
    if code == 0 {
        lines.push(format!(
            "## ✅ All Checks Passed ({} successful)\n",
            ok.len()
        ));
    } else {
        lines.push(format!(
            "## ❌ Build Failed ({} failed, {} successful)\n",
            bad.len(),
            ok.len()
        ));
    }
    fn of<'r>(rs: &[&'r Record], k: Kind) -> Vec<&'r Record> {
        rs.iter().copied().filter(|r| r.kind == k).collect()
    }
    let bad_eval = of(&bad, Kind::Eval);
    if !bad_eval.is_empty() {
        lines.push("\n### Failed Evaluations\n".into());
        for r in bad_eval {
            lines.push(format!("**`{}`**\n", r.attr));
            let Some(e) = &r.error else { continue };
            let el: Vec<&str> = e.trim().split('\n').collect();
            if el.len() > 3 {
                lines.push("<details>".into());
                lines.push("<summary>Error details</summary>\n".into());
                lines.push("```".into());
                lines.extend(el.iter().map(|l| (*l).to_string()));
                lines.push("```".into());
                lines.push("</details>\n".into());
            } else {
                lines.push(format!("Error: {e}\n"));
            }
        }
    }
    let bad_build = of(&bad, Kind::Build);
    if !bad_build.is_empty() {
        lines.push("\n### Failed Builds\n".into());
        for r in bad_build {
            lines.push(format!("\n**{}** (duration: {:.2}s)\n", r.attr, r.duration));
            match (&r.log_output, &r.error) {
                (Some(log), _) => {
                    let stripped = strip_ansi(log);
                    let mut ll: Vec<String> =
                        stripped.trim().split('\n').map(str::to_string).collect();
                    if ll.len() > 100 {
                        let tail = ll.split_off(ll.len() - 100);
                        ll = std::iter::once(
                            "... (truncated, showing last 100 lines) ...".to_string(),
                        )
                        .chain(tail)
                        .collect();
                    }
                    lines.push("\n<details>".into());
                    lines.push(format!(
                        "<summary>Build Log ({} lines)</summary>\n",
                        ll.len()
                    ));
                    lines.push("```".into());
                    lines.extend(ll);
                    lines.push("```".into());
                    lines.push("</details>\n".into());
                }
                (None, Some(e)) => lines.push(format!("Error: {e}\n")),
                (None, None) => {}
            }
        }
    }
    if !ok.is_empty() {
        lines.push("\n## Successful Operations\n".into());
        let ok_build = of(&ok, Kind::Build);
        if !ok_build.is_empty() {
            lines.push("\n<details>".into());
            lines.push(format!(
                "<summary>Built {} packages</summary>\n",
                ok_build.len()
            ));
            lines.extend(
                ok_build
                    .iter()
                    .map(|r| format!("- {} ({:.2}s)", r.attr, r.duration)),
            );
            lines.push("</details>\n".into());
        }
        let ok_eval = of(&ok, Kind::Eval);
        if !ok_eval.is_empty() {
            lines.push("\n<details>".into());
            lines.push(format!(
                "<summary>Evaluated {} attributes</summary>\n",
                ok_eval.len()
            ));
            lines.extend(ok_eval.iter().map(|r| format!("- {}", r.attr)));
            lines.push("</details>\n".into());
        }
    }
    w.write_all(lines.join("\n").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed_build() -> Record {
        Record {
            error: Some("build exited with 1".into()),
            success: false,
            log_output: Some("\x1b[31mboom\x1b[0m\nsecond\x01".into()),
            duration: 1.5,
            drv_path: Some("/nix/store/x.drv".into()),
            ..Record::build("checks.x86_64-linux.bad")
        }
    }

    #[test]
    fn json_keys_are_sorted_and_optionals_omitted() {
        let mut out = Vec::new();
        dump_json(&mut out, &[Record::eval("a"), failed_build()]).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("{\n  \"results\": [\n"));
        let a = text.find("\"attr\"").unwrap();
        let d = text.find("\"duration\"").unwrap();
        assert!(a < d, "attr before duration");
        assert!(!text.contains("\"skipped\""));
        assert!(text.contains("\"type\": \"BUILD\""));
        assert!(text.contains("\"drvPath\": \"/nix/store/x.drv\""));
    }

    #[test]
    fn junit_counts_and_escapes() {
        let mut out = Vec::new();
        let skipped = Record {
            skipped: true,
            error: Some("unsupported <system>".into()),
            ..Record::build("checks.aarch64-linux.x")
        };
        dump_junit(&mut out, "ietsp fast-build", &[failed_build(), skipped]).unwrap();
        let x = String::from_utf8(out).unwrap();
        assert!(x.contains("tests=\"2\" failures=\"1\" skipped=\"1\""));
        assert!(x.contains("classname=\"Build\""));
        assert!(x.contains("time=\"1.5\""));
        assert!(x.contains("type=\"BuildFailure\""));
        // ANSI and the C0 control are gone from the failure body
        assert!(x.contains(">boom\nsecond<"), "{x}");
        assert!(x.contains("message=\"unsupported &lt;system&gt;\""));
    }

    #[test]
    fn step_summary_collapses_failures() {
        let mut out = Vec::new();
        step_summary(&mut out, &[failed_build(), Record::build("ok")], 1).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("## ❌ Build Failed (1 failed, 1 successful)"));
        assert!(s.contains("### Failed Builds"));
        assert!(s.contains("<summary>Build Log (2 lines)</summary>"));
        assert!(s.contains("<summary>Built 1 packages</summary>"));
    }

    #[test]
    fn ansi_stripping() {
        assert_eq!(strip_ansi("\x1b[1;32mgreen\x1b[0m"), "green");
        assert_eq!(strip_ansi("plain"), "plain");
        // `]` is one of the 7-bit C1 Fe bytes, so it goes with the ESC
        assert_eq!(strip_ansi("a\x1b]0;titl"), "a0;titl");
    }

    #[test]
    fn python_float_format() {
        assert_eq!(py_float(0.0), "0.0");
        assert_eq!(py_float(1.5), "1.5");
        assert_eq!(py_float(3.0), "3.0");
    }
}
