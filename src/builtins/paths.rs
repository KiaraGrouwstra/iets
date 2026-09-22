//! File system primops (primops.cc): readFile, readDir, readFileType,
//! pathExists, toPath, storePath, filterSource, path, toFile. The network
//! fetchers and flake primops are registered so `builtins ? fetchGit`
//! holds, but they fail when called; only `fetchTree` on a locked input,
//! `fetchTarball` with a `sha256` and `getFlake` work (`flake`).

use std::collections::{BTreeSet, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::Registry;
use super::store_hooks::{self, At};
use crate::compile::canon_path;
use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos, bail};
use crate::runtime::*;
use crate::store::hash::{Hash, HashAlgo, Hasher, hash_bytes};
use crate::store::nar::FileType;
use crate::store::path::{
    IngestionMethod, STORE_DIR, StorePath, make_fixed_output_path, make_text_path,
};
use crate::store::source::{SourceOrigin, Src, SrcPath};
use crate::trace::Kind;

pub fn register(r: &mut Registry) {
    r.op("__readFile", 1, read_file);
    r.op("__readDir", 1, read_dir);
    r.op("__readFileType", 1, read_file_type);
    r.op("__pathExists", 1, path_exists);
    r.op("__toPath", 1, to_path);
    r.op("__storePath", 1, store_path);
    r.op("__filterSource", 2, filter_source);
    r.op("__path", 1, path);
    r.op("__toFile", 2, to_file);
    r.op("__outputOf", 2, output_of);
    r.op("__fetchurl", 1, fetchurl);
    r.op(
        "fetchTarball",
        1,
        crate::flake::fetch::fetch_tarball_builtin,
    );
    r.op("fetchGit", 1, fetch_git);
    r.op("fetchTree", 1, crate::flake::fetch::fetch_tree);
    r.op("fetchMercurial", 1, fetch_mercurial);
    r.op("__fetchClosure", 1, fetch_closure);
    r.op("__getFlake", 1, crate::flake::get_flake);
    r.op("__parseFlakeRef", 1, parse_flake_ref);
    r.op("__flakeRefToString", 1, flake_ref_to_string);
}

macro_rules! unsupported {
    ($fn:ident, $msg:expr) => {
        fn $fn(ev: &mut Evaluator, _args: &[Word], pos: Pos) -> EvalResult<Word> {
            ev.trace.mark_uncacheable();
            bail!(ErrorKind::Unsupported, pos, "{}", $msg)
        }
    };
    ($fn:ident, $name:literal, $msg:expr) => {
        fn $fn(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
            ev.trace.mark_uncacheable();
            crate::lint::purity::unhashed_fetch(ev, $name, args[0], pos)?;
            bail!(ErrorKind::Unsupported, pos, "{}", $msg)
        }
    };
}

unsupported!(
    fetchurl,
    "fetchurl",
    "ietsp: network fetchers are not supported (builtins.fetchurl)"
);
unsupported!(
    fetch_git,
    "fetchGit",
    "ietsp: network fetchers are not supported (builtins.fetchGit)"
);
unsupported!(
    fetch_mercurial,
    "ietsp: network fetchers are not supported (builtins.fetchMercurial)"
);
unsupported!(
    fetch_closure,
    "ietsp: network fetchers are not supported (builtins.fetchClosure)"
);
unsupported!(
    parse_flake_ref,
    "ietsp: flakes are not supported (builtins.parseFlakeRef)"
);
unsupported!(
    flake_ref_to_string,
    "ietsp: flakes are not supported (builtins.flakeRefToString)"
);
unsupported!(
    output_of,
    "ietsp: dynamic derivations are not supported (builtins.outputOf)"
);

fn lossy(b: &[u8]) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(b)
}

fn os_path(p: &[u8]) -> &Path {
    Path::new(OsStr::from_bytes(p))
}

/// Strip Rust's " (os error N)" suffix so messages read like Nix's strerror text.
fn io_text(e: &io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}

