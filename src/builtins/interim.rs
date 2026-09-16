//! INTERIM builtins for the nixpkgs parity ladder on branch `b-drv`.
//!
//! These belong to the strings/regex/json/hash and paths/files/contexts
//! families, which sibling branches implement properly. Delete this file
//! (and its two lines in `mod.rs`) when those branches merge; a duplicate
//! registration panics at startup, so the overlap cannot go unnoticed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::Registry;
use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos, bail};
use crate::runtime::*;
use crate::store::hash::{Hash, HashAlgo, HashFormat, hash_bytes};
use crate::store::nar::{FileType, flat_hash, nar_hash_filtered};
use crate::store::path::{IngestionMethod, StorePath, make_fixed_output_path, make_text_path};
use crate::syntax::symbol::Sym;

pub fn register(r: &mut Registry) {
    r.op("__appendContext", 2, append_context);
    r.op("__getContext", 1, get_context);
    r.op("break", 1, break_);
    r.op("__ceil", 1, ceil);
    r.op("__floor", 1, floor);
    r.op("__convertHash", 1, convert_hash);
    r.op("__currentTime", 0, current_time);
    r.op("__hashFile", 2, hash_file);
    r.op("__hashString", 2, hash_string);
    r.op("__match", 2, match_);
    r.op("__split", 2, split);
    r.op("__replaceStrings", 3, replace_strings);
    r.op("__fromJSON", 1, from_json);
    r.op("__toJSON", 1, to_json);
    r.op("__genericClosure", 1, generic_closure);
    r.op("__getEnv", 1, get_env);
    r.op("__pathExists", 1, path_exists);
    r.op("__readDir", 1, read_dir);
    r.op("__readFile", 1, read_file);
    r.op("__readFileType", 1, read_file_type);
    r.op("__filterSource", 2, filter_source);
    r.op("__path", 1, path);
    r.op("__storePath", 1, store_path);
    r.op("__toFile", 2, to_file);
    r.op("__toPath", 1, to_path);
    r.op("__toXML", 1, unsupported);
    r.op("fromTOML", 1, unsupported);
    r.op("__fetchClosure", 1, unsupported);
    r.op("fetchGit", 1, unsupported);
    r.op("fetchMercurial", 1, unsupported);
    r.op("fetchTarball", 1, unsupported);
    r.op("fetchTree", 1, unsupported);
    r.op("__fetchurl", 1, unsupported);
    r.op("__flakeRefToString", 1, unsupported);
    r.op("__parseFlakeRef", 1, unsupported);
    r.op("__getFlake", 1, unsupported);
}

fn unsupported(_ev: &mut Evaluator, _args: &[Word], pos: Pos) -> EvalResult<Word> {
    bail!(
        ErrorKind::Eval,
        pos,
        "builtin not supported by ietsp (interim)"
    )
}

fn utf8(b: &[u8], pos: Pos) -> EvalResult<&str> {
    std::str::from_utf8(b)
        .map_err(|_| Error::boxed(ErrorKind::Eval, "string is not valid UTF-8", pos))
}

fn io_err(what: &str, p: &[u8], e: std::io::Error, pos: Pos) -> Box<Error> {
    Error::boxed(
        ErrorKind::Io,
        format!("{what} '{}': {e}", String::from_utf8_lossy(p)),
        pos,
    )
}

/// Mirrors `EvalState::coerceToPath` for the read-only cases: paths, strings
/// (absolute), and derivations via `outPath`. Context is discarded.
fn coerce_to_path(ev: &mut Evaluator, w: Word, pos: Pos, err_ctx: &str) -> EvalResult<Vec<u8>> {
    let mut ctx = Vec::new();
    let s = ev.coerce_to_string(w, &mut ctx, pos, err_ctx, false, false, true)?;
    if !s.starts_with(b"/") {
        bail!(
            ErrorKind::Eval,
            pos,
            "string '{}' doesn't represent an absolute path",
            String::from_utf8_lossy(&s)
        );
    }
    Ok(crate::compile::canon_path(&s))
}

fn file_type_of(p: &Path) -> Option<FileType> {
    let m = std::fs::symlink_metadata(p).ok()?;
    let ft = m.file_type();
    Some(if ft.is_symlink() {
        FileType::Symlink
    } else if ft.is_dir() {
        FileType::Directory
    } else if ft.is_file() {
        FileType::Regular
    } else {
        FileType::Unknown
    })
}

// ---- context -----------------------------------------------------------------

