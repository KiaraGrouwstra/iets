//! `builtins.fetchTree` for locked inputs (fetchTree.cc, fetchers.cc): the
//! store path follows from the `narHash`, the tree must be in `/nix/store`,
//! and the result attrset mirrors `emitTreeAttrs`. A tree that is not in
//! the store is fetched by running `nix` once; this is the only place
//! ietsp runs nix.

use std::collections::BTreeMap;
use std::path::Path;

use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos, bail};
use crate::runtime::*;
use crate::store::hash::{Hash, HashAlgo};
use crate::store::path::{IngestionMethod, StorePath, make_fixed_output_path};
use crate::syntax::symbol::Sym;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttrVal {
    Str(String),
    Bool(bool),
    Int(u64),
}

/// fetchers::Attrs: the input attributes in name order.
pub type Attrs = BTreeMap<String, AttrVal>;

fn str_attr<'a>(a: &'a Attrs, name: &str) -> Option<&'a str> {
    match a.get(name) {
        Some(AttrVal::Str(s)) => Some(s),
        _ => None,
    }
}

fn int_attr(a: &Attrs, name: &str) -> Option<u64> {
    match a.get(name) {
        Some(AttrVal::Int(n)) => Some(*n),
        _ => None,
    }
}

fn bool_attr(a: &Attrs, name: &str) -> Option<bool> {
    match a.get(name) {
        Some(AttrVal::Bool(b)) => Some(*b),
        _ => None,
    }
}

pub fn source_store_path(nar_hash: &Hash) -> Result<StorePath, String> {
    make_fixed_output_path(
        IngestionMethod::Recursive,
        nar_hash,
        "source",
        &std::collections::BTreeSet::new(),
    )
    .map_err(|e| e.0)
}

pub fn parse_nar_hash(s: &str) -> Result<Hash, String> {
    Hash::parse_any(s, Some(HashAlgo::Sha256)).map_err(|e| e.0)
}

