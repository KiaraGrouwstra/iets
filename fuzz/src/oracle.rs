//! A long-lived `parse-oracle` child: one framed request per input, so a
//! comparison costs a pipe round trip and not a process start.
//!
//! Request: u32 length (little endian), then the source bytes.
//! Reply: u8 kind (0 parsed, 1 error), u32 line, u32 column, u32 length,
//! then the tree (kind 0) or the error message (kind 1).

use std::io::{BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub enum Reply {
    Parsed(Vec<u8>),
    Error {
        line: u32,
        col: u32,
        msg: String,
    },
    /// The child died on this input; it is started again for the next one.
    Crashed(String),
}

pub struct Oracle {
    program: String,
    child: Option<(Child, ChildStdin, BufReader<ChildStdout>)>,
}

impl Oracle {
    /// `$IETS_PARSE_ORACLE`, else `parse-oracle` on `PATH`.
    pub fn from_env() -> Oracle {
        let program = std::env::var("IETS_PARSE_ORACLE").unwrap_or_else(|_| "parse-oracle".into());
        Oracle {
            program,
            child: None,
        }
    }

    /// Stops the child; the next request starts a fresh one.
    pub fn restart(&mut self) {
        if let Some((mut c, ..)) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    pub fn ask(&mut self, src: &[u8]) -> Reply {
        match self.round_trip(src) {
            Ok(r) => r,
            Err(e) => {
                let status = self.child.take().and_then(|(mut c, ..)| {
                    let _ = c.kill();
                    c.wait().ok()
                });
                Reply::Crashed(match status {
                    Some(s) => format!("{e} ({s})"),
                    None => e.to_string(),
                })
            }
        }
    }

    fn round_trip(&mut self, src: &[u8]) -> std::io::Result<Reply> {
        if self.child.is_none() {
            let mut c = Command::new(&self.program)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                // the cursed-`or` and lint warnings: the reply carries what counts
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| {
                    std::io::Error::other(format!("cannot start {}: {e}", self.program))
                })?;
            let (i, o) = (c.stdin.take().unwrap(), c.stdout.take().unwrap());
            self.child = Some((c, i, BufReader::new(o)));
        }
        let (_, stdin, stdout) = self.child.as_mut().unwrap();
        stdin.write_all(&(src.len() as u32).to_le_bytes())?;
        stdin.write_all(src)?;
        stdin.flush()?;
        let mut head = [0u8; 13];
        stdout.read_exact(&mut head)?;
        let word = |i: usize| u32::from_le_bytes(head[i..i + 4].try_into().unwrap());
        let mut body = vec![0u8; word(9) as usize];
        stdout.read_exact(&mut body)?;
        Ok(match head[0] {
            0 => Reply::Parsed(body),
            _ => Reply::Error {
                line: word(1),
                col: word(5),
                msg: String::from_utf8_lossy(&body).into_owned(),
            },
        })
    }
}