/// Mirrors primops/context.cc:prim_getContext.
fn get_context(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let s = ev.force_string(
        args[0],
        pos,
        "while evaluating the argument passed to builtins.getContext",
    )?;
    #[derive(Default)]
    struct Info {
        path: bool,
        all_outputs: bool,
        outputs: Vec<&'static [u8]>,
    }
    let mut infos: BTreeMap<&'static [u8], Info> = BTreeMap::new();
    for c in s.ctx {
        match c {
            Ctx::Opaque(p) => infos.entry(p).or_default().path = true,
            Ctx::DrvDeep(d) => infos.entry(d).or_default().all_outputs = true,
            Ctx::Built { drv, output } => infos.entry(drv).or_default().outputs.push(output),
        }
    }
    let s_path = ev.sym(b"path");
    let s_all = ev.sym(b"allOutputs");
    let s_outputs = ev.sym(b"outputs");
    let mut entries = Vec::with_capacity(infos.len());
    for (p, info) in infos {
        let mut inner = Vec::new();
        if info.path {
            inner.push((s_path, Pos::NONE, Word::TRUE));
        }
        if info.all_outputs {
            inner.push((s_all, Pos::NONE, Word::TRUE));
        }
        if !info.outputs.is_empty() {
            let items: Vec<Word> = info
                .outputs
                .iter()
                .map(|o| ev.arena.mk_string_static(o, &[]))
                .collect();
            let l = ev.mk_list(&items);
            inner.push((s_outputs, Pos::NONE, l));
        }
        let a = ev.mk_attrs(&mut inner, pos)?;
        entries.push((ev.sym(p), Pos::NONE, a));
    }
    ev.mk_attrs(&mut entries, pos)
}

/// Mirrors primops/context.cc:prim_appendContext.
fn append_context(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let orig = ev.force_string(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.appendContext",
    )?;
    let mut ctx: Vec<Ctx> = orig.ctx.to_vec();
    let attrs = ev.force_attrs(
        args[1],
        pos,
        "while evaluating the second argument passed to builtins.appendContext",
    )?;
    let s_path = ev.sym(b"path");
    let s_all = ev.sym(b"allOutputs");
    let s_outputs = ev.sym(b"outputs");
    for a in attrs.attrs() {
        let name = ev.interner.name(a.sym).to_vec();
        let name = ev.arena.alloc_bytes(&name);
        let name_s = utf8(name, a.pos)?;
        if StorePath::parse(name_s).is_err() {
            bail!(
                ErrorKind::Eval,
                a.pos,
                "context key '{name_s}' is not a store path"
            );
        }
        let is_drv = name.ends_with(b".drv");
        let v = ev.force_attrs(
            a.cell.get(),
            a.pos,
            "while evaluating the value of a string context",
        )?;
        if let Some(p) = v.get(s_path)
            && ev.force_bool(
                p.cell.get(),
                p.pos,
                "while evaluating the `path` attribute of a string context",
            )?
        {
            ctx.push(Ctx::Opaque(name));
        }
        if let Some(p) = v.get(s_all)
            && ev.force_bool(
                p.cell.get(),
                p.pos,
                "while evaluating the `allOutputs` attribute of a string context",
            )?
        {
            if !is_drv {
                bail!(
                    ErrorKind::Eval,
                    a.pos,
                    "tried to add all-outputs context of {name_s}, which is not a derivation, to a string"
                );
            }
            ctx.push(Ctx::DrvDeep(name));
        }
        if let Some(p) = v.get(s_outputs) {
            let l = ev.force_list(
                p.cell.get(),
                p.pos,
                "while evaluating the `outputs` attribute of a string context",
            )?;
            if l.len > 0 && !is_drv {
                bail!(
                    ErrorKind::Eval,
                    a.pos,
                    "tried to add derivation output context of {name_s}, which is not a derivation, to a string"
                );
            }
            for c in l.cells() {
                let o = ev.force_string_no_ctx(
                    c.get(),
                    p.pos,
                    "while evaluating an output name within a string context",
                )?;
                ctx.push(Ctx::Built {
                    drv: name,
                    output: o,
                });
            }
        }
    }
    let ctx = ctx_to_arena(&mut ev.arena, ctx);
    Ok(ev.arena.mk_string_static(orig.bytes, ctx))
}

// ---- misc --------------------------------------------------------------------

fn break_(ev: &mut Evaluator, args: &[Word], _pos: Pos) -> EvalResult<Word> {
    ev.force(args[0])
}

