//! Reference side of the ietsp differential test: ingest PATH with the
//! chunking parameters our amber deployment uses and print every object.
//!
//! Output, sorted by key hex, one object per line:
//!   <key hex> <type> <key length> [<object bytes hex>]
//! The bytes are printed for every non-Blob object. The last line is
//! `root <key hex>`.
//!
//! usage: cargo run --release --example amber-keys -- PATH

use std::collections::BTreeMap;
use std::io::Write;

use amber_store_core::chunkers::ByteOpts;
use amber_store_core::ingest::{self, ChunkOpts, DEFAULT_ITEM_BITS, Opts};
use amber_store_core::key::Type;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: amber-keys PATH");
    let opts = Opts {
        jobs: 0,
        chunk: ChunkOpts {
            byte: Some(ByteOpts {
                min_size: 8 * 1024,
                normal_size: 32 * 1024,
                max_size: 64 * 1024,
                key: Vec::new(),
            }),
            item_bits: DEFAULT_ITEM_BITS,
            xattr_inline_max: 0,
        },
        no_ignore: true,
        progress: None,
    };
    let (stream, root) = ingest::objects(&path, opts).expect("ingest");
    let mut objects = BTreeMap::new();
    for r in stream {
        let o = r.expect("object");
        objects.insert(o.key, o.bytes);
    }
    let root = root.get().expect("root");
    let out = std::io::stdout();
    let mut out = std::io::BufWriter::new(out.lock());
    for (k, bytes) in &objects {
        if k.type_() == Type::Blob {
            writeln!(out, "{k} Blob {}", k.length()).unwrap();
        } else {
            writeln!(out, "{k} {} {} {}", k.type_(), k.length(), hex(bytes)).unwrap();
        }
    }
    writeln!(out, "root {root}").unwrap();
}
