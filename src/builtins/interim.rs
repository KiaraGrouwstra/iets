//! INTERIM builtins for the nixpkgs parity ladder on branch `b-drv`.
//!
//! These belong to the strings/regex/json/hash family, which branch
//! `b-strings` implements properly. Delete this file (and its two lines in
//! `mod.rs`) when it merges; a duplicate registration panics at startup, so
//! the overlap cannot go unnoticed.

use std::path::Path;

use super::Registry;
use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos, bail};
use crate::runtime::*;
use crate::store::hash::{Hash, HashAlgo, HashFormat, hash_bytes};
use crate::store::nar::flat_hash;
use crate::syntax::symbol::Sym;

pub fn register(r: &mut Registry) {
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
    r.op("__toXML", 1, unsupported);
    r.op("fromTOML", 1, from_toml);
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
    let mut ctx = Vec::new();
    let p = crate::builtins::paths::coerce_to_path(
        ev,
        args[1],
        &mut ctx,
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

// ---- TOML --------------------------------------------------------------------

fn toml_to_value(ev: &mut Evaluator, v: &toml::Value, pos: Pos) -> EvalResult<Word> {
    Ok(match v {
        toml::Value::String(s) => ev.mk_string(s.as_bytes()),
        toml::Value::Integer(i) => ev.mk_int(*i),
        toml::Value::Float(f) => ev.mk_float(*f),
        toml::Value::Boolean(b) => Word::from_bool(*b),
        toml::Value::Datetime(_) => bail!(
            ErrorKind::Eval,
            pos,
            "while parsing TOML: Dates and times are not supported"
        ),
        toml::Value::Array(items) => {
            let mut ws = Vec::with_capacity(items.len());
            for i in items {
                ws.push(toml_to_value(ev, i, pos)?);
            }
            ev.mk_list(&ws)
        }
        toml::Value::Table(t) => {
            let mut entries: Vec<(Sym, Pos, Word)> = Vec::with_capacity(t.len());
            for (k, v) in t {
                let w = toml_to_value(ev, v, pos)?;
                entries.push((ev.sym(k.as_bytes()), Pos::NONE, w));
            }
            ev.mk_attrs(&mut entries, pos)?
        }
    })
}

fn from_toml(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let s = ev.force_string_no_ctx(
        args[0],
        pos,
        "while evaluating the argument passed to builtins.fromTOML",
    )?;
    let s = utf8(s, pos)?;
    let v: toml::Value = toml::from_str(s)
        .map_err(|e| Error::boxed(ErrorKind::Eval, format!("while parsing TOML: {e}"), pos))?;
    toml_to_value(ev, &v, pos)
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