fn ceil(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let f = ev.force_float(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.ceil",
    )?;
    Ok(ev.mk_int(f.ceil() as i64))
}

fn floor(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let f = ev.force_float(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.floor",
    )?;
    Ok(ev.mk_int(f.floor() as i64))
}

fn current_time(ev: &mut Evaluator, _args: &[Word], _pos: Pos) -> EvalResult<Word> {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Ok(ev.mk_int(t))
}

fn get_env(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let name = ev.force_string_no_ctx(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.getEnv",
    )?;
    let name = utf8(name, pos)?;
    let v = std::env::var_os(name).unwrap_or_default();
    Ok(ev.mk_string(v.as_encoded_bytes()))
}

fn to_path(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let mut ctx = Vec::new();
    let s = ev.coerce_to_string(
        args[0],
        &mut ctx,
        pos,
        "while evaluating the first argument passed to builtins.toPath",
        false,
        false,
        true,
    )?;
    if !s.starts_with(b"/") {
        bail!(
            ErrorKind::Eval,
            pos,
            "string '{}' doesn't represent an absolute path",
            String::from_utf8_lossy(&s)
        );
    }
    let p = crate::compile::canon_path(&s);
    let ctx = ctx_to_arena(&mut ev.arena, ctx);
    let bytes = ev.arena.alloc_bytes(&p);
    Ok(ev.arena.mk_string_static(bytes, ctx))
}

// ---- hashes ------------------------------------------------------------------

fn parse_algo(s: &str, pos: Pos) -> EvalResult<HashAlgo> {
    HashAlgo::parse(s).ok_or_else(|| {
        Error::boxed(
            ErrorKind::Eval,
            format!("unknown hash algorithm '{s}'"),
            pos,
        )
    })
}

fn hash_string(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let algo = ev.force_string_no_ctx(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.hashString",
    )?;
    let algo = parse_algo(utf8(algo, pos)?, pos)?;
    let s = ev.force_string(
        args[1],
        pos,
        "while evaluating the second argument passed to builtins.hashString",
    )?;
    Ok(ev.mk_string(hash_bytes(algo, s.bytes).to_base16().as_bytes()))
}

fn hash_file(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let algo = ev.force_string_no_ctx(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.hashFile",
    )?;
    let algo = parse_algo(utf8(algo, pos)?, pos)?;
    let p = coerce_to_path(
        ev,
        args[1],
        pos,
        "while evaluating the second argument passed to builtins.hashFile",
    )?;
    let h = flat_hash(Path::new(utf8(&p, pos)?), algo)
        .map_err(|e| io_err("opening file", &p, e, pos))?;
    Ok(ev.mk_string(h.to_base16().as_bytes()))
}

fn convert_hash(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let attrs = ev.force_attrs(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.convertHash",
    )?;
    let s_hash = ev.sym(b"hash");
    let s_algo = ev.sym(b"hashAlgo");
    let s_fmt = ev.sym(b"toHashFormat");
    let Some(h) = attrs.get(s_hash) else {
        bail!(ErrorKind::MissingAttr, pos, "attribute 'hash' missing");
    };
    let h = ev.force_string_no_ctx(h.cell.get(), pos, "while evaluating the attribute 'hash'")?;
    let h = utf8(h, pos)?.to_string();
    let algo = match attrs.get(s_algo) {
        Some(a) => {
            let s = ev.force_string_no_ctx(
                a.cell.get(),
                pos,
                "while evaluating the attribute 'hashAlgo'",
            )?;
            Some(parse_algo(utf8(s, pos)?, pos)?)
        }
        None => None,
    };
    let Some(f) = attrs.get(s_fmt) else {
        bail!(
            ErrorKind::MissingAttr,
            pos,
            "attribute 'toHashFormat' missing"
        );
    };
    let f = ev.force_string_no_ctx(
        f.cell.get(),
        pos,
        "while evaluating the attribute 'toHashFormat'",
    )?;
    let (fmt, sri) = match utf8(f, pos)? {
        "base16" => (HashFormat::Base16, false),
        "nix32" | "base32" => (HashFormat::Nix32, false),
        "base64" => (HashFormat::Base64, false),
        "sri" => (HashFormat::Sri, true),
        other => bail!(ErrorKind::Eval, pos, "unknown hash format '{other}'"),
    };
    let hash = Hash::parse_any(&h, algo).map_err(|e| Error::boxed(ErrorKind::Eval, e.0, pos))?;
    Ok(ev.mk_string(hash.to_string_with(fmt, sri).as_bytes()))
}

