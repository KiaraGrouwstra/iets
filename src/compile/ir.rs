//! Resolved intermediate representation. Variables are `(depth, index)`
//! pairs into the runtime env chain, attrpaths are merged, `inherit` is
//! expanded, and `builtins.x` on the unshadowed global is folded to a
//! constant. IR nodes live forever (`&'static`) so thunks can point at them.

use crate::eval::Pos;
use crate::runtime::Word;
use crate::syntax::symbol::Sym;

pub type IrRef = &'static Ir;

pub enum Ir {
    /// Literal, or pre-resolved builtin.
    Const(Word),
    /// Lexically bound variable: walk `depth` envs up, take slot `idx`.
    Local {
        depth: u32,
        idx: u32,
    },
    /// Variable bound by `with`. `withs` lists, innermost first, the depth of
    /// each enclosing with-env relative to the current env.
    WithVar {
        sym: Sym,
        withs: &'static [u32],
        pos: Pos,
    },
    /// `__curPos`.
    CurPos(Pos),
    /// `+` chains and interpolated strings/paths. When `force_string` the
    /// result is a string even for a single path part; when the first
    /// part is a path the result is a path; ints and floats add.
    Concat {
        force_string: bool,
        parts: &'static [IrRef],
        pos: Pos,
    },
    /// `<name>`: search path lookup of the string produced by `name`.
    SearchPath {
        name: IrRef,
        pos: Pos,
    },
    Select {
        subject: IrRef,
        path: &'static [AttrElemIr],
        default: Option<IrRef>,
        pos: Pos,
    },
    HasAttr {
        subject: IrRef,
        path: &'static [AttrElemIr],
    },
    Attrs(&'static AttrsIr),
    List(&'static [IrRef]),
    Lambda(&'static LambdaIr),
    Apply {
        func: IrRef,
        args: &'static [IrRef],
        pos: Pos,
    },
    /// `let`: new env with `binds.len() + from.len()` slots.
    Let {
        binds: &'static [BindIr],
        from: &'static [IrRef],
        body: IrRef,
    },
    /// New env with one slot holding the (thunked) scope attrs.
    With {
        scope: IrRef,
        body: IrRef,
        pos: Pos,
    },
    If {
        cond: IrRef,
        then: IrRef,
        else_: IrRef,
        pos: Pos,
    },
    Assert {
        cond: IrRef,
        body: IrRef,
        pos: Pos,
    },
    Not(IrRef, Pos),
    Neg(IrRef, Pos),
    Eq(IrRef, IrRef, Pos),
    Ne(IrRef, IrRef, Pos),
    Lt(IrRef, IrRef, Pos),
    Gt(IrRef, IrRef, Pos),
    Le(IrRef, IrRef, Pos),
    Ge(IrRef, IrRef, Pos),
    And(IrRef, IrRef, Pos),
    Or(IrRef, IrRef, Pos),
    Impl(IrRef, IrRef, Pos),
    Update(IrRef, IrRef, Pos),
    ConcatLists(IrRef, IrRef, Pos),
    Sub(IrRef, IrRef, Pos),
    Mul(IrRef, IrRef, Pos),
    Div(IrRef, IrRef, Pos),
}

pub enum AttrElemIr {
    Static(Sym),
    Dynamic(IrRef),
}

/// Where a binding's value expression is evaluated.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BindScope {
    /// In the attrs' own env (rec attrs, let) or the enclosing env (plain attrs).
    Own,
    /// `inherit x` in a rec set or let: the enclosing env.
    Outer,
    /// `inherit (e) x`: the env holding the `from` thunks.
    From,
}

pub struct BindIr {
    pub sym: Sym,
    pub value: IrRef,
    pub pos: Pos,
    pub scope: BindScope,
}

pub struct AttrsIr {
    pub rec: bool,
    /// Sorted by sym. For rec sets, slot `i` of the new env holds bind `i`.
    pub binds: &'static [BindIr],
    /// `inherit (e)` sources; occupy slots after the binds (rec) or their own
    /// small env (plain attrs).
    pub from: &'static [IrRef],
    pub dynamic: &'static [DynBindIr],
    pub pos: Pos,
    /// Index of `__overrides` in `binds`, rec sets only.
    pub overrides: Option<u32>,
}

pub struct DynBindIr {
    pub name: IrRef,
    pub value: IrRef,
    pub pos: Pos,
}

pub struct LambdaIr {
    pub param: ParamIr,
    pub body: IrRef,
    pub pos: Pos,
    pub name: Option<Sym>,
    /// The `x` in `x: ...` or the `args` in `{ ... }@args: ...`; `toXML` prints it.
    pub arg: Option<Sym>,
}

pub enum ParamIr {
    /// One slot.
    Ident,
    /// Slot 0 is the `@` binding when `has_bind`; formals follow in sorted
    /// order.
    Formals {
        has_bind: bool,
        formals: &'static [FormalIr],
        ellipsis: bool,
    },
}

pub struct FormalIr {
    pub sym: Sym,
    pub default: Option<IrRef>,
    pub pos: Pos,
}

impl LambdaIr {
    pub fn nslots(&self) -> usize {
        match &self.param {
            ParamIr::Ident => 1,
            ParamIr::Formals {
                has_bind, formals, ..
            } => formals.len() + usize::from(*has_bind),
        }
    }
}

impl Ir {
    /// Cheap expressions are never thunked: the evaluator reads them directly.
    #[inline]
    pub fn is_cheap(&self) -> bool {
        matches!(self, Ir::Const(_) | Ir::Local { .. } | Ir::Lambda(_))
    }
}
