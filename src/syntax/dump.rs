//! S-expression dump of an `Ast`, for `ietsp parse` and for tests.

use super::ast::*;
use super::symbol::Interner;

pub fn dump(ast: &Ast, interner: &Interner) -> String {
    let mut out = String::new();
    Dumper { ast, interner }.expr(ast.root, &mut out);
    out
}

struct Dumper<'a> {
    ast: &'a Ast,
    interner: &'a Interner,
}

fn quote(bytes: &[u8], out: &mut String) {
    out.push('"');
    for &b in bytes {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out.push('"');
}

impl Dumper<'_> {
    fn sym(&self, s: super::symbol::Sym, out: &mut String) {
        out.push_str(self.interner.name_str(s));
    }

    fn parts(&self, parts: &[StrPart], out: &mut String) {
        for p in parts {
            out.push(' ');
            match p {
                StrPart::Lit(l) => quote(self.ast.bytes(*l), out),
                StrPart::Interp(e) => {
                    out.push_str("(interp ");
                    self.expr(*e, out);
                    out.push(')');
                }
            }
        }
    }

    fn attrpath(&self, path: &[AttrElem], out: &mut String) {
        out.push('(');
        for (i, a) in path.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            match a {
                AttrElem::Static(s) => self.sym(*s, out),
                AttrElem::Dynamic(e) => {
                    out.push_str("(dyn ");
                    self.expr(*e, out);
                    out.push(')');
                }
            }
        }
        out.push(')');
    }

    fn binds(&self, binds: &[Bind], out: &mut String) {
        for b in binds {
            out.push(' ');
            match &b.kind {
                BindKind::Value { path, value } => {
                    out.push_str("(= ");
                    self.attrpath(self.ast.elems(*path), out);
                    out.push(' ');
                    self.expr(*value, out);
                    out.push(')');
                }
                BindKind::Inherit { from, names } => {
                    out.push_str("(inherit");
                    if let Some(f) = from {
                        out.push_str(" (from ");
                        self.expr(*f, out);
                        out.push(')');
                    }
                    for (s, _) in self.ast.names(*names) {
                        out.push(' ');
                        self.sym(*s, out);
                    }
                    out.push(')');
                }
            }
        }
    }

    fn expr(&self, id: ExprId, out: &mut String) {
        match self.ast.expr(id) {
            Expr::Int(i) => out.push_str(&i.to_string()),
            Expr::Float(f) => out.push_str(&format!("{f:?}")),
            Expr::Str(parts) => {
                out.push_str("(str");
                self.parts(self.ast.parts(*parts), out);
                out.push(')');
            }
            Expr::Path(p) => {
                out.push_str(match p.kind {
                    PathKind::Relative => "(path rel",
                    PathKind::Absolute => "(path abs",
                    PathKind::Home => "(path home",
                    PathKind::Search => "(path search",
                });
                self.parts(self.ast.parts(p.parts), out);
                out.push(')');
            }
            Expr::Var(s) => self.sym(*s, out),
            Expr::Select {
                subject,
                path,
                default,
            } => {
                out.push_str("(select ");
                self.expr(*subject, out);
                out.push(' ');
                self.attrpath(self.ast.elems(*path), out);
                if let Some(d) = default {
                    out.push_str(" (or ");
                    self.expr(*d, out);
                    out.push(')');
                }
                out.push(')');
            }
            Expr::HasAttr { subject, path } => {
                out.push_str("(? ");
                self.expr(*subject, out);
                out.push(' ');
                self.attrpath(self.ast.elems(*path), out);
                out.push(')');
            }
            Expr::Attrs { rec, binds } => {
                out.push_str(if *rec { "(rec-attrs" } else { "(attrs" });
                self.binds(self.ast.binds(*binds), out);
                out.push(')');
            }
            Expr::List(items) => {
                out.push_str("(list");
                for i in self.ast.ids(*items) {
                    out.push(' ');
                    self.expr(*i, out);
                }
                out.push(')');
            }
            Expr::Lambda { param, body } => {
                out.push_str("(lambda ");
                match param {
                    Param::Ident(s) => self.sym(*s, out),
                    Param::Formals {
                        bind,
                        formals,
                        ellipsis,
                    } => {
                        let formals = self.ast.formals(*formals);
                        out.push('{');
                        for (i, f) in formals.iter().enumerate() {
                            if i > 0 {
                                out.push_str(", ");
                            }
                            self.sym(f.name, out);
                            if let Some(d) = f.default {
                                out.push_str(" ? ");
                                self.expr(d, out);
                            }
                        }
                        if *ellipsis {
                            if !formals.is_empty() {
                                out.push_str(", ");
                            }
                            out.push_str("...");
                        }
                        out.push('}');
                        if let Some(b) = bind {
                            out.push_str(" @ ");
                            self.sym(*b, out);
                        }
                    }
                }
                out.push(' ');
                self.expr(*body, out);
                out.push(')');
            }
            Expr::Apply { func, args } => {
                out.push_str("(apply ");
                self.expr(*func, out);
                for a in self.ast.ids(*args) {
                    out.push(' ');
                    self.expr(*a, out);
                }
                out.push(')');
            }
            Expr::Let { binds, body } => {
                out.push_str("(let (");
                self.binds(self.ast.binds(*binds), out);
                out.push_str(") ");
                self.expr(*body, out);
                out.push(')');
            }
            Expr::With { scope, body } => {
                out.push_str("(with ");
                self.expr(*scope, out);
                out.push(' ');
                self.expr(*body, out);
                out.push(')');
            }
            Expr::If { cond, then, else_ } => {
                out.push_str("(if ");
                self.expr(*cond, out);
                out.push(' ');
                self.expr(*then, out);
                out.push(' ');
                self.expr(*else_, out);
                out.push(')');
            }
            Expr::Assert { cond, body } => {
                out.push_str("(assert ");
                self.expr(*cond, out);
                out.push(' ');
                self.expr(*body, out);
                out.push(')');
            }
            Expr::Unary { op, expr } => {
                out.push_str(match op {
                    UnOp::Not => "(! ",
                    UnOp::Neg => "(neg ",
                });
                self.expr(*expr, out);
                out.push(')');
            }
            Expr::Binary { op, lhs, rhs } => {
                out.push('(');
                out.push_str(match op {
                    BinOp::Eq => "==",
                    BinOp::Ne => "!=",
                    BinOp::Lt => "<",
                    BinOp::Gt => ">",
                    BinOp::Le => "<=",
                    BinOp::Ge => ">=",
                    BinOp::And => "&&",
                    BinOp::Or => "||",
                    BinOp::Impl => "->",
                    BinOp::Update => "//",
                    BinOp::Concat => "++",
                    BinOp::Add => "+",
                    BinOp::Sub => "-",
                    BinOp::Mul => "*",
                    BinOp::Div => "/",
                });
                out.push(' ');
                self.expr(*lhs, out);
                out.push(' ');
                self.expr(*rhs, out);
                out.push(')');
            }
        }
    }
}