// ---- regex -------------------------------------------------------------------

/// POSIX ERE as libstdc++ runs it: `.` matches newlines.
fn compile_regex(
    ev: &mut Evaluator,
    re: &[u8],
    anchored: bool,
    pos: Pos,
) -> EvalResult<regex::bytes::Regex> {
    let re_s = utf8(re, pos)?;
    let pat = if anchored {
        format!("^(?s:{re_s})$")
    } else {
        format!("(?s:{re_s})")
    };
    if let Some(r) = ev.regex_cache.get(&pat) {
        return Ok(r.clone());
    }
    let r = regex::bytes::Regex::new(&pat).map_err(|_| {
        Error::boxed(
            ErrorKind::Eval,
            format!("invalid regular expression '{re_s}'"),
            pos,
        )
    })?;
    ev.regex_cache.insert(pat, r.clone());
    Ok(r)
}

fn captures_to_list(ev: &mut Evaluator, caps: &regex::bytes::Captures<'_>) -> Word {
    let n = caps.len() - 1;
    let l = ev.arena.alloc_list(n);
    for (i, c) in l.cells().iter().enumerate() {
        c.set(match caps.get(i + 1) {
            Some(m) => ev.mk_string(m.as_bytes()),
            None => Word::NULL,
        });
    }
    Word::from_ptr(l)
}

fn match_(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let re = ev.force_string_no_ctx(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.match",
    )?;
    let re = compile_regex(ev, re, true, pos)?;
    let s = ev.force_string(
        args[1],
        pos,
        "while evaluating the second argument passed to builtins.match",
    )?;
    match re.captures(s.bytes) {
        None => Ok(Word::NULL),
        Some(caps) => Ok(captures_to_list(ev, &caps)),
    }
}

fn split(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let re = ev.force_string_no_ctx(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.split",
    )?;
    let re = compile_regex(ev, re, false, pos)?;
    let s = ev.force_string(
        args[1],
        pos,
        "while evaluating the second argument passed to builtins.split",
    )?;
    let mut items: Vec<Word> = Vec::new();
    let mut last = 0;
    for caps in re.captures_iter(s.bytes) {
        let m = caps.get(0).unwrap();
        items.push(ev.mk_string(&s.bytes[last..m.start()]));
        items.push(captures_to_list(ev, &caps));
        last = m.end();
    }
    if items.is_empty() {
        return Ok(ev.mk_list(&[Word::from_ptr(s)]));
    }
    items.push(ev.mk_string(&s.bytes[last..]));
    Ok(ev.mk_list(&items))
}

/// Mirrors primops.cc:prim_replaceStrings (including the empty-`from` case).
fn replace_strings(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let from_l = ev.force_list(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.replaceStrings",
    )?;
    let to_l = ev.force_list(
        args[1],
        pos,
        "while evaluating the second argument passed to builtins.replaceStrings",
    )?;
    if from_l.len != to_l.len {
        bail!(
            ErrorKind::Eval,
            pos,
            "'from' and 'to' arguments passed to builtins.replaceStrings have different lengths"
        );
    }
    let mut from: Vec<&'static [u8]> = Vec::with_capacity(from_l.len as usize);
    for c in from_l.cells() {
        from.push(
            ev.force_string(
                c.get(),
                pos,
                "while evaluating one of the strings to replace passed to builtins.replaceStrings",
            )?
            .bytes,
        );
    }
    let s = ev.force_string(
        args[2],
        pos,
        "while evaluating the third argument passed to builtins.replaceStrings",
    )?;
    let mut ctx: Vec<Ctx> = s.ctx.to_vec();
    let mut to: Vec<Option<&'static StringObj>> = vec![None; from.len()];
    let bytes = s.bytes;
    let mut res: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut p = 0;
    while p <= bytes.len() {
        let mut found = false;
        for (i, f) in from.iter().enumerate() {
            if bytes[p..].starts_with(f) {
                found = true;
                let t = match to[i] {
                    Some(t) => t,
                    None => {
                        let t = ev.force_string(
                            to_l.cells()[i].get(),
                            pos,
                            "while evaluating one of the replacement strings passed to builtins.replaceStrings",
                        )?;
                        ctx.extend_from_slice(t.ctx);
                        to[i] = Some(t);
                        t
                    }
                };
                res.extend_from_slice(t.bytes);
                if f.is_empty() {
                    if p < bytes.len() {
                        res.push(bytes[p]);
                    }
                    p += 1;
                } else {
                    p += f.len();
                }
                break;
            }
        }
        if !found {
            if p < bytes.len() {
                res.push(bytes[p]);
            }
            p += 1;
        }
    }
    let ctx = ctx_to_arena(&mut ev.arena, ctx);
    let out = ev.arena.alloc_bytes(&res);
    Ok(ev.arena.mk_string_static(out, ctx))
}

