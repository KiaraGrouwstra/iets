//! Value printers: the `nix-instantiate --eval` format (`printAmbiguous`),
//! JSON (`printValueAsJSON`), and short forms for error messages.

use std::collections::HashSet;

use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos};
use crate::runtime::*;
use crate::syntax::symbol::Sym;

/// C's `%g` with precision 6, as `std::ostream << double` prints.
pub fn format_g(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() {
            "-0".into()
        } else {
            "0".into()
        };
    }
    const P: i32 = 6;
    let sci = format!("{:.*e}", (P - 1) as usize, f);
    let (mant, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    if !(-4..P).contains(&exp) {
        let mant = strip_zeros(mant);
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{mant}e{sign}{:02}", exp.abs())
    } else {
        let decimals = (P - 1 - exp).max(0) as usize;
        let s = format!("{:.*}", decimals, f);
        strip_zeros(&s).to_string()
    }
}

fn strip_zeros(s: &str) -> &str {
    if s.contains('.') {
        let s = s.trim_end_matches('0');
        s.strip_suffix('.').unwrap_or(s)
    } else {
        s
    }
}

pub fn print_literal_string(out: &mut Vec<u8>, s: &[u8]) {
    out.push(b'"');
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        match c {
            b'"' | b'\\' => {
                out.push(b'\\');
                out.push(c);
            }
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'$' if i + 1 < s.len() && s[i + 1] == b'{' => {
                out.extend_from_slice(b"\\$");
            }
            _ => out.push(c),
        }
        i += 1;
    }
    out.push(b'"');
}

/// `print.cc: isVarName`.
fn is_var_name(s: &[u8]) -> bool {
    const KEYWORDS: [&[u8]; 9] = [
        b"if", b"then", b"else", b"assert", b"with", b"let", b"in", b"rec", b"inherit",
    ];
    if s.is_empty() || KEYWORDS.contains(&s) {
        return false;
    }
    if matches!(s[0], b'0'..=b'9' | b'-' | b'\'') {
        return false;
    }
    s.iter()
        .all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'\''))
}

/// `print.cc: printAttributeName`: bare when it is a valid identifier,
/// a string literal otherwise.
pub fn print_attr_name(out: &mut Vec<u8>, name: &[u8]) {
    if is_var_name(name) {
        out.extend_from_slice(name);
    } else {
        print_literal_string(out, name);
    }
}

