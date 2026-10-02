//! Instrumentation: one JSON object per line, naming what an evaluation did.
//! The `IETS_STATS` counters say how much; these say which path, from where,
//! and how big. `IETS_EVENTS=PATH` opens the stream (`-` is stderr), and a
//! host opens it with [`Events::write_to`] (docs/reference/settings.md).
//!
//! The stream is for a program to read: every record has an `event` name and
//! keeps the fields it was published with. Adding a field is not a change a
//! reader must follow.

use std::cell::RefCell;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

thread_local! {
    /// The line a record is built in. A thread keeps it, so a record in a
    /// watched run allocates nothing.
    static LINE: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

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

    /// Whether anything reads the stream.
    pub fn on(&self) -> bool {
        self.sink.get().is_some_and(Option::is_some)
    }

    /// A builder for one `{"event": NAME, …}` line, and `None` when nothing
    /// reads the stream: a call site asks first and builds nothing for a run
    /// that does not watch. [`Rec`] writes the line when it drops.
    pub fn rec<'a>(&'a self, event: &str) -> Option<Rec<'a>> {
        let Some(Some(sink)) = self.sink.get() else {
            return None;
        };
        let mut line = LINE.with(|b| std::mem::take(&mut *b.borrow_mut()));
        line.clear();
        line.extend_from_slice(b"{\"event\":");
        push_str(&mut line, event.as_bytes());
        Some(Rec { sink, line })
    }
}

/// `s` as a JSON string. Invalid UTF-8 is replaced, because a path is bytes
/// and a record is never worth failing a run over.
fn push_str(out: &mut Vec<u8>, s: &[u8]) {
    let lossy = String::from_utf8_lossy(s);
    // the only error is invalid UTF-8, which `lossy` has already replaced
    let _ = crate::print::json_escape(out, lossy.as_bytes());
}

/// How one value appears in a record: it writes a whole JSON value, and
/// nothing else about the record is its business. The types of the domain
/// implement this next to themselves, so this module knows nothing about
/// store paths, source trees or positions.
pub trait Field {
    fn write(self, out: &mut Vec<u8>);
}

impl Field for u64 {
    fn write(self, out: &mut Vec<u8>) {
        push_u64(out, self);
    }
}

impl Field for bool {
    fn write(self, out: &mut Vec<u8>) {
        out.extend_from_slice(if self { b"true" } else { b"false" });
    }
}

impl Field for &str {
    fn write(self, out: &mut Vec<u8>) {
        push_str(out, self.as_bytes());
    }
}

impl Field for &[u8] {
    fn write(self, out: &mut Vec<u8>) {
        push_str(out, self);
    }
}

/// Quotes what a [`Field`] appended from `start`, so a value built out of
/// several pieces needs no buffer of its own. A value with nothing to
/// escape — nearly every path and name — copies nothing and inserts two
/// bytes; one that needs escaping pays a copy.
pub fn quote_in_place(out: &mut Vec<u8>, start: usize) {
    let plain = out[start..]
        .iter()
        .all(|&b| b >= 0x20 && b != b'"' && b != b'\\')
        && std::str::from_utf8(&out[start..]).is_ok();
    if plain {
        out.insert(start, b'"');
        out.push(b'"');
        return;
    }
    let raw = out.split_off(start);
    push_str(out, &raw);
}

/// One record under construction. Each field appends to a line the thread
/// owns; the line reaches the sink in one write when this drops, because a
/// write per fragment cost 6% of a nixpkgs evaluation's instructions.
pub struct Rec<'a> {
    sink: &'a Mutex<Box<dyn Write + Send>>,
    line: Vec<u8>,
}

impl Rec<'_> {
    /// One field. Chains, so a call site reads as the record it writes.
    pub fn set<F: Field>(&mut self, key: &str, value: F) -> &mut Self {
        self.line.push(b',');
        push_str(&mut self.line, key.as_bytes());
        self.line.push(b':');
        value.write(&mut self.line);
        self
    }
}

pub fn push_u64(out: &mut Vec<u8>, mut n: u64) {
    let mut d = [0u8; 20];
    let mut i = d.len();
    loop {
        i -= 1;
        d[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    out.extend_from_slice(&d[i..]);
}

impl Drop for Rec<'_> {
    fn drop(&mut self) {
        self.line.extend_from_slice(b"}\n");
        let mut w = self.sink.lock().unwrap_or_else(|e| e.into_inner());
        let _ = w.write_all(&self.line);
        drop(w);
        let line = std::mem::take(&mut self.line);
        LINE.with(|b| *b.borrow_mut() = line);
    }
}
