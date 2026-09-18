//! What the daemon reports while it builds, decoded from the `STDERR_*` stream.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Started(Started),
    Log(Log),
    Built(Built),
    Failed(Failed),
    Done(Done),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    pub drv_path: String,
}

/// `drv_path` is empty when the daemon did not attribute the line to a build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Log {
    pub drv_path: String,
    pub line: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Built {
    pub drv_path: String,
    pub output: String,
    pub store_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failed {
    pub drv_path: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub success: bool,
}