fn io_error(e: io::Error, verb: &str, p: &[u8], pos: Pos) -> Box<Error> {
    let msg = if e.kind() == io::ErrorKind::NotFound {
        format!("path '{}' does not exist", lossy(p))
    } else {
        format!("{verb} '{}': {}", lossy(p), io_text(&e))
    };
    Error::boxed(ErrorKind::Io, msg, pos)
}

fn utf8(b: &[u8], pos: Pos) -> EvalResult<&str> {
    std::str::from_utf8(b).map_err(|_| {
        Error::boxed(
            ErrorKind::Eval,
            format!("'{}' is not valid UTF-8", lossy(b)),
            pos,
        )
    })
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Symlink => FileType::Symlink.nix_name(),
        Kind::Directory => FileType::Directory.nix_name(),
        Kind::Regular => FileType::Regular.nix_name(),
        _ => FileType::Unknown.nix_name(),
    }
}

/// A failed [`SrcPath::read`] of a file, `readFile`'s way.
fn read_error((verb, e): (&str, io::Error), p: &[u8], pos: Pos) -> Box<Error> {
    if e.raw_os_error() == Some(libc::EISDIR) {
        Error::boxed(
            ErrorKind::Io,
            format!("file '{}' is not a regular file", lossy(p)),
            pos,
        )
    } else {
        io_error(e, verb, p, pos)
    }
}

/// Nix's `coerceToPath`: path values pass through, `__toString` sets are
/// called, everything else is coerced to a string that must be absolute.
pub fn coerce_to_path(
    ev: &mut Evaluator,
    w: Word,
    ctx: &mut Vec<Ctx>,
    pos: Pos,
    err_ctx: &str,
) -> EvalResult<Vec<u8>> {
    let w = ev.force(w).map_err(|mut e| {
        e.add_trace(pos, err_ctx);
        e
    })?;
    match w.view() {
        Value::Path(p) => return Ok(p.path.to_vec()),
        Value::Attrs(a) => {
            if let Some(f) = a.get(ev.s.to_string) {
                let f = ev.force_cell(&f.cell)?;
                let r = ev.call1(f, w, pos)?;
                return coerce_to_path(ev, r, ctx, pos, err_ctx);
            }
        }
        _ => {}
    }
    let s = ev.coerce_to_string(w, ctx, pos, err_ctx, false, false, true)?;
    if s.first() != Some(&b'/') {
        let mut e = Error::new(
            ErrorKind::Eval,
            format!("string '{}' doesn't represent an absolute path", lossy(&s)),
            pos,
        );
        e.add_trace(pos, err_ctx);
        return Err(Box::new(e));
    }
    Ok(canon_path(&s))
}

/// Nix's `realisePath` without building: a path with a derivation output in
/// its context cannot be realised here, and an opaque store path or `.drv`
/// (primops.cc:realiseContext treats `DrvDeep` as opaque) must exist or be
/// one this evaluation made. The result is the path as Nix names it;
/// `store_hooks::locate` gives the file to read.
pub fn realise_path(ev: &mut Evaluator, w: Word, pos: Pos) -> EvalResult<Vec<u8>> {
    let mut ctx = Vec::new();
    let p = coerce_to_path(
        ev,
        w,
        &mut ctx,
        Pos::NONE,
        "while realising the context of a path",
    )?;
    for c in &ctx {
        let mut kind = ErrorKind::Eval;
        let msg = match c {
            Ctx::Impure(_) => continue,
            Ctx::Opaque(sp) | Ctx::DrvDeep(sp) => {
                if store_hooks::locate(ev, sp).is_some()
                    || ev.trace.stat_now(sp, true) != crate::trace::Kind::Missing
                {
                    continue;
                }
                if let Ok(Ok(d)) = std::str::from_utf8(sp).map(StorePath::parse)
                    && ev.drv.drvs.lock().contains_key(&d)
                {
                    kind = ErrorKind::Unsupported;
                    format!(
                        "ietsp: '{}' is not in the store and this run did not keep its text (--no-cache)",
                        lossy(sp)
                    )
                } else {
                    format!("path '{}' is not valid", lossy(sp))
                }
            }
            Ctx::Built { drv: d, .. } => {
                kind = ErrorKind::Unsupported;
                format!(
                    "ietsp: import from derivation is not supported: '{}' is not built",
                    lossy(d)
                )
            }
        };
        let mut e = Error::new(kind, msg, pos);
        e.add_trace(
            pos,
            format!("while realising the context of path '{}'", lossy(&p)),
        );
        return Err(Box::new(e));
    }
    Ok(p)
}

