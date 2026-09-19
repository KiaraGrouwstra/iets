//! Scope and shadowing: `with`, shadowed globals and bindings, unused
//! bindings and formals, `rec` without recursion. Hook: the compiler's
//! lexical frames, which record every binding and every reference.

use std::cell::Cell;

use super::LintId;
use crate::eval::Pos;
use crate::syntax::ast::Expr;
use crate::syntax::symbol::{FxHashMap, Sym};

/// What introduced a binding; the message names it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BindKind {
    Let,
    Rec,
    Formal,
    /// `x:` or the `@name` of a pattern.
    Arg,
    /// A `scopedImport` name: never reported.
    Extra,
}

impl BindKind {
    fn what(self) -> &'static str {
        match self {
            BindKind::Let => "let binding",
            BindKind::Rec => "rec binding",
            BindKind::Formal => "formal",
            BindKind::Arg => "argument",
            BindKind::Extra => "binding",
        }
    }
}

pub struct Binding {
    pub sym: Sym,
    pub pos: Pos,
    pub kind: BindKind,
    pub used: Cell<bool>,
}

/// One lexical frame: the slot of every name plus what the lints need.
pub struct Frame {
    pub slots: FxHashMap<Sym, u32>,
    pub binds: Vec<Binding>,
    /// A `rec { }` frame: any reference from inside marks it recursive.
    pub rec: bool,
    pub recursive: Cell<bool>,
    /// A pattern frame whose `@name` was referenced: its formals count as used.
    pub bind_used: Cell<bool>,
    pub bind_sym: Option<Sym>,
}

impl Frame {
    pub fn new(rec: bool) -> Frame {
        Frame {
            slots: FxHashMap::default(),
            binds: Vec::new(),
            rec,
            recursive: Cell::new(false),
            bind_used: Cell::new(false),
            bind_sym: None,
        }
    }

    pub fn from_names(names: &[Sym]) -> Frame {
        let mut f = Frame::new(false);
        for (i, s) in names.iter().enumerate() {
            f.slots.insert(*s, i as u32);
            f.binds.push(Binding {
                sym: *s,
                pos: Pos::NONE,
                kind: BindKind::Extra,
                used: Cell::new(true),
            });
        }
        f
    }

    /// Records the binding of slot `slots.len()`; `binds` stays empty when
    /// no scope lint is on, so `note_use` costs one bounds check then.
    pub fn push(&mut self, sym: Sym, pos: Pos, kind: BindKind) {
        self.binds.push(Binding {
            sym,
            pos,
            kind,
            used: Cell::new(false),
        });
    }

    /// A reference to slot `idx` from inside the frame's scope.
    #[inline]
    pub fn note_use(&self, idx: u32) {
        if let Some(b) = self.binds.get(idx as usize) {
            b.used.set(true);
            if self.rec {
                self.recursive.set(true);
            }
            if self.bind_sym == Some(b.sym) {
                self.bind_used.set(true);
            }
        }
    }
}

fn ignored(name: &str) -> bool {
    name.starts_with('_')
}

/// `shadow-builtin` and `shadow-binding` for a new binding: `outer` says
/// whether an enclosing lexical frame binds the name.
pub fn shadow(kind: BindKind, name: &str, is_global: bool, outer: bool) -> Vec<(LintId, String)> {
    let mut out = Vec::new();
    if kind == BindKind::Extra {
        return out;
    }
    if is_global {
        out.push((
            LintId::ShadowBuiltin,
            format!("{} `{name}` shadows the builtin `{name}`", kind.what()),
        ));
    }
    if outer && !ignored(name) {
        out.push((
            LintId::ShadowBinding,
            format!(
                "{} `{name}` shadows an outer binding of the same name",
                kind.what()
            ),
        ));
    }
    out
}

/// The unused-binding, unused-formal and rec-unused reports of a frame
/// that is being closed. `name_of` renders a symbol.
pub fn closed(
    frame: &Frame,
    pos: Pos,
    name_of: &dyn Fn(Sym) -> String,
) -> Vec<(LintId, Pos, String)> {
    let mut out = Vec::new();
    for b in &frame.binds {
        if b.used.get() || b.kind == BindKind::Extra || b.kind == BindKind::Arg {
            continue;
        }
        let name = name_of(b.sym);
        match b.kind {
            // a let name is the author's: `_x` says unused on purpose
            BindKind::Let if !ignored(&name) => out.push((
                LintId::UnusedBinding,
                b.pos,
                format!("let binding `{name}` is never used"),
            )),
            // a formal is the caller's contract: `{ _pkgs }:` still
            // demands the attribute, so no spelling marks it unused
            BindKind::Formal if !frame.bind_used.get() => out.push((
                LintId::UnusedFormal,
                b.pos,
                format!("formal `{name}` is never used; remove it from the pattern"),
            )),
            _ => {}
        }
    }
    if frame.rec && !frame.recursive.get() && !frame.binds.is_empty() {
        out.push((
            LintId::RecUnused,
            pos,
            "`rec` on a set where no binding references a sibling".into(),
        ));
    }
    out
}

/// `with`, `with-non-literal` and `with-nested` for a `with` whose scope
/// expression is `scope`.
pub fn with(scope: &Expr, nested: bool) -> Vec<(LintId, String)> {
    let mut out = vec![(
        LintId::With,
        "`with` brings unknown names into scope; prefer `inherit` or `let`".into(),
    )];
    if !matches!(scope, Expr::Attrs { .. } | Expr::Var(_)) {
        out.push((
            LintId::WithNonLiteral,
            "the scope of this `with` is neither an attrset literal nor a plain variable".into(),
        ));
    }
    if nested {
        out.push((
            LintId::WithNested,
            "`with` inside another `with`: names resolve through both scopes".into(),
        ));
    }
    out
}
