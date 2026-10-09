//! iets's syntax tree in the form `parse-oracle` prints Nix's: the
//! desugaring of `parser.y` and the attribute merge of `parser-state.hh`
//! (Nix 2.34.8), applied to the raw tree iets's parser keeps.

use crate::sx::{Sx, atom, float, node};
use iets::syntax::ast::*;
use iets::syntax::symbol::{Interner, Sym};

/// Relative paths resolve against this, as the oracle's base path
/// (SPEC.md of nix-conformance).
pub const BASE: &str = "/case";
/// `~` is this, as the oracle's `$HOME`.
pub const HOME: &str = "/home/case";

pub fn tree(ast: &Ast, src: &[u8], interner: &Interner) -> Sx {
    Canon { ast, src, interner }.expr(ast.root)
}

struct Canon<'a> {
    ast: &'a Ast,
    src: &'a [u8],
    interner: &'a Interner,
}

struct Set {
    rec: bool,
    binds: Vec<(Vec<u8>, Def)>,
    dynamic: Vec<(Sx, Val)>,
}

enum Def {
    Plain(Val),
    Inherit,
    InheritFrom(Sx),
}

enum Val {
    Set(Set),
    Expr(Sx),
}

fn var(name: &str) -> Sx {
    node("var", vec![Sx::Str(name.as_bytes().to_vec())])
}

fn str_lit(s: &[u8]) -> Sx {
    node("str", vec![Sx::Str(s.to_vec())])
}

fn call(f: Sx, args: Vec<Sx>) -> Sx {
    let mut v = vec![atom("call"), f];
    v.extend(args);
    Sx::List(v)
}

