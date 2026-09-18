//! Locked flakes: `PATH#attr.path` on the command line, evaluated through
//! Nix 2.34.8's `call-flake.nix` (embedded verbatim) with `fetchTree` for
//! the locked inputs and the root tree copied like Nix copies it. Only a
//! complete `flake.lock` is accepted; nothing is locked or looked up in a
//! registry. See docs/flakes.md.

pub mod fetch;
pub mod lock;
pub mod root;

use std::path::{Path, PathBuf};

use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos};
use crate::runtime::Word;

pub const CALL_FLAKE: &str = include_str!("call-flake.nix");
pub const LOCK_ERROR: &str = "ietsp: flake.lock missing or incomplete, run nix flake lock";

/// Where the root flake's tree comes from (flakeref.cc: a directory inside
/// a git repository is `git+file://`, everything else is `path:`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Path(PathBuf),
    Git { root: PathBuf, subdir: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlakeRef {
    /// The directory holding `flake.nix` and `flake.lock`.
    pub dir: PathBuf,
    pub source: Source,
    /// The attribute path after `#`, selected from the flake's outputs.
    pub attr: Vec<String>,
}

/// A command-line input that names a flake rather than a `.nix` file: it
/// has a `#`, a `path:` prefix, or is a directory with `flake.nix` and no
/// `default.nix`.
pub fn is_flake_ref(arg: &str) -> bool {
    if arg.contains('#') || arg.starts_with("path:") {
        return true;
    }
    let p = Path::new(arg);
    p.join("flake.nix").is_file() && !p.join("default.nix").exists()
}

/// `PATH[#attr.path]` where PATH is `.`, `./x`, `/abs`, `~/x` or `path:/abs`.
pub fn parse_flake_ref(arg: &str, cwd: &[u8], home: Option<&[u8]>) -> Result<FlakeRef, String> {
    let (loc, frag) = match arg.split_once('#') {
        Some((l, f)) => (l, Some(f)),
        None => (arg, None),
    };
    if loc.contains('?') {
        return Err(format!(
            "ietsp: query parameters in flake reference '{arg}' are not supported"
        ));
    }
    let (explicit_path, loc) = match loc.strip_prefix("path:") {
        Some(rest) => (true, rest),
        None => (false, loc),
    };
    if loc.contains(':') && !loc.starts_with('/') && !loc.starts_with('.') && !loc.starts_with('~')
    {
        return Err(format!(
            "ietsp: flake reference '{arg}' is not a path; only local paths are supported"
        ));
    }
    let mut full: Vec<u8> = if loc == "~" || loc.starts_with("~/") {
        let Some(h) = home else {
            return Err("ietsp: $HOME is not set".to_string());
        };
        let mut v = h.to_vec();
        v.extend_from_slice(&loc.as_bytes()[1..]);
        v
    } else if loc.starts_with('/') {
        loc.as_bytes().to_vec()
    } else {
        let mut v = cwd.to_vec();
        v.push(b'/');
        v.extend_from_slice(loc.as_bytes());
        v
    };
    if full.is_empty() {
        full = cwd.to_vec();
    }
    let full = crate::compile::canon_path(&full);
    let dir = PathBuf::from(<std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(&full));
    if !dir.is_dir() {
        return Err(format!(
            "path '{}' is not a flake (because it's not a directory)",
            dir.display()
        ));
    }
    if !dir.join("flake.nix").is_file() {
        return Err(format!(
            "path '{}' is not a flake (because it doesn't contain a 'flake.nix' file)",
            dir.display()
        ));
    }
    let source = if explicit_path {
        Source::Path(dir.clone())
    } else {
        git_root(&dir).map_or_else(
            || Source::Path(dir.clone()),
            |root| Source::Git {
                subdir: dir
                    .strip_prefix(&root)
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                root,
            },
        )
    };
    let attr = match frag {
        Some(f) => parse_attr_path(f)?,
        None => Vec::new(),
    };
    Ok(FlakeRef { dir, source, attr })
}

/// The nearest ancestor (or `dir` itself) holding a `.git` entry.
fn git_root(dir: &Path) -> Option<PathBuf> {
    let mut r = dir.to_path_buf();
    loop {
        if r.join(".git").exists() {
            return Some(r);
        }
        if !r.pop() {
            return None;
        }
    }
}

/// attr-path.cc:parseAttrPath: `.` separates, `"..."` quotes.
pub fn parse_attr_path(s: &str) -> Result<Vec<String>, String> {
    let mut res = Vec::new();
    let mut cur = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match c {
            '.' => res.push(std::mem::take(&mut cur)),
            '"' => loop {
                match it.next() {
                    None => {
                        return Err(format!("missing closing quote in selection path '{s}'"));
                    }
                    Some('"') => break,
                    Some(c) => cur.push(c),
                }
            },
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        res.push(cur);
    }
    Ok(res)
}

fn read_traced(ev: &Evaluator, p: &Path) -> std::io::Result<Vec<u8>> {
    let meta = std::fs::metadata(p)?;
    let bytes = std::fs::read(p)?;
    ev.trace.file(
        <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::as_bytes(p.as_os_str()),
        &meta,
        &bytes,
    );
    Ok(bytes)
}

/// Evaluates the flake: the `callFlake` result (`_type`, `inputs`, `outputs`,
/// `sourceInfo` and its fields), as `nix-eval-jobs --flake DIR` walks it.
pub fn eval_flake_value(ev: &mut Evaluator, fr: &FlakeRef) -> EvalResult<Word> {
    let pos = Pos::NONE;
    let lock_bytes = read_traced(ev, &fr.dir.join("flake.lock"))
        .map_err(|_| Error::boxed(ErrorKind::Io, LOCK_ERROR, pos))?;
    let lock = lock::check(&lock_bytes)
        .map_err(|why| Error::boxed(ErrorKind::Eval, format!("{LOCK_ERROR} ({why})"), pos))?;

    let (source_info, subdir) = root::source_info(ev, &fr.source, pos)?;
    let out_path = {
        let ctx = format!(
            "while evaluating the sourceInfo of flake '{}'",
            fr.dir.display()
        );
        let a = ev.force_attrs(source_info, pos, &ctx)?;
        let Some(attr) = a.get(ev.s.out_path) else {
            return Err(Error::boxed(
                ErrorKind::MissingAttr,
                format!(
                    "flake '{}': sourceInfo has no attribute 'outPath'",
                    fr.dir.display()
                ),
                pos,
            ));
        };
        ev.force_string(attr.cell.get(), pos, "while evaluating the flake's outPath")?
            .bytes
            .to_vec()
    };
    let mut flake_dir = out_path.clone();
    if !subdir.is_empty() {
        flake_dir.push(b'/');
        flake_dir.extend_from_slice(subdir.as_bytes());
    }
    check_declared_inputs(ev, &flake_dir, &lock, pos)?;

    let ir = ev.compile_source(
        b"\xc2\xabcall-flake\xc2\xbb",
        CALL_FLAKE.as_bytes().to_vec(),
        b"/",
        None,
    )?;
    let root_env = ev.root_env;
    let call_flake = ev.eval(root_env, ir)?;
    let lock_str = ev.mk_string(&lock_bytes);
    let dir_w = ev.mk_string(subdir.as_bytes());
    let mut ov = vec![
        (ev.sym(b"sourceInfo"), pos, source_info),
        (ev.sym(b"dir"), pos, dir_w),
    ];
    let ov = ev.mk_attrs(&mut ov, pos)?;
    let mut overrides = vec![(ev.sym(lock.root.as_bytes()), pos, ov)];
    let overrides = ev.mk_attrs(&mut overrides, pos)?;
    let fetch_sym = ev.sym(b"fetchTree");
    let fetch_tree = ev
        .globals
        .builtins
        .and_then(|b| b.get(fetch_sym))
        .map(|a| a.cell.get())
        .expect("builtins.fetchTree is registered");
    ev.apply(call_flake, &[lock_str, overrides, fetch_tree], pos)
}

/// Evaluates the flake and selects `attr` from its outputs (or returns the
/// outputs attrset for an empty path).
pub fn eval_flake(ev: &mut Evaluator, fr: &FlakeRef) -> EvalResult<Word> {
    let pos = Pos::NONE;
    let result = eval_flake_value(ev, fr)?;

    let attrs = ev.force_attrs(result, pos, "while evaluating the flake")?;
    let outputs_sym = ev.sym(b"outputs");
    let Some(outputs) = attrs.get(outputs_sym) else {
        return Err(Error::boxed(
            ErrorKind::MissingAttr,
            format!("flake '{}' has no attribute 'outputs'", fr.dir.display()),
            pos,
        ));
    };
    let mut v = outputs.cell.get();
    for (i, name) in fr.attr.iter().enumerate() {
        let a = ev.force_attrs(v, pos, "while selecting a flake output attribute")?;
        let sym = ev.sym(name.as_bytes());
        let Some(attr) = a.get(sym) else {
            return Err(Error::boxed(
                ErrorKind::MissingAttr,
                format!(
                    "flake '{}' does not provide attribute '{}'",
                    fr.dir.display(),
                    fr.attr[..=i].join(".")
                ),
                pos,
            ));
        };
        v = attr.cell.get();
    }
    ev.force(v)
}

/// eval-cache.cc:AttrCursor::findAlongAttrPath: walks `path` from `v`, forcing each
/// step; `None` when a step is missing or the value at a step is not an attrset.
pub fn select(ev: &mut Evaluator, mut v: Word, path: &[String]) -> EvalResult<Option<Word>> {
    for name in path {
        let forced = ev.force(v)?;
        let Some(attrs) = forced.as_attrs() else {
            return Ok(None);
        };
        let sym = ev.sym(name.as_bytes());
        let Some(a) = attrs.get(sym) else {
            return Ok(None);
        };
        v = a.cell.get();
    }
    Ok(Some(v))
}

/// Every input `flake.nix` declares must have a lock entry; Nix would
/// otherwise fetch and lock it.
fn check_declared_inputs(
    ev: &mut Evaluator,
    flake_dir: &[u8],
    lock: &lock::Lock,
    pos: Pos,
) -> EvalResult<()> {
    let mut p = flake_dir.to_vec();
    p.extend_from_slice(b"/flake.nix");
    let v = ev.eval_file(&p, pos)?;
    let attrs = ev.force_attrs(v, pos, "while evaluating flake.nix")?;
    let inputs_sym = ev.sym(b"inputs");
    let Some(inputs) = attrs.get(inputs_sym) else {
        return Ok(());
    };
    let inputs = ev.force_attrs(
        inputs.cell.get(),
        pos,
        "while evaluating the 'inputs' attribute of flake.nix",
    )?;
    for a in inputs.iter() {
        let name = ev.interner.name(a.sym).to_vec();
        if !lock.root_inputs().any(|k| k.as_bytes() == name) {
            return Err(Error::boxed(
                ErrorKind::Eval,
                format!(
                    "{LOCK_ERROR} (input '{}' has no lock entry)",
                    String::from_utf8_lossy(&name)
                ),
                pos,
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attr_paths() {
        assert_eq!(
            parse_attr_path("devShells.x86_64-linux.default.drvPath").unwrap(),
            ["devShells", "x86_64-linux", "default", "drvPath"]
        );
        assert_eq!(
            parse_attr_path("packages.\"x86_64-linux\".\"a.b\"").unwrap(),
            ["packages", "x86_64-linux", "a.b"]
        );
        assert_eq!(parse_attr_path("").unwrap(), Vec::<String>::new());
        assert!(parse_attr_path("a.\"b").is_err());
    }

    #[test]
    fn refs_resolve_against_cwd_home_and_git() {
        let d = crate::store::nar::tests::fixture_tree("flakeref");
        std::fs::write(d.join("flake.nix"), b"{ outputs = _: {}; }").unwrap();
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("sub/flake.nix"), b"{ outputs = _: {}; }").unwrap();
        let cwd = d.as_os_str().as_encoded_bytes();
        let r = parse_flake_ref(".#a.b", cwd, None).unwrap();
        assert_eq!(r.dir, d);
        assert_eq!(r.source, Source::Path(d.clone()));
        assert_eq!(r.attr, ["a", "b"]);
        let r = parse_flake_ref("./sub", cwd, None).unwrap();
        assert_eq!(r.dir, d.join("sub"));
        assert!(r.attr.is_empty());
        let r = parse_flake_ref("~/sub#", b"/", Some(cwd)).unwrap();
        assert_eq!(r.dir, d.join("sub"));
        assert!(parse_flake_ref("./missing", cwd, None).is_err());
        assert!(parse_flake_ref("github:a/b", cwd, None).is_err());

        std::fs::create_dir_all(d.join(".git")).unwrap();
        let r = parse_flake_ref(&format!("{}/sub#x", d.display()), b"/", None).unwrap();
        assert_eq!(
            r.source,
            Source::Git {
                root: d.clone(),
                subdir: "sub".into()
            }
        );
        let r = parse_flake_ref(&format!("path:{}/sub", d.display()), b"/", None).unwrap();
        assert_eq!(r.source, Source::Path(d.join("sub")));
        assert!(is_flake_ref(&format!("{}/sub", d.display())));
        assert!(!is_flake_ref("x.nix"));
    }
}
