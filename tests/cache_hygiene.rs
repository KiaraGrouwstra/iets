//! What the two caches promise when many processes share one directory: no
//! torn entry is ever read, a killed writer leaves the store usable, a
//! corrupt entry or store goes instead of being repaired, and the sweep
//! removes only what the TTL says. Shapes follow nix-tarmac's
//! tests/{crash,concurrent,corrupt,gc}_test.cpp. See docs/explanation/caching.md.

use std::path::{Path, PathBuf};
use std::process::Command;

use ietsp::cachefs;
use ietsp::trace::Obs;
use ietsp::trace::run::Cache;
use ietsp::trace::store::{ENTRY, Opened, Part, Store};
use ietsp::trace::{key::Key, wire};

const KEYS: usize = 4;
const OBS: usize = 400;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ietsp-hygiene-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

fn key_of(i: usize) -> String {
    format!("{i:064x}")
}

/// One writer's read set for `key`: every value is the writer's own byte, so a
/// reader that sees two writers' bytes in one entry has read a torn file.
fn payload(key: usize, writer: u8) -> Vec<u8> {
    let obs: Vec<Obs> = (0..OBS)
        .map(|i| Obs::Env {
            name: format!("IETSP_TEST_{key}_{i}").into_bytes(),
            value: Some(vec![writer; 300]),
        })
        .collect();
    wire::encode_readset(&obs)
}

/// One writer's whole entry. The parts have the writer's byte and a length
/// only that writer uses, so a reader that mixes generations sees it.
fn publish(store: &Store, key: usize, writer: u8) -> bool {
    let readset = payload(key, writer);
    let result = vec![writer; 700 + usize::from(writer) * 137];
    let warnings = vec![writer; 64 + usize::from(writer)];
    store
        .publish(
            &key_of(key),
            &[
                (Part::Warnings, &warnings),
                (Part::Result, &result),
                (Part::Readset, &readset),
            ],
        )
        .unwrap_or(false)
}

/// The writer of a part whose bytes are all its own.
fn writer_of(bytes: &[u8], what: &str) -> u8 {
    assert!(!bytes.is_empty(), "{what} is not empty");
    assert!(
        bytes.iter().all(|b| *b == bytes[0]),
        "one writer per {what}"
    );
    bytes[0]
}

/// `None` when the entry is absent; panics when it is present but incomplete,
/// or when its parts do not all come from one writer.
fn check(store: &Store, key: usize) -> Option<usize> {
    let e = match store.open(&key_of(key)) {
        Opened::None => return None,
        Opened::Bad => panic!("a published entry parses"),
        Opened::Entry(e) => e,
    };
    let bytes = e
        .part(Part::Readset)
        .expect("a published entry has a read set");
    let obs = wire::decode_readset(&bytes).expect("a published entry decodes");
    assert_eq!(obs.len(), OBS, "an entry holds every observation");
    let mut writers = std::collections::HashSet::new();
    for (i, o) in obs.iter().enumerate() {
        let Obs::Env { name, value } = o else {
            panic!("wrong observation kind")
        };
        assert_eq!(name, format!("IETSP_TEST_{key}_{i}").as_bytes());
        let v = value.as_ref().expect("a value");
        assert_eq!(v.len(), 300);
        writers.insert(writer_of(v, "value"));
    }
    // the review's case: a new result beside an old read set, or two
    // generations mixed by a sibling that published in between
    let result = e
        .part(Part::Result)
        .expect("a published entry has a result");
    writers.insert(writer_of(&result, "result"));
    let warnings = e
        .part(Part::Warnings)
        .expect("a published entry has warnings");
    writers.insert(writer_of(&warnings, "warnings"));
    assert_eq!(writers.len(), 1, "one writer per entry, never a mix");
    let w = usize::from(writers.into_iter().next().expect("a writer"));
    assert_eq!(result.len(), 700 + w * 137, "the result of that writer");
    assert_eq!(warnings.len(), 64 + w, "the warnings of that writer");
    Some(obs.len())
}

#[test]
fn concurrent_readers_and_writers_never_see_a_torn_entry() {
    let dir = tmp("concurrent");
    let store = Store::locate(Some(dir.to_str().expect("utf-8 path"))).expect("store");
    assert!(cachefs::prepare(store.root(), cachefs::Shape::Dirs, false));
    std::thread::scope(|s| {
        for w in 0..8u8 {
            let base = dir.clone();
            s.spawn(move || {
                let store = Store::locate(Some(base.to_str().expect("utf-8 path"))).expect("store");
                for round in 0..60 {
                    let key = (round + w as usize) % KEYS;
                    publish(&store, key, w + 1);
                    check(&store, (key + 1) % KEYS);
                }
            });
        }
    });
    for key in 0..KEYS {
        assert!(check(&store, key).is_some(), "every key ends published");
    }
}

/// The child of `a_killed_writer_leaves_the_store_usable`: writes entries as
/// fast as it can until SIGKILL. A no-op in a normal test run.
#[test]
fn crash_writer_child() {
    let Some(dir) = std::env::var_os("IETSP_CRASH_DIR") else {
        return;
    };
    let store = Store::locate(Some(&dir.to_string_lossy())).expect("store");
    for i in 0..usize::MAX {
        let key = i % 64;
        publish(&store, key, u8::try_from(i % 8).expect("a writer") + 1);
    }
}

