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
    Eval(Box<crate::eval::error::Error>),
    /// Bad command-line input.
    Usage(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn io(context: impl Into<String>, source: io::Error) -> Error {
        Error::Io {
            context: context.into(),
            source,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { context, source } if context.is_empty() => write!(f, "{source}"),
            Error::Io { context, source } => write!(f, "{context}: {source}"),
            Error::Protocol(m)
            | Error::Plan(m)
            | Error::Store(m)
            | Error::Flake(m)
            | Error::Cache(m)
            | Error::Usage(m) => f.write_str(m),
            Error::Eval(e) => f.write_str(&e.msg),
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
