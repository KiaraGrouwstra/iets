use crate::syntax::ast::FileId;

/// A source position: file plus byte offset. Line and column are derived on
/// demand from the file's line table.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Pos {
    pub file: FileId,
    pub offset: u32,
}

impl Pos {
    pub const NONE: Pos = Pos {
        file: FileId(u32::MAX),
        offset: 0,
    };

    #[inline]
    pub fn is_none(self) -> bool {
        self.file.0 == u32::MAX
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrorKind {
    Eval,
    Type,
    /// `builtins.throw`. Caught by `tryEval`.
    Throw,
    /// Failed `assert`. Caught by `tryEval`.
    Assert,
    /// `builtins.abort`. Never caught.
    Abort,
    UndefinedVar,
    InfiniteRecursion,
    StackOverflow,
    Parse,
    MissingAttr,
    Io,
    /// A parallel helper fiber was told to stop. Never caught.
    Cancelled,
}

#[derive(Debug)]
pub struct Error {
    pub kind: ErrorKind,
    pub msg: String,
    pub pos: Pos,
    pub trace: Vec<(Pos, String)>,
}

impl Error {
    pub fn new(kind: ErrorKind, msg: String, pos: Pos) -> Error {
        Error {
            kind,
            msg,
            pos,
            trace: Vec::new(),
        }
    }

    pub fn boxed(kind: ErrorKind, msg: impl Into<String>, pos: Pos) -> Box<Error> {
        Box::new(Error::new(kind, msg.into(), pos))
    }

    pub fn add_trace(&mut self, pos: Pos, msg: impl Into<String>) {
        self.trace.push((pos, msg.into()));
    }

    /// Errors that `tryEval` turns into `{ success = false; }`.
    pub fn is_catchable(&self) -> bool {
        matches!(self.kind, ErrorKind::Throw | ErrorKind::Assert)
    }
}

pub type EvalResult<T> = Result<T, Box<Error>>;
