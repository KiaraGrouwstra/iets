//! Style lints on the syntax tree: `if c then true else false`,
//! `x == true`, `a = b.a;`, `let in e`, `{ a, ... }:` without `@`. Hook:
//! the compiler, which has the tree and knows what `true` resolves to.

use super::LintId;
use crate::syntax::ast::{Ast, AttrElem, BinOp, Bind, BindKind, Expr, ExprId, Param};
use crate::syntax::symbol::Sym;

/// The Boolean literal `e` is, when it is `true` or `false` and those
/// names resolve to the globals.
fn bool_literal(ast: &Ast, e: ExprId, global: &dyn Fn(Sym) -> Option<bool>) -> Option<bool> {
    match ast.expr(e) {
        Expr::Var(s) => global(*s),
        _ => None,
    }
}

pub fn if_bool_literal(
    ast: &Ast,
    then: ExprId,
    else_: ExprId,
    global: &dyn Fn(Sym) -> Option<bool>,
) -> Option<(LintId, String)> {
    let t = bool_literal(ast, then, global)?;
    let e = bool_literal(ast, else_, global)?;
    if t == e {
        return None;
    }
    let hint = if t { "use `c`" } else { "use `!c`" };
    Some((
        LintId::IfBoolLiteral,
        format!("`if c then {t} else {e}` is the condition itself: {hint}"),
    ))
}

pub fn eq_bool(
    ast: &Ast,
    op: BinOp,
    lhs: ExprId,
    rhs: ExprId,
    global: &dyn Fn(Sym) -> Option<bool>,
) -> Option<(LintId, String)> {
    let b = bool_literal(ast, lhs, global).or_else(|| bool_literal(ast, rhs, global))?;
    let op_s = match op {
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        _ => return None,
    };
    let hint = match (op, b) {
        (BinOp::Eq, true) | (BinOp::Ne, false) => "use `x`",
        _ => "use `!x`",
    };
    Some((
        LintId::EqBool,
        format!("`x {op_s} {b}` compares with a Boolean literal: {hint}"),
    ))
}

/// `a = a;` (in a non-recursive set only: elsewhere it recurses) and
/// `a = e.a;` anywhere.
pub fn manual_inherit(
    ast: &Ast,
    b: &Bind,
    rec: bool,
    name_of: &dyn Fn(Sym) -> String,
) -> Option<(LintId, String)> {
    let BindKind::Value { path, value } = b.kind else {
        return None;
    };
    let [AttrElem::Static(name)] = ast.elems(path) else {
        return None;
    };
    match ast.expr(value) {
        Expr::Var(v) if v == name && !rec => Some((
            LintId::ManualInherit,
            format!("`{0} = {0};` is `inherit {0};`", name_of(*name)),
        )),
        Expr::Select {
            path: sel,
            default: None,
            ..
        } => match ast.elems(*sel) {
            [AttrElem::Static(last)] if last == name => Some((
                LintId::ManualInherit,
                format!("`{0} = e.{0};` is `inherit (e) {0};`", name_of(*name)),
            )),
            _ => None,
        },
        _ => None,
    }
}

pub fn empty_let(binds: usize) -> Option<(LintId, String)> {
    (binds == 0).then(|| (LintId::EmptyLet, "`let in e` binds nothing: use `e`".into()))
}

/// `{ a, ... }:` without an `@` binding.
pub fn pattern(param: &Param) -> Option<(LintId, String)> {
    let Param::Formals { bind, ellipsis, .. } = param else {
        return None;
    };
    (*ellipsis && bind.is_none()).then(|| {
        (
            LintId::EllipsisWithoutBind,
            "`...` without an `@name` binding drops the extra attributes silently".into(),
        )
    })
}
