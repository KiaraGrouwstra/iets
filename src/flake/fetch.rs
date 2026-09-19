//! `builtins.fetchTree` for locked inputs (fetchTree.cc, fetchers.cc): the
//! store path follows from the `narHash` and the result attrset mirrors
//! `emitTreeAttrs`. A `path` input is a virtual copy of the local
//! directory, like the root tree. A remote input (github, git, tarball,
//! ...) must be in `/nix/store`; one that is not is substituted by the daemon,
//! else downloaded and registered by ietsp itself (`github`, `gitlab`,
//! `sourcehut`, `tarball`), else fetched by running `nix` once (`git`, `hg`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos, bail};
use crate::remote::worker::{Connection, Transport};
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

pub fn source_store_path(nar_hash: &Hash) -> crate::error::Result<StorePath> {
    make_fixed_output_path(
        IngestionMethod::Recursive,
        nar_hash,
        "source",
        &std::collections::BTreeSet::new(),
    )
    .map_err(Into::into)
}

pub fn parse_nar_hash(s: &str) -> crate::error::Result<Hash> {
    Hash::parse_any(s, Some(HashAlgo::Sha256)).map_err(Into::into)
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
                let exp = parse_nar_hash(expected)
                    .map_err(|e| Error::boxed(ErrorKind::Eval, e.to_string(), pos))?;
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
            source_store_path(&hash)
                .map_err(|e| Error::boxed(ErrorKind::Eval, e.to_string(), pos))?
        }
        "github" | "gitlab" | "sourcehut" | "git" | "tarball" | "hg" => {
            let Some(nar_hash) = str_attr(&attrs, "narHash") else {
                bail!(
                    ErrorKind::Unsupported,
                    pos,
                    "ietsp: cannot fetch unlocked input '{}' (no narHash); run nix flake lock",
                    to_url(&attrs)
                );
            };
            let hash = parse_nar_hash(nar_hash)
                .map_err(|e| Error::boxed(ErrorKind::Eval, e.to_string(), pos))?;
            attrs.insert("narHash".into(), AttrVal::Str(hash.to_sri()));
            let sp = source_store_path(&hash)
                .map_err(|e| Error::boxed(ErrorKind::Eval, e.to_string(), pos))?;
            ensure_in_store(ev, &sp, &attrs, pos)?;
            sp
        }
        other => bail!(
            ErrorKind::Unsupported,
            pos,
            "ietsp: fetchTree input type '{other}' is not supported"
        ),
    };
    emit_tree_attrs(ev, &sp, &attrs, pos)
}