/// Attr names in the order Nix prints them: by name bytes.
pub fn lexicographic_order(ev: &Evaluator, a: &'static AttrsObj) -> Vec<&'static Attr> {
    let mut v: Vec<&'static Attr> = a.attrs().iter().collect();
    v.sort_by(|x, y| ev.interner.name(x.sym).cmp(ev.interner.name(y.sym)));
    v
}

/// `nix-instantiate --eval` output. Does not force; `--strict` forces first.
/// `cell` identifies the slot the value was read from; Nix keys repeated
/// lists on the `Value` address, which is the cell in our model. The top
/// level passes any unique number.
pub fn print_ambiguous(
    ev: &mut Evaluator,
    w: Word,
    cell: u64,
    out: &mut Vec<u8>,
    seen: &mut HashSet<u64>,
    depth: usize,
) -> EvalResult<()> {
    if depth > crate::eval::MAX_CALL_DEPTH as usize {
        return Err(Error::boxed(
            ErrorKind::StackOverflow,
            "stack overflow; max-call-depth exceeded",
            Pos::NONE,
        ));
    }
    if w.is_thunk() {
        let r = w.thunk_result_cell().get();
        if r == Word::BLACKHOLE {
            out.extend_from_slice("«potential infinite recursion»".as_bytes());
        } else if r == Word::EMPTY {
            out.extend_from_slice(b"<CODE>");
        } else {
            return print_ambiguous(ev, r, cell, out, seen, depth);
        }
        return Ok(());
    }
    match w.view() {
        Value::Int(i) => out.extend_from_slice(i.to_string().as_bytes()),
        Value::Float(f) => out.extend_from_slice(format_g(f).as_bytes()),
        Value::Bool(b) => out.extend_from_slice(if b { b"true" } else { b"false" }),
        Value::Null => out.extend_from_slice(b"null"),
        Value::String(s) => print_literal_string(out, s.bytes),
        Value::Path(p) => out.extend_from_slice(p.path),
        Value::Attrs(a) => {
            if !a.is_empty() && !seen.insert(w.0) {
                out.extend_from_slice("«repeated»".as_bytes());
                return Ok(());
            }
            out.extend_from_slice(b"{ ");
            for attr in lexicographic_order(ev, a) {
                print_attr_name(out, ev.interner.name(attr.sym));
                out.extend_from_slice(b" = ");
                print_ambiguous(
                    ev,
                    attr.cell.get(),
                    &attr.cell as *const _ as u64,
                    out,
                    seen,
                    depth + 1,
                )?;
                out.extend_from_slice(b"; ");
            }
            out.push(b'}');
        }
        Value::List(l) => {
            if l.len > 0 && !seen.insert(cell) {
                out.extend_from_slice("«repeated»".as_bytes());
                return Ok(());
            }
            out.extend_from_slice(b"[ ");
            for c in l.cells() {
                print_ambiguous(ev, c.get(), c as *const _ as u64, out, seen, depth + 1)?;
                out.push(b' ');
            }
            out.push(b']');
        }
        Value::Lambda(_) => out.extend_from_slice(b"<LAMBDA>"),
        Value::PrimOp(_) => out.extend_from_slice(b"<PRIMOP>"),
        Value::PrimOpApp(_) => out.extend_from_slice(b"<PRIMOP-APP>"),
    }
    Ok(())
}

/// Short rendering for error messages: like the modern printer with limits.
pub fn show_value_short(ev: &Evaluator, w: Word) -> String {
    let mut out = String::new();
    show_short(ev, w, &mut out, 0);
    out
}

fn show_short(ev: &Evaluator, w: Word, out: &mut String, depth: usize) {
    if w.is_thunk() {
        out.push_str("«thunk»");
        return;
    }
    match w.view() {
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::Float(f) => out.push_str(&format_g(f)),
        Value::Bool(b) => out.push_str(if b { "true" } else { "false" }),
        Value::Null => out.push_str("null"),
        Value::String(s) => {
            let mut v = Vec::new();
            print_literal_string(&mut v, s.bytes);
            out.push_str(&String::from_utf8_lossy(&v));
        }
        Value::Path(p) => out.push_str(&String::from_utf8_lossy(p.path)),
        Value::Attrs(a) => {
            if depth >= 1 {
                out.push_str("{ ... }");
                return;
            }
            out.push('{');
            for (n, attr) in lexicographic_order(ev, a).iter().enumerate() {
                if n >= 8 {
                    out.push_str(" «more attributes elided»");
                    break;
                }
                out.push(' ');
                out.push_str(ev.interner.name_str(attr.sym));
                out.push_str(" = ");
                show_short(ev, attr.cell.get(), out, depth + 1);
                out.push(';');
            }
            out.push_str(" }");
        }
        Value::List(l) => {
            if depth >= 1 {
                out.push_str("[ ... ]");
                return;
            }
            out.push('[');
            for (n, c) in l.cells().iter().enumerate() {
                if n >= 8 {
                    out.push_str(" «more items elided»");
                    break;
                }
                out.push(' ');
                show_short(ev, c.get(), out, depth + 1);
            }
            out.push_str(" ]");
        }
        Value::Lambda(l) => {
            out.push_str("«lambda");
            if let Some(n) = l.lam.name {
                out.push(' ');
                out.push_str(ev.interner.name_str(n));
            }
            out.push_str(" @ ");
            out.push_str(&ev.describe_pos(l.lam.pos));
            out.push('»');
        }
        Value::PrimOp(p) => {
            out.push_str("«primop ");
            out.push_str(p.name);
            out.push('»');
        }
        Value::PrimOpApp(p) => {
            out.push_str("«partially applied primop ");
            out.push_str(p.op.name);
            out.push('»');
        }
    }
}

pub fn ctx_display(c: &Ctx) -> String {
    match c {
        Ctx::Opaque(p) => String::from_utf8_lossy(p).into_owned(),
        Ctx::DrvDeep(d) => format!("={}", String::from_utf8_lossy(d)),
        Ctx::Built { drv, output } => {
            format!(
                "!{}!{}",
                String::from_utf8_lossy(output),
                String::from_utf8_lossy(drv)
            )
        }
    }
}

// ---- JSON --------------------------------------------------------------------

pub fn json_escape(out: &mut Vec<u8>, s: &[u8]) -> EvalResult<()> {
    let st = std::str::from_utf8(s).map_err(|_| {
        Error::boxed(
            ErrorKind::Eval,
            "JSON serialization error: invalid UTF-8 byte in string",
            Pos::NONE,
        )
    })?;
    out.push(b'"');
    for ch in st.chars() {
        match ch {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\u{8}' => out.extend_from_slice(b"\\b"),
            '\u{c}' => out.extend_from_slice(b"\\f"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            c if (c as u32) < 0x20 => {
                out.extend_from_slice(format!("\\u{:04x}", c as u32).as_bytes());
            }
            c => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    out.push(b'"');
    Ok(())
}

/// Shortest round-trip float, like nlohmann::json.
pub fn json_float(f: f64) -> String {
    if !f.is_finite() {
        return "null".into();
    }
    let sci = format!("{f:e}");
    let (mant, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    if (-4..15).contains(&exp) {
        let s = format!("{f}");
        if s.contains('.') { s } else { format!("{s}.0") }
    } else {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{mant}e{sign}{:02}", exp.abs())
    }
}

/// `printValueAsJSON`. Counts against the call depth like Nix's
/// `addCallDepth`, so deep structures fail with a stack overflow error.
pub fn print_json(
    ev: &mut Evaluator,
    w: Word,
    out: &mut Vec<u8>,
    ctx: &mut Vec<Ctx>,
    strict: bool,
    pos: Pos,
) -> EvalResult<()> {
    ev.call_depth += 1;
    if ev.call_depth > crate::eval::MAX_CALL_DEPTH {
        ev.call_depth -= 1;
        return Err(Error::boxed(
            ErrorKind::StackOverflow,
            "stack overflow; max-call-depth exceeded",
            pos,
        ));
    }
    let r = print_json_inner(ev, w, out, ctx, strict, pos);
    ev.call_depth -= 1;
    r
}

fn print_json_inner(
    ev: &mut Evaluator,
    w: Word,
    out: &mut Vec<u8>,
    ctx: &mut Vec<Ctx>,
    strict: bool,
    pos: Pos,
) -> EvalResult<()> {
    let w = if strict { ev.force(w)? } else { w };
    if w.is_thunk() {
        return Err(Error::boxed(
            ErrorKind::Type,
            "cannot convert a thunk to JSON",
            pos,
        ));
    }
    match w.view() {
        Value::Int(i) => out.extend_from_slice(i.to_string().as_bytes()),
        Value::Float(f) => out.extend_from_slice(json_float(f).as_bytes()),
        Value::Bool(b) => out.extend_from_slice(if b { b"true" } else { b"false" }),
        Value::Null => out.extend_from_slice(b"null"),
        Value::String(s) => {
            ctx.extend_from_slice(s.ctx);
            json_escape(out, s.bytes)?;
        }
        Value::Path(p) => {
            let sp = ev.copy_path_to_store(p.path, pos)?;
            ctx.push(Ctx::Opaque(sp));
            json_escape(out, sp)?;
        }
        Value::Attrs(a) => {
            if let Some(f) = a.get(ev.s.to_string) {
                let f = ev.force_cell(&f.cell)?;
                let r = ev.call1(f, w, pos)?;
                let mut c2 = Vec::new();
                let s = ev.coerce_to_string(r, &mut c2, pos, "", false, false, true)?;
                ctx.extend(c2);
                return json_escape(out, &s);
            }
            if let Some(o) = a.get(ev.s.out_path) {
                return print_json(ev, o.cell.get(), out, ctx, strict, o.pos);
            }
            out.push(b'{');
            let mut first = true;
            for attr in lexicographic_order(ev, a) {
                if !first {
                    out.push(b',');
                }
                first = false;
                json_escape(out, ev.interner.name(attr.sym))?;
                out.push(b':');
                print_json(ev, attr.cell.get(), out, ctx, strict, attr.pos).map_err(|mut e| {
                    e.add_trace(
                        attr.pos,
                        format!(
                            "while evaluating attribute '{}'",
                            ev.interner.name_str(attr.sym)
                        ),
                    );
                    e
                })?;
            }
            out.push(b'}');
        }
        Value::List(l) => {
            out.push(b'[');
            for (i, c) in l.cells().iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                print_json(ev, c.get(), out, ctx, strict, pos).map_err(|mut e| {
                    e.add_trace(pos, format!("while evaluating list element at index {i}"));
                    e
                })?;
            }
            out.push(b']');
        }
        Value::Lambda(_) | Value::PrimOp(_) | Value::PrimOpApp(_) => {
            return Err(Error::boxed(
                ErrorKind::Type,
                "cannot convert a function to JSON",
                pos,
            ));
        }
    }
    Ok(())
}

pub fn sym_name(ev: &Evaluator, s: Sym) -> &str {
    ev.interner.name_str(s)
}