/// What a realised path reads, and the path its messages name: the file on
/// disk, else the path itself. An entry a copied tree's filter left out is
/// `path '...' does not exist`.
fn realise_existing(ev: &mut Evaluator, w: Word, pos: Pos) -> EvalResult<(SrcPath, Vec<u8>)> {
    let p = realise_path(ev, w, pos)?;
    let at = store_hooks::open(ev, &p, pos)?;
    if at.hidden() {
        return Err(io_error(
            io::Error::from_raw_os_error(libc::ENOENT),
            "getting status of",
            &p,
            pos,
        ));
    }
    crate::lint::coerce::read_through_copy(ev, &p, pos)?;
    let shown = at.local().map_or(p, |l| l.into_os_string().into_vec());
    Ok((at, shown))
}

/// Reads `st_size` bytes like `PosixSourceAccessor::readFile`, so a procfs
/// file reads as empty.
fn read_file(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let (at, p) = realise_existing(ev, args[0], pos)?;
    let bytes = at.read(ev, true).map_err(|e| read_error(e, &p, pos))?;
    if bytes.contains(&0) {
        bail!(
            ErrorKind::Eval,
            pos,
            "the contents of the file '{}' cannot be represented as a Nix string",
            lossy(&p)
        );
    }
    Ok(ev.mk_string(&bytes))
}

fn read_dir(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let (at, p) = realise_existing(ev, args[0], pos)?;
    let listing = at.read_dir(ev).map_err(|(verb, e)| {
        if e.raw_os_error() == Some(libc::ENOTDIR) {
            Error::boxed(
                ErrorKind::Io,
                format!("'{}' is not a directory", lossy(&p)),
                pos,
            )
        } else {
            io_error(e, verb, &p, pos)
        }
    })?;
    let names = [
        ev.mk_string(FileType::Regular.nix_name().as_bytes()),
        ev.mk_string(FileType::Directory.nix_name().as_bytes()),
        ev.mk_string(FileType::Symlink.nix_name().as_bytes()),
        ev.mk_string(FileType::Unknown.nix_name().as_bytes()),
    ];
    let mut entries = Vec::with_capacity(listing.len());
    for (name, t) in listing {
        entries.push((ev.sym(&name), Pos::NONE, names[t as usize]));
    }
    ev.mk_attrs(&mut entries, pos)
}

fn read_file_type(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let (at, p) = realise_existing(ev, args[0], pos)?;
    let kind = at
        .stat(ev, false)
        .map_err(|e| io_error(e, "getting status of", &p, pos))?;
    Ok(ev.mk_string(kind_name(kind).as_bytes()))
}

fn path_exists(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let w = ev.force(args[0])?;
    let must_be_dir = matches!(
        w.view(),
        Value::String(s) if s.bytes.ends_with(b"/") || s.bytes.ends_with(b"/.")
    );
    let p = realise_path(ev, w, pos)?;
    if !store_hooks::allowed(ev, &p) {
        return Ok(Word::FALSE);
    }
    crate::lint::coerce::read_through_copy(ev, &p, pos)?;
    let at = store_hooks::resolve(ev, &p);
    match at.stat(ev, must_be_dir) {
        Ok(k) => Ok(Word::from_bool(k == Kind::Directory || !must_be_dir)),
        Err(e)
            if e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ENOTDIR) =>
        {
            Ok(Word::FALSE)
        }
        Err(e) => {
            let shown = at.local().map_or(p, |l| l.into_os_string().into_vec());
            Err(io_error(e, "getting status of", &shown, pos))
        }
    }
}

