//! String coercion, equality, and ordering, mirroring `EvalState::coerceToString`,
//! `eqValues`, and `CompareValues`.

use std::borrow::Cow;

use super::{Error, ErrorKind, EvalResult, Evaluator, Pos, bail};
use crate::runtime::*;

impl Evaluator {
    /// Coerce a value to a string, collecting context into `ctx`.
    ///
    /// `coerce_more` also converts ints, floats, bools, null and lists (as
    /// `toString` does). `copy_to_store` turns paths into store paths.
    /// `canonicalize` is false only for the first segment of an
    /// interpolated path, preserving a trailing slash.
    #[allow(clippy::too_many_arguments)]
    pub fn coerce_to_string(
        &mut self,
        w: Word,
        ctx: &mut Vec<Ctx>,
        pos: Pos,
        err_ctx: &str,
        coerce_more: bool,
        copy_to_store: bool,
        canonicalize: bool,
    ) -> EvalResult<Cow<'static, [u8]>> {
        self.call_depth += 1;
        if self.call_depth > super::MAX_CALL_DEPTH {
            self.call_depth -= 1;
            bail!(
                ErrorKind::StackOverflow,
                pos,
                "stack overflow; max-call-depth exceeded"
            );
        }
        let r = self.coerce_inner(
            w,
            ctx,
            pos,
            err_ctx,
            coerce_more,
            copy_to_store,
            canonicalize,
        );
        self.call_depth -= 1;
        r
    }

    #[allow(clippy::too_many_arguments)]
    fn coerce_inner(
        &mut self,
        w: Word,
        ctx: &mut Vec<Ctx>,
        pos: Pos,
        err_ctx: &str,
        coerce_more: bool,
        copy_to_store: bool,
        canonicalize: bool,
    ) -> EvalResult<Cow<'static, [u8]>> {
        let w = self.force(w)?;
        match w.view() {
            Value::String(s) => {
                ctx.extend_from_slice(s.ctx);
                Ok(Cow::Borrowed(s.bytes))
            }
            Value::Path(p) => {
                if !canonicalize && !copy_to_store {
                    Ok(Cow::Borrowed(p.path))
                } else if copy_to_store {
                    let sp = self.copy_path_to_store(p.path, pos)?;
                    ctx.push(Ctx::Opaque(sp));
                    Ok(Cow::Borrowed(sp))
                } else {
                    Ok(Cow::Borrowed(p.path))
                }
            }
            Value::Attrs(a) => {
                if let Some(f) = a.get(self.s.to_string) {
                    let f = self.force_cell(&f.cell)?;
                    let r = self.call1(f, w, pos)?;
                    return self.coerce_to_string(
                        r,
                        ctx,
                        pos,
                        "while evaluating the result of the `__toString` attribute",
                        coerce_more,
                        copy_to_store,
                        canonicalize,
                    );
                }
                match a.get(self.s.out_path) {
                    Some(o) => {
                        let v = o.cell.get();
                        self.coerce_to_string(
                            v,
                            ctx,
                            pos,
                            err_ctx,
                            coerce_more,
                            copy_to_store,
                            canonicalize,
                        )
                    }
                    None => self.cannot_coerce(w, pos, err_ctx),
                }
            }
            Value::Bool(b) if coerce_more => Ok(Cow::Borrowed(if b { b"1" } else { b"" })),
            Value::Int(i) if coerce_more => Ok(Cow::Owned(i.to_string().into_bytes())),
            Value::Float(f) if coerce_more => Ok(Cow::Owned(format!("{f:.6}").into_bytes())),
            Value::Null if coerce_more => Ok(Cow::Borrowed(b"")),
            Value::List(l) if coerce_more => {
                let mut out: Vec<u8> = Vec::new();
                let cells = l.cells();
                for (n, c) in cells.iter().enumerate() {
                    let v = c.get();
                    let s = self
                        .coerce_to_string(
                            v,
                            ctx,
                            pos,
                            "while evaluating one element of the list",
                            coerce_more,
                            copy_to_store,
                            canonicalize,
                        )
                        .map_err(|mut e| {
                            e.add_trace(pos, err_ctx);
                            e
                        })?;
                    out.extend_from_slice(&s);
                    if n + 1 < cells.len() {
                        let v2 = self.force(v)?;
                        let empty_list = matches!(v2.view(), Value::List(l2) if l2.len == 0);
                        if !empty_list {
                            out.push(b' ');
                        }
                    }
                }
                Ok(Cow::Owned(out))
            }
            _ => self.cannot_coerce(w, pos, err_ctx),
        }
    }

    fn cannot_coerce<T>(&self, w: Word, pos: Pos, err_ctx: &str) -> EvalResult<T> {
        let mut e = Error::new(
            ErrorKind::Type,
            format!(
                "cannot coerce {} to a string: {}",
                w.value_type().name(),
                crate::print::show_value_short(self, w)
            ),
            pos,
        );
        if !err_ctx.is_empty() {
            e.add_trace(pos, err_ctx);
        }
        Err(Box::new(e))
    }

    /// Store path of a source path. The store module supplies the NAR hash;
    /// until it is linked this is a hard error.
    pub fn copy_path_to_store(
        &mut self,
        path: &'static [u8],
        pos: Pos,
    ) -> EvalResult<&'static [u8]> {
        if path.ends_with(b".drv") {
            bail!(
                ErrorKind::Eval,
                pos,
                "file names are not allowed to end in '.drv'"
            );
        }
        crate::builtins::store_hooks::copy_path_to_store(self, path, pos)
    }

    /// The `==` operator. Nix compares the two operand temporaries, so two
    /// references to one function are unequal here but equal when reached
    /// through a shared cell (see `eq_values`).
    pub fn eq_values_top(&mut self, a: Word, b: Word, pos: Pos) -> EvalResult<bool> {
        let a = self.force(a)?;
        let b = self.force(b)?;
        if a == b && a.value_type() == ValueType::Function {
            return Ok(false);
        }
        self.eq_values(a, b, pos)
    }

    /// Nix `eqValues`: identical words are equal (pointer shortcut).
    #[allow(clippy::only_used_in_recursion)]
    pub fn eq_values(&mut self, a: Word, b: Word, pos: Pos) -> EvalResult<bool> {
        let a = self.force(a)?;
        let b = self.force(b)?;
        if a == b {
            return Ok(true);
        }
        match (a.view(), b.view()) {
            (Value::Int(x), Value::Int(y)) => Ok(x == y),
            (Value::Int(x), Value::Float(y)) => Ok(x as f64 == y),
            (Value::Float(x), Value::Int(y)) => Ok(x == y as f64),
            (Value::Float(x), Value::Float(y)) => Ok(x == y),
            (Value::Bool(x), Value::Bool(y)) => Ok(x == y),
            (Value::Null, Value::Null) => Ok(true),
            (Value::String(x), Value::String(y)) => Ok(x.bytes == y.bytes),
            (Value::Path(x), Value::Path(y)) => Ok(x.path == y.path),
            (Value::List(x), Value::List(y)) => {
                if x.len != y.len {
                    return Ok(false);
                }
                for (cx, cy) in x.cells().iter().zip(y.cells()) {
                    let vx = self.force_cell(cx)?;
                    let vy = self.force_cell(cy)?;
                    if !self.eq_values(vx, vy, pos)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            (Value::Attrs(x), Value::Attrs(y)) => {
                if self.is_derivation(a) && self.is_derivation(b) {
                    let i = x.get(self.s.out_path);
                    let j = y.get(self.s.out_path);
                    if let (Some(i), Some(j)) = (i, j) {
                        let vi = self.force_cell(&i.cell)?;
                        let vj = self.force_cell(&j.cell)?;
                        return self.eq_values(vi, vj, pos);
                    }
                }
                if x.len != y.len {
                    return Ok(false);
                }
                for (ax, ay) in x.attrs().iter().zip(y.attrs()) {
                    if ax.sym != ay.sym {
                        return Ok(false);
                    }
                    let vx = self.force_cell(&ax.cell)?;
                    let vy = self.force_cell(&ay.cell)?;
                    if !self.eq_values(vx, vy, pos)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Nix `<` (also used by `lessThan` and `sort`).
    pub fn less_than(&mut self, a: Word, b: Word, pos: Pos) -> EvalResult<bool> {
        let a = self.force(a)?;
        let b = self.force(b)?;
        match (a.view(), b.view()) {
            (Value::Int(x), Value::Int(y)) => Ok(x < y),
            (Value::Float(x), Value::Int(y)) => Ok(x < y as f64),
            (Value::Int(x), Value::Float(y)) => Ok((x as f64) < y),
            (Value::Float(x), Value::Float(y)) => Ok(x < y),
            (Value::String(x), Value::String(y)) => Ok(x.bytes < y.bytes),
            (Value::Path(x), Value::Path(y)) => Ok(x.path < y.path),
            (Value::List(x), Value::List(y)) => {
                let (cx, cy) = (x.cells(), y.cells());
                let mut i = 0;
                loop {
                    if i == cy.len() {
                        return Ok(false);
                    }
                    if i == cx.len() {
                        return Ok(true);
                    }
                    let vx = self.force_cell(&cx[i])?;
                    let vy = self.force_cell(&cy[i])?;
                    if !self.eq_values(vx, vy, pos)? {
                        return self.less_than(vx, vy, pos);
                    }
                    i += 1;
                }
            }
            _ => bail!(
                ErrorKind::Eval,
                pos,
                "cannot compare {} with {}; values are {} and {}",
                a.value_type().name(),
                b.value_type().name(),
                crate::print::show_value_short(self, a),
                crate::print::show_value_short(self, b)
            ),
        }
    }
}