/// The lock file's name for a locked input, else its attrs as a URL.
fn input_name(ev: &Evaluator, attrs: &Attrs) -> String {
    str_attr(attrs, "narHash")
        .and_then(|h| ev.input_names.get(h))
        .unwrap_or_else(|| to_url(attrs))
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

/// The evaluator's connection to the eval store, opened on the first input
/// that is missing from it and kept for the rest of the evaluation. The eval
/// store is nix.conf's `store`, never the `builder` setting: a remote builder
/// has no say in what `import` reads. A `grpc://` store is not a
/// worker-protocol daemon, so there is nothing to open and nothing to
/// substitute with.
#[derive(Default)]
pub struct LazyStore {
    conn: Option<Connection>,
    tried: bool,
}

impl Clone for LazyStore {
    /// A fiber's evaluator starts without one: the worker protocol is one
    /// request at a time, so a connection is never shared.
    fn clone(&self) -> LazyStore {
        LazyStore::default()
    }
}

impl LazyStore {
    fn get(&mut self) -> Option<&mut Connection> {
        if !self.tried {
            self.tried = true;
            match Transport::parse(&nix_conf().store()) {
                Some(Transport::Grpc(_)) | None => {}
                Some(t) => match Connection::open(&t, &mut |_| {}) {
                    Ok(c) => self.conn = Some(c),
                    Err(e) => eprintln!("ietsp: cannot reach the store to fetch inputs: {e}"),
                },
            }
        }
        self.conn.as_mut()
    }
}

/// fetchers.cc:getAccessorUnchecked: a locked input carries `__final`, so Nix
/// substitutes its store path before it runs any fetcher. An expression cannot
/// mark an input final (`fetchFinalTree` is an internal primop), so ietsp
/// substitutes the tree itself first, and only then fetches. One
/// `QueryValidPaths` with the substitute flag does it: the daemon runs
/// `Store::substitutePaths` and answers with the paths it now has
/// (daemon.cc:performOp, worker-protocol.hh:wopQueryValidPaths, >= 1.27).
/// Nothing is evaluated and no derivation is built.
fn substitute(ev: &mut Evaluator, store_path: &str) -> bool {
    let Some(conn) = ev.store.get() else {
        return false;
    };
    match conn.substitute_paths(std::slice::from_ref(&store_path.to_string()), &mut |_| {}) {
        Ok(valid) => valid.iter().any(|p| p == store_path),
        Err(e) => {
            eprintln!("ietsp: cannot substitute '{store_path}': {e}");
            false
        }
    }
}

/// A remote input's tree must exist at its store path so `import` reads
/// exactly what Nix reads. A missing tree is substituted by the daemon, else
/// downloaded and registered by ietsp (the tarball-shaped types), else fetched
/// by handing the locked attrs to Nix's own `fetchTree`; whichever way, the
/// store path must be the one the lock implies. Never called for the root tree
/// or a `path` input.
pub fn ensure_in_store(
    ev: &mut Evaluator,
    sp: &StorePath,
    attrs: &Attrs,
    pos: Pos,
) -> EvalResult<()> {
    let full = sp.to_full_string();
    let p = Path::new(&full);
    if crate::trace::stat_kind(p, true) == crate::trace::Kind::Missing {
        let name = input_name(ev, attrs);
        eprintln!("ietsp: input '{name}' is not in the store ({full}), fetching it");
        if !substitute(ev, &full) {
            if is_tarball_type(attrs) {
                fetch_tarball(ev, sp, attrs, pos)?;
            } else {
                fetch_with_nix(&full, &name, attrs, pos)?;
            }
        }
    }
    if ev.trace.stat_now(full.as_bytes(), true) == crate::trace::Kind::Missing {
        bail!(
            ErrorKind::Io,
            pos,
            "ietsp: input '{}' is not in the store ({full})",
            input_name(ev, attrs)
        );
    }
    Ok(())
}

/// nix.conf, read once: `store` for the eval store and `access-tokens` for the
/// forge headers. The file cannot change under a running evaluation.
fn nix_conf() -> &'static crate::config::Config {
    static CFG: std::sync::OnceLock<crate::config::Config> = std::sync::OnceLock::new();
    CFG.get_or_init(|| {
        crate::config::Config::load(&[]).unwrap_or_else(|_| crate::config::Config::empty())
    })
}

/// The types whose fetch is "download one archive and unpack it"
/// (github.cc:GitArchiveInputScheme, tarball.cc:TarballInputScheme). `git` and
/// `hg` are not here: they need a repository, so they still go through nix.
/// `file` is not either: its store path is the flat hash of the bytes under the
/// input's name, which the lock's `narHash` does not give, so ietsp cannot say
/// where the file belongs before it has it.
fn is_tarball_type(attrs: &Attrs) -> bool {
    matches!(
        str_attr(attrs, "type").unwrap_or(""),
        "github" | "gitlab" | "sourcehut" | "tarball"
    )
}

/// github.cc:getAccessToken: the longest `access-tokens` key that is a prefix of
/// `host/owner/repo` and ends on a path segment, else the entry for the host.
fn access_token(cfg: &crate::config::Config, host: &str, host_and_path: &str) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    let mut for_host = None;
    for entry in cfg.list("access-tokens") {
        let Some((key, value)) = entry.split_once('=') else {
            continue;
        };
        if key == host {
            for_host = Some(value.to_string());
        }
        if host_and_path.starts_with(key)
            && (host_and_path.len() == key.len() || host_and_path.as_bytes()[key.len()] == b'/')
            && best.as_ref().is_none_or(|(len, _)| key.len() > *len)
        {
            best = Some((key.len(), value.to_string()));
        }
    }
    best.map(|(_, v)| v).or(for_host)
}

/// The `Authorization`-style header each forge wants for its token
/// (github.cc:accessHeaderFromToken, one override per scheme).
fn access_header(ty: &str, token: &str) -> (String, String) {
    match ty {
        "sourcehut" => ("Authorization".into(), format!("Bearer {token}")),
        "gitlab" => match token.split_once(':') {
            Some(("OAuth2", t)) => ("Authorization".into(), format!("Bearer {t}")),
            Some(("PAT", t)) => ("Private-token".into(), t.into()),
            Some((name, value)) => (name.into(), value.into()),
            None => (token.into(), token.into()),
        },
        _ => ("Authorization".into(), format!("token {token}")),
    }
}