fn to_path(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let mut ctx = Vec::new();
    let p = coerce_to_path(
        ev,
        args[0],
        &mut ctx,
        pos,
        "while evaluating the first argument passed to builtins.toPath",
    )?;
    let ctx = ctx_to_arena(&mut ev.arena, ctx);
    Ok(ev.arena.mk_string_shared(&p, ctx))
}

/// `/nix/store/<hash>-<name>` with nothing after it.
fn is_store_path(p: &[u8]) -> bool {
    to_store_path(p).is_some_and(|(sp, rest)| rest.is_empty() && sp.is_ok())
}

/// Split `/nix/store/<base>/rest` into the store path and the rest.
fn to_store_path(p: &[u8]) -> Option<(Result<StorePath, ()>, &[u8])> {
    let rest = p.strip_prefix(STORE_DIR.as_bytes())?.strip_prefix(b"/")?;
    let end = rest.iter().position(|&b| b == b'/').unwrap_or(rest.len());
    let base = std::str::from_utf8(&rest[..end]).ok()?;
    Some((
        StorePath::from_base_name(base).map_err(|_| ()),
        &rest[end..],
    ))
}

fn store_path(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    if ev.pure {
        bail!(
            ErrorKind::Eval,
            pos,
            "'builtins.storePath' is not allowed in pure evaluation mode"
        );
    }
    let mut ctx = Vec::new();
    let mut p = coerce_to_path(
        ev,
        args[0],
        &mut ctx,
        pos,
        "while evaluating the first argument passed to 'builtins.storePath'",
    )?;
    if !is_store_path(&p) && store_hooks::locate(ev, &p).is_none_or(|at| at.hidden()) {
        let resolved = fs::canonicalize(os_path(&p));
        ev.trace.canon(&p, resolved.as_deref().ok());
        let resolved = resolved.map_err(|e| io_error(e, "getting status of", &p, pos))?;
        p = resolved.into_os_string().into_vec();
    }
    let sp = match to_store_path(&p) {
        Some((Ok(sp), _)) => sp,
        _ => bail!(
            ErrorKind::Eval,
            pos,
            "path '{}' is not in the Nix store",
            lossy(&p)
        ),
    };
    let full = ev.arena.alloc_bytes(sp.to_full_string().as_bytes());
    ctx.push(Ctx::Opaque(full));
    let ctx = ctx_to_arena(&mut ev.arena, ctx);
    Ok(ev.arena.mk_string_shared(&p, ctx))
}

/// The base name of a canonical path; `source` for the root, like
/// `SourcePath::baseName`.
fn base_name(p: &[u8]) -> &[u8] {
    match p.iter().rposition(|&b| b == b'/') {
        Some(i) if i + 1 < p.len() => &p[i + 1..],
        _ => b"source",
    }
}

/// A store path as a string whose context is that path.
fn store_path_string(ev: &mut Evaluator, sp: &StorePath) -> Word {
    let full = ev.arena.alloc_bytes(sp.to_full_string().as_bytes());
    let ctx = ev.arena.alloc_slice(&[Ctx::Opaque(full)]);
    ev.arena.mk_string_static(full, ctx)
}

fn call_filter(
    ev: &mut Evaluator,
    filter: Word,
    p: &Path,
    t: FileType,
    pos: Pos,
) -> EvalResult<bool> {
    let a = ev.mk_string(p.as_os_str().as_bytes());
    let b = ev.mk_string(t.nix_name().as_bytes());
    let r = ev.call2(filter, a, b, pos)?;
    ev.force_bool(
        r,
        pos,
        "while evaluating the return value of the path filter function",
    )
}