// ---- JSON --------------------------------------------------------------------

fn to_json(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let mut out = Vec::new();
    let mut ctx = Vec::new();
    crate::print::print_json(ev, args[0], &mut out, &mut ctx, true, pos)?;
    let ctx = ctx_to_arena(&mut ev.arena, ctx);
    let bytes = ev.arena.alloc_bytes(&out);
    Ok(ev.arena.mk_string_static(bytes, ctx))
}

struct JsonParser<'a> {
    s: &'a [u8],
    i: usize,
    pos: Pos,
}

impl JsonParser<'_> {
    fn err<T>(&self, msg: &str) -> EvalResult<T> {
        Err(Error::boxed(
            ErrorKind::Eval,
            format!("while parsing a JSON value: {msg} at byte {}", self.i),
            self.pos,
        ))
    }

    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn value(&mut self, ev: &mut Evaluator) -> EvalResult<Word> {
        self.ws();
        match self.peek() {
            None => self.err("unexpected end of input"),
            Some(b'{') => {
                self.i += 1;
                let mut entries: Vec<(Sym, Pos, Word)> = Vec::new();
                self.ws();
                if self.peek() == Some(b'}') {
                    self.i += 1;
                    return Ok(ev.empty_attrs());
                }
                loop {
                    self.ws();
                    if self.peek() != Some(b'"') {
                        return self.err("expected string key");
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.peek() != Some(b':') {
                        return self.err("expected ':'");
                    }
                    self.i += 1;
                    let v = self.value(ev)?;
                    let sym = ev.sym(&k);
                    match entries.iter_mut().find(|e| e.0 == sym) {
                        Some(e) => e.2 = v,
                        None => entries.push((sym, Pos::NONE, v)),
                    }
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            break;
                        }
                        _ => return self.err("expected ',' or '}'"),
                    }
                }
                ev.mk_attrs(&mut entries, self.pos)
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.peek() == Some(b']') {
                    self.i += 1;
                    return Ok(ev.mk_list(&[]));
                }
                loop {
                    items.push(self.value(ev)?);
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            break;
                        }
                        _ => return self.err("expected ',' or ']'"),
                    }
                }
                Ok(ev.mk_list(&items))
            }
            Some(b'"') => {
                let s = self.string()?;
                Ok(ev.mk_string(&s))
            }
            Some(b't') if self.s[self.i..].starts_with(b"true") => {
                self.i += 4;
                Ok(Word::TRUE)
            }
            Some(b'f') if self.s[self.i..].starts_with(b"false") => {
                self.i += 5;
                Ok(Word::FALSE)
            }
            Some(b'n') if self.s[self.i..].starts_with(b"null") => {
                self.i += 4;
                Ok(Word::NULL)
            }
            Some(c) if c == b'-' || c.is_ascii_digit() => {
                let start = self.i;
                self.i += 1;
                let mut is_float = false;
                while let Some(c) = self.peek() {
                    match c {
                        b'0'..=b'9' => {}
                        b'.' | b'e' | b'E' | b'+' | b'-' => is_float = true,
                        _ => break,
                    }
                    self.i += 1;
                }
                let t = std::str::from_utf8(&self.s[start..self.i]).unwrap();
                if is_float {
                    match t.parse::<f64>() {
                        Ok(f) => Ok(ev.mk_float(f)),
                        Err(_) => self.err("invalid number"),
                    }
                } else {
                    match t.parse::<i64>() {
                        Ok(i) => Ok(ev.mk_int(i)),
                        Err(_) => self.err("invalid number"),
                    }
                }
            }
            Some(_) => self.err("unexpected character"),
        }
    }

    fn hex4(&mut self) -> EvalResult<u32> {
        if self.i + 4 > self.s.len() {
            return self.err("truncated \\u escape");
        }
        let h = std::str::from_utf8(&self.s[self.i..self.i + 4]).unwrap_or("");
        self.i += 4;
        u32::from_str_radix(h, 16).or_else(|_| self.err("bad \\u escape"))
    }

    fn string(&mut self) -> EvalResult<Vec<u8>> {
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let Some(c) = self.peek() else {
                return self.err("unterminated string");
            };
            self.i += 1;
            match c {
                b'"' => return Ok(out),
                b'\\' => {
                    let Some(e) = self.peek() else {
                        return self.err("unterminated escape");
                    };
                    self.i += 1;
                    match e {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) {
                                if !self.s[self.i..].starts_with(b"\\u") {
                                    return self.err("missing low surrogate");
                                }
                                self.i += 2;
                                let lo = self.hex4()?;
                                cp = 0x10000
                                    + ((cp - 0xD800) << 10)
                                    + (lo.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            let ch = char::from_u32(cp).unwrap_or('\u{FFFD}');
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        _ => return self.err("bad escape"),
                    }
                }
                c => out.push(c),
            }
        }
    }
}

