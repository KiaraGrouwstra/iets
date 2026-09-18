//! `ietsp cache-dump`: a tracing-cache read set as text.

/// One line per observation of a `readset` file, for debugging misses.
pub fn cmd_cache_dump(file: &str) -> i32 {
    use crate::trace::Obs;
    let Ok(bytes) = std::fs::read(file) else {
        eprintln!("error: cannot read {file}");
        return 1;
    };
    let Some(obs) = crate::trace::wire::decode_readset(&bytes) else {
        eprintln!("error: {file} is not a readset");
        return 1;
    };
    let hex = |d: &[u8]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let s = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    for o in &obs {
        match o {
            Obs::File {
                path, size, hash, ..
            } => println!("file {} {size} {}", s(path), &hex(hash)[..16]),
            Obs::Dir { path, hash } => println!("dir {} {}", s(path), &hex(hash)[..16]),
            Obs::Stat { path, follow, kind } => {
                println!("stat {} follow={follow} {kind:?}", s(path))
            }
            Obs::Tree { root, filtered, .. } => println!(
                "tree {} accepted={}",
                s(root),
                filtered
                    .as_ref()
                    .map_or("all".to_string(), |f| f.accepted.len().to_string())
            ),
            Obs::Canon { path, target } => println!(
                "canon {} -> {}",
                s(path),
                target.as_deref().map_or("(none)".to_string(), s)
            ),
            Obs::Env { name, value } => println!(
                "env {}={}",
                s(name),
                value.as_deref().map_or("(unset)".to_string(), s)
            ),
            Obs::Cmd { dir, argv, hash } => println!(
                "cmd {} `{}` {}",
                s(dir),
                argv.iter().map(|a| s(a)).collect::<Vec<_>>().join(" "),
                &hex(hash)[..16]
            ),
        }
    }
    0
}