/// Mirrors primops.cc:addPath. References of a store path are unknown here
/// (no store database), so an in-store source gets none.
pub(crate) fn add_path(
    ev: &mut Evaluator,
    pos: Pos,
    name: &[u8],
    path: &[u8],
    filter: Option<Word>,
    method: IngestionMethod,
    expected: Option<Hash>,
) -> EvalResult<StorePath> {
    add_path_inner(ev, pos, name, path, filter, method, expected).map_err(|mut e| {
        e.add_trace(pos, format!("while adding path '{}'", lossy(path)));
        e
    })
}

fn add_path_inner(
    ev: &mut Evaluator,
    pos: Pos,
    name: &[u8],
    path: &[u8],
    filter: Option<Word>,
    method: IngestionMethod,
    expected: Option<Hash>,
) -> EvalResult<StorePath> {
    let name = utf8(name, pos)?;
    let refs = BTreeSet::new();
    crate::lint::coerce::store_path_recopy(ev, path, pos)?;
    crate::lint::coerce::read_through_copy(ev, path, pos)?;
    let store_err = |e: crate::store::path::StorePathError| Error::boxed(ErrorKind::Eval, e.0, pos);
    let expected_sp = expected
        .map(|h| make_fixed_output_path(method, &h, name, &refs))
        .transpose()
        .map_err(store_err)?;
    if let Some(sp) = &expected_sp
        && ev.trace.stat_now(sp.to_full_string().as_bytes(), true) != crate::trace::Kind::Missing
    {
        ev.known.lock().entry(sp.clone()).or_default();
        return Ok(sp.clone());
    }
    // A path under a tree this evaluation copied reads the local tree, with
    // that tree's filter applied before this call's `filter`, or the input's
    // spool; the filter function still sees the store path Nix would show it.
    let at = store_hooks::open(ev, path, pos)?;
    let at = if store_hooks::locate(ev, path).is_none() {
        let resolved =
            fs::canonicalize(os_path(path)).unwrap_or_else(|_| os_path(path).to_path_buf());
        if resolved.as_os_str().as_bytes() != path {
            ev.trace.canon(path, Some(&resolved));
        }
        SrcPath::fs(resolved, None)
    } else {
        at
    };
    let on_disk = match at.at() {
        None => {
            return Err(io_error(
                io::Error::from_raw_os_error(libc::ENOENT),
                "getting status of",
                path,
                pos,
            ));
        }
        Some(At::Mem(_)) => return Err(store_hooks::recopy_unsupported(path, pos)),
        Some(At::Spool(entry, base)) => {
            crate::inputs::record(ev, &entry, base);
            false
        }
        Some(At::Disk(_)) => true,
    };
    // a spool dump names its members under `/`
    let resolved = at.local().unwrap_or_else(|| PathBuf::from("/"));
    let parent = match &at.src {
        Src::Fs { accepted, .. } => accepted.clone(),
        _ => None,
    };
    let shown = |p: &Path| -> PathBuf {
        match p.strip_prefix(&resolved) {
            Ok(rest) if parent.is_some() || !on_disk => os_path(path).join(rest),
            _ => p.to_path_buf(),
        }
    };
    let mut accepted: Option<HashSet<PathBuf>> = filter.map(|_| HashSet::new());
    let mut nar_size = 0u64;
    let hash = match method {
        IngestionMethod::Flat => hash_bytes(
            HashAlgo::Sha256,
            &at.read(ev, true).map_err(|e| read_error(e, path, pos))?,
        ),
        IngestionMethod::Recursive => {
            // The observation records `accepted`, which is the dump's own decisions unless
            // an enclosing tree's `parent` set is the only thing narrowing them: then the
            // recorded filter is accept-all and only a separate walk matches it.
            let mut fp = (on_disk && (filter.is_some() || parent.is_none()))
                .then(|| ev.trace.fp(&resolved))
                .flatten();
            let mut err: Option<Box<Error>> = None;
            let git_lint = on_disk && ev.lint_on(crate::lint::LintId::GitInSource);
            let mut saw_git = false;
            let mut f = |p: &Path, t: FileType| -> bool {
                let keep = if !at.accepts(p) {
                    false
                } else if let Some(filter) = filter {
                    if err.is_some() {
                        false
                    } else {
                        match call_filter(ev, filter, &shown(p), t, pos) {
                            Ok(b) => {
                                if b && let Some(a) = &mut accepted {
                                    a.insert(p.to_path_buf());
                                }
                                b
                            }
                            Err(e) => {
                                err = Some(e);
                                false
                            }
                        }
                    }
                } else {
                    true
                };
                if let Some(fp) = &mut fp {
                    fp.entry(p, t, keep);
                }
                if keep && git_lint && store_hooks::is_git_entry(&resolved, p) {
                    saw_git = true;
                }
                keep
            };
            let mut h = Hasher::new(HashAlgo::Sha256);
            let r = at.dump(&mut h, &mut f);
            if let Some(e) = err {
                return Err(e);
            }
            r.map_err(|e| io_error(e, "copying", path, pos))?;
            nar_size = h.bytes_written();
            let h = h.finish();
            if on_disk {
                ev.trace.tree(
                    &resolved,
                    crate::trace::hash_to_digest(&h),
                    accepted.as_ref(),
                    fp,
                );
            }
            if saw_git {
                crate::lint::purity::git_in_source(ev, path, pos)?;
            }
            if on_disk {
                crate::lint::purity::large_source_copy(ev, path, nar_size, pos)?;
            }
            h
        }
    };
    let sp = make_fixed_output_path(method, &hash, name, &refs).map_err(store_err)?;
    if method == IngestionMethod::Recursive {
        // an input member's origin keeps no `filter`: a filtered copy would
        // upload the whole member, which its recorded NAR size refuses
        if on_disk {
            let at = SrcPath::fs(resolved, accepted.map(Arc::new).or(parent));
            ev.known
                .add_copy(sp.clone(), SourceOrigin::tree(at, hash, nar_size));
        } else {
            ev.known
                .add(sp.clone(), SourceOrigin::tree(at, hash, nar_size));
        }
    }
    if let Some(exp) = expected_sp
        && exp != sp
    {
        bail!(
            ErrorKind::Eval,
            pos,
            "store path mismatch in (possibly filtered) path added from '{}'",
            lossy(path)
        );
    }
    Ok(sp)
}

