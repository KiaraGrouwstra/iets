//! `builtins.fetchGit` (fetchTree.cc with `isFetchGit`, git.cc). A local
//! working tree without a `ref` or a `rev` is copied the way the flake's own
//! tree is (git.cc:getAccessorFromWorkdir), so the tracing cache follows its
//! files. A commit goes to Nix's own `fetchGit`, as a git flake input does.

use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::fetch::{emit_tree_attrs, input_attrs};
use crate::error;
use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos, bail};
use crate::flake::{eval_error, flakeref};
use crate::lint::purity;
use crate::log;
use crate::runtime::Word;
use crate::source::{self, AttrVal, Attrs, Tree, bool_attr, scm, str_attr};
use crate::store::path::StorePath;
use crate::trace::Kind;

const NULL_REV: &str = "0000000000000000000000000000000000000000";

/// git.cc:GitInputScheme::allowedAttrs.
const ALLOWED: &[&str] = &[
    "url",
    "ref",
    "rev",
    "shallow",
    "submodules",
    "lfs",
    "exportIgnore",
    "lastModified",
    "revCount",
    "narHash",
    "allRefs",
    "name",
    "dirtyRev",
    "dirtyShortRev",
    "verifyCommit",
    "keytype",
    "publicKey",
    "publicKeys",
];

const FLAGS: &[&str] = &[
    "shallow",
    "submodules",
    "lfs",
    "exportIgnore",
    "allRefs",
    "verifyCommit",
];

/// The attributes the copy of a working tree reads; any other goes to Nix.
const FROM_WORKDIR: &[&str] = &[
    "url",
    "name",
    "shallow",
    "submodules",
    "lfs",
    "exportIgnore",
    "allRefs",
];

pub fn fetch_git(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    purity::unhashed_fetch(ev, "fetchGit", args[0], pos)?;
    let mut args = call_attrs(ev, args[0], pos)?;
    let Some(url) = str_attr(&args, "url").map(String::from) else {
        bail!(ErrorKind::Eval, pos, "input attribute 'url' is missing");
    };
    let fixed = match flakeref::fix_git_url(&url) {
        Ok(u) => Some(u),
        Err(e @ error::Error::Unsupported(_)) if ev.pure() => return Err(eval_error(e, pos)),
        Err(error::Error::Unsupported(_)) => None,
        Err(e) => return Err(eval_error(e, pos)),
    };
    let submodules = bool_attr(&args, "submodules").unwrap_or(false);
    if !submodules {
        args.entry("exportIgnore".into())
            .or_insert(AttrVal::Bool(true));
    }
    if ev.pure() {
        check_locked(&args, fixed.as_deref().unwrap_or(&url), pos)?;
    }
    let root = fixed.as_deref().and_then(workdir_root);
    if let Some(root) = root.filter(|_| args.keys().all(|k| FROM_WORKDIR.contains(&k.as_str())))
        && let Some(tree) = from_workdir(ev, &args, Path::new(&root), pos)?
    {
        return emit_tree_attrs(ev, &tree, None, pos);
    }
    let shown = input_string(&args, fixed.as_deref().unwrap_or(&url)).unwrap_or(url);
    from_nix(ev, &args, &shown, pos)
}

/// fetchTree.cc:fetchTree with `isFetchGit`: a string is the URL, and an
/// attribute set has no `type`.
fn call_attrs(ev: &mut Evaluator, arg: Word, pos: Pos) -> EvalResult<Attrs> {
    let w = ev.force(arg)?;
    let Some(a) = w.as_attrs() else {
        let mut ctx = Vec::new();
        let what = "while evaluating the first argument passed to 'fetchGit'";
        let url = ev.coerce_to_string(w, &mut ctx, pos, what, false, false, true)?;
        let url = String::from_utf8_lossy(&url).into_owned();
        return Ok(Attrs::from([("url".into(), AttrVal::Str(url))]));
    };
    if a.get(ev.sym(b"type")).is_some() {
        bail!(ErrorKind::Eval, pos, "unexpected argument 'type'");
    }
    let attrs = input_attrs(ev, a, "fetchGit", pos)?;
    // fetchers.cc:Input::fromAttrs, then git.cc:inputFromAttrs
    if let Some(k) = attrs.keys().find(|k| !ALLOWED.contains(&k.as_str())) {
        bail!(
            ErrorKind::Eval,
            pos,
            "input attribute '{k}' not supported by scheme 'git'"
        );
    }
    if let Some(k) = FLAGS.iter().find(|k| {
        attrs
            .get(**k)
            .is_some_and(|v| !matches!(v, AttrVal::Bool(_)))
    }) {
        bail!(
            ErrorKind::Eval,
            pos,
            "input attribute '{k}' is not a Boolean"
        );
    }
    Ok(attrs)
}

/// fetchers.cc:Input::to_string of the input `args` make.
fn input_string(args: &Attrs, url: &str) -> Option<String> {
    let mut input = args.clone();
    input.insert("type".into(), AttrVal::Str("git".into()));
    input.insert("url".into(), AttrVal::Str(url.into()));
    flakeref::attrs_to_string(input).ok()
}