impl Canon<'_> {
    fn name(&self, s: Sym) -> Vec<u8> {
        self.interner.name(s).to_vec()
    }

    fn expr(&self, id: ExprId) -> Sx {
        match *self.ast.expr(id) {
            Expr::Int(i) => node("int", vec![Sx::Atom(i.to_string())]),
            Expr::Float(f) => float(f),
            Expr::Str(parts) => self.string(id, parts),
            Expr::Path(p) => self.path(p),
            Expr::Var(s) if self.interner.name(s) == b"__curPos" => node("cur-pos", vec![]),
            Expr::Var(s) => node("var", vec![Sx::Str(self.name(s))]),
            Expr::Select {
                subject,
                path,
                default,
            } => {
                let mut v = vec![self.expr(subject), self.attrpath(path)];
                v.extend(default.map(|d| self.expr(d)));
                node("select", v)
            }
            Expr::HasAttr { subject, path } => {
                node("has", vec![self.expr(subject), self.attrpath(path)])
            }
            Expr::Attrs { rec, binds } => self.set(self.binds(rec, binds)),
            Expr::List(items) => node(
                "list",
                self.ast.ids(items).iter().map(|e| self.expr(*e)).collect(),
            ),
            Expr::Lambda { param, body } => {
                let (arg, formals) = match param {
                    Param::Ident(s) => (Sx::Str(self.name(s)), atom("_")),
                    Param::Formals {
                        bind,
                        formals,
                        ellipsis,
                    } => {
                        let mut v: Vec<Sx> = self
                            .ast
                            .formals(formals)
                            .iter()
                            .map(|f| {
                                let mut v = vec![Sx::Str(self.name(f.name))];
                                v.extend(f.default.map(|d| self.expr(d)));
                                node("formal", v)
                            })
                            .collect();
                        if ellipsis {
                            v.push(atom("..."));
                        }
                        (
                            bind.map_or(atom("_"), |b| Sx::Str(self.name(b))),
                            node("formals", v),
                        )
                    }
                };
                node("lambda", vec![arg, formals, self.expr(body)])
            }
            // `e or` is `expr_simple OR_KW` in parser.y: a call of its own,
            // never an extension of `e`
            Expr::Apply { func, args } if self.cursed_or(args) => call(
                self.expr(func),
                self.ast.ids(args).iter().map(|a| self.expr(*a)).collect(),
            ),
            // makeCall: applying a call extends it, whatever made the call
            Expr::Apply { func, args } => {
                let mut f = self.expr(func);
                for a in self.ast.ids(args) {
                    let a = self.expr(*a);
                    f = match f {
                        Sx::List(mut v) if v[0] == atom("call") => {
                            v.push(a);
                            Sx::List(v)
                        }
                        f => call(f, vec![a]),
                    };
                }
                f
            }
            Expr::Let { binds, body } => node(
                "let",
                vec![self.set(self.binds(false, binds)), self.expr(body)],
            ),
            Expr::With { scope, body } => node("with", vec![self.expr(scope), self.expr(body)]),
            Expr::If { cond, then, else_ } => node(
                "if",
                vec![self.expr(cond), self.expr(then), self.expr(else_)],
            ),
            Expr::Assert { cond, body } => node("assert", vec![self.expr(cond), self.expr(body)]),
            Expr::Unary {
                op: UnOp::Not,
                expr,
            } => node("!", vec![self.expr(expr)]),
            Expr::Unary {
                op: UnOp::Neg,
                expr,
            } => call(
                var("__sub"),
                vec![node("int", vec![atom("0")]), self.expr(expr)],
            ),
            Expr::Binary { op, lhs, rhs } => {
                let (l, r) = (self.expr(lhs), self.expr(rhs));
                let less = |a, b| call(var("__lessThan"), vec![a, b]);
                match op {
                    BinOp::Eq => node("==", vec![l, r]),
                    BinOp::Ne => node("!=", vec![l, r]),
                    BinOp::And => node("&&", vec![l, r]),
                    BinOp::Or => node("||", vec![l, r]),
                    BinOp::Impl => node("->", vec![l, r]),
                    BinOp::Update => node("//", vec![l, r]),
                    BinOp::Concat => node("++", vec![l, r]),
                    BinOp::Add => node("+", vec![l, r]),
                    BinOp::Sub => call(var("__sub"), vec![l, r]),
                    BinOp::Mul => call(var("__mul"), vec![l, r]),
                    BinOp::Div => call(var("__div"), vec![l, r]),
                    BinOp::Lt => less(l, r),
                    BinOp::Gt => less(r, l),
                    BinOp::Le => node("!", vec![less(r, l)]),
                    BinOp::Ge => node("!", vec![less(l, r)]),
                }
            }
        }
    }

    /// `or` lexes as a keyword, so a variable `or` can only be the first
    /// argument of such a call; later arguments extend that call.
    fn cursed_or(&self, args: Range) -> bool {
        matches!(self.ast.ids(args), [a, ..] if matches!(self.ast.expr(*a), Expr::Var(s) if self.interner.name(*s) == b"or"))
    }

    fn parts(&self, parts: Range) -> Vec<Sx> {
        self.ast
            .parts(parts)
            .iter()
            .map(|p| match p {
                StrPart::Lit(l) => str_lit(self.ast.bytes(*l)),
                StrPart::Interp(e) => self.expr(*e),
            })
            .collect()
    }

    /// A URL literal is a plain string to Nix; the source tells it apart
    /// from a quoted one.
    fn string(&self, id: ExprId, parts: Range) -> Sx {
        let quoted = matches!(
            self.src.get(self.ast.span(id).start as usize),
            Some(b'"' | b'\'')
        );
        match (quoted, self.ast.single_lit(parts)) {
            (_, Some(s)) => str_lit(s),
            (false, None) => str_lit(&[]),
            (true, None) if parts.is_empty() => str_lit(&[]),
            (true, None) => node("interp", self.parts(parts)),
        }
    }

    fn path(&self, p: PathExpr) -> Sx {
        let parts = self.ast.parts(p.parts);
        let lead = parts
            .iter()
            .take_while(|x| matches!(x, StrPart::Lit(_)))
            .count();
        let literal: Vec<u8> = parts[..lead]
            .iter()
            .flat_map(|x| match x {
                StrPart::Lit(l) => self.ast.bytes(*l).to_vec(),
                StrPart::Interp(_) => vec![],
            })
            .collect();
        if p.kind == PathKind::Search {
            return call(var("__findFile"), vec![var("__nixPath"), str_lit(&literal)]);
        }
        let interpolated = lead < parts.len();
        let (token, rest) = literal.split_at(if interpolated {
            first_token(&literal)
        } else {
            literal.len()
        });
        let mut resolved = match p.kind {
            PathKind::Home => [HOME.as_bytes(), token].concat(),
            PathKind::Absolute => canon_path(token),
            _ => canon_path(&[BASE.as_bytes(), b"/", token].concat()),
        };
        if p.kind != PathKind::Home && token.len() > 1 && token.ends_with(b"/") {
            resolved.push(b'/');
        }
        let head = node("path", vec![Sx::Str(resolved)]);
        if !interpolated {
            return head;
        }
        let mut v = vec![head];
        if !rest.is_empty() {
            v.push(str_lit(rest));
        }
        v.extend(self.parts(Range {
            start: p.parts.start + lead as u32,
            len: p.parts.len - lead as u32,
        }));
        node("+", v)
    }

    fn attrpath(&self, path: Range) -> Sx {
        node(
            "attrpath",
            self.ast.elems(path).iter().map(|e| self.elem(e)).collect(),
        )
    }

    fn elem(&self, e: &AttrElem) -> Sx {
        match e {
            AttrElem::Static(s) => Sx::Str(self.name(*s)),
            AttrElem::Dynamic(e) => node("dyn", vec![self.expr(*e)]),
        }
    }

    fn binds(&self, rec: bool, binds: Range) -> Set {
        let mut set = Set {
            rec,
            binds: vec![],
            dynamic: vec![],
        };
        for b in self.ast.binds(binds) {
            match b.kind {
                BindKind::Value { path, value } => {
                    let elems: Vec<&AttrElem> = self.ast.elems(path).iter().collect();
                    let value = self.value(value);
                    add(&mut set, &elems, value, self);
                }
                BindKind::Inherit { from, names } => {
                    let from = from.map(|f| self.expr(f));
                    for (s, _) in self.ast.names(names) {
                        let def = from.clone().map_or(Def::Inherit, Def::InheritFrom);
                        set.binds.push((self.name(*s), def));
                    }
                }
            }
        }
        set
    }

    fn value(&self, id: ExprId) -> Val {
        match *self.ast.expr(id) {
            Expr::Attrs { rec, binds } => Val::Set(self.binds(rec, binds)),
            _ => Val::Expr(self.expr(id)),
        }
    }

    fn set(&self, s: Set) -> Sx {
        let mut v = vec![atom(if s.rec { "rec" } else { "nonrec" })];
        for (name, def) in s.binds {
            let d = match def {
                Def::Plain(val) => self.val(val),
                Def::Inherit => node("inherit", vec![]),
                Def::InheritFrom(e) => node("inherit-from", vec![e]),
            };
            v.push(node("bind", vec![Sx::Str(name), d]));
        }
        for (n, val) in s.dynamic {
            v.push(node("dyn-bind", vec![n, self.val(val)]));
        }
        node("attrs", v)
    }

    fn val(&self, v: Val) -> Sx {
        match v {
            Val::Set(s) => self.set(s),
            Val::Expr(e) => e,
        }
    }
}

