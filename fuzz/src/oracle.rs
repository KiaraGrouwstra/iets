//! A long-lived oracle child: one framed request per input, so a comparison
//! costs a pipe round trip and not a process start. Each target defines its
//! frames (`parse.rs`, `nar.rs`).

use std::io::{BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub type Out = BufReader<ChildStdout>;

pub struct Oracle {
    program: String,
    child: Option<(Child, ChildStdin, Out)>,
}

impl Oracle {
    /// The program `$var` names, else `default` on `PATH`.
    pub fn from_env(var: &str, default: &str) -> Oracle {
        Oracle {
            program: std::env::var(var).unwrap_or_else(|_| default.into()),
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
        read: impl FnOnce(&mut Out) -> std::io::Result<T>,
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

    fn start(&mut self) -> std::io::Result<()> {
        if self.child.is_some() {
            return Ok(());
        }
        let mut c = Command::new(&self.program)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // warnings: the reply carries what counts
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| std::io::Error::other(format!("cannot start {}: {e}", self.program)))?;
        let (i, o) = (c.stdin.take().unwrap(), c.stdout.take().unwrap());
        self.child = Some((c, i, BufReader::new(o)));
        Ok(())
    }
}

pub fn u32_le(out: &mut Out) -> std::io::Result<u32> {
    let mut b = [0u8; 4];
    std::io::Read::read_exact(out, &mut b)?;
    Ok(u32::from_le_bytes(b))
}

pub fn bytes(out: &mut Out, n: usize) -> std::io::Result<Vec<u8>> {
    let mut b = vec![0u8; n];
    std::io::Read::read_exact(out, &mut b)?;
    Ok(b)
}
