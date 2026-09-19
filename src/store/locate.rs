//! Where the local store keeps an input source, and the derivations a build
//! result names.

use std::collections::HashMap;

use crate::error::Error as CrateError;
use crate::eval::{Error, Evaluator, Pos};
use crate::runtime::Word;

/// Answers the plan's locator from the local Nix store: one `nix path-info --json` call
/// tells how each input source was content-addressed and what it references. Paths named
/// by `--source` are read from the given local tree instead and are plain sources. A path
/// that is not valid locally, or whose content address v1 cannot send (an input-addressed
/// output pinned with `builtins.storePath`), is `None`: the daemon substitutes it.
pub fn locate_in_store(
    paths: &[crate::store::path::StorePath],
    overrides: &HashMap<String, std::path::PathBuf>,
) -> crate::error::Result<Vec<Option<crate::remote::plan::SourceInfo>>> {
    use crate::remote::plan::{SourceData, SourceInfo, SourceKind};
    use crate::store::path::{STORE_DIR, StorePath};

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
            if let Some(local) = overrides.get(&full) {
                return Ok(Some(SourceInfo::tree(local.clone())));
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
            Ok(Some(SourceInfo {
                data: SourceData::Local {
                    path: std::path::PathBuf::from(format!("{STORE_DIR}/{}", p.base_name())),
                    accepted: None,
                },
                kind,
                hashed: None,
            }))
        })
        .collect()
}

/// The derivations a build result names: an attrset with `drvPath`, a string with derivation
/// context, or a list of those. `!out!drv` context also names the wanted output.
pub fn drv_roots(
    ev: &mut Evaluator,
    v: Word,
    roots: &mut Vec<(crate::store::path::StorePath, Option<String>)>,
) -> Result<(), Box<Error>> {
    use crate::runtime::Ctx;
    use crate::store::path::StorePath;
    let bad = |msg: String| Error::boxed(crate::eval::ErrorKind::Eval, msg, Pos::NONE);
    let parse = |p: &[u8]| {
        StorePath::parse(&String::from_utf8_lossy(p))
            .map_err(|e| Error::boxed(crate::eval::ErrorKind::Eval, e.0, Pos::NONE))
    };
    let v = ev.force(v)?;
    if let Some(attrs) = v.as_attrs() {
        let Some(a) = attrs.get(ev.s.drv_path) else {
            return Err(bad(
                "expression evaluated to an attrset without 'drvPath'".into()
            ));
        };
        let s = ev.force_string(a.cell.get(), Pos::NONE, "while evaluating 'drvPath'")?;
        let p = parse(s.bytes)?;
        if !p.is_derivation() {
            return Err(bad(format!("'{p}' is not a derivation path")));
        }
        roots.push((p, None));
        return Ok(());
    }
    if let Some(s) = v.as_string() {
        let mut found = false;
        for c in s.ctx {
            match c {
                Ctx::Built { drv, output } => {
                    let out = String::from_utf8_lossy(output).into_owned();
                    roots.push((parse(drv)?, Some(out)));
                    found = true;
                }
                Ctx::DrvDeep(drv) => {
                    roots.push((parse(drv)?, None));
                    found = true;
                }
                Ctx::Opaque(_) => {}
            }
        }
        if !found {
            return Err(bad(format!(
                "string '{}' has no derivation in its context",
                String::from_utf8_lossy(s.bytes)
            )));
        }
        return Ok(());
    }
    if let Some(l) = v.as_list() {
        for c in l.cells() {
            drv_roots(ev, c.get(), roots)?;
        }
        return Ok(());
    }
    Err(bad(
        "expression must evaluate to a derivation, a string with derivation context, or a list of those".into(),
    ))
}
