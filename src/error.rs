//! The one error type for everything outside the evaluator. `src/eval/error.rs`
//! keeps its own: eval errors carry a source position and a trace.

use std::fmt;
use std::io;

#[derive(Debug)]
pub enum Error {
    Io {
        context: String,
        source: io::Error,
    },
    /// Malformed data from another process: the nix-daemon worker protocol, a
    /// nix-eval-jobs line, `nix path-info`, a dev-env dump.
    Protocol(String),
    Plan(String),
    /// Store path, derivation or hash.
    Store(String),
    Flake(String),
    Cache(String),
    /// Something Nix does that iets does not, such as a flake registry
    /// lookup: classic mode falls back on it (`ErrorKind::Unsupported`).
    Unsupported(String),
    Eval(Box<crate::eval::error::Error>),
    /// Bad command-line input.
    Usage(String),
    /// nix.conf: a syntax error, a missing include, or a bad setting value.
    Config(String),
    /// Another error and the fix for it, which the record shows as `hint`.
    Hint(Box<Error>, String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// `strerror` text without Rust's " (os error N)" suffix, as Nix shows it.
pub fn io_text(e: &io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}

impl Error {
    pub fn io(context: impl Into<String>, source: io::Error) -> Error {
        Error::Io {
            context: context.into(),
            source,
        }
    }

    /// The stable code of docs/reference/errors.md.
    pub fn code(&self) -> &'static str {
        match self {
            Error::Io { .. } => "io",
            Error::Protocol(_) => "protocol",
            Error::Plan(_) => "plan",
            Error::Store(_) => "store",
            Error::Flake(_) => "flake",
            Error::Cache(_) => "cache",
            Error::Unsupported(_) => "unsupported",
            Error::Eval(e) => e.kind.code(),
            Error::Usage(_) => "usage",
            Error::Config(_) => "config",
            Error::Hint(e, _) => e.code(),
        }
    }

    pub fn to_report(&self) -> crate::log::Report {
        let hint = match self {
            Error::Eval(e) => e.hint.clone(),
            Error::Hint(_, h) => Some(h.clone()),
            _ => None,
        };
        crate::log::Report {
            hint,
            ..crate::log::Report::new(self.code(), self.to_string())
        }
    }

    /// `error: MSG` on stderr, or its record; returns the exit code.
    pub fn report(&self) -> i32 {
        self.to_report().emit()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { context, source } if context.is_empty() => f.write_str(&io_text(source)),
            Error::Io { context, source } => write!(f, "{context}: {}", io_text(source)),
            Error::Protocol(m)
            | Error::Plan(m)
            | Error::Store(m)
            | Error::Flake(m)
            | Error::Cache(m)
            | Error::Unsupported(m)
            | Error::Usage(m)
            | Error::Config(m) => f.write_str(m),
            Error::Eval(e) => f.write_str(&e.msg),
            Error::Hint(e, _) => e.fmt(f),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(source: io::Error) -> Error {
        Error::Io {
            context: String::new(),
            source,
        }
    }
}

impl From<Box<crate::eval::error::Error>> for Error {
    fn from(e: Box<crate::eval::error::Error>) -> Error {
        Error::Eval(e)
    }
}

impl From<crate::store::path::StorePathError> for Error {
    fn from(e: crate::store::path::StorePathError) -> Error {
        Error::Store(e.0)
    }
}

impl From<crate::store::drv::DrvError> for Error {
    fn from(e: crate::store::drv::DrvError) -> Error {
        Error::Store(e.0)
    }
}

impl From<crate::store::hash::HashError> for Error {
    fn from(e: crate::store::hash::HashError) -> Error {
        Error::Store(e.0)
    }
}
