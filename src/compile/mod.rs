//! AST -> IR: scope resolution, attrpath merging, inherit expansion,
//! literal allocation, and folding of `builtins.x` to constants.

pub mod ir;

use std::collections::HashMap;

use crate::eval::{Error, ErrorKind, Pos};
use crate::runtime::{Arena, AttrsObj, Word};
use crate::syntax::ast::{
    Ast, AttrElem, BinOp, Bind, BindKind, Expr, ExprId, Param, PathKind, StrPart, UnOp,
};
use crate::syntax::symbol::{Interner, Sym};
use ir::*;

/// Names visible at the root of every file.
pub struct Globals {
    pub table: HashMap<Sym, Word>,
    /// The `builtins` attrset, for `builtins.x` folding.
    pub builtins: Option<&'static AttrsObj>,
}

enum Scope {
    Lexical(HashMap<Sym, u32>),
    With,
}

pub struct Compiler<'a> {
    ast: &'a Ast,
    arena: &'a mut Arena,
    interner: &'a mut Interner,
    globals: &'a Globals,
    scopes: Vec<Scope>,
    /// Directory of the file being compiled, for relative paths.
    base_dir: Vec<u8>,
    /// Global offset of this file's first byte.
    base: u32,
    home: Option<Vec<u8>>,
    settings: CompileSettings,
    sym_builtins: Sym,
    sym_cur_pos: Sym,
    sym_overrides: Sym,
    sym_body: Sym,
}

type CResult<T> = Result<T, Box<Error>>;

/// Nix's `Diagnose` setting for the `lint-*` options.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Lint {
    #[default]
    Ignore,
    Warn,
    Fatal,
}

impl Lint {
    pub fn parse(s: &str) -> Option<Lint> {
        match s {
            "ignore" => Some(Lint::Ignore),
            "warn" => Some(Lint::Warn),
            "fatal" => Some(Lint::Fatal),
            _ => None,
        }
    }
}

/// Settings the compiler needs from the evaluator.
#[derive(Clone, Copy, Default)]
pub struct CompileSettings {
    pub lint_absolute_path_literals: Lint,
    pub lint_short_path_literals: Lint,
}

fn err<T>(kind: ErrorKind, msg: String, pos: Pos) -> CResult<T> {
    Err(Box::new(Error::new(kind, msg, pos)))
}

/// Intermediate attrpath-merged binding tree, mirroring Nix's `addAttr`.
struct Merged {
    rec: bool,
    statics: Vec<(Sym, Def, Pos)>,
    index: HashMap<Sym, usize>,
    dynamics: Vec<(ExprId, Def, Pos)>,
    from_exprs: Vec<ExprId>,
    pos: Pos,
}

enum Def {
    Expr(ExprId),
    Nested(Box<Merged>),
    Inherit(Sym),
    InheritFrom(usize, Sym),
}

impl Merged {
    fn new(rec: bool, pos: Pos) -> Self {
        Merged {
            rec,
            statics: Vec::new(),
            index: HashMap::new(),
            dynamics: Vec::new(),
            from_exprs: Vec::new(),
            pos,
        }
    }
}

impl<'a> Compiler<'a> {
    pub fn new(
        ast: &'a Ast,
        arena: &'a mut Arena,
        interner: &'a mut Interner,
        globals: &'a Globals,
        base: u32,
        base_dir: &[u8],
        settings: CompileSettings,
    ) -> Self {
        let sym_builtins = interner.intern(b"builtins");
        let sym_cur_pos = interner.intern(b"__curPos");
        let sym_overrides = interner.intern(b"__overrides");
        let sym_body = interner.intern(b"body");
        Compiler {
            ast,
            arena,
            interner,
            globals,
            scopes: Vec::new(),
            base_dir: base_dir.to_vec(),
            base,
            home: std::env::var_os("HOME").map(|h| h.into_encoded_bytes()),
            settings,
            sym_builtins,
            sym_cur_pos,
            sym_overrides,
            sym_body,
        }
    }

    /// Compile a whole file. `extra_scope`, when given, is a lexical frame
    /// (for `scopedImport`) that the runtime supplies as the env above root.
    pub fn compile_root(&mut self, extra_scope: Option<&[Sym]>) -> CResult<IrRef> {
        if let Some(names) = extra_scope {
            let mut m = HashMap::new();
            for (i, s) in names.iter().enumerate() {
                m.insert(*s, i as u32);
            }
            self.scopes.push(Scope::Lexical(m));
        }
        let root = self.ast.root;
        let ir = self.expr(root)?;
        self.scopes.clear();
        Ok(ir)
    }

    fn pos(&self, id: ExprId) -> Pos {
        Pos(self.base + self.ast.span(id).start)
    }