/// `ParserState::addAttr`: a path creates the sets it passes through, and
/// a set literal merges into one already there. The `rec` of the second
/// set is dropped, as Nix does (NixOS/nix#9020). A clash keeps the first
/// definition: iets has refused the file then, and so has Nix.
fn add(set: &mut Set, path: &[&AttrElem], value: Val, c: &Canon) {
    let (first, rest) = path
        .split_first()
        .expect("an attribute path is never empty");
    let name = match first {
        AttrElem::Static(s) => c.name(*s),
        AttrElem::Dynamic(e) => {
            let v = if rest.is_empty() {
                value
            } else {
                let mut nested = Set {
                    rec: false,
                    binds: vec![],
                    dynamic: vec![],
                };
                add(&mut nested, rest, value, c);
                Val::Set(nested)
            };
            set.dynamic.push((c.expr(*e), v));
            return;
        }
    };
    let at = set.binds.iter().position(|(n, _)| *n == name);
    if rest.is_empty() {
        match (at, value) {
            (None, value) => set.binds.push((name, Def::Plain(value))),
            (Some(i), Val::Set(new)) => {
                if let Def::Plain(Val::Set(old)) = &mut set.binds[i].1 {
                    merge(old, new);
                }
            }
            (Some(_), Val::Expr(_)) => {}
        }
        return;
    }
    let i = match at {
        Some(i) => i,
        None => {
            set.binds.push((
                name,
                Def::Plain(Val::Set(Set {
                    rec: false,
                    binds: vec![],
                    dynamic: vec![],
                })),
            ));
            set.binds.len() - 1
        }
    };
    if let Def::Plain(Val::Set(nested)) = &mut set.binds[i].1 {
        add(nested, rest, value, c);
    }
}

