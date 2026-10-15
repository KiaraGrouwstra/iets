use std::env;
use std::process::exit;

use iets::cli::run::{classic_name, dispatch};
use iets::sys;

// musl's malloc costs up to 30% (docs/explanation/static-build.md)
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    sys::tty::restore_sigpipe();
    let session = iets::session::Session::new();
    session.log.write_to_stderr();
    let _entered = session.enter();
    let argv: Vec<String> = env::args_os().skip(1).map(iets::argv::encode).collect();
    let classic = env::args_os().next().and_then(|a| classic_name(&a));
    exit(dispatch(&session, argv, classic))
}
