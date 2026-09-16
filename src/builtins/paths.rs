//! File system primops (primops.cc): readFile, readDir, readFileType,
//! pathExists, toPath, storePath, filterSource, path, toFile. The fetchers
//! and flake primops are registered so `builtins ? fetchTree` holds, but
//! they fail when called: ietsp never touches the network or the store.

use std::collections::{BTreeSet, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use super::Registry;
use crate::compile::canon_path;
use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos, bail};
use crate::runtime::*;
use crate::store::hash::{Hash, HashAlgo, hash_bytes};
use crate::store::nar::{self, FileType};
use crate::store::path::{
    IngestionMethod, STORE_DIR, StorePath, make_fixed_output_path, make_text_path,
};
use crate::store::source::SourceOrigin;

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
    r.op("fetchTarball", 1, fetch_tarball);
    r.op("fetchGit", 1, fetch_git);
    r.op("fetchTree", 1, fetch_tree);
    r.op("fetchMercurial", 1, fetch_mercurial);
    r.op("__fetchClosure", 1, fetch_closure);
    r.op("__getFlake", 1, get_flake);
    r.op("__parseFlakeRef", 1, parse_flake_ref);
    r.op("__flakeRefToString", 1, flake_ref_to_string);
}

macro_rules! unsupported {
    ($fn:ident, $msg:expr) => {
        fn $fn(_ev: &mut Evaluator, _args: &[Word], pos: Pos) -> EvalResult<Word> {
            bail!(ErrorKind::Eval, pos, "{}", $msg)
        }
    };
}