fn merge(into: &mut Set, from: Set) {
    for (name, def) in from.binds {
        match set_at(into, &name) {
            None => into.binds.push((name, def)),
            Some(i) => {
                if let (Def::Plain(Val::Set(old)), Def::Plain(Val::Set(new))) =
                    (&mut into.binds[i].1, def)
                {
                    merge(old, new);
                }
            }
        }
    }
    into.dynamic.extend(from.dynamic);
}

/// The length of the first token of a path whose text before its first
/// `${` is `lit`: the longest match of `PATH` or `PATH_SEG\$\{` (lexer.l).
/// For `~` paths `lit` starts after the `~`, which makes `HPATH` and
/// `HPATH_START` the same patterns.
fn first_token(lit: &[u8]) -> usize {
    let pc = |b: Option<&u8>| b.is_some_and(|&b| b.is_ascii_alphanumeric() || b"._-+".contains(&b));
    let head = lit.iter().take_while(|b| pc(Some(b))).count();
    let (mut i, mut segs) = (head, 0);
    while lit.get(i) == Some(&b'/') && pc(lit.get(i + 1)) {
        i += 1;
        while pc(lit.get(i)) {
            i += 1;
        }
        segs += 1;
    }
    if lit.get(i) == Some(&b'/') {
        i += 1;
    }
    let path = if segs > 0 { i } else { 0 };
    let seg_then_interp = lit.get(head) == Some(&b'/') && head + 1 == lit.len();
    if seg_then_interp && head + 3 > path {
        head + 1
    } else {
        path
    }
}

fn set_at(s: &Set, name: &[u8]) -> Option<usize> {
    s.binds.iter().position(|(n, _)| n == name)
}

/// `canonPathInner` without symlinks (file-path-impl.hh): `.` goes, `..`
/// drops a component, slashes collapse.
fn canon_path(p: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    for comp in p.split(|&b| b == b'/') {
        match comp {
            b"" | b"." => {}
            b".." => {
                let cut = out.iter().rposition(|&b| b == b'/').unwrap_or(0);
                out.truncate(cut);
            }
            c => {
                out.push(b'/');
                out.extend_from_slice(c);
            }
        }
    }
    if out.is_empty() {
        out.push(b'/');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::canon_path;

    #[test]
    fn canon() {
        assert_eq!(canon_path(b"/fuzz/./a/../b//c"), b"/fuzz/b/c");
        assert_eq!(canon_path(b"/../.."), b"/");
    }
}