/// github.cc:getDownloadUrl and tarball.cc: the URL and headers Nix downloads a
/// locked archive input from. The `rev` is the 40 hex digits the lock holds.
fn tarball_url(
    cfg: &crate::config::Config,
    attrs: &Attrs,
) -> std::result::Result<(String, Vec<(String, String)>), String> {
    let ty = str_attr(attrs, "type").unwrap_or("");
    if ty == "tarball" {
        let url = str_attr(attrs, "url").ok_or("the lock has no 'url'")?;
        return Ok((url.to_string(), Vec::new()));
    }
    let owner = str_attr(attrs, "owner").ok_or("the lock has no 'owner'")?;
    let repo = str_attr(attrs, "repo").ok_or("the lock has no 'repo'")?;
    let rev = str_attr(attrs, "rev").ok_or("the lock has no 'rev'")?;
    let default_host = match ty {
        "gitlab" => "gitlab.com",
        "sourcehut" => "git.sr.ht",
        _ => "github.com",
    };
    let host = str_attr(attrs, "host").unwrap_or(default_host);
    let headers: Vec<(String, String)> = access_token(cfg, host, &format!("{host}/{owner}/{repo}"))
        .map(|t| vec![access_header(ty, &t)])
        .unwrap_or_default();
    let url = match ty {
        "gitlab" => format!(
            "https://{host}/api/v4/projects/{owner}%2F{repo}/repository/archive.tar.gz?sha={rev}"
        ),
        "sourcehut" => format!("https://{host}/{owner}/{repo}/archive/{rev}.tar.gz"),
        // With a token the API endpoint; without one the public archive, which
        // has the looser rate limit.
        _ if host != "github.com" => {
            format!("https://{host}/api/v3/repos/{owner}/{repo}/tarball/{rev}")
        }
        _ if headers.is_empty() => {
            format!("https://{host}/{owner}/{repo}/archive/{rev}.tar.gz")
        }
        _ => format!("https://api.{host}/repos/{owner}/{repo}/tarball/{rev}"),
    };
    Ok((url, headers))
}

/// A directory removed however the function that made it returns.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Downloads the input's archive, unpacks it like Nix, checks the NAR hash
/// against the lock, and registers the tree with one `AddMultipleToStore` as a
/// `fixed:r:sha256` source, so the daemon recomputes the content address and
/// the path it writes is the one the lock implies. Nothing is cached on disk:
/// the unpacked copy is gone before this returns.
fn fetch_tarball(ev: &mut Evaluator, sp: &StorePath, attrs: &Attrs, pos: Pos) -> EvalResult<()> {
    let name = input_name(ev, attrs);
    let fail = |e: String| Error::boxed(ErrorKind::Eval, e, pos);
    let (url, headers) = tarball_url(nix_conf(), attrs)
        .map_err(|e| fail(format!("ietsp: cannot fetch input '{name}': {e}")))?;
    let expected = str_attr(attrs, "narHash")
        .and_then(|h| parse_nar_hash(h).ok())
        .ok_or_else(|| fail(format!("ietsp: input '{name}' has no usable 'narHash'")))?;

    let bytes = crate::flake::http::get(&url, &headers)
        .and_then(crate::flake::http::decompress)
        .map_err(|e| fail(format!("ietsp: cannot fetch input '{name}': {e}")))?;
    let scratch = Scratch(std::env::temp_dir().join(format!(
        "ietsp-fetch-{}-{}",
        std::process::id(),
        sp.base_name()
    )));
    let _ = std::fs::remove_dir_all(&scratch.0);
    let root = crate::flake::unpack::unpack(bytes, &scratch.0)
        .map_err(|e| fail(format!("ietsp: cannot unpack input '{name}': {e}")))?;

    let object = crate::remote::plan::Object::source(
        sp.clone(),
        crate::remote::plan::SourceInfo::tree(&root),
    )
    .map_err(|e| {
        Error::boxed(
            ErrorKind::Io,
            format!("ietsp: cannot hash the tree of input '{name}': {e}"),
            pos,
        )
    })?;
    let crate::remote::plan::Object::Source { nar_hash, .. } = &object else {
        unreachable!("a source object");
    };
    if *nar_hash != expected {
        bail!(
            ErrorKind::Eval,
            pos,
            "ietsp: NAR hash mismatch in input '{name}' ({url}): the lock says '{}' \
             but the tree ietsp fetched hashes to '{}'",
            expected.to_sri(),
            nar_hash.to_sri()
        );
    }
    let Some(conn) = ev.store.get() else {
        bail!(
            ErrorKind::Eval,
            pos,
            "ietsp: input '{name}' was fetched but there is no store to register it in"
        );
    };
    let mut body = |w: &mut dyn std::io::Write| -> std::io::Result<()> {
        crate::remote::worker::wire::write_u64(w, 1)?;
        crate::remote::worker::build::write_object(w, &object)
    };
    conn.add_multiple_to_store(false, &mut body, &mut |_| {})
        .map_err(|e| fail(format!("ietsp: cannot register input '{name}': {e}")))?;
    Ok(())
}

