//! `builtins.fetchTree` for locked inputs (fetchTree.cc, fetchers.cc): the
//! attrs the evaluator passes become a `source::SourceRef`, which resolves
//! to a tree, and the result attrset mirrors `emitTreeAttrs`. A `path` input
//! is a virtual copy of the local directory, like the root tree; everything
//! else is locked, and `crate::source` fetches it.

use std::path::Path;

use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos, bail};
use crate::runtime::*;
use crate::source::{
    AttrVal, Attrs, SourceRef, bool_attr, int_attr, last_modified_date, parse_nar_hash, str_attr,
    to_url,
};
use crate::store::hash::{Hash, HashAlgo};
use crate::store::path::{IngestionMethod, StorePath};
use crate::syntax::symbol::Sym;

/// The attrs `fetchTree` accepts: strings and paths, booleans, non-negative
/// integers (fetchTree.cc).
fn collect_attrs(ev: &mut Evaluator, w: Word, pos: Pos) -> EvalResult<Attrs> {
    let w = ev.force(w)?;
    let Some(a) = w.as_attrs() else {
        bail!(
            ErrorKind::Unsupported,
            pos,
            "ietsp: 'fetchTree' with a URL argument is not supported; pass the locked attribute set"
        );
    };
    let mut out = Attrs::new();
    for attr in a.iter() {
        let name = String::from_utf8_lossy(ev.interner.name(attr.sym)).into_owned();
        if name == "__final" {
            continue;
        }
        let v = ev.force(attr.cell.get())?;
        let val = match v.view() {
            Value::String(s) => AttrVal::Str(String::from_utf8_lossy(s.bytes).into_owned()),
            Value::Path(p) => AttrVal::Str(String::from_utf8_lossy(p.path).into_owned()),
            Value::Bool(b) => AttrVal::Bool(b),
            Value::Int(n) if n >= 0 => AttrVal::Int(n as u64),
            Value::Int(n) => bail!(
                ErrorKind::Eval,
                pos,
                "negative value given for 'fetchTree' argument '{name}': {n}"
            ),
            _ => bail!(
                ErrorKind::Type,
                pos,
                "argument '{name}' to 'fetchTree' is {} while a string, Boolean or integer is expected",
                v.value_type().type_of()
            ),
        };
        out.insert(name, val);
    }
    if !out.contains_key("type") {
        bail!(
            ErrorKind::Eval,
            pos,
            "argument 'type' is missing in call to 'fetchTree'"
        );
    }
    Ok(out)
}

/// fetchTree.cc:fetchTree: a pure evaluation refuses an input that is not
/// locked (a `narHash` for `path` and `tarball`, a `rev` for the rest). The
/// message names the input the way `Input::to_string` does
/// (`flakeref::attrs_to_string`); for `git` (fetchTree adds `?shallow=1`) and
/// what ietsp cannot show, it is unsupported and classic mode falls back.
fn check_locked(attrs: &Attrs, pos: Pos) -> EvalResult<()> {
    let ty = str_attr(attrs, "type").unwrap_or("");
    let locked = match ty {
        "path" | "tarball" => attrs.contains_key("narHash"),
        _ => attrs.contains_key("rev"),
    };
    if locked {
        return Ok(());
    }
    // fetchTree.cc rejects `name` first, and only warns on a `narHash`
    let shown = match ty {
        _ if attrs.contains_key("name") || attrs.contains_key("narHash") => None,
        "path" | "tarball" | "github" | "gitlab" => {
            super::flakeref::attrs_to_string(attrs.clone()).ok()
        }
        _ => None,
    };
    match shown {
        Some(s) => bail!(
            ErrorKind::Eval,
            pos,
            "in pure evaluation mode, 'fetchTree' doesn't fetch unlocked input '{s}'"
        ),
        None => bail!(
            ErrorKind::Unsupported,
            pos,
            "ietsp: an unlocked '{ty}' input in pure evaluation mode is not supported"
        ),
    }
}

