//! The terminal and the user behind it.

use std::io;

/// Columns of stderr's terminal, if it has one.
pub fn columns() -> Option<usize> {
    match rustix::termios::tcgetwinsize(io::stderr()) {
        Ok(ws) if ws.ws_col > 1 => Some(usize::from(ws.ws_col)),
        _ => None,
    }
}

pub fn is_root() -> bool {
    rustix::process::getuid().is_root()
}

/// Rust masks SIGPIPE, so a closed stdout surfaces as an EPIPE panic. The
/// default disposition makes the process die quietly instead.
pub fn restore_sigpipe() {
    // SAFETY: SIG_DFL is a valid disposition and no handler is replaced.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}
