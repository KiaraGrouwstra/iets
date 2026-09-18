//! `ietsp cache gc`: sweep both cache stores by entry age.

use std::time::Duration;

use crate::cachefs;

/// `ietsp cache gc [--ttl DAYS] [--dry-run] [--cache-dir DIR]`. Entries unread
/// for longer than the TTL go; a swept tracing entry simply misses next time.
pub fn cmd_cache_gc(argv: &[String]) -> i32 {
    let mut ttl_days = cachefs::TTL_DAYS;
    let mut dry_run = false;
    let mut cache_dir: Option<String> = None;
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--dry-run" | "-n" => dry_run = true,
            "--ttl" => {
                i += 1;
                match argv.get(i).and_then(|v| v.parse().ok()) {
                    Some(d) => ttl_days = d,
                    None => {
                        eprintln!("error: --ttl wants a number of days");
                        return 2;
                    }
                }
            }
            "--cache-dir" => {
                i += 1;
                match argv.get(i) {
                    Some(d) => cache_dir = Some(d.clone()),
                    None => {
                        eprintln!("error: --cache-dir wants a directory");
                        return 2;
                    }
                }
            }
            s => {
                eprintln!("error: unknown flag '{s}'");
                eprintln!("usage: ietsp cache gc [--ttl DAYS] [--dry-run] [--cache-dir DIR]");
                return 2;
            }
        }
        i += 1;
    }
    let stores = cachefs::stores(cache_dir.as_deref());
    if stores.is_empty() {
        eprintln!("error: no cache directory (no HOME and no --cache-dir)");
        return 1;
    }
    let ttl = Duration::from_secs(ttl_days * 24 * 60 * 60);
    for (root, shape) in stores {
        let s = cachefs::sweep(&root, shape, ttl, dry_run);
        println!(
            "{}: {} of {} entries, {:.1} MiB{}",
            root.display(),
            s.removed,
            s.scanned,
            s.bytes as f64 / (1024.0 * 1024.0),
            if dry_run { " (dry run)" } else { " removed" }
        );
    }
    0
}