/// fetchTree.cc:fetchTree: a pure evaluation fetches a `rev` that is not
/// the null revision, or warns and fetches what a `narHash` checks.
fn check_locked(args: &Attrs, url: &str, pos: Pos) -> EvalResult<()> {
    if str_attr(args, "rev").is_some_and(|r| r != NULL_REV) {
        return Ok(());
    }
    let shown = match input_string(args, url) {
        Some(s) => s,
        None => bail!(
            ErrorKind::Unsupported,
            pos,
            "an unlocked Git input '{url}' in pure evaluation mode is not supported"
        ),
    };
    if args.contains_key("narHash") {
        log::line(&format!(
            "warning: Input '{shown}' is unlocked (e.g. lacks a Git revision) but is checked by NAR hash. \
             This is not reproducible and will break after garbage collection or when shared."
        ));
        return Ok(());
    }
    bail!(
        ErrorKind::Eval,
        pos,
        "in pure evaluation mode, 'fetchGit' doesn't fetch unlocked input '{shown}'"
    )
}

/// git.cc:getRepoInfo: the path of a `file:` URL, when it is absolute. It
/// is a working tree when it has a `.git`; any other is a repository.
fn workdir_root(url: &str) -> Option<String> {
    flakeref::file_url_path(url).filter(|p| p.starts_with('/'))
}

/// git.cc:getAccessorFromWorkdir, with fetchTree.cc:emitTreeAttrs'
/// `emptyRevFallback`. `None` when Nix must answer: the tree is not a
/// working tree, it has submodules to copy, or its count needs history
/// that a shallow clone does not have.
fn from_workdir(
    ev: &mut Evaluator,
    args: &Attrs,
    root: &Path,
    pos: Pos,
) -> EvalResult<Option<Tree>> {
    let git = scm::named("git").expect("iets knows git");
    let marker = git.root_marker(root);
    if ev.trace().stat_now(marker.as_os_str().as_bytes(), true) == Kind::Missing {
        return Ok(None);
    }
    let submodules = bool_attr(args, "submodules").unwrap_or(false);
    if submodules {
        let gitmodules = root.join(".gitmodules");
        let found = ev
            .trace()
            .stat_now(gitmodules.as_os_str().as_bytes(), false);
        if found != Kind::Missing {
            return Ok(None);
        }
    }
    let name = str_attr(args, "name").unwrap_or("source");
    let export = bool_attr(args, "exportIgnore").unwrap_or(false);
    let (mut tree, dirty) = source::workdir(ev, git, root, name, export, pos)?;
    let shallow = bool_attr(args, "shallow").unwrap_or(false);
    if !dirty && !shallow && tree.counter.is_none() {
        return Ok(None);
    }
    if dirty {
        log::line(&format!("warning: Git tree '{}' is dirty", root.display()));
    }
    if shallow {
        tree.counter = None;
    }
    let a = &mut tree.attrs;
    a.insert("submodules".into(), AttrVal::Bool(submodules));
    a.entry("rev".into())
        .or_insert_with(|| AttrVal::Str(NULL_REV.into()));
    if tree.counter.is_none() {
        a.insert("revCount".into(), AttrVal::Int(0));
    }
    Ok(Some(tree))
}

/// Nix's `fetchGit` with the call's own attributes. A result with a `rev`
/// asked for follows from it, so only the store path is an observation;
/// any other depends on the repository now, and the run is uncacheable.
fn from_nix(ev: &mut Evaluator, args: &Attrs, shown: &str, pos: Pos) -> EvalResult<Word> {
    let locked = str_attr(args, "rev").is_some_and(|r| r != NULL_REV);
    if !locked {
        ev.trace().mark_uncacheable();
    }
    let got = source::fetch_git_with_nix(args, shown, pos)?;
    let mut attrs = Attrs::from([("type".into(), AttrVal::Str("git".into()))]);
    for (k, v) in &got {
        let v = match v {
            serde_json::Value::String(s) => AttrVal::Str(s.clone()),
            serde_json::Value::Bool(b) => AttrVal::Bool(*b),
            serde_json::Value::Number(n) => match n.as_u64() {
                Some(n) => AttrVal::Int(n),
                None => continue,
            },
            _ => continue,
        };
        attrs.insert(k.clone(), v);
    }
    let bad = || Error::boxed(ErrorKind::Eval, "iets: nix gave no store path", pos);
    let out = str_attr(&attrs, "outPath").ok_or_else(bad)?;
    let path = StorePath::parse(out).map_err(|_| bad())?;
    let mut input = Attrs::new();
    for k in ["type", "rev", "narHash"] {
        input.extend(attrs.get(k).map(|v| (k.to_string(), v.clone())));
    }
    input.extend(args.get("url").map(|v| ("url".to_string(), v.clone())));
    source::ensure_readable(ev, &path, &input, pos)?;
    let tree = Tree {
        path,
        attrs,
        subdir: String::new(),
        counter: None,
    };
    emit_tree_attrs(ev, &tree, None, pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_working_tree_is_a_file_url_of_an_absolute_path() {
        for (url, root) in [
            ("/r/a b", Some("/r/a b")),
            ("file:///r/x", Some("/r/x")),
            ("git+file:///r/x", Some("/r/x")),
            ("rel/x", None),
            ("https://x.org/a.git", None),
        ] {
            let fixed = flakeref::fix_git_url(url).unwrap();
            assert_eq!(workdir_root(&fixed).as_deref(), root, "{url}");
        }
    }
}