pub fn fetch_tree(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    crate::lint::purity::unhashed_fetch(ev, "fetchTree", args[0], pos)?;
    let mut attrs = collect_attrs(ev, args[0], pos)?;
    if ev.pure() {
        check_locked(&attrs, pos)?;
    }
    let ty = str_attr(&attrs, "type").unwrap_or("").to_string();
    let sp = match ty.as_str() {
        "path" => {
            let Some(path) = str_attr(&attrs, "path") else {
                bail!(
                    ErrorKind::Eval,
                    pos,
                    "fetchTree: 'path' attribute is missing"
                );
            };
            if !path.starts_with('/') {
                bail!(
                    ErrorKind::Eval,
                    pos,
                    "cannot fetch input '{}' because it uses a relative path",
                    to_url(&attrs)
                );
            }
            let (hash, mtime) = match crate::builtins::store_hooks::locate(ev, path.as_bytes()) {
                None => crate::source::path_tree(ev, Path::new(path), pos)?,
                // a store tree's mtimes are all 1, so that is its lastModified
                Some(_) => {
                    let sp = crate::builtins::paths::add_path(
                        ev,
                        pos,
                        b"source",
                        path.as_bytes(),
                        None,
                        IngestionMethod::Recursive,
                        None,
                    )?;
                    let nar = ev.known.origin(&sp).and_then(|o| o.nar);
                    (nar.expect("add_path records a recursive copy").0, 1)
                }
            };
            if let Some(expected) = str_attr(&attrs, "narHash") {
                let exp = parse_nar_hash(expected)
                    .map_err(|e| Error::boxed(ErrorKind::Eval, e.to_string(), pos))?;
                if exp != hash {
                    let msg = format!(
                        "NAR hash mismatch in input '{}', expected '{}' but got '{}'",
                        to_url(&attrs),
                        exp.to_sri(),
                        hash.to_sri()
                    );
                    let sym = ev.sym(b"narHash");
                    let at = ev
                        .force(args[0])?
                        .as_attrs()
                        .and_then(|a| a.get(sym))
                        .map(|a| a.pos);
                    let at = at.map(|p| {
                        crate::provenance::describe_pos(ev, p).unwrap_or_else(|| ev.describe_pos(p))
                    });
                    return Err(Error::hash_mismatch(msg, &hash, at, pos));
                }
            }
            attrs.insert("narHash".into(), AttrVal::Str(hash.to_sri()));
            attrs
                .entry("lastModified".into())
                .or_insert(AttrVal::Int(mtime.max(0) as u64));
            crate::treestore::store_path("source", &hash)
                .map_err(|e| Error::boxed(ErrorKind::Eval, e.to_string(), pos))?
        }
        "github" | "gitlab" | "sourcehut" | "git" | "tarball" | "hg" => {
            let r = SourceRef::Locked(attrs);
            let t = crate::source::tree(ev, &r, pos)?;
            attrs = t.attrs;
            t.path
        }
        other => bail!(
            ErrorKind::Unsupported,
            pos,
            "ietsp: fetchTree input type '{other}' is not supported"
        ),
    };
    if let Some(name) = str_attr(&attrs, "narHash").and_then(|h| ev.input_names.get(h)) {
        ev.known.name_flake(&sp, name);
    }
    emit_tree_attrs(ev, &sp, &attrs, pos)
}

