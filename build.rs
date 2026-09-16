use std::path::Path;

use sha2::{Digest, Sha256};

/// Hashes every file under `dir` in path order into `h`, so the id changes with any source
/// edit and stays the same for an unchanged tree (the nix build stays reproducible).
fn hash_tree(h: &mut Sha256, dir: &Path) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            hash_tree(h, &p);
        } else if let Ok(bytes) = std::fs::read(&p) {
            h.update(p.to_string_lossy().as_bytes());
            h.update((bytes.len() as u64).to_le_bytes());
            h.update(&bytes);
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/builder.proto");
    let fds = protox::compile(["proto/builder.proto"], ["proto"])?;
    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_fds(fds)?;

    for p in ["src", "proto", "Cargo.lock", "build.rs"] {
        println!("cargo:rerun-if-changed={p}");
    }
    let mut h = Sha256::new();
    hash_tree(&mut h, Path::new("src"));
    hash_tree(&mut h, Path::new("proto"));
    for f in ["Cargo.lock", "build.rs"] {
        h.update(std::fs::read(f).unwrap_or_default());
    }
    let id: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    println!("cargo:rustc-env=IETSP_BUILD_ID={id}");
    Ok(())
}
