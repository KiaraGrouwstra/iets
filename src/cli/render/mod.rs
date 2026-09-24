//! What a command puts on a terminal: the stdout writer, the JSON dump and
//! the status line. Presentation is porcelain-side and may be shared
//! freely; the values themselves are not (docs/plumbing.md).

use serde_json::Value;

/// Everything a command writes on stdout goes through here. Nothing is
/// added: `b` is the output.
pub fn bytes(b: &[u8]) {
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = out.write_all(b);
    let _ = out.flush();
}

/// `nlohmann::json::dump`: an object's keys are sorted, `--pretty` indents
/// by two spaces, and the value ends in a newline, all of which
/// `serde_json` does the same way.
pub fn json(v: &Value, pretty: bool) -> Vec<u8> {
    let mut out = if pretty {
        serde_json::to_vec_pretty(v)
    } else {
        serde_json::to_vec(v)
    }
    .unwrap_or_default();
    out.push(b'\n');
    out
}

/// `--pretty`'s default: a terminal on stdout.
pub fn pretty() -> bool {
    unsafe { libc::isatty(1) == 1 }
}

/// A status line on stderr: what a run did, not what it produced.
pub fn status(text: &str) {
    crate::log::line(text);
}