fn filter_source(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let mut ctx = Vec::new();
    let p = coerce_to_path(
        ev,
        args[1],
        &mut ctx,
        pos,
        "while evaluating the second argument (the path to filter) passed to 'builtins.filterSource'",
    )?;
    let f = ev.force_function(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.filterSource",
    )?;
    let name = base_name(&p).to_vec();
    let sp = add_path(
        ev,
        pos,
        &name,
        &p,
        Some(f),
        IngestionMethod::Recursive,
        None,
    )?;
    Ok(store_path_string(ev, &sp))
}

/// Mirrors `newHashAllowEmpty`: an empty hash string means all zeros.
fn parse_sha256_allow_empty(s: &[u8], pos: Pos) -> EvalResult<Hash> {
    let s = utf8(s, pos)?;
    if s.is_empty() {
        let zero = Hash::from_bytes(HashAlgo::Sha256, &[0u8; 32]).expect("32 bytes");
        eprintln!("warning: found empty hash, assuming '{}'", zero.to_sri());
        return Ok(zero);
    }
    Hash::parse_any(s, Some(HashAlgo::Sha256)).map_err(|e| Error::boxed(ErrorKind::Eval, e.0, pos))
}

fn path(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let a = ev.force_attrs(
        args[0],
        pos,
        "while evaluating the argument passed to 'builtins.path'",
    )?;
    let mut ctx = Vec::new();
    let mut p: Option<Vec<u8>> = None;
    let mut name: Vec<u8> = Vec::new();
    let mut filter: Option<Word> = None;
    let mut method = IngestionMethod::Recursive;
    let mut expected: Option<Hash> = None;
    for attr in a.iter() {
        let n: Vec<u8> = ev.interner.name(attr.sym).to_vec();
        let v = attr.cell.get();
        match n.as_slice() {
            b"path" => {
                p = Some(coerce_to_path(
                    ev,
                    v,
                    &mut ctx,
                    attr.pos,
                    "while evaluating the 'path' attribute passed to 'builtins.path'",
                )?)
            }
            b"name" => {
                name = ev
                    .force_string_no_ctx(
                        v,
                        attr.pos,
                        "while evaluating the `name` attribute passed to builtins.path",
                    )?
                    .to_vec()
            }
            b"filter" => {
                filter = Some(ev.force_function(
                    v,
                    attr.pos,
                    "while evaluating the `filter` parameter passed to builtins.path",
                )?)
            }
            b"recursive" => {
                method = if ev.force_bool(
                    v,
                    attr.pos,
                    "while evaluating the `recursive` attribute passed to builtins.path",
                )? {
                    IngestionMethod::Recursive
                } else {
                    IngestionMethod::Flat
                }
            }
            b"sha256" => {
                let s = ev.force_string_no_ctx(
                    v,
                    attr.pos,
                    "while evaluating the `sha256` attribute passed to builtins.path",
                )?;
                expected = Some(parse_sha256_allow_empty(s, attr.pos)?);
            }
            _ => bail!(
                ErrorKind::Eval,
                attr.pos,
                "unsupported argument '{}' to 'builtins.path'",
                lossy(&n)
            ),
        }
    }
    let Some(p) = p else {
        bail!(
            ErrorKind::Eval,
            pos,
            "missing required 'path' attribute in the first argument to 'builtins.path'"
        );
    };
    if name.is_empty() {
        name = base_name(&p).to_vec();
    }
    let sp = add_path(ev, pos, &name, &p, filter, method, expected)?;
    Ok(store_path_string(ev, &sp))
}