fn from_json(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let s = ev.force_string_no_ctx(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.fromJSON",
    )?;
    let mut p = JsonParser { s, i: 0, pos };
    let v = p.value(ev)?;
    p.ws();
    if p.i != s.len() {
        return p.err("trailing characters");
    }
    Ok(v)
}

// ---- genericClosure ----------------------------------------------------------

fn generic_closure(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let attrs = ev.force_attrs(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.genericClosure",
    )?;
    let s_start = ev.sym(b"startSet");
    let s_op = ev.sym(b"operator");
    let s_key = ev.sym(b"key");
    let Some(start) = attrs.get(s_start) else {
        bail!(ErrorKind::MissingAttr, pos, "attribute 'startSet' missing");
    };
    let start_w = start.cell.get();
    let start_l = ev.force_list(
        start_w,
        pos,
        "while evaluating the 'startSet' attribute passed as argument to builtins.genericClosure",
    )?;
    if start_l.len == 0 {
        return ev.force(start_w);
    }
    let Some(op) = attrs.get(s_op) else {
        bail!(ErrorKind::MissingAttr, pos, "attribute 'operator' missing");
    };
    let op = ev.force_function(
        op.cell.get(),
        pos,
        "while evaluating the 'operator' attribute passed as argument to builtins.genericClosure",
    )?;
    let mut work: std::collections::VecDeque<Word> =
        start_l.cells().iter().map(|c| c.get()).collect();
    let mut keys: Vec<Word> = Vec::new();
    let mut res: Vec<Word> = Vec::new();
    while let Some(e) = work.pop_front() {
        let a = ev.force_attrs(e, pos, "")?;
        let Some(k) = a.get(s_key) else {
            bail!(ErrorKind::MissingAttr, pos, "attribute 'key' missing");
        };
        let kv = ev.force_cell(&k.cell)?;
        let mut dup = false;
        for &other in &keys {
            if ev.eq_values(kv, other, pos)? {
                dup = true;
                break;
            }
        }
        if dup {
            continue;
        }
        keys.push(kv);
        let e = ev.force(e)?;
        res.push(e);
        let r = ev.call1(op, e, pos)?;
        let l = ev.force_list(
            r,
            pos,
            "while evaluating the return value of the `operator` passed to builtins.genericClosure",
        )?;
        work.extend(l.cells().iter().map(|c| c.get()));
    }
    Ok(ev.mk_list(&res))
}

// ---- files -------------------------------------------------------------------

fn read_file(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let p = coerce_to_path(
        ev,
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.readFile",
    )?;
    let data = std::fs::read(utf8(&p, pos)?).map_err(|e| io_err("reading file", &p, e, pos))?;
    if data.contains(&0) {
        bail!(
            ErrorKind::Eval,
            pos,
            "the contents of the file '{}' cannot be represented as a Nix string",
            String::from_utf8_lossy(&p)
        );
    }
    // Nix adds the store path's references that occur in the file; sources
    // fetched into the store have none, and we cannot query the store DB.
    let ctx: &[Ctx] = &[];
    let bytes = ev.arena.alloc_bytes(&data);
    Ok(ev.arena.mk_string_static(bytes, ctx))
}

fn path_exists(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let w = ev.force(args[0])?;
    let p = match coerce_to_path(ev, w, pos, "while realising the context of a path") {
        Ok(p) => p,
        Err(e) if e.kind == ErrorKind::Type => return Err(e),
        Err(_) => return Ok(Word::FALSE),
    };
    let path = Path::new(utf8(&p, pos)?);
    Ok(Word::from_bool(
        path.exists() || path.symlink_metadata().is_ok(),
    ))
}

