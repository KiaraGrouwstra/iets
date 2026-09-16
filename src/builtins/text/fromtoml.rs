//! `builtins.fromTOML` on top of the `toml` crate, with Nix's datetime
//! rendering (`primops/fromTOML.cc: normalizeDatetimeFormat`).

use std::cell::Cell;

use crate::eval::{ErrorKind, EvalResult, Evaluator, Pos, bail};
use crate::runtime::*;
use crate::syntax::symbol::Sym;

thread_local! {
    static PARSE_TOML_TIMESTAMPS: Cell<bool> = const { Cell::new(false) };
}

/// The `parse-toml-timestamps` experimental feature.
pub fn set_parse_toml_timestamps(on: bool) {
    PARSE_TOML_TIMESTAMPS.with(|c| c.set(on));
}

pub fn from_toml(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let s = ev.force_string_no_ctx(
        args[0],
        pos,
        "while evaluating the argument passed to builtins.fromTOML",
    )?;
    let text = match std::str::from_utf8(s) {
        Ok(t) => t,
        Err(e) => bail!(ErrorKind::Eval, pos, "while parsing TOML: {}", e),
    };
    let table: ::toml::Table = match ::toml::from_str(text) {
        Ok(t) => t,
        Err(e) => bail!(ErrorKind::Eval, pos, "while parsing TOML: {}", e.message()),
    };
    convert(ev, ::toml::Value::Table(table), pos)
}

fn convert(ev: &mut Evaluator, v: ::toml::Value, pos: Pos) -> EvalResult<Word> {
    use ::toml::Value as T;
    Ok(match v {
        T::String(s) => {
            no_nul(s.as_bytes(), pos)?;
            ev.mk_string(s.as_bytes())
        }
        T::Integer(i) => ev.mk_int(i),
        T::Float(f) => ev.mk_float(f),
        T::Boolean(b) => Word::from_bool(b),
        T::Datetime(d) => {
            if !PARSE_TOML_TIMESTAMPS.with(|c| c.get()) {
                bail!(
                    ErrorKind::Eval,
                    pos,
                    "while parsing TOML: Dates and times are not supported"
                );
            }
            let s = format_datetime(&d);
            let ty = ev.mk_string(b"timestamp");
            let val = ev.mk_string(s.as_bytes());
            let ty_sym = ev.sym(b"_type");
            let mut entries = vec![(ty_sym, Pos::NONE, ty), (ev.s.value, Pos::NONE, val)];
            ev.mk_attrs(&mut entries, pos)?
        }
        T::Array(items) => {
            let mut words = Vec::with_capacity(items.len());
            for it in items {
                words.push(convert(ev, it, pos)?);
            }
            ev.mk_list(&words)
        }
        T::Table(t) => {
            let mut entries: Vec<(Sym, Pos, Word)> = Vec::with_capacity(t.len());
            for (k, v) in t {
                no_nul(k.as_bytes(), pos)?;
                let sym = ev.sym(k.as_bytes());
                let w = convert(ev, v, pos)?;
                entries.push((sym, Pos::NONE, w));
            }
            ev.mk_attrs(&mut entries, pos)?
        }
    })
}

fn no_nul(s: &[u8], pos: Pos) -> EvalResult<()> {
    if s.contains(&0) {
        bail!(
            ErrorKind::Eval,
            pos,
            "input string '{}' cannot be represented as Nix string because it contains null bytes",
            String::from_utf8_lossy(s)
        );
    }
    Ok(())
}

/// toml11 < 4 subsecond precision: 3, 6 or 9 digits, or none.
fn subsecond_precision(ns: u32) -> u32 {
    if ns == 0 {
        0
    } else if ns.is_multiple_of(1_000_000) {
        3
    } else if ns.is_multiple_of(1_000) {
        6
    } else {
        9
    }
}

fn format_datetime(d: &::toml::value::Datetime) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    if let Some(date) = d.date {
        let _ = write!(s, "{:04}-{:02}-{:02}", date.year, date.month, date.day);
    }
    if let Some(time) = d.time {
        if d.date.is_some() {
            s.push('T');
        }
        let _ = write!(
            s,
            "{:02}:{:02}:{:02}",
            time.hour,
            time.minute,
            time.second.unwrap_or(0)
        );
        let ns = time.nanosecond.unwrap_or(0);
        let prec = subsecond_precision(ns);
        if prec > 0 {
            let frac = ns / 10u32.pow(9 - prec);
            let _ = write!(s, ".{:0width$}", frac, width = prec as usize);
        }
    }
    if let Some(off) = d.offset {
        match off {
            ::toml::value::Offset::Z => s.push('Z'),
            ::toml::value::Offset::Custom { minutes } => {
                let sign = if minutes < 0 { '-' } else { '+' };
                let m = minutes.unsigned_abs();
                let _ = write!(s, "{}{:02}:{:02}", sign, m / 60, m % 60);
            }
        }
    }
    s
}
