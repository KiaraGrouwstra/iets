//! Where the local store keeps an input source.

use std::collections::HashMap;

use crate::error::Error as CrateError;

/// Answers the plan's locator from the local Nix store: one `nix path-info --json` call
/// tells how each input source was content-addressed and what it references. Paths named
/// by `--source` are read from the given local tree instead and are plain sources. A path
/// that is not valid locally, or whose content address v1 cannot send (an input-addressed
/// output pinned with `builtins.storePath`), is `None`: the daemon substitutes it.
pub fn locate_in_store(
    paths: &[crate::store::path::StorePath],
    overrides: &HashMap<String, std::path::PathBuf>,
) -> crate::error::Result<Vec<Option<crate::store::source::SourceOrigin>>> {
    use crate::store::path::{STORE_DIR, StorePath};
    use crate::store::source::{SourceKind, SourceOrigin, SrcPath};

    let ask: Vec<String> = paths
        .iter()
        .map(|p| p.to_string())
        .filter(|p| !overrides.contains_key(p))
        .collect();
    let mut info: HashMap<String, serde_json::Value> = HashMap::new();
    if !ask.is_empty() {
        let out = std::process::Command::new(crate::nixbin::nix())
            .args([
                "--extra-experimental-features",
                "nix-command",
                "path-info",
                "--json",
            ])
            .args(&ask)
            .stderr(std::process::Stdio::inherit())
            .output()
            .map_err(|e| CrateError::io("nix path-info", e))?;
        if !out.status.success() {
            return Err(CrateError::Protocol("nix path-info failed".into()));
        }
        info = serde_json::from_slice(&out.stdout)
            .map_err(|e| CrateError::Protocol(format!("nix path-info: {e}")))?;
    }
    paths
        .iter()
        .map(|p| {
            let full = p.to_string();
            let unhashed = |at, kind| SourceOrigin {
                at,
                kind,
                nar: None,
            };
            if let Some(local) = overrides.get(&full) {
                return Ok(Some(unhashed(
                    SrcPath::fs(local.clone(), None),
                    SourceKind::Source,
                )));
            }
            // `null` is what `nix path-info` prints for a path that is not valid.
            let v = match info.get(&full) {
                Some(v) if !v.is_null() => v,
                _ => return Ok(None),
            };
            let ca = v["ca"].as_str().unwrap_or("");
            let kind = if ca.starts_with("fixed:r:sha256:") {
                SourceKind::Source
            } else if ca.starts_with("text:sha256:") {
                let references = v["references"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|r| r.as_str()).collect::<Vec<_>>())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|r| StorePath::parse(r).map_err(CrateError::from))
                    .collect::<crate::error::Result<_>>()?;
                SourceKind::Text { references }
            } else {
                return Ok(None);
            };
            Ok(Some(unhashed(
                SrcPath::fs(format!("{STORE_DIR}/{}", p.base_name()), None),
                kind,
            )))
        })
        .collect()
}