    fn alloc<T>(&mut self, v: T) -> &'static T {
        self.arena.alloc(v)
    }

    fn alloc_refs(&mut self, v: Vec<IrRef>) -> &'static [IrRef] {
        self.arena.alloc_slice(&v)
    }

    fn resolve(&self, sym: Sym) -> Option<Ir> {
        let mut withs: Vec<u32> = Vec::new();
        for (d, scope) in self.scopes.iter().rev().enumerate() {
            match scope {
                Scope::Lexical(m) => {
                    if let Some(&idx) = m.get(&sym) {
                        return Some(Ir::Local {
                            depth: d as u32,
                            idx,
                        });
                    }
                }
                Scope::With => withs.push(d as u32),
            }
        }
        // Globals live in Nix's static base env, a lexical scope, so they
        // beat any enclosing `with` (`with { true = 1; }; true` is `true`).
        if let Some(w) = self.globals.table.get(&sym) {
            return Some(Ir::Const(*w));
        }
        if withs.is_empty() {
            return None;
        }
        Some(
            Ir::WithVar {
                sym,
                withs: &[],
                pos: Pos::NONE,
            }
            .with_withs(withs),
        )
    }

    fn expr(&mut self, id: ExprId) -> CResult<IrRef> {
        let ir = self.expr_inner(id)?;
        Ok(self.alloc(ir))
    }

    fn expr_inner(&mut self, id: ExprId) -> CResult<Ir> {
        let pos = self.pos(id);
        let ast: &'a Ast = self.ast;
        Ok(match ast.expr(id) {
            Expr::Int(i) => Ir::Const(self.arena.mk_int(*i)),
            Expr::Float(f) => Ir::Const(self.arena.mk_float(*f)),
            Expr::Str(parts) => self.string(parts, pos)?,
            Expr::Path(p) => self.path(&p.kind, &p.parts, pos)?,
            Expr::Var(sym) => {
                let sym = *sym;
                if sym == self.sym_cur_pos && self.lexical_lookup(sym).is_none() {
                    return Ok(Ir::CurPos(pos));
                }
                match self.resolve(sym) {
                    Some(Ir::WithVar { sym, withs, .. }) => Ir::WithVar { sym, withs, pos },
                    Some(ir) => ir,
                    None => {
                        return err(
                            ErrorKind::UndefinedVar,
                            format!("undefined variable '{}'", self.interner.name_str(sym)),
                            pos,
                        );
                    }
                }
            }
            Expr::Select {
                subject,
                path,
                default,
            } => {
                if let Some(folded) = self.fold_builtins_select(*subject, path, default.is_none()) {
                    return Ok(folded);
                }
                let subject = self.expr(*subject)?;
                let path = self.attr_path(path)?;
                let default = match default {
                    Some(d) => Some(self.expr(*d)?),
                    None => None,
                };
                Ir::Select {
                    subject,
                    path,
                    default,
                    pos,
                }
            }
            Expr::HasAttr { subject, path } => {
                let subject = self.expr(*subject)?;
                let path = self.attr_path(path)?;
                Ir::HasAttr { subject, path }
            }
            Expr::Attrs { rec, binds } => {
                let merged = self.merge_binds(*rec, binds, pos)?;
                let a = self.lower_attrs(merged)?;
                Ir::Attrs(a)
            }
            Expr::List(items) => {
                let mut v = Vec::with_capacity(items.len());
                for i in items {
                    v.push(self.expr(*i)?);
                }
                Ir::List(self.alloc_refs(v))
            }
            Expr::Lambda { param, body } => Ir::Lambda(self.lambda(param, *body, pos, None)?),
            Expr::Apply { func, args } => {
                let func = self.expr(*func)?;
                let mut v = Vec::with_capacity(args.len());
                for a in args {
                    v.push(self.expr(*a)?);
                }
                Ir::Apply {
                    func,
                    args: self.alloc_refs(v),
                    pos,
                }
            }
            Expr::Let { binds, body } => {
                let merged = self.merge_binds(true, binds, pos)?;
                if !merged.dynamics.is_empty() {
                    return err(
                        ErrorKind::Eval,
                        "dynamic attributes not allowed in let".into(),
                        pos,
                    );
                }
                let (binds, from) = self.lower_rec_binds(merged)?;
                let body = self.expr(*body)?;
                self.scopes.pop();
                Ir::Let { binds, from, body }
            }
            Expr::With { scope, body } => {
                let scope = self.expr(*scope)?;
                self.scopes.push(Scope::With);
                let body = self.expr(*body)?;
                self.scopes.pop();
                Ir::With { scope, body, pos }
            }
            Expr::If { cond, then, else_ } => Ir::If {
                cond: self.expr(*cond)?,
                then: self.expr(*then)?,
                else_: self.expr(*else_)?,
                pos: self.pos(*cond),
            },
            Expr::Assert { cond, body } => Ir::Assert {
                cond: self.expr(*cond)?,
                body: self.expr(*body)?,
                pos,
            },
            Expr::Unary { op, expr } => {
                let e = self.expr(*expr)?;
                match op {
                    UnOp::Not => Ir::Not(e, pos),
                    UnOp::Neg => Ir::Neg(e, pos),
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                let l = self.expr(*lhs)?;
                let r = self.expr(*rhs)?;
                match op {
                    BinOp::Eq => Ir::Eq(l, r, pos),
                    BinOp::Ne => Ir::Ne(l, r, pos),
                    BinOp::Lt => Ir::Lt(l, r, pos),
                    BinOp::Gt => Ir::Gt(l, r, pos),
                    BinOp::Le => Ir::Le(l, r, pos),
                    BinOp::Ge => Ir::Ge(l, r, pos),
                    BinOp::And => Ir::And(l, r, pos),
                    BinOp::Or => Ir::Or(l, r, pos),
                    BinOp::Impl => Ir::Impl(l, r, pos),
                    BinOp::Update => Ir::Update(l, r, pos),
                    BinOp::Concat => Ir::ConcatLists(l, r, pos),
                    BinOp::Add => Ir::Concat {
                        force_string: false,
                        parts: self.alloc_refs(vec![l, r]),
                        pos,
                    },
                    BinOp::Sub => Ir::Sub(l, r, pos),
                    BinOp::Mul => Ir::Mul(l, r, pos),
                    BinOp::Div => Ir::Div(l, r, pos),
                }
            }
        })
    }

    fn lexical_lookup(&self, sym: Sym) -> Option<(u32, u32)> {
        for (d, scope) in self.scopes.iter().rev().enumerate() {
            if let Scope::Lexical(m) = scope
                && let Some(&idx) = m.get(&sym)
            {
                return Some((d as u32, idx));
            }
        }
        None
    }

    /// `builtins.x` where `builtins` is the global: fold to the constant.
    fn fold_builtins_select(
        &mut self,
        subject: ExprId,
        path: &[AttrElem],
        strict: bool,
    ) -> Option<Ir> {
        let Expr::Var(s) = self.ast.expr(subject) else {
            return None;
        };
        if *s != self.sym_builtins || self.lexical_lookup(*s).is_some() {
            return None;
        }
        if self.scopes.iter().any(|s| matches!(s, Scope::With)) {
            return None;
        }
        let b = self.globals.builtins?;
        let AttrElem::Static(first) = path.first()? else {
            return None;
        };
        let attr = b.get(*first)?;
        let w = attr.cell.get();
        if w.is_thunk() {
            return None;
        }
        if path.len() == 1 {
            return Some(Ir::Const(w));
        }
        if !strict {
            return None;
        }
        let rest = self.attr_path(&path[1..]).ok()?;
        let subject = self.alloc(Ir::Const(w));
        Some(Ir::Select {
            subject,
            path: rest,
            default: None,
            pos: self.pos(subject_pos_hack(path)),
        })
    }

    fn attr_path(&mut self, path: &[AttrElem]) -> CResult<&'static [AttrElemIr]> {
        let mut v = Vec::with_capacity(path.len());
        for e in path {
            v.push(match e {
                AttrElem::Static(s) => AttrElemIr::Static(*s),
                AttrElem::Dynamic(id) => {
                    // A dynamic element that is a constant string is static.
                    if let Expr::Str(parts) = self.ast.expr(*id)
                        && let [StrPart::Lit(lit)] = parts.as_slice()
                    {
                        AttrElemIr::Static(self.interner.intern(lit))
                    } else {
                        AttrElemIr::Dynamic(self.expr(*id)?)
                    }
                }
            });
        }
        let refs: Vec<&'static AttrElemIr> = Vec::new();
        drop(refs);
        Ok(self.alloc_elems(v))
    }

    fn alloc_elems(&mut self, v: Vec<AttrElemIr>) -> &'static [AttrElemIr] {
        let n = v.len();
        if n == 0 {
            return &[];
        }
        let p = self
            .arena
            .alloc_raw(n * size_of::<AttrElemIr>(), align_of::<AttrElemIr>())
            as *mut AttrElemIr;
        // SAFETY: fresh allocation of n elements; values moved in.
        unsafe {
            for (i, e) in v.into_iter().enumerate() {
                p.add(i).write(e);
            }
            std::slice::from_raw_parts(p, n)
        }
    }

    fn alloc_binds(&mut self, v: Vec<BindIr>) -> &'static [BindIr] {
        let n = v.len();
        if n == 0 {
            return &[];
        }
        let p = self
            .arena
            .alloc_raw(n * size_of::<BindIr>(), align_of::<BindIr>())
            as *mut BindIr;
        // SAFETY: as above.
        unsafe {
            for (i, e) in v.into_iter().enumerate() {
                p.add(i).write(e);
            }
            std::slice::from_raw_parts(p, n)
        }
    }

    fn alloc_dyn(&mut self, v: Vec<DynBindIr>) -> &'static [DynBindIr] {
        let n = v.len();
        if n == 0 {
            return &[];
        }
        let p = self
            .arena
            .alloc_raw(n * size_of::<DynBindIr>(), align_of::<DynBindIr>())
            as *mut DynBindIr;
        // SAFETY: as above.
        unsafe {
            for (i, e) in v.into_iter().enumerate() {
                p.add(i).write(e);
            }
            std::slice::from_raw_parts(p, n)
        }
    }

    fn alloc_formals(&mut self, v: Vec<FormalIr>) -> &'static [FormalIr] {
        let n = v.len();
        if n == 0 {
            return &[];
        }
        let p = self
            .arena
            .alloc_raw(n * size_of::<FormalIr>(), align_of::<FormalIr>())
            as *mut FormalIr;
        // SAFETY: as above.
        unsafe {
            for (i, e) in v.into_iter().enumerate() {
                p.add(i).write(e);
            }
            std::slice::from_raw_parts(p, n)
        }
    }

    fn string(&mut self, parts: &[StrPart], pos: Pos) -> CResult<Ir> {
        if parts.iter().all(|p| matches!(p, StrPart::Lit(_))) {
            let mut bytes = Vec::new();
            for p in parts {
                if let StrPart::Lit(l) = p {
                    bytes.extend_from_slice(l);
                }
            }
            return Ok(Ir::Const(self.arena.mk_string(&bytes)));
        }
        let mut v = Vec::with_capacity(parts.len());
        for p in parts {
            v.push(match p {
                StrPart::Lit(l) => {
                    let w = self.arena.mk_string(l);
                    self.alloc(Ir::Const(w))
                }
                StrPart::Interp(e) => self.expr(*e)?,
            });
        }
        Ok(Ir::Concat {
            force_string: true,
            parts: self.alloc_refs(v),
            pos,
        })
    }

    fn resolve_path_literal(&self, kind: &PathKind, text: &[u8]) -> Vec<u8> {
        match kind {
            PathKind::Absolute => canon_path(text),
            PathKind::Relative => {
                let mut p = self.base_dir.clone();
                p.push(b'/');
                p.extend_from_slice(text);
                canon_path(&p)
            }
            PathKind::Home => {
                let mut p = self.home.clone().unwrap_or_else(|| b"/".to_vec());
                p.extend_from_slice(text);
                canon_path(&p)
            }
            PathKind::Search => unreachable!(),
        }
    }

    /// parser.y `path_start`: the `lint-absolute-path-literals` and
    /// `lint-short-path-literals` diagnostics.
    fn lint_path_literal(&self, kind: &PathKind, lit: &[u8], pos: Pos) -> CResult<()> {
        let (mode, msg, name) = match kind {
            PathKind::Absolute => (
                self.settings.lint_absolute_path_literals,
                format!(
                    "absolute path literals are not portable. Consider replacing path literal '{}' by a string, relative path, or parameter",
                    String::from_utf8_lossy(lit)
                ),
                "lint-absolute-path-literals",
            ),
            PathKind::Home => (
                self.settings.lint_absolute_path_literals,
                format!(
                    "home path literals are not portable. Consider replacing path literal '~{}' by a string, relative path, or parameter",
                    String::from_utf8_lossy(lit)
                ),
                "lint-absolute-path-literals",
            ),
            PathKind::Relative if lit.first() != Some(&b'.') => (
                self.settings.lint_short_path_literals,
                format!(
                    "relative path literal '{0}' should be prefixed with '.' for clarity: './{0}'",
                    String::from_utf8_lossy(lit)
                ),
                "lint-short-path-literals",
            ),
            _ => return Ok(()),
        };
        match mode {
            Lint::Ignore => Ok(()),
            Lint::Warn => {
                eprintln!("warning: {msg} ({name})");
                Ok(())
            }
            Lint::Fatal => err(ErrorKind::Parse, format!("{msg} ({name})"), pos),
        }
    }

    fn path(&mut self, kind: &PathKind, parts: &[StrPart], pos: Pos) -> CResult<Ir> {
        if *kind == PathKind::Search {
            let name = self.string(parts, pos)?;
            let name = self.alloc(name);
            // `<x>` is `__findFile __nixPath "x"`, so a local `__nixPath`
            // or `__findFile` takes part like in Nix
            let ff = self.interner.intern(b"__findFile");
            let np = self.interner.intern(b"__nixPath");
            if self.lexical_lookup(ff).is_some()
                || self.lexical_lookup(np).is_some()
                || self.scopes.iter().any(|s| matches!(s, Scope::With))
            {
                let func = self.var_ir(ff, pos)?;
                let path = self.var_ir(np, pos)?;
                return Ok(Ir::Apply {
                    func,
                    args: self.alloc_refs(vec![path, name]),
                    pos,
                });
            }
            return Ok(Ir::SearchPath { name, pos });
        }
        let all_lit = parts.iter().all(|p| matches!(p, StrPart::Lit(_)));
        let first_lit: Vec<u8> = match parts.first() {
            Some(StrPart::Lit(l)) => l.clone(),
            _ => Vec::new(),
        };
        self.lint_path_literal(kind, &first_lit, pos)?;
        let trailing_slash = first_lit.ends_with(b"/") && !all_lit;
        let mut resolved = self.resolve_path_literal(kind, &first_lit);
        if trailing_slash && !resolved.ends_with(b"/") {
            resolved.push(b'/');
        }
        let first = self.arena.mk_path(&resolved);
        if all_lit {
            return Ok(Ir::Const(first));
        }
        let mut v = Vec::with_capacity(parts.len());
        v.push(self.alloc(Ir::Const(first)));
        let rest = if matches!(parts.first(), Some(StrPart::Lit(_))) {
            &parts[1..]
        } else {
            parts
        };
        for p in rest {
            v.push(match p {
                StrPart::Lit(l) => {
                    let w = self.arena.mk_string(l);
                    self.alloc(Ir::Const(w))
                }
                StrPart::Interp(e) => self.expr(*e)?,
            });
        }
        Ok(Ir::Concat {
            force_string: false,
            parts: self.alloc_refs(v),
            pos,
        })
    }

    // ---- attribute sets -------------------------------------------------

    fn merge_binds(&mut self, rec: bool, binds: &[Bind], pos: Pos) -> CResult<Merged> {
        let mut m = Merged::new(rec, pos);
        for b in binds {
            self.add_bind(&mut m, b)?;
        }
        Ok(m)
    }

    fn bind_pos(&self, b: &Bind) -> Pos {
        Pos(self.base + b.span.start)
    }

    fn add_bind(&mut self, m: &mut Merged, b: &Bind) -> CResult<()> {
        let pos = self.bind_pos(b);
        match &b.kind {
            BindKind::Value { path, value } => self.add_path(m, path, Def::Expr(*value), pos),
            BindKind::Inherit { from: None, names } => {
                for (name, span) in names {
                    let p = Pos(self.base + span.start);
                    self.add_static(m, *name, Def::Inherit(*name), p)?;
                }
                Ok(())
            }
            BindKind::Inherit {
                from: Some(e),
                names,
            } => {
                let idx = m.from_exprs.len();
                m.from_exprs.push(*e);
                for (name, span) in names {
                    let p = Pos(self.base + span.start);
                    self.add_static(m, *name, Def::InheritFrom(idx, *name), p)?;
                }
                Ok(())
            }
        }
    }

    fn add_path(&mut self, m: &mut Merged, path: &[AttrElem], def: Def, pos: Pos) -> CResult<()> {
        let (last, prefix) = path.split_last().expect("attrpath has an element");
        let mut cur = m;
        for elem in prefix {
            match self.static_name(elem) {
                Some(sym) => {
                    let idx = match cur.index.get(&sym) {
                        Some(&i) => {
                            self.open_nested(cur, i, pos)?;
                            i
                        }
                        None => {
                            let i = cur.statics.len();
                            cur.statics.push((
                                sym,
                                Def::Nested(Box::new(Merged::new(false, pos))),
                                pos,
                            ));
                            cur.index.insert(sym, i);
                            i
                        }
                    };
                    let Def::Nested(n) = &mut cur.statics[idx].1 else {
                        unreachable!()
                    };
                    cur = n;
                }
                None => {
                    let AttrElem::Dynamic(e) = elem else {
                        unreachable!()
                    };
                    cur.dynamics
                        .push((*e, Def::Nested(Box::new(Merged::new(false, pos))), pos));
                    let Def::Nested(n) = &mut cur.dynamics.last_mut().unwrap().1 else {
                        unreachable!()
                    };
                    cur = n;
                }
            }
        }
        match self.static_name(last) {
            Some(sym) => self.add_static(cur, sym, def, pos),
            None => {
                let AttrElem::Dynamic(e) = last else {
                    unreachable!()
                };
                cur.dynamics.push((*e, def, pos));
                Ok(())
            }
        }
    }

    fn static_name(&mut self, e: &AttrElem) -> Option<Sym> {
        let ast: &'a Ast = self.ast;
        match e {
            AttrElem::Static(s) => Some(*s),
            AttrElem::Dynamic(id) => {
                if let Expr::Str(parts) = ast.expr(*id)
                    && let [StrPart::Lit(lit)] = parts.as_slice()
                {
                    Some(self.interner.intern(lit))
                } else {
                    None
                }
            }
        }
    }

    /// Turn an `a = { ... };` literal into a mergeable nested node.
    fn open_nested(&mut self, m: &mut Merged, idx: usize, pos: Pos) -> CResult<()> {
        let sym = m.statics[idx].0;
        match &m.statics[idx].1 {
            Def::Nested(_) => Ok(()),
            Def::Expr(e) => {
                let ast: &'a Ast = self.ast;
                if let Expr::Attrs { rec, binds } = ast.expr(*e) {
                    let nested = self.merge_binds(*rec, binds, self.pos(*e))?;
                    m.statics[idx].1 = Def::Nested(Box::new(nested));
                    Ok(())
                } else {
                    self.dup_attr(sym, pos, m.statics[idx].2)
                }
            }
            _ => self.dup_attr(sym, pos, m.statics[idx].2),
        }
    }

    fn dup_attr<T>(&self, sym: Sym, pos: Pos, _prev: Pos) -> CResult<T> {
        err(
            ErrorKind::Eval,
            format!(
                "attribute '{}' already defined",
                self.interner.name_str(sym)
            ),
            pos,
        )
    }

    fn add_static(&mut self, m: &mut Merged, sym: Sym, def: Def, pos: Pos) -> CResult<()> {
        if let Some(&i) = m.index.get(&sym) {
            // Both sides attrs literals: merge (Nix discards the inner `rec`).
            let ast: &'a Ast = self.ast;
            let new_binds: Option<&'a [Bind]> = match &def {
                Def::Expr(e) => match ast.expr(*e) {
                    Expr::Attrs { binds, .. } => Some(binds),
                    _ => None,
                },
                Def::Nested(_) => Some(&[]),
                _ => None,
            };
            let Some(new_binds) = new_binds else {
                return self.dup_attr(sym, pos, m.statics[i].2);
            };
            self.open_nested(m, i, pos)?;
            let Def::Nested(target) = &mut m.statics[i].1 else {
                unreachable!()
            };
            if let Def::Nested(src) = def {
                let src = *src;
                for (s, d, p) in src.statics {
                    Self::add_static_def(self, target, s, d, p)?;
                }
                target.dynamics.extend(src.dynamics);
            } else {
                for b in new_binds {
                    self.add_bind(target, b)?;
                }
            }
            return Ok(());
        }
        let i = m.statics.len();
        m.statics.push((sym, def, pos));
        m.index.insert(sym, i);
        Ok(())
    }

    fn add_static_def(
        this: &mut Self,
        m: &mut Merged,
        sym: Sym,
        def: Def,
        pos: Pos,
    ) -> CResult<()> {
        this.add_static(m, sym, def, pos)
    }

    /// Plain (non-rec) attrs and nested attrpath sets.
    fn lower_attrs(&mut self, m: Merged) -> CResult<&'static AttrsIr> {
        if m.rec {
            let pos = m.pos;
            let mut m = m;
            let dynamics = std::mem::take(&mut m.dynamics);
            let (binds, from) = self.lower_rec_binds(m)?;
            // dynamics of rec sets are evaluated in the rec env: compile them
            // while its scope is still pushed
            let dynamic = self.lower_dynamics(dynamics)?;
            let overrides = binds
                .iter()
                .position(|b| b.sym == self.sym_overrides)
                .map(|i| i as u32);
            self.scopes.pop();
            return Ok(self.alloc(AttrsIr {
                rec: true,
                binds,
                from,
                dynamic,
                pos,
                overrides,
            }));
        }
        let Merged {
            statics,
            dynamics,
            from_exprs,
            pos,
            ..
        } = m;
        let mut binds: Vec<BindIr> = Vec::with_capacity(statics.len());
        let mut deferred_from: Vec<(usize, Sym, Pos, usize)> = Vec::new();
        for (sym, def, bpos) in statics {
            match def {
                Def::Expr(e) => {
                    let value = self.value_expr(e, Some(sym))?;
                    binds.push(BindIr {
                        sym,
                        value,
                        pos: bpos,
                        scope: BindScope::Own,
                    });
                }
                Def::Nested(n) => {
                    let a = self.lower_attrs(*n)?;
                    let value = self.alloc(Ir::Attrs(a));
                    binds.push(BindIr {
                        sym,
                        value,
                        pos: bpos,
                        scope: BindScope::Own,
                    });
                }
                Def::Inherit(var) => {
                    let value = self.var_ir(var, bpos)?;
                    binds.push(BindIr {
                        sym,
                        value,
                        pos: bpos,
                        scope: BindScope::Own,
                    });
                }
                Def::InheritFrom(fi, name) => {
                    deferred_from.push((fi, name, bpos, binds.len()));
                    // placeholder, patched below
                    binds.push(BindIr {
                        sym,
                        value: self.alloc(Ir::Const(Word::NULL)),
                        pos: bpos,
                        scope: BindScope::From,
                    });
                }
            }
        }
        let mut from: Vec<IrRef> = Vec::with_capacity(from_exprs.len());
        for e in &from_exprs {
            from.push(self.expr(*e)?);
        }
        if !deferred_from.is_empty() {
            self.scopes.push(Scope::Lexical(HashMap::new()));
            // the from-env has `from.len()` anonymous slots at depth 0
            for (fi, name, bpos, bi) in deferred_from {
                let subject = self.alloc(Ir::Local {
                    depth: 0,
                    idx: fi as u32,
                });
                let path = self.alloc_elems(vec![AttrElemIr::Static(name)]);
                binds[bi].value = self.alloc(Ir::Select {
                    subject,
                    path,
                    default: None,
                    pos: bpos,
                });
            }
            self.scopes.pop();
        }
        let dynamic = self.lower_dynamics(dynamics)?;
        binds.sort_by_key(|b| b.sym);
        let binds = self.alloc_binds(binds);
        let from = self.alloc_refs(from);
        Ok(self.alloc(AttrsIr {
            rec: false,
            binds,
            from,
            dynamic,
            pos,
            overrides: None,
        }))
    }

    fn lower_dynamics(
        &mut self,
        dynamics: Vec<(ExprId, Def, Pos)>,
    ) -> CResult<&'static [DynBindIr]> {
        let mut dyns = Vec::with_capacity(dynamics.len());
        for (name_e, def, dpos) in dynamics {
            let name = self.expr(name_e)?;
            let value = match def {
                Def::Expr(e) => self.expr(e)?,
                Def::Nested(n) => {
                    let a = self.lower_attrs(*n)?;
                    self.alloc(Ir::Attrs(a))
                }
                _ => unreachable!("inherit is never dynamic"),
            };
            dyns.push(DynBindIr {
                name,
                value,
                pos: dpos,
            });
        }
        Ok(self.alloc_dyn(dyns))
    }

    /// Recursive bindings (`rec { }` and `let`). Pushes the new scope and
    /// leaves it on the stack so the caller can compile the body; the caller
    /// pops it.
    fn lower_rec_binds(&mut self, m: Merged) -> CResult<(&'static [BindIr], &'static [IrRef])> {
        let Merged {
            mut statics,
            from_exprs,
            ..
        } = m;
        statics.sort_by_key(|s| s.0);
        // `inherit x` values resolve in the enclosing scope, so compile them
        // before pushing the new frame.
        let mut outer_vals: Vec<Option<IrRef>> = Vec::with_capacity(statics.len());
        for (_, def, bpos) in &statics {
            outer_vals.push(match def {
                Def::Inherit(var) => Some(self.var_ir(*var, *bpos)?),
                _ => None,
            });
        }
        let mut names = HashMap::with_capacity(statics.len());
        for (i, (sym, _, _)) in statics.iter().enumerate() {
            names.insert(*sym, i as u32);
        }
        self.scopes.push(Scope::Lexical(names));
        let nbinds = statics.len();
        let mut binds = Vec::with_capacity(nbinds);
        for (i, (sym, def, bpos)) in statics.into_iter().enumerate() {
            let (value, scope) = match def {
                Def::Expr(e) => (self.value_expr(e, Some(sym))?, BindScope::Own),
                Def::Nested(n) => {
                    let a = self.lower_attrs(*n)?;
                    (self.alloc(Ir::Attrs(a)), BindScope::Own)
                }
                Def::Inherit(_) => (outer_vals[i].unwrap(), BindScope::Outer),
                Def::InheritFrom(fi, name) => {
                    let subject = self.alloc(Ir::Local {
                        depth: 0,
                        idx: (nbinds + fi) as u32,
                    });
                    let path = self.alloc_elems(vec![AttrElemIr::Static(name)]);
                    (
                        self.alloc(Ir::Select {
                            subject,
                            path,
                            default: None,
                            pos: bpos,
                        }),
                        BindScope::Own,
                    )
                }
            };
            binds.push(BindIr {
                sym,
                value,
                pos: bpos,
                scope,
            });
        }
        let mut from = Vec::with_capacity(from_exprs.len());
        for e in from_exprs {
            from.push(self.expr(e)?);
        }
        Ok((self.alloc_binds(binds), self.alloc_refs(from)))
    }

    fn var_ir(&mut self, var: Sym, pos: Pos) -> CResult<IrRef> {
        if var == self.sym_cur_pos && self.lexical_lookup(var).is_none() {
            return Ok(self.alloc(Ir::CurPos(pos)));
        }
        match self.resolve(var) {
            Some(Ir::WithVar { sym, withs, .. }) => Ok(self.alloc(Ir::WithVar { sym, withs, pos })),
            Some(ir) => Ok(self.alloc(ir)),
            None => err(
                ErrorKind::UndefinedVar,
                format!("undefined variable '{}'", self.interner.name_str(var)),
                pos,
            ),
        }
    }

    /// A binding's value; names lambdas after their binding.
    fn value_expr(&mut self, e: ExprId, name: Option<Sym>) -> CResult<IrRef> {
        let ast: &'a Ast = self.ast;
        if let Expr::Lambda { param, body } = ast.expr(e) {
            let pos = self.pos(e);
            let l = self.lambda(param, *body, pos, name)?;
            return Ok(self.alloc(Ir::Lambda(l)));
        }
        self.expr(e)
    }

    fn lambda(
        &mut self,
        param: &Param,
        body: ExprId,
        pos: Pos,
        name: Option<Sym>,
    ) -> CResult<&'static LambdaIr> {
        let _ = self.sym_body;
        match param {
            Param::Ident(sym) => {
                let mut m = HashMap::new();
                m.insert(*sym, 0);
                self.scopes.push(Scope::Lexical(m));
                let body = self.expr(body)?;
                self.scopes.pop();
                Ok(self.alloc(LambdaIr {
                    param: ParamIr::Ident,
                    body,
                    pos,
                    name,
                    arg: Some(*sym),
                }))
            }
            Param::Formals {
                bind,
                formals,
                ellipsis,
            } => {
                let mut sorted: Vec<_> = formals.iter().collect();
                sorted.sort_by_key(|f| f.name);
                for w in sorted.windows(2) {
                    if w[0].name == w[1].name {
                        return err(
                            ErrorKind::Eval,
                            format!(
                                "duplicate formal function argument '{}'",
                                self.interner.name_str(w[0].name)
                            ),
                            pos,
                        );
                    }
                }
                let mut m = HashMap::new();
                let mut idx = 0u32;
                if let Some(b) = bind {
                    m.insert(*b, idx);
                    idx += 1;
                }
                for f in &sorted {
                    if m.insert(f.name, idx).is_some() {
                        return err(
                            ErrorKind::Eval,
                            format!(
                                "duplicate formal function argument '{}'",
                                self.interner.name_str(f.name)
                            ),
                            pos,
                        );
                    }
                    idx += 1;
                }
                self.scopes.push(Scope::Lexical(m));
                let mut firs = Vec::with_capacity(sorted.len());
                for f in &sorted {
                    let default = match f.default {
                        Some(d) => Some(self.expr(d)?),
                        None => None,
                    };
                    firs.push(FormalIr {
                        sym: f.name,
                        default,
                        pos: Pos(self.base + f.span.start),
                    });
                }
                let body = self.expr(body)?;
                self.scopes.pop();
                let formals = self.alloc_formals(firs);
                Ok(self.alloc(LambdaIr {
                    param: ParamIr::Formals {
                        has_bind: bind.is_some(),
                        formals,
                        ellipsis: *ellipsis,
                    },
                    body,
                    pos,
                    name,
                    arg: *bind,
                }))
            }
        }
    }
}

fn subject_pos_hack(_path: &[AttrElem]) -> ExprId {
    ExprId(0)
}

impl Ir {
    fn with_withs(self, withs: Vec<u32>) -> Ir {
        match self {
            Ir::WithVar { sym, pos, .. } => Ir::WithVar {
                sym,
                withs: Box::leak(withs.into_boxed_slice()),
                pos,
            },
            other => other,
        }
    }
}

/// Lexical path canonicalisation like Nix's `CanonPath`: collapse `//`,
/// drop `.`, resolve `..`, strip a trailing slash (except root).
pub fn canon_path(p: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(p.len());
    let mut comps: Vec<&[u8]> = Vec::new();
    for c in p.split(|&b| b == b'/') {
        match c {
            b"" | b"." => {}
            b".." => {
                comps.pop();
            }
            c => comps.push(c),
        }
    }
    if comps.is_empty() {
        out.push(b'/');
        return out;
    }
    for c in comps {
        out.push(b'/');
        out.extend_from_slice(c);
    }
    out
}
