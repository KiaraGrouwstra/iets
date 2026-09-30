//! `iets prefetch`: the hash of what a URL serves, or of what a fixed-output
//! derivation produces, on stdout (docs/reference/commands.md). It replaces nurl,
//! nix-prefetch and nix-prefetch-url for the common cases. Nothing is written
//! to a store and no file is edited: the hash is the whole answer. The
//! operation is `plumbing fetch`; what is here is the `--json` choice.

use crate::cli::plumbing;

pub const HELP: &str = "\
usage: iets prefetch [FLAGS] URL
       iets prefetch [FLAGS] (-E EXPR | FILE -A ATTR | FLAKEREF#ATTR)

Print the SRI hash of what URL serves, or the hash a fixed-output
derivation produces, so nothing has to be edited and built to learn it
('iets help commands'). Nothing is added to the store.

  --unpack              hash the NAR of the unpacked tarball (fetchzip)
  --hash-type ALGO      md5, sha1, sha256 (default) or sha512
  --json                print {\"hash\", \"url\"} or {\"hash\", \"drvPath\"}
";

pub fn cmd_prefetch(argv: &[String]) -> i32 {
    let mut json = false;
    let argv: Vec<String> = (argv.iter())
        .filter(|s| {
            let is = s.as_str() == "--json";
            json |= is;
            !is
        })
        .cloned()
        .collect();
    let (unpack, algo, rest) = plumbing::fetch_flags(&argv);
    plumbing::fetch(unpack, algo, rest, json)
}