/// `%Y%m%d%H%M%S` of a Unix time in UTC (Howard Hinnant's civil_from_days).
pub fn last_modified_date(t: i64) -> String {
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}{:02}{:02}{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// The attrs `fetchTree` accepts: strings and paths, booleans, non-negative
/// integers (fetchTree.cc).
fn collect_attrs(ev: &mut Evaluator, w: Word, pos: Pos) -> EvalResult<Attrs> {
    let w = ev.force(w)?;
    let Some(a) = w.as_attrs() else {
        bail!(
            ErrorKind::Eval,
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

pub fn fetch_tree(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let mut attrs = collect_attrs(ev, args[0], pos)?;
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
            let (hash, mtime) = super::root::path_tree(ev, Path::new(path), pos)?;
            if let Some(expected) = str_attr(&attrs, "narHash") {
                let exp =
                    parse_nar_hash(expected).map_err(|e| Error::boxed(ErrorKind::Eval, e, pos))?;
                if exp != hash {
                    bail!(
                        ErrorKind::Eval,
                        pos,
                        "NAR hash mismatch in input '{}', expected '{}' but got '{}'",
                        to_url(&attrs),
                        exp.to_sri(),
                        hash.to_sri()
                    );
                }
            }
            attrs.insert("narHash".into(), AttrVal::Str(hash.to_sri()));
            attrs
                .entry("lastModified".into())
                .or_insert(AttrVal::Int(mtime.max(0) as u64));
            source_store_path(&hash).map_err(|e| Error::boxed(ErrorKind::Eval, e, pos))?
        }
        "github" | "gitlab" | "sourcehut" | "git" | "tarball" | "hg" => {
            let Some(nar_hash) = str_attr(&attrs, "narHash") else {
                bail!(
                    ErrorKind::Eval,
                    pos,
                    "ietsp: cannot fetch unlocked input '{}' (no narHash); run nix flake lock",
                    to_url(&attrs)
                );
            };
            let hash =
                parse_nar_hash(nar_hash).map_err(|e| Error::boxed(ErrorKind::Eval, e, pos))?;
            attrs.insert("narHash".into(), AttrVal::Str(hash.to_sri()));
            source_store_path(&hash).map_err(|e| Error::boxed(ErrorKind::Eval, e, pos))?
        }
        other => bail!(
            ErrorKind::Eval,
            pos,
            "ietsp: fetchTree input type '{other}' is not supported"
        ),
    };
    ensure_in_store(ev, &sp, &attrs, pos)?;
    emit_tree_attrs(ev, &sp, &attrs, pos)
}

/// `flake:...`-style display of an input for messages; not a parseable ref.
fn to_url(a: &Attrs) -> String {
    let mut parts = Vec::new();
    for (k, v) in a {
        let v = match v {
            AttrVal::Str(s) => s.clone(),
            AttrVal::Bool(b) => b.to_string(),
            AttrVal::Int(n) => n.to_string(),
        };
        parts.push(format!("{k}={v}"));
    }
    parts.join("&")
}

fn attrs_to_json(a: &Attrs) -> String {
    let mut m = serde_json::Map::new();
    for (k, v) in a {
        let v = match v {
            AttrVal::Str(s) => serde_json::Value::String(s.clone()),
            AttrVal::Bool(b) => serde_json::Value::Bool(*b),
            AttrVal::Int(n) => serde_json::Value::from(*n),
        };
        m.insert(k.clone(), v);
    }
    serde_json::Value::Object(m).to_string()
}

/// A Nix `"..."` literal holding `s`.
fn nix_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '$' => out.push_str("\\$"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The tree must exist at its store path so `import` reads exactly what Nix
/// reads. A missing tree is fetched by handing the locked attrs to Nix's own
/// `fetchTree`; the store path Nix reports must be the one the lock implies.
pub fn ensure_in_store(
    ev: &mut Evaluator,
    sp: &StorePath,
    attrs: &Attrs,
    pos: Pos,
) -> EvalResult<()> {
    let full = sp.to_full_string();
    let p = Path::new(&full);
    if crate::trace::stat_kind(p, true) == crate::trace::Kind::Missing {
        let expr = format!(
            "(builtins.fetchTree (builtins.fromJSON {})).outPath",
            nix_string_literal(&attrs_to_json(attrs))
        );
        eprintln!(
            "ietsp: '{full}' is not in the store, fetching {} input with nix",
            str_attr(attrs, "type").unwrap_or("?")
        );
        let out = std::process::Command::new("nix")
            .args([
                "--extra-experimental-features",
                "nix-command flakes",
                "eval",
                "--raw",
                "--impure",
                "--expr",
                &expr,
            ])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .output()
            .map_err(|e| Error::boxed(ErrorKind::Io, format!("ietsp: cannot run nix: {e}"), pos))?;
        if !out.status.success() {
            bail!(
                ErrorKind::Eval,
                pos,
                "ietsp: nix could not fetch input {}",
                to_url(attrs)
            );
        }
        let got = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if got != full {
            bail!(
                ErrorKind::Eval,
                pos,
                "ietsp: nix fetched '{got}' but the lock implies '{full}' for input {}",
                to_url(attrs)
            );
        }
    }
    if ev.trace.stat_now(full.as_bytes(), true) == crate::trace::Kind::Missing {
        bail!(ErrorKind::Io, pos, "ietsp: '{full}' is not in the store");
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_path_from_nar_hash() {
        let h = parse_nar_hash("sha256-9i/VTdusq/+NM/tz+J1Re+ojkMB8MBf0QshnYfzHz30=").unwrap();
        assert_eq!(
            source_store_path(&h).unwrap().to_full_string(),
            "/nix/store/z4wkdlk69gck09s64a8ghpn6h7bxhkyy-source"
        );
    }

    #[test]
    fn nix_literal_escapes() {
        assert_eq!(
            nix_string_literal(r#"{"a":"x\"y${z}\\"}"#),
            r#""{\"a\":\"x\\\"y\${z}\\\\\"}""#
        );
    }

    #[test]
    fn dates() {
        assert_eq!(last_modified_date(1789565004), "20260916132324");
        assert_eq!(last_modified_date(1700000000), "20231114221320");
        assert_eq!(last_modified_date(0), "19700101000000");
        assert_eq!(last_modified_date(951782400), "20000229000000");
    }
}