/// The last shell-out: a `git` or `hg` input needs a repository, which ietsp
/// cannot clone yet, so its locked attrs go to Nix's own `fetchTree` and the
/// store path Nix reports must be the one the lock implies.
fn fetch_with_nix(full: &str, name: &str, attrs: &Attrs, pos: Pos) -> EvalResult<()> {
    if crate::settings::get().fetching {
        bail!(
            ErrorKind::Eval,
            pos,
            "ietsp: recursive fetch of input '{name}': the nix on PATH is ietsp itself"
        );
    }
    let Some(nix) = crate::nixbin::real("nix") else {
        bail!(
            ErrorKind::Eval,
            pos,
            "ietsp: input '{name}' is not in the store and nix is not on PATH; \
             fetch it with nix ({}) first",
            to_url(attrs)
        );
    };
    let expr = format!(
        "(builtins.fetchTree (builtins.fromJSON {})).outPath",
        nix_string_literal(&attrs_to_json(attrs))
    );
    let out = std::process::Command::new(&nix)
        .env("IETSP_FETCHING", "1")
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
        .map_err(|e| {
            Error::boxed(
                ErrorKind::Io,
                format!("ietsp: cannot run nix to fetch input '{name}': {e}"),
                pos,
            )
        })?;
    if !out.status.success() {
        bail!(
            ErrorKind::Eval,
            pos,
            "ietsp: nix could not fetch input '{name}' ({})",
            to_url(attrs)
        );
    }
    let got = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if got != full {
        bail!(
            ErrorKind::Eval,
            pos,
            "ietsp: nix fetched '{got}' for input '{name}' but the lock implies '{full}'"
        );
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

    fn attrs(pairs: &[(&str, &str)]) -> Attrs {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), AttrVal::Str((*v).to_string())))
            .collect()
    }

    fn conf(text: &str) -> crate::config::Config {
        crate::config::Config::from_str(text, "test").unwrap()
    }

    const REV: &str = "b1d9ab70662946ef0850d488da1c9019f3a9752a";

    #[test]
    fn download_urls_match_the_fetchers() {
        let none = conf("");
        let gh = attrs(&[
            ("type", "github"),
            ("owner", "numtide"),
            ("repo", "flake-utils"),
            ("rev", REV),
        ]);
        assert_eq!(
            tarball_url(&none, &gh).unwrap(),
            (
                format!("https://github.com/numtide/flake-utils/archive/{REV}.tar.gz"),
                vec![]
            )
        );
        // A token moves github.com to the API endpoint (github.cc:getDownloadUrl).
        assert_eq!(
            tarball_url(&conf("access-tokens = github.com=T"), &gh).unwrap(),
            (
                format!("https://api.github.com/repos/numtide/flake-utils/tarball/{REV}"),
                vec![("Authorization".into(), "token T".into())]
            )
        );
        let mut ghe = gh.clone();
        ghe.insert("host".into(), AttrVal::Str("git.corp".into()));
        assert_eq!(
            tarball_url(&none, &ghe).unwrap().0,
            format!("https://git.corp/api/v3/repos/numtide/flake-utils/tarball/{REV}")
        );

        let gl = attrs(&[
            ("type", "gitlab"),
            ("owner", "o"),
            ("repo", "r"),
            ("rev", REV),
        ]);
        assert_eq!(
            tarball_url(&conf("access-tokens = gitlab.com=PAT:T"), &gl).unwrap(),
            (
                format!(
                    "https://gitlab.com/api/v4/projects/o%2Fr/repository/archive.tar.gz?sha={REV}"
                ),
                vec![("Private-token".into(), "T".into())]
            )
        );
        assert_eq!(
            tarball_url(&conf("access-tokens = gitlab.com=OAuth2:T"), &gl)
                .unwrap()
                .1,
            vec![("Authorization".into(), "Bearer T".into())]
        );

        let sh = attrs(&[
            ("type", "sourcehut"),
            ("owner", "~o"),
            ("repo", "r"),
            ("rev", REV),
        ]);
        assert_eq!(
            tarball_url(&conf("access-tokens = git.sr.ht=T"), &sh).unwrap(),
            (
                format!("https://git.sr.ht/~o/r/archive/{REV}.tar.gz"),
                vec![("Authorization".into(), "Bearer T".into())]
            )
        );

        let tb = attrs(&[("type", "tarball"), ("url", "https://x.example/a.tar.xz")]);
        assert_eq!(
            tarball_url(&none, &tb).unwrap(),
            ("https://x.example/a.tar.xz".to_string(), vec![])
        );

        assert!(tarball_url(&none, &attrs(&[("type", "github")])).is_err());
    }

    /// github.cc:getAccessToken: the longest key that is a prefix of
    /// `host/owner/repo` on a path boundary wins, else the host's own entry.
    #[test]
    fn access_tokens_match_the_longest_prefix() {
        let c = conf(
            "access-tokens = github.com=host github.com/a=owner github.com/a/b=repo github.com/ab=other",
        );
        assert_eq!(
            access_token(&c, "github.com", "github.com/a/b").as_deref(),
            Some("repo")
        );
        assert_eq!(
            access_token(&c, "github.com", "github.com/a/c").as_deref(),
            Some("owner")
        );
        assert_eq!(
            access_token(&c, "github.com", "github.com/ab/c").as_deref(),
            Some("other")
        );
        assert_eq!(
            access_token(&c, "github.com", "github.com/z/y").as_deref(),
            Some("host")
        );
        assert_eq!(access_token(&c, "gitlab.com", "gitlab.com/z/y"), None);
    }

    /// What the substitution step costs, against the `nix build --no-link` it
    /// replaced, on a store path no substituter has: both must answer "no", and
    /// the op must be the cheaper way to hear it. Needs a running daemon and a
    /// nix on `PATH`, so it is ignored by default:
    /// `cargo test --release -- --ignored --nocapture substitution`.
    #[test]
    #[ignore]
    fn substitution_costs_less_than_the_subprocess() {
        let path = "/nix/store/zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-source";
        let mut store = LazyStore::default();
        let t = std::time::Instant::now();
        let conn = store.get().expect("a daemon");
        let connect = t.elapsed();
        let t = std::time::Instant::now();
        let valid = conn
            .substitute_paths(&[path.to_string()], &mut |_| {})
            .unwrap();
        let op = t.elapsed();
        assert!(valid.is_empty());

        let nix = crate::nixbin::real("nix").expect("a nix");
        let t = std::time::Instant::now();
        let status = std::process::Command::new(&nix)
            .args([
                "--extra-experimental-features",
                "nix-command flakes",
                "build",
                "--no-link",
                path,
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        let subprocess = t.elapsed();
        assert!(!status.success());
        eprintln!(
            "connect: {:.3}s, QueryValidPaths(substitute): {:.3}s, nix build --no-link: {:.3}s",
            connect.as_secs_f64(),
            op.as_secs_f64(),
            subprocess.as_secs_f64()
        );
        // The connection is opened once and the registration reuses it; the op
        // is what replaces the subprocess.
        assert!(op < subprocess);
    }

    #[test]
    fn only_the_archive_types_are_fetched_by_ietsp() {
        for ty in ["github", "gitlab", "sourcehut", "tarball"] {
            assert!(is_tarball_type(&attrs(&[("type", ty)])), "{ty}");
        }
        for ty in ["git", "hg", "path", "file"] {
            assert!(!is_tarball_type(&attrs(&[("type", ty)])), "{ty}");
        }
    }

    #[test]
    fn dates() {
        assert_eq!(last_modified_date(1789565004), "20260916132324");
        assert_eq!(last_modified_date(1700000000), "20231114221320");
        assert_eq!(last_modified_date(0), "19700101000000");
        assert_eq!(last_modified_date(951782400), "20000229000000");
    }
}
