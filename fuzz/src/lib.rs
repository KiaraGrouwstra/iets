//! Differential fuzzing of iets against Nix 2.34.8 (docs/how-to/fuzz.md).

pub mod canon;
pub mod known;
pub mod oracle;
pub mod sx;

use iets::syntax::{self, ast::FileId, symbol::Interner};
use oracle::{Oracle, Reply};
use sx::Sx;

pub enum Side {
    Parsed(Sx),
    Error { line: u32, col: u32, msg: String },
}

/// How the two sides differ. The names are the `kind` of a known
/// divergence and of a report.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Nix parses it, iets refuses it.
    NixOnly,
    /// iets parses it, Nix refuses it.
    IetsOnly,
    /// Both refuse it, at different places.
    Position,
    /// Both parse it, to different trees.
    Tree,
    /// The iets parser panicked.
    IetsPanic,
    /// The oracle died: Nix itself crashes on the input.
    NixCrash,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::NixOnly => "nix-only",
            Kind::IetsOnly => "iets-only",
            Kind::Position => "position",
            Kind::Tree => "tree",
            Kind::IetsPanic => "iets-panic",
            Kind::NixCrash => "nix-crash",
        }
    }
}

pub struct Divergence {
    pub kind: Kind,
    pub nix: String,
    pub iets: String,
    /// Where two trees part, as `tag[child]/...`, or empty.
    pub at: String,
}

pub enum Verdict {
    Same,
    Expected(String),
    Bug(Divergence),
}

pub struct Checker {
    oracle: Oracle,
    interner: Interner,
    known: Vec<known::Entry>,
}

impl Checker {
    pub fn new(known: Vec<known::Entry>) -> Checker {
        Checker {
            oracle: Oracle::from_env(),
            interner: Interner::new(),
            known,
        }
    }

    pub fn iets(&mut self, src: &[u8]) -> Result<Side, String> {
        let interner = &mut self.interner;
        let run = std::panic::AssertUnwindSafe(|| match syntax::parse(src, FileId(0), interner) {
            Ok(ast) => Side::Parsed(sx::normalise(canon::tree(&ast, src, interner))),
            Err(e) => {
                let (line, col) = syntax::line_col(src, e.span.start);
                Side::Error {
                    line,
                    col,
                    msg: e.msg,
                }
            }
        });
        std::panic::catch_unwind(run).map_err(|p| {
            p.downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default()
        })
    }

    pub fn nix(&mut self, src: &[u8]) -> Result<Side, String> {
        match self.oracle.ask(src) {
            Reply::Parsed(text) => sx::parse(&text)
                .map(|t| Side::Parsed(sx::normalise(t)))
                .map_err(|e| format!("unreadable oracle tree: {e}")),
            Reply::Error { line, col, msg } => Ok(Side::Error { line, col, msg }),
            Reply::Crashed(why) => Err(why),
        }
    }

    pub fn check(&mut self, src: &[u8]) -> Verdict {
        if self.compare(src).is_none() {
            return Verdict::Same;
        }
        // Both sides keep a symbol table across inputs, and which of two
        // clashing names an error reports follows symbol order. A file is
        // parsed in a fresh process, so a difference counts only when it
        // holds with fresh tables.
        self.oracle.restart();
        self.interner = Interner::new();
        let Some(d) = self.compare(src) else {
            return Verdict::Same;
        };
        match self.known.iter().find(|k| k.matches(&d, src)) {
            Some(k) => Verdict::Expected(k.id.clone()),
            None => Verdict::Bug(d),
        }
    }

    fn compare(&mut self, src: &[u8]) -> Option<Divergence> {
        let nix = match self.nix(src) {
            Ok(n) => n,
            Err(why) => {
                return Some(Divergence {
                    kind: Kind::NixCrash,
                    nix: why,
                    iets: String::new(),
                    at: String::new(),
                });
            }
        };
        let iets = match self.iets(src) {
            Ok(i) => i,
            Err(msg) => {
                return Some(Divergence {
                    kind: Kind::IetsPanic,
                    nix: show(&nix),
                    iets: msg,
                    at: String::new(),
                });
            }
        };
        let (kind, at) = match (&nix, &iets) {
            (Side::Parsed(a), Side::Parsed(b)) => match sx::first_difference(a, b) {
                None => return None,
                Some((at, ..)) => (Kind::Tree, at),
            },
            (
                Side::Error {
                    line: l1, col: c1, ..
                },
                Side::Error {
                    line: l2, col: c2, ..
                },
            ) => {
                if *l1 == 0 || (l1, c1) == (l2, c2) {
                    return None;
                }
                (Kind::Position, String::new())
            }
            (Side::Parsed(_), Side::Error { .. }) => (Kind::NixOnly, String::new()),
            (Side::Error { .. }, Side::Parsed(_)) => (Kind::IetsOnly, String::new()),
        };
        Some(Divergence {
            kind,
            nix: show(&nix),
            iets: show(&iets),
            at,
        })
    }
}

pub fn show(s: &Side) -> String {
    match s {
        Side::Parsed(t) => sx::print(t),
        Side::Error { line, col, msg } => format!("error at {line}:{col}: {msg}"),
    }
}