fn spawn_and_kill(dir: &Path, ms: u64) {
    let mut child = Command::new(std::env::current_exe().expect("this test binary"))
        .args(["crash_writer_child", "--exact", "--nocapture"])
        .env("IETSP_CRASH_DIR", dir)
        .spawn()
        .expect("spawn the writer");
    std::thread::sleep(std::time::Duration::from_millis(ms));
    child.kill().expect("kill the writer");
    let status = child.wait().expect("reap the writer");
    assert!(!status.success(), "the writer died on the signal");
}

#[test]
fn a_killed_writer_leaves_the_store_usable() {
    let dir = tmp("crash");
    let store = Store::locate(Some(dir.to_str().expect("utf-8 path"))).expect("store");
    assert!(cachefs::prepare(store.root(), cachefs::Shape::Dirs, false));
    for round in 0..6 {
        spawn_and_kill(&dir, 20 + round * 10);
    }
    let mut published = 0;
    for key in 0..64 {
        published += usize::from(check(&store, key).is_some());
    }
    assert!(published > 0, "the writer published before it died");
    assert!(
        publish(&store, 1000, 9),
        "the next writer takes the lock a killed one held"
    );
}

fn hit_entry() -> Vec<u8> {
    wire::encode_readset(&[Obs::Env {
        name: b"IETSP_NO_SUCH_VARIABLE".to_vec(),
        value: None,
    }])
}

/// A cache opened as a new process opens one: a lookup holds the generation
/// it opened, so a second lookup of changed bytes needs a new [`Cache`].
fn cached(path: &str) -> Cache {
    Cache::open(Some(path), Key::new("eval"), false).expect("cache")
}

fn put_hit(c: &Cache, readset: &[u8]) {
    assert!(
        c.store
            .publish(&c.key, &[(Part::Readset, readset)])
            .expect("publish")
    );
}

#[test]
fn a_corrupt_entry_is_dropped_and_a_corrupt_store_is_wiped() {
    let dir = tmp("corrupt");
    let path = dir.to_str().expect("utf-8 path");

    let c = cached(path);
    put_hit(&c, &hit_entry());
    assert!(c.lookup(), "a read set that validates is a hit");

    let entry = c.store.entry_dir(&c.key).join(ENTRY);
    let good = std::fs::read(&entry).expect("entry");
    std::fs::write(&entry, &good[..good.len() / 2]).expect("truncate");
    let c = cached(path);
    assert!(!c.lookup(), "a truncated entry misses");
    assert!(
        !c.store.entry_dir(&c.key).exists(),
        "and the entry is dropped, never repaired"
    );

    put_hit(&c, b"garbage");
    let c = cached(path);
    assert!(!c.lookup());
    assert!(!c.store.entry_dir(&c.key).exists());

    std::fs::create_dir_all(c.store.entry_dir(&c.key)).expect("entry dir");
    std::fs::write(&entry, b"not an entry at all").expect("entry");
    let c = cached(path);
    assert!(!c.lookup(), "bytes that are not an entry miss");
    assert!(!c.store.entry_dir(&c.key).exists());

    put_hit(&c, &hit_entry());
    std::fs::write(c.store.root().join("gc-stamp"), b"\x01not a time").expect("stamp");
    let c = cached(path);
    assert!(
        !c.store.entry_dir(&c.key).exists(),
        "an unreadable store-level file wipes the store"
    );
    put_hit(&c, &hit_entry());
    let c = cached(path);
    assert!(c.lookup(), "and the store works again");
}

#[test]
fn cache_gc_removes_entries_past_the_ttl() {
    let dir = tmp("gc");
    let path = dir.to_str().expect("utf-8 path");
    let store = Store::locate(Some(path)).expect("store");
    assert!(cachefs::prepare(store.root(), cachefs::Shape::Dirs, false));
    for key in 0..3 {
        assert!(
            store
                .publish(&key_of(key), &[(Part::Readset, &hit_entry())])
                .expect("publish")
        );
    }
    let ir_key = key_of(0);
    let ir_cache = ietsp::compile::cache::IrCache::locate(Some(path), false).expect("ir cache");
    ir_cache.write(&ir_key, b"ir entry");
    let ir = dir.join("ir").join(&ir_key[..2]);
    assert!(ir.join(&ir_key[2..]).exists(), "an IR entry to sweep");

    let gc = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_ietsp"))
            .args(["cache", "gc", "--cache-dir"])
            .arg(&dir)
            .args(args)
            .env("XDG_CONFIG_HOME", "/nonexistent")
            .output()
            .expect("run ietsp cache gc");
        assert!(out.status.success(), "cache gc exits 0");
        String::from_utf8(out.stdout).expect("utf-8 output")
    };

    let fresh = gc(&["--ttl", "30"]);
    assert!(fresh.contains("0 of 3 entries"), "{fresh}");
    assert!(fresh.contains("0 of 1 entries"), "{fresh}");

    let dry = gc(&["--ttl", "0", "--dry-run"]);
    assert!(
        dry.contains("3 of 3 entries") && dry.contains("(dry run)"),
        "{dry}"
    );
    assert!(
        matches!(store.open(&key_of(0)), Opened::Entry(_)),
        "a dry run keeps everything"
    );

    let done = gc(&["--ttl", "0"]);
    assert!(
        done.contains("3 of 3 entries") && done.contains("1 of 1 entries"),
        "{done}"
    );
    for key in 0..3 {
        assert!(
            matches!(store.open(&key_of(key)), Opened::None),
            "the entry is gone"
        );
    }
    assert!(!ir.join(&ir_key[2..]).exists(), "the IR store is swept too");
    assert!(
        store.root().join("version").exists(),
        "the store stays usable"
    );
}
