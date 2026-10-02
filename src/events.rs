//! Instrumentation: one JSON object per line, naming what an evaluation did.
//! The `IETS_STATS` counters say how much; these say which path, from where,
//! and how big. `IETS_EVENTS=PATH` opens the stream (`-` is stderr), and a
//! host opens it with [`Events::write_to`] (docs/reference/settings.md).
//!
//! The stream is for a program to read: every record has an `event` name and
//! keeps the fields it was published with. Adding a field is not a change a
//! reader must follow.

use std::io::Write;
use std::sync::{Mutex, OnceLock};

use serde_json::{Value, json};

/// Where one session's events go. `None` is the normal case: nothing asked
/// for them, so no hook builds a record.
#[derive(Default)]
pub struct Events {
    sink: OnceLock<Option<Mutex<Box<dyn Write + Send>>>>,
}

impl Events {
    /// Sends every event to `w`.
    pub fn write_to(&self, w: Box<dyn Write + Send>) {
        let _ = self.sink.set(Some(Mutex::new(w)));
    }

    /// Opens what `IETS_EVENTS` names, if it names anything. A path that
    /// does not open is reported once and the stream stays off, as a
    /// `json-log-path` that does not open is.
    pub fn open(&self, settings: &crate::settings::Settings) {
        let Some(path) = settings.events.as_deref() else {
            return;
        };
        if path == "-" {
            self.write_to(Box::new(std::io::stderr()));
            return;
        }
        match crate::log::open_sink(std::path::Path::new(path)) {
            Ok(w) => self.write_to(w),
            Err(e) => crate::errln!(
                "error (ignored): opening events file \"{path}\": {}",
                crate::error::io_text(&e)
            ),
        }
    }

    /// Whether anything reads the stream. A hook asks first: the record
    /// costs strings a run without `IETS_EVENTS` must not pay for.
    pub fn on(&self) -> bool {
        self.sink.get().is_some_and(Option::is_some)
    }

    /// One `{"event": NAME, ...fields}` line, in one write: a `Display` of
    /// the value straight into the sink costs a syscall per JSON fragment,
    /// which was 4.5% of a nixpkgs evaluation's instructions.
    pub fn emit(&self, name: &str, mut fields: Value) {
        let Some(Some(w)) = self.sink.get() else {
            return;
        };
        if let Some(o) = fields.as_object_mut() {
            o.insert("event".into(), json!(name));
        }
        let mut line = fields.to_string();
        line.push('\n');
        let mut w = w.lock().unwrap_or_else(|e| e.into_inner());
        let _ = w.write_all(line.as_bytes());
    }
}