/// Nix's internal context form: base names, `=` and `!out!` prefixes.
fn ctx_internal(c: &Ctx) -> String {
    let base = |p: &[u8]| lossy(base_name(p)).into_owned();
    match c {
        Ctx::Opaque(p) => base(p),
        Ctx::DrvDeep(d) => format!("={}", base(d)),
        Ctx::Built { drv, output } => format!("!{}!{}", lossy(output), base(drv)),
        Ctx::Impure(p) => lossy(p).into_owned(),
    }
}

fn to_file(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let name = ev.force_string_no_ctx(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.toFile",
    )?;
    let contents = ev.force_string(
        args[1],
        pos,
        "while evaluating the second argument passed to builtins.toFile",
    )?;
    let mut refs = BTreeSet::new();
    for c in contents.ctx {
        match c {
            Ctx::Impure(_) => {}
            Ctx::Opaque(p) => {
                let sp = StorePath::parse(utf8(p, pos)?)
                    .map_err(|e| Error::boxed(ErrorKind::Eval, e.0, pos))?;
                refs.insert(sp);
            }
            other => bail!(
                ErrorKind::Eval,
                pos,
                "files created by builtins.toFile may not reference derivations, but {} references {}",
                lossy(name),
                ctx_internal(other)
            ),
        }
    }
    let h = hash_bytes(HashAlgo::Sha256, contents.bytes);
    let sp = make_text_path(utf8(name, pos)?, &h, &refs)
        .map_err(|e| Error::boxed(ErrorKind::Eval, e.0, pos))?;
    ev.known.add(
        sp.clone(),
        SourceOrigin::text(contents.bytes.to_vec(), refs),
    );
    Ok(store_path_string(ev, &sp))
}
