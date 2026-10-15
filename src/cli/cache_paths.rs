//! `iets cache paths`: the printing over `api::watch::watch_paths`.

use std::path::Path;

use crate::api::watch;
use crate::cli::render;
use crate::log;

/// NUL-separated paths on stdout, or `--json` with the kind of each.
pub fn cmd_cache_paths(file: &str, json: bool) -> i32 {
    let w = match watch::watch_paths(Path::new(file)) {
        Ok(w) => w,
        Err(e) => return e.report(),
    };
    if w.len() == watch::MAX {
        log::line(&format!(
            "iets: the answer may depend on more than {0} paths; the first {0} are printed",
            crate::api::watch::MAX
        ));
    }
    if json {
        let a: Vec<serde_json::Value> = w
            .iter()
            .map(|x| serde_json::json!({ "path": x.path, "kind": x.kind }))
            .collect();
        render::line(&serde_json::Value::Array(a).to_string());
    } else {
        let mut out = Vec::new();
        for x in &w {
            out.extend_from_slice(x.path.as_bytes());
            out.push(0);
        }
        render::bytes(&out);
    }
    0
}
