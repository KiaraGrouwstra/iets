//! Purity and reproducibility: impure builtins, unhashed fetches, import
//! from derivation, fixed-output derivations, large or git-bearing source
//! copies. Hook: the evaluator at the call; `currentSystem` and
//! `currentTime` are constants, so the compiler reports their references.

use super::LintId;
use crate::eval::{EvalResult, Evaluator, Pos};
use crate::runtime::{Ctx, Value, Word};

/// Cheap prefilter for `reference`.
#[inline]
pub fn may_lint(name: &[u8]) -> bool {
    let bare = name.strip_prefix(b"__").unwrap_or(name);
    matches!(bare, b"currentSystem" | b"currentTime")
}

/// The lint a reference to the constant builtin `name` triggers.
pub fn reference(name: &str) -> Option<(LintId, String)> {
    match name.strip_prefix("__").unwrap_or(name) {
        "currentSystem" => Some((
            LintId::CurrentSystem,
            "`currentSystem` makes the result depend on the evaluating machine; take the system as an argument".into(),
        )),
        "currentTime" => Some((
            LintId::ImpureBuiltin,
            "`currentTime` is impure: the result changes with every run".into(),
        )),
        _ => None,
    }
}

pub fn get_env(ev: &mut Evaluator, name: &[u8], pos: Pos) -> EvalResult<()> {
    ev.lint(LintId::ImpureBuiltin, pos, || {
        format!(
            "`getEnv \"{}\"` is impure: the result depends on the environment",
            String::from_utf8_lossy(name)
        )
    })
}

const HASH_ATTRS: &[&[u8]] = &[b"sha256", b"hash", b"narHash", b"rev", b"outputHash"];

/// A fetcher call: `arg` is a URL string or an attrset; either way a hash
/// (or a `rev`) must pin the result.
pub fn unhashed_fetch(ev: &mut Evaluator, builtin: &str, arg: Word, pos: Pos) -> EvalResult<()> {
    if !ev.lint_on(LintId::UnhashedFetch) {
        return Ok(());
    }
    let w = ev.force(arg)?;
    let pinned = match w.view() {
        Value::Attrs(a) => a.iter().any(|attr| {
            let n = ev.interner.name(attr.sym);
            HASH_ATTRS.contains(&n)
        }),
        Value::String(_) => false,
        _ => return Ok(()),
    };
    if pinned {
        return Ok(());
    }
    ev.lint(LintId::UnhashedFetch, pos, || {
        format!(
            "`{builtin}` is called without a hash, narHash or rev: the result is not reproducible"
        )
    })
}

/// `import` of a string whose context names a derivation.
pub fn ifd(ev: &mut Evaluator, ctx: &[Ctx], pos: Pos) -> EvalResult<()> {
    let drv = ctx.iter().find_map(|c| match c {
        Ctx::DrvDeep(d) | Ctx::Built { drv: d, .. } => Some(*d),
        Ctx::Opaque(_) | Ctx::Impure(_) => None,
    });
    let Some(drv) = drv else {
        return Ok(());
    };
    ev.lint(LintId::Ifd, pos, || {
        format!(
            "import from derivation: this import needs '{}' built first",
            String::from_utf8_lossy(drv)
        )
    })
}

pub fn fod(ev: &mut Evaluator, name: &str, mode: &str, algo: &str, pos: Pos) -> EvalResult<()> {
    ev.lint(LintId::Fod, pos, || {
        format!("derivation '{name}' is fixed-output ({mode}, {algo})")
    })
}

/// A recursive copy of `path` whose NAR has `size` bytes.
pub fn large_source_copy(ev: &mut Evaluator, path: &[u8], size: u64, pos: Pos) -> EvalResult<()> {
    let max = ev.lints.config.max_bytes;
    if size <= max {
        return Ok(());
    }
    let shown = ev.lint_shown_path(path);
    ev.lint(LintId::LargeSourceCopy, pos, || {
        format!(
            "copying '{}' to the store takes {} bytes of NAR, over the {} byte limit; filter the source",
            shown,
            size,
            max
        )
    })
}

/// A string coercion copied the directory `path` whole: no filter, and not
/// a tree the flake machinery already turned into a tracked file set.
pub fn dir_copy_unfiltered(ev: &mut Evaluator, path: &[u8], pos: Pos) -> EvalResult<()> {
    ev.lint(LintId::DirCopyUnfiltered, pos, || {
        format!(
            "directory '{}' is copied to the store whole, untracked and ignored files included; use `builtins.path {{ path = ...; filter = ...; }}`",
            String::from_utf8_lossy(path)
        )
    })
}

/// The nearest ancestor of `path` (or `path`) that holds a `flake.nix`.
/// Every probe is a `Stat` observation, so a new `flake.nix` is a cache miss.
fn flake_root_of(ev: &Evaluator, path: &[u8]) -> Option<Vec<u8>> {
    let mut dir = crate::compile::canon_path(path);
    loop {
        let mut probe = dir.clone();
        probe.extend_from_slice(b"/flake.nix");
        if ev.trace.stat_now(&probe, true) == crate::trace::Kind::Regular {
            return Some(dir);
        }
        let i = dir.iter().rposition(|&b| b == b'/')?;
        if i == 0 {
            return None;
        }
        dir.truncate(i);
    }
}

/// Derivation `drv_name` received attribute `key` whose value carries the
/// text of the non-store path `path`: `absolute-path-in-drv` under a
/// flake root, `impure-path-in-drv` elsewhere.
pub fn path_in_drv(
    ev: &mut Evaluator,
    drv_name: &str,
    key: &str,
    path: &[u8],
    attr_pos: Pos,
) -> EvalResult<()> {
    match flake_root_of(ev, path) {
        Some(root) => ev.lint(LintId::AbsolutePathInDrv, attr_pos, || {
            format!(
                "derivation '{drv_name}': attribute '{key}' holds the absolute path '{}' of the flake at '{}'; interpolate the path to copy it, or pass a relative name",
                String::from_utf8_lossy(path),
                String::from_utf8_lossy(&root)
            )
        }),
        None => ev.lint(LintId::ImpurePathInDrv, attr_pos, || {
            format!(
                "derivation '{drv_name}': attribute '{key}' holds the checkout path '{}', so the .drv depends on where the repository lives; interpolate the path to copy it to the store",
                String::from_utf8_lossy(path)
            )
        }),
    }
}

/// A recursive copy of `path` whose top level holds a `.git` entry.
pub fn git_in_source(ev: &mut Evaluator, path: &[u8], pos: Pos) -> EvalResult<()> {
    ev.lint(LintId::GitInSource, pos, || {
        format!(
            "copying '{}' to the store includes its `.git` entry; filter the source or use a git fetcher",
            String::from_utf8_lossy(path)
        )
    })
}