fn read_file_type(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let p = coerce_to_path(
        ev,
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.readFileType",
    )?;
    let path = Path::new(utf8(&p, pos)?);
    match file_type_of(path) {
        Some(t) => Ok(ev.mk_string(t.nix_name().as_bytes())),
        None => bail!(
            ErrorKind::Io,
            pos,
            "getting status of '{}': No such file or directory",
            String::from_utf8_lossy(&p)
        ),
    }
}

fn read_dir(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let p = coerce_to_path(
        ev,
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.readDir",
    )?;
    let rd =
        std::fs::read_dir(utf8(&p, pos)?).map_err(|e| io_err("reading directory", &p, e, pos))?;
    let mut entries: Vec<(Sym, Pos, Word)> = Vec::new();
    for e in rd {
        let e = e.map_err(|e| io_err("reading directory", &p, e, pos))?;
        let name = e.file_name();
        let ft = e
            .file_type()
            .map_err(|e| io_err("reading directory", &p, e, pos))?;
        let t = if ft.is_symlink() {
            FileType::Symlink
        } else if ft.is_dir() {
            FileType::Directory
        } else if ft.is_file() {
            FileType::Regular
        } else {
            FileType::Unknown
        };
        let ts = ev.mk_string(t.nix_name().as_bytes());
        entries.push((ev.sym(name.as_encoded_bytes()), Pos::NONE, ts));
    }
    ev.mk_attrs(&mut entries, pos)
}

// ---- store paths -------------------------------------------------------------

/// Mirrors primops.cc:addPath in read-only mode: compute the store path of
/// `path` with an optional filter, no writes.
#[allow(clippy::too_many_arguments)]
fn add_path(
    ev: &mut Evaluator,
    name: &str,
    path: &[u8],
    filter: Option<Word>,
    method: IngestionMethod,
    expected: Option<Hash>,
    pos: Pos,
) -> EvalResult<Word> {
    let p = Path::new(utf8(path, pos)?);
    let refs = BTreeSet::new();
    if let Some(h) = &expected {
        let sp = make_fixed_output_path(method, h, name, &refs)
            .map_err(|e| Error::boxed(ErrorKind::Eval, e.0, pos))?;
        if Path::new(&sp.to_string()).exists() {
            let full = ev.arena.alloc_bytes(sp.to_string().as_bytes());
            return Ok(ev.arena.mk_string_ctx(full, &[Ctx::Opaque(full)]));
        }
    }
    let hash = match method {
        IngestionMethod::Flat => {
            flat_hash(p, HashAlgo::Sha256).map_err(|e| io_err("hashing", path, e, pos))?
        }
        IngestionMethod::Recursive => {
            let mut err: Option<Box<Error>> = None;
            let mut f = |fp: &Path, t: FileType| -> bool {
                let Some(fun) = filter else { return true };
                if err.is_some() {
                    return false;
                }
                let ps = ev.mk_string(fp.as_os_str().as_encoded_bytes());
                let ts = ev.mk_string(t.nix_name().as_bytes());
                match ev.call2(fun, ps, ts, pos).and_then(|r| {
                    ev.force_bool(
                        r,
                        pos,
                        "while evaluating the return value of the path filter function",
                    )
                }) {
                    Ok(b) => b,
                    Err(e) => {
                        err = Some(e);
                        false
                    }
                }
            };
            let r = nar_hash_filtered(p, HashAlgo::Sha256, &mut f);
            if let Some(e) = err {
                return Err(e);
            }
            r.map_err(|e| io_err("hashing", path, e, pos))?.0
        }
    };
    let sp = make_fixed_output_path(method, &hash, name, &refs)
        .map_err(|e| Error::boxed(ErrorKind::Eval, e.0, pos))?;
    if let Some(h) = &expected
        && *h != hash
    {
        bail!(
            ErrorKind::Eval,
            pos,
            "store path mismatch in (possibly filtered) path added from '{}'",
            String::from_utf8_lossy(path)
        );
    }
    let full = ev.arena.alloc_bytes(sp.to_string().as_bytes());
    Ok(ev.arena.mk_string_ctx(full, &[Ctx::Opaque(full)]))
}

fn base_name(p: &[u8]) -> &[u8] {
    p.rsplit(|&b| b == b'/').next().unwrap_or(p)
}

fn filter_source(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let p = coerce_to_path(
        ev,
        args[1],
        pos,
        "while evaluating the second argument (the path to filter) passed to 'builtins.filterSource'",
    )?;
    let f = ev.force_function(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.filterSource",
    )?;
    let name = utf8(base_name(&p), pos)?.to_string();
    add_path(
        ev,
        &name,
        &p,
        Some(f),
        IngestionMethod::Recursive,
        None,
        pos,
    )
}

