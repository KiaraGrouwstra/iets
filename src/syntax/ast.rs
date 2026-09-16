//! Shared AST. The parser produces it, `compile` consumes it. The shape is
//! deliberately raw: attrpaths are not merged, scopes are not resolved,
//! `inherit` is kept as written. Those transforms live in `compile`.

use super::symbol::Sym;

/// Index into `Ast::exprs`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ExprId(pub u32);

/// Index into the evaluator's file table.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FileId(pub u32);

/// Byte range in the file. Line and column are derived from a line table.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

/// One parsed file. `exprs[i]` and `spans[i]` describe the same node.
pub struct Ast {
    pub file: FileId,
    pub exprs: Vec<Expr>,
    pub spans: Vec<Span>,
    pub root: ExprId,
}

impl Ast {
    pub fn expr(&self, id: ExprId) -> &Expr {
        &self.exprs[id.0 as usize]
    }
    pub fn span(&self, id: ExprId) -> Span {
        self.spans[id.0 as usize]
    }
}

#[derive(Clone, Debug)]
pub enum Expr {
    Int(i64),
    Float(f64),
    /// `"..."` and `''...''`. Indentation of `''` strings is already
    /// stripped; escapes are already decoded. A plain string is one `Lit`.
    Str(Vec<StrPart>),
    /// `./foo`, `/abs`, `~/x`, `<nixpkgs>`, with optional interpolation.
    Path(PathExpr),
    Var(Sym),
    /// `e.a.b or d`
    Select {
        subject: ExprId,
        path: Vec<AttrElem>,
        default: Option<ExprId>,
    },
    /// `e ? a.b`
    HasAttr {
        subject: ExprId,
        path: Vec<AttrElem>,
    },
    Attrs {
        rec: bool,
        binds: Vec<Bind>,
    },
    List(Vec<ExprId>),
    Lambda {
        param: Param,
        body: ExprId,
    },
    /// `f a b c` is one node with `args = [a, b, c]`.
    Apply {
        func: ExprId,
        args: Vec<ExprId>,
    },
    Let {
        binds: Vec<Bind>,
        body: ExprId,
    },
    With {
        scope: ExprId,
        body: ExprId,
    },
    If {
        cond: ExprId,
        then: ExprId,
        else_: ExprId,
    },
    Assert {
        cond: ExprId,
        body: ExprId,
    },
    Unary {
        op: UnOp,
        expr: ExprId,
    },
    Binary {
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
    },
}

#[derive(Clone, Debug)]
pub enum StrPart {
    Lit(Vec<u8>),
    Interp(ExprId),
}

#[derive(Clone, Debug)]
pub struct PathExpr {
    pub kind: PathKind,
    /// The literal text after the kind prefix, plus interpolations.
    /// For `<nixpkgs/lib>` this is the text between the angle brackets.
    pub parts: Vec<StrPart>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PathKind {
    /// `./x` or `../x`; parts start with `./` or `../` as written.
    Relative,
    /// `/x`; parts start with `/`.
    Absolute,
    /// `~/x`; parts start with `/` (the `~` is dropped).
    Home,
    /// `<x>`.
    Search,
}

/// One element of an attribute path: `a`, `"a"`, `${e}`, `"a${e}"`.
#[derive(Clone, Debug)]
pub enum AttrElem {
    Static(Sym),
    /// Interpolated or `${}`; a string whose value must coerce to a name.
    Dynamic(ExprId),
}

#[derive(Clone, Debug)]
pub struct Bind {
    pub span: Span,
    pub kind: BindKind,
}

#[derive(Clone, Debug)]
pub enum BindKind {
    /// `a.b.c = e;`
    Value { path: Vec<AttrElem>, value: ExprId },
    /// `inherit a b;` or `inherit (e) a b;`
    Inherit {
        from: Option<ExprId>,
        names: Vec<(Sym, Span)>,
    },
}

#[derive(Clone, Debug)]
pub enum Param {
    Ident(Sym),
    Formals {
        /// `name @ { ... }` or `{ ... } @ name`.
        bind: Option<Sym>,
        formals: Vec<Formal>,
        ellipsis: bool,
    },
}

#[derive(Clone, Debug)]
pub struct Formal {
    pub name: Sym,
    pub default: Option<ExprId>,
    pub span: Span,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnOp {
    Not,
    Neg,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinOp {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
    Impl,
    Update,
    Concat,
    Add,
    Sub,
    Mul,
    Div,
}