/// fetchTree.cc:fetch for `builtins.fetchTarball`: with a `sha256` the store
/// path is known before the download, so it is the same fetch as a locked
/// `tarball` input. Without one it is not, and the call is unsupported.
pub fn fetch_tarball_builtin(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    crate::lint::purity::unhashed_fetch(ev, "fetchTarball", args[0], pos)?;
    let w = ev.force(args[0])?;
    let (mut url, mut sha256, mut name) = (None, None, "source".to_string());
    if let Some(a) = w.as_attrs() {
        for attr in a.iter() {
            let n = String::from_utf8_lossy(ev.interner.name(attr.sym)).into_owned();
            let what = match n.as_str() {
                "url" => "while evaluating the url we should fetch",
                "sha256" => "while evaluating the sha256 of the content we should fetch",
                "name" => "while evaluating the name of the content we should fetch",
                _ => bail!(
                    ErrorKind::Eval,
                    pos,
                    "unsupported argument '{n}' to 'fetchTarball'"
                ),
            };
            let v = ev.force_string_no_ctx(attr.cell.get(), pos, what)?;
            let v = String::from_utf8_lossy(v).into_owned();
            match n.as_str() {
                "url" => url = Some(v),
                "sha256" => sha256 = Some(v),
                _ => name = v,
            }
        }
    } else {
        let v = ev.force_string_no_ctx(w, pos, "while evaluating the url we should fetch")?;
        url = Some(String::from_utf8_lossy(v).into_owned());
    }
    let Some(url) = url else {
        bail!(ErrorKind::Eval, pos, "'url' argument required");
    };
    let url = match url.strip_prefix("channel:") {
        Some(c) => format!("https://channels.nixos.org/{c}/nixexprs.tar.xz"),
        None => url,
    };
    if ev.pure() && sha256.is_none() {
        bail!(
            ErrorKind::Eval,
            pos,
            "in pure evaluation mode, 'fetchTarball' requires a 'sha256' argument"
        );
    }
    let Some(hash) = sha256
        .filter(|h| !h.is_empty())
        .and_then(|h| Hash::parse_any(&h, Some(HashAlgo::Sha256)).ok())
    else {
        ev.trace().mark_uncacheable();
        bail!(
            ErrorKind::Unsupported,
            pos,
            "ietsp: builtins.fetchTarball without a sha256 is not supported"
        );
    };
    // not `source::tree`: `fetchTarball` names the entry itself, and the
    // input store's entry names are what an upload looks up
    let sp = crate::treestore::store_path(&name, &hash)
        .map_err(|e| Error::boxed(ErrorKind::Eval, e.to_string(), pos))?;
    let attrs = Attrs::from([
        ("type".into(), AttrVal::Str("tarball".into())),
        ("url".into(), AttrVal::Str(url)),
        ("narHash".into(), AttrVal::Str(hash.to_sri())),
    ]);
    crate::source::ensure_readable(ev, &sp, &attrs, pos)?;
    let full = ev.arena.alloc_bytes(sp.to_full_string().as_bytes());
    let ctx = ev.arena.alloc_slice(&[Ctx::Opaque(full)]);
    Ok(ev.arena.mk_string_static(full, ctx))
}

/// fetchTree.cc:emitTreeAttrs with `emptyRevFallback = false`; `forceDirty`
/// is expressed by the caller leaving out `rev`/`revCount`.
pub fn emit_tree_attrs(
    ev: &mut Evaluator,
    sp: &StorePath,
    a: &Attrs,
    pos: Pos,
) -> EvalResult<Word> {
    let mut entries: Vec<(Sym, Pos, Word)> = Vec::with_capacity(10);
    let full = ev.arena.alloc_bytes(sp.to_full_string().as_bytes());
    let ctx = ev.arena.alloc_slice(&[Ctx::Opaque(full)]);
    let out_path = ev.arena.mk_string_static(full, ctx);
    entries.push((ev.s.out_path, pos, out_path));
    let mut push_str = |ev: &mut Evaluator, name: &[u8], v: &str| {
        let w = ev.mk_string(v.as_bytes());
        entries.push((ev.sym(name), pos, w));
    };
    if let Some(h) = str_attr(a, "narHash") {
        push_str(ev, b"narHash", h);
    }
    if let Some(rev) = str_attr(a, "rev") {
        push_str(ev, b"rev", rev);
        push_str(ev, b"shortRev", &rev[..rev.len().min(7)]);
    }
    if let Some(d) = str_attr(a, "dirtyRev") {
        push_str(ev, b"dirtyRev", d);
        push_str(
            ev,
            b"dirtyShortRev",
            str_attr(a, "dirtyShortRev").unwrap_or(""),
        );
    }
    if let Some(t) = int_attr(a, "lastModified") {
        push_str(ev, b"lastModifiedDate", &last_modified_date(t as i64));
        let w = ev.mk_int(t as i64);
        entries.push((ev.sym(b"lastModified"), pos, w));
    }
    if let Some(n) = int_attr(a, "revCount") {
        let w = ev.mk_int(n as i64);
        entries.push((ev.sym(b"revCount"), pos, w));
    }
    if str_attr(a, "type") == Some("git") {
        let w = Word::from_bool(bool_attr(a, "submodules").unwrap_or(false));
        entries.push((ev.sym(b"submodules"), pos, w));
    }
    ev.mk_attrs(&mut entries, pos)
}
