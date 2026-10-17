use std::path::Path;
use std::{env, error, fs, io};

use sha2::{Digest, Sha256};

/// Hashes every file under `dir` in path order into `h`, so the id changes with any source
/// edit and stays the same for an unchanged tree (the nix build stays reproducible).
fn hash_tree(h: &mut Sha256, dir: &Path) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            hash_tree(h, &p);
        } else if let Ok(bytes) = fs::read(&p) {
            h.update(p.to_string_lossy().as_bytes());
            h.update((bytes.len() as u64).to_le_bytes());
            h.update(&bytes);
        }
    }
}

/// One `#[test]` per tests/lang/eval-{okay,fail}-*.nix, so nextest runs and
/// reports each case on its own. A build without the tests gets none.
fn lang_cases(out: &Path) -> io::Result<()> {
    println!("cargo:rerun-if-changed=tests/lang");
    let mut stems: Vec<String> = fs::read_dir("tests/lang")
        .into_iter()
        .flatten()
        .filter_map(|e| {
            e.ok()?
                .file_name()
                .into_string()
                .ok()?
                .strip_suffix(".nix")
                .map(String::from)
        })
        .filter(|s| s.starts_with("eval-okay-") || s.starts_with("eval-fail-"))
        .collect();
    stems.sort();
    let code: String = stems
        .iter()
        .map(|stem| {
            let name = stem
                .to_ascii_lowercase()
                .replace(|c: char| !c.is_ascii_alphanumeric(), "_");
            format!("#[test]\nfn {name}() {{\n    run({stem:?});\n}}\n")
        })
        .collect();
    fs::write(out.join("lang_cases.rs"), code)
}

fn main() -> Result<(), Box<dyn error::Error>> {
    lang_cases(Path::new(&env::var("OUT_DIR")?))?;
    let fds = protox::compile(["nix_remote.proto"], ["proto"])?;
    tonic_prost_build::configure()
        .build_server(false)
        .build_client(true)
        // The service has its own `Connect` RPC (the worker-protocol tunnel),
        // whose generated method would clash with the transport constructor.
        .build_transport(false)
        .compile_fds(fds)?;

    for p in ["src", "proto", "Cargo.lock", "build.rs"] {
        println!("cargo:rerun-if-changed={p}");
    }
    let mut h = Sha256::new();
    hash_tree(&mut h, Path::new("src"));
    hash_tree(&mut h, Path::new("proto"));
    for f in ["Cargo.lock", "build.rs"] {
        h.update(fs::read(f).unwrap_or_default());
    }
    let id: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    println!("cargo:rustc-env=IETS_BUILD_ID={id}");
    let version = env::var("CARGO_PKG_VERSION")?;
    println!("cargo:rustc-env=IETS_VERSION=iets {version}");
    Ok(())
}