fn path(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let attrs = ev.force_attrs(
        args[0],
        pos,
        "while evaluating the argument passed to 'builtins.path'",
    )?;
    let mut p: Option<Vec<u8>> = None;
    let mut name: Option<String> = None;
    let mut filter: Option<Word> = None;
    let mut method = IngestionMethod::Recursive;
    let mut expected: Option<Hash> = None;
    for a in attrs.attrs() {
        let n = ev.interner.name(a.sym);
        match n {
            b"path" => {
                p = Some(coerce_to_path(
                    ev,
                    a.cell.get(),
                    a.pos,
                    "while evaluating the 'path' attribute passed to 'builtins.path'",
                )?)
            }
            b"name" => {
                let s = ev.force_string_no_ctx(
                    a.cell.get(),
                    a.pos,
                    "while evaluating the `name` attribute passed to builtins.path",
                )?;
                name = Some(utf8(s, pos)?.to_string());
            }
            b"filter" => {
                filter = Some(ev.force_function(
                    a.cell.get(),
                    a.pos,
                    "while evaluating the `filter` parameter passed to builtins.path",
                )?)
            }
            b"recursive" => {
                method = if ev.force_bool(
                    a.cell.get(),
                    a.pos,
                    "while evaluating the `recursive` attribute passed to builtins.path",
                )? {
                    IngestionMethod::Recursive
                } else {
                    IngestionMethod::Flat
                }
            }
            b"sha256" => {
                let s = ev.force_string_no_ctx(
                    a.cell.get(),
                    a.pos,
                    "while evaluating the `sha256` attribute passed to builtins.path",
                )?;
                let s = utf8(s, pos)?;
                if !s.is_empty() {
                    expected = Some(
                        Hash::parse_any(s, Some(HashAlgo::Sha256))
                            .map_err(|e| Error::boxed(ErrorKind::Eval, e.0, pos))?,
                    );
                }
            }
            other => bail!(
                ErrorKind::Eval,
                a.pos,
                "unsupported argument '{}' to 'builtins.path'",
                String::from_utf8_lossy(other)
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
    let name = match name {
        Some(n) if !n.is_empty() => n,
        _ => utf8(base_name(&p), pos)?.to_string(),
    };
    add_path(ev, &name, &p, filter, method, expected, pos)
}

fn store_path(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let p = coerce_to_path(
        ev,
        args[0],
        pos,
        "while evaluating the first argument passed to 'builtins.storePath'",
    )?;
    let ps = utf8(&p, pos)?;
    let Some(rest) = ps.strip_prefix("/nix/store/") else {
        bail!(ErrorKind::Eval, pos, "path '{ps}' is not in the Nix store");
    };
    let base = rest.split('/').next().unwrap_or(rest);
    let sp =
        StorePath::from_base_name(base).map_err(|e| Error::boxed(ErrorKind::Eval, e.0, pos))?;
    let full = ev.arena.alloc_bytes(sp.to_string().as_bytes());
    let bytes = ev.arena.alloc_bytes(&p);
    Ok(ev.arena.mk_string_ctx(bytes, &[Ctx::Opaque(full)]))
}

fn to_file(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let name = ev.force_string_no_ctx(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.toFile",
    )?;
    let name = utf8(name, pos)?.to_string();
    let contents = ev.force_string(
        args[1],
        pos,
        "while evaluating the second argument passed to builtins.toFile",
    )?;
    let mut refs = BTreeSet::new();
    for c in contents.ctx {
        match c {
            Ctx::Opaque(p) => {
                refs.insert(
                    StorePath::parse(utf8(p, pos)?)
                        .map_err(|e| Error::boxed(ErrorKind::Eval, e.0, pos))?,
                );
            }
            other => bail!(
                ErrorKind::Eval,
                pos,
                "files created by builtins.toFile may not reference derivations, but {name} references {}",
                crate::print::ctx_display(other)
            ),
        }
    }
    let h = hash_bytes(HashAlgo::Sha256, contents.bytes);
    let sp =
        make_text_path(&name, &h, &refs).map_err(|e| Error::boxed(ErrorKind::Eval, e.0, pos))?;
    let full = ev.arena.alloc_bytes(sp.to_string().as_bytes());
    Ok(ev.arena.mk_string_ctx(full, &[Ctx::Opaque(full)]))
}
