//! A long-lived oracle child: one framed request per input, so a comparison
//! costs a pipe round trip and not a process start. Each target defines its
//! frames (`parse.rs`, `nar.rs`).

use std::env;
use std::io::{self, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::process::{Child, ChildStdin, Command, Stdio};

use rustix::io::Errno;
use rustix::net::{RecvFlags, recv};

/// The child's stdout: a socket, not a pipe (`fill`).
pub type Out = UnixStream;

pub struct Oracle {
    program: String,
    env: Option<(&'static str, &'static str)>,
    child: Option<(Child, ChildStdin, Out)>,
}

impl Oracle {
    /// The program `$var` names, else `default` on `PATH`.
    pub fn from_env(var: &str, default: &str) -> Oracle {
        Oracle {
            program: env::var(var).unwrap_or_else(|_| default.into()),
            env: None,
            child: None,
        }
    }

    /// `program` run with the variable `env` set.
    pub fn with_env(program: String, env: (&'static str, &'static str)) -> Oracle {
        Oracle {
            program,
            env: Some(env),
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

    /// Sends `frame` and reads the reply with `read`. An error means the
    /// child died on this input; it is started again for the next one.
    pub fn ask<T>(
        &mut self,
        frame: &[u8],
        read: impl FnOnce(&mut Out) -> io::Result<T>,
    ) -> Result<T, String> {
        let r = self.start().and_then(|()| {
            let (_, stdin, stdout) = self.child.as_mut().unwrap();
            stdin.write_all(frame)?;
            stdin.flush()?;
            read(stdout)
        });
        r.map_err(|e| {
            let status = self.child.take().and_then(|(mut c, ..)| {
                let _ = c.kill();
                c.wait().ok()
            });
            match status {
                Some(s) => format!("{e} ({s})"),
                None => e.to_string(),
            }
        })
    }

    fn start(&mut self) -> io::Result<()> {
        if self.child.is_some() {
            return Ok(());
        }
        let (out, theirs) = UnixStream::pair()?;
        let mut c = Command::new(&self.program)
            .envs(self.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::from(OwnedFd::from(theirs)))
            // warnings: the reply carries what counts
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| io::Error::other(format!("cannot start {}: {e}", self.program)))?;
        let i = c.stdin.take().unwrap();
        self.child = Some((c, i, out));
        Ok(())
    }
}

pub fn u32_le(out: &mut Out) -> io::Result<u32> {
    let mut b = [0u8; 4];
    fill(out, &mut b)?;
    Ok(u32::from_le_bytes(b))
}

pub fn bytes(out: &mut Out, n: usize) -> io::Result<Vec<u8>> {
    let mut b = vec![0u8; n];
    fill(out, &mut b)?;
    Ok(b)
}

/// Reads all of `b` with one `recv` (`MSG_WAITALL`), however the child's
/// writes arrive. Read in parts, as through a pipe, the number of parts
/// varies with timing, and so does the coverage libFuzzer sees: two runs
/// with one seed then fuzz different inputs.
fn fill(out: &mut Out, b: &mut [u8]) -> io::Result<()> {
    let mut at = 0;
    while at < b.len() {
        match recv(&*out, &mut b[at..], RecvFlags::WAITALL) {
            Ok((0, _)) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok((n, _)) => at += n,
            Err(Errno::INTR) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