unsupported!(
    fetchurl,
    "ietsp: network fetchers are not supported (builtins.fetchurl)"
);
unsupported!(
    fetch_tarball,
    "ietsp: network fetchers are not supported (builtins.fetchTarball)"
);
unsupported!(
    fetch_git,
    "ietsp: network fetchers are not supported (builtins.fetchGit)"
);
unsupported!(
    fetch_tree,
    "ietsp: network fetchers are not supported (builtins.fetchTree)"
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
    get_flake,
    "ietsp: flakes are not supported (builtins.getFlake)"
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

fn file_type_name(ft: fs::FileType) -> &'static str {
    if ft.is_symlink() {
        FileType::Symlink.nix_name()
    } else if ft.is_dir() {
        FileType::Directory.nix_name()
    } else if ft.is_file() {
        FileType::Regular.nix_name()
    } else {
        FileType::Unknown.nix_name()
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

/// Nix's `realisePath` without building: a path with derivation context
/// cannot be realised here, and an opaque store path must already exist.
/// A virtual store path produced by `"${./x}"` maps back to its source.
fn realise_path(ev: &mut Evaluator, w: Word, pos: Pos) -> EvalResult<Vec<u8>> {
    let mut ctx = Vec::new();
    let p = coerce_to_path(
        ev,
        w,
        &mut ctx,
        Pos::NONE,
        "while realising the context of a path",
    )?;
    let mut p = p;
    for c in &ctx {
        let msg = match c {
            Ctx::Opaque(sp) => {
                if os_path(sp).exists() {
                    continue;
                }
                let src = ev.src_to_store.iter().find(|(_, v)| v == sp);
                if let Some((src, _)) = src
                    && p.starts_with(sp)
                {
                    let mut q = src.clone();
                    q.extend_from_slice(&p[sp.len()..]);
                    p = q;
                    continue;
                }
                format!("path '{}' is not valid", lossy(sp))
            }
            Ctx::DrvDeep(d) | Ctx::Built { drv: d, .. } => format!(
                "ietsp: cannot build derivation '{}' (building is not supported)",
                lossy(d)
            ),
        };
        let mut e = Error::new(ErrorKind::Eval, msg, pos);
        e.add_trace(
            pos,
            format!("while realising the context of path '{}'", lossy(&p)),
        );
        return Err(Box::new(e));
    }
    Ok(p)
}

/// Reads `st_size` bytes like `PosixSourceAccessor::readFile`, so a procfs
/// file reads as empty.
fn read_file(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let p = realise_path(ev, args[0], pos)?;
    let meta = fs::metadata(os_path(&p)).map_err(|e| io_error(e, "getting status of", &p, pos))?;
    if !meta.is_file() {
        bail!(
            ErrorKind::Io,
            pos,
            "file '{}' is not a regular file",
            lossy(&p)
        );
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    fs::File::open(os_path(&p))
        .and_then(|f| io::Read::take(f, meta.len()).read_to_end(&mut bytes))
        .map_err(|e| io_error(e, "reading file", &p, pos))?;
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
    let p = realise_path(ev, args[0], pos)?;
    let rd = fs::read_dir(os_path(&p)).map_err(|e| {
        if e.raw_os_error() == Some(libc_enotdir()) {
            Error::boxed(
                ErrorKind::Io,
                format!("'{}' is not a directory", lossy(&p)),
                pos,
            )
        } else {
            io_error(e, "opening directory", &p, pos)
        }
    })?;
    let names = [
        ev.mk_string(FileType::Regular.nix_name().as_bytes()),
        ev.mk_string(FileType::Directory.nix_name().as_bytes()),
        ev.mk_string(FileType::Symlink.nix_name().as_bytes()),
        ev.mk_string(FileType::Unknown.nix_name().as_bytes()),
    ];
    let mut entries = Vec::new();
    for e in rd {
        let e = e.map_err(|e| io_error(e, "reading directory", &p, pos))?;
        let ft = e
            .file_type()
            .map_err(|e| io_error(e, "getting status of", &p, pos))?;
        let idx = if ft.is_symlink() {
            2
        } else if ft.is_dir() {
            1
        } else if ft.is_file() {
            0
        } else {
            3
        };
        let sym = ev.sym(e.file_name().as_bytes());
        entries.push((sym, Pos::NONE, names[idx]));
    }
    ev.mk_attrs(&mut entries, pos)
}

fn libc_enotdir() -> i32 {
    20
}

fn read_file_type(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let p = realise_path(ev, args[0], pos)?;
    let meta =
        fs::symlink_metadata(os_path(&p)).map_err(|e| io_error(e, "getting status of", &p, pos))?;
    Ok(ev.mk_string(file_type_name(meta.file_type()).as_bytes()))
}

fn path_exists(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let w = ev.force(args[0])?;
    let must_be_dir = matches!(
        w.view(),
        Value::String(s) if s.bytes.ends_with(b"/") || s.bytes.ends_with(b"/.")
    );
    let p = realise_path(ev, w, pos)?;
    let r = if must_be_dir {
        fs::metadata(os_path(&p)).map(|m| m.is_dir())
    } else {
        fs::symlink_metadata(os_path(&p)).map(|_| true)
    };
    match r {
        Ok(b) => Ok(Word::from_bool(b)),
        Err(e)
            if e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(libc_enotdir()) =>
        {
            Ok(Word::FALSE)
        }
        Err(e) => Err(io_error(e, "getting status of", &p, pos)),
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
    let bytes = ev.arena.alloc_bytes(&p);
    Ok(ev.arena.mk_string_static(bytes, ctx))
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
    let mut ctx = Vec::new();
    let mut p = coerce_to_path(
        ev,
        args[0],
        &mut ctx,
        pos,
        "while evaluating the first argument passed to 'builtins.storePath'",
    )?;
    if !is_store_path(&p) {
        let resolved =
            fs::canonicalize(os_path(&p)).map_err(|e| io_error(e, "getting status of", &p, pos))?;
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
    let bytes = ev.arena.alloc_bytes(&p);
    Ok(ev.arena.mk_string_static(bytes, ctx))
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
fn add_path(
    ev: &mut Evaluator,
    pos: Pos,
    name: &[u8],
    path: &[u8],
    filter: Option<Word>,
    method: IngestionMethod,
    expected: Option<Hash>,
) -> EvalResult<Word> {
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
) -> EvalResult<Word> {
    let name = utf8(name, pos)?;
    let refs = BTreeSet::new();
    let store_err = |e: crate::store::path::StorePathError| Error::boxed(ErrorKind::Eval, e.0, pos);
    let expected_sp = expected
        .map(|h| make_fixed_output_path(method, &h, name, &refs))
        .transpose()
        .map_err(store_err)?;
    if let Some(sp) = &expected_sp
        && Path::new(&sp.to_full_string()).exists()
    {
        return Ok(store_path_string(ev, sp));
    }
    let resolved: PathBuf =
        fs::canonicalize(os_path(path)).unwrap_or_else(|_| os_path(path).to_path_buf());
    let mut accepted: Option<HashSet<PathBuf>> = filter.map(|_| HashSet::new());
    let hash = match method {
        IngestionMethod::Flat => {
            let meta =
                fs::metadata(&resolved).map_err(|e| io_error(e, "getting status of", path, pos))?;
            if !meta.is_file() {
                bail!(
                    ErrorKind::Io,
                    pos,
                    "file '{}' is not a regular file",
                    lossy(path)
                );
            }
            nar::flat_hash(&resolved, HashAlgo::Sha256)
                .map_err(|e| io_error(e, "reading file", path, pos))?
        }
        IngestionMethod::Recursive => {
            let mut err: Option<Box<Error>> = None;
            let mut f = |p: &Path, t: FileType| -> bool {
                let Some(filter) = filter else {
                    return true;
                };
                if err.is_some() {
                    return false;
                }
                match call_filter(ev, filter, p, t, pos) {
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
            };
            let r = nar::nar_hash_filtered(&resolved, HashAlgo::Sha256, &mut f);
            if let Some(e) = err {
                return Err(e);
            }
            r.map_err(|e| io_error(e, "copying", path, pos))?.0
        }
    };
    let sp = make_fixed_output_path(method, &hash, name, &refs).map_err(store_err)?;
    if method == IngestionMethod::Recursive {
        ev.sources.insert(
            sp.clone(),
            SourceOrigin::Tree {
                local: resolved,
                accepted,
            },
        );
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
    Ok(store_path_string(ev, &sp))
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
    add_path(
        ev,
        pos,
        &name,
        &p,
        Some(f),
        IngestionMethod::Recursive,
        None,
    )
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
    for attr in a.attrs() {
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
    add_path(ev, pos, &name, &p, filter, method, expected)
}

/// Nix's internal context form: base names, `=` and `!out!` prefixes.
fn ctx_internal(c: &Ctx) -> String {
    let base = |p: &[u8]| lossy(base_name(p)).into_owned();
    match c {
        Ctx::Opaque(p) => base(p),
        Ctx::DrvDeep(d) => format!("={}", base(d)),
        Ctx::Built { drv, output } => format!("!{}!{}", lossy(output), base(drv)),
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
    ev.sources.insert(
        sp.clone(),
        SourceOrigin::Text {
            contents: contents.bytes.to_vec(),
            references: refs,
        },
    );
    Ok(store_path_string(ev, &sp))
}
