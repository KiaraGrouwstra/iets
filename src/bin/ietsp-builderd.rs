//! Reference remote builder: `ietsp-builderd --listen ADDR --store-dir DIR`.

use std::path::PathBuf;
use std::process::exit;

fn usage() -> ! {
    eprintln!("usage: ietsp-builderd [--listen ADDR] [--store-dir DIR]");
    exit(2)
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut listen = "127.0.0.1:9455".to_string();
    let mut dir = PathBuf::from("ietsp-builderd-store");
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--listen" => {
                i += 1;
                listen = argv.get(i).cloned().unwrap_or_else(|| usage());
            }
            "--store-dir" => {
                i += 1;
                dir = PathBuf::from(argv.get(i).cloned().unwrap_or_else(|| usage()));
            }
            _ => usage(),
        }
        i += 1;
    }
    let addr = match listen.parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: bad --listen '{listen}': {e}");
            exit(2)
        }
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    if let Err(e) = rt.block_on(ietsp::remote::server::serve(addr, dir)) {
        eprintln!("error: {e}");
        exit(1)
    }
}
