//! Prototype for docs/hash-proposal.md proposal 1: hash a tree's NAR with the
//! filesystem work taken off the hasher's critical path.
//!
//! The NAR is one serial byte stream and its digest has to be Nix's, so the
//! hashing cannot go wide. The syscalls can. On a 54630-file tree the
//! single-threaded dump spends ~290 ms opening and reading files and ~115 ms
//! enumerating directories, against ~76 ms of SHA-256.
//!
//! Four stages, all running at once:
//!
//!  1. **enumerate**: a pool reads directories breadth first from the root and
//!     publishes each listing, sorted the way a NAR demands.
//!  2. **plan**: one thread walks those listings depth first, which is NAR
//!     order, blocking only on a listing the pool has not reached yet. It
//!     emits the events the writer will replay, and groups the files it passes
//!     into runs.
//!  3. **read**: readers fill one slab per run. Run `k` goes to reader
//!     `k % readers`, so the writer needs no lock to take them in order, and
//!     the bounded channel is the backpressure.
//!  4. **write**: replays the events into the NAR writer and the hasher, and
//!     touches no file at all.
//!
//! Handing contents over per file instead of per run costs more than the
//! syscalls it saves, because the buffer is then allocated on one thread and
//! freed on another. Two earlier shapes of this prototype lost the whole win
//! that way.
//!
//! `cargo run --release --example nar_prefetch -- <path> [reps] [readers]`

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{self, Read as _};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Condvar, Mutex};
use std::time::Instant;

use iets::store::hash::{Hash, HashAlgo, Hasher};
use iets::store::nar::{self, TreeWriter};

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

/// A run stops at this many bytes or this many files, whichever comes first,
/// so memory in flight is bounded by `readers * DEPTH * RUN_BYTES`.
fn run_bytes() -> u64 {
    env_usize("RUN_BYTES", 1 << 20) as u64
}
fn run_files() -> usize {
    env_usize("RUN_FILES", 64)
}
fn depth() -> usize {
    env_usize("DEPTH", 3)
}
/// Files over this size are opened by the writer, so one big file neither
/// blows a slab nor waits for a reader.
fn prefetch_max() -> u64 {
    env_usize("PREFETCH_MAX", 1 << 20) as u64
}

struct Entry {
    name: Vec<u8>,
    path: PathBuf,
    /// 0 regular, 1 directory, 2 symlink.
    kind: u8,
    len: u64,
    executable: bool,
}

fn entry_of(name: Vec<u8>, path: PathBuf, md: &fs::Metadata) -> Entry {
    let kind = if md.is_symlink() {
        2
    } else if md.is_dir() {
        1
    } else {
        0
    };
    Entry {
        name,
        path,
        kind,
        len: md.len(),
        executable: md.permissions().mode() & 0o100 != 0,
    }
}

/// One directory's entries, sorted by name bytes, which is the order a NAR
/// demands (archive.cc:dumpPath sorts the same way).
fn listing(dir: &Path) -> io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir)? {
        let e = e?;
        let md = fs::symlink_metadata(e.path())?;
        out.push(entry_of(e.file_name().as_bytes().to_vec(), e.path(), &md));
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Stage 1's output, read by stage 2 as it fills.
struct Dirs {
    state: Mutex<DirState>,
    ready: Condvar,
    work: Condvar,
}

struct DirState {
    queue: VecDeque<PathBuf>,
    done: HashMap<PathBuf, io::Result<Vec<Entry>>>,
    /// Directories handed out but not yet finished.
    running: usize,
    stop: bool,
}

impl Dirs {
    fn push(&self, dir: PathBuf) {
        let mut st = self.state.lock().unwrap();
        st.queue.push_back(dir);
        drop(st);
        self.work.notify_one();
    }

    /// A pool thread: take the next directory, publish its listing, and queue
    /// the subdirectories it found, so the pool runs ahead of the planner.
    fn serve(&self) {
        loop {
            let dir = {
                let mut st = self.state.lock().unwrap();
                loop {
                    if st.stop {
                        return;
                    }
                    if let Some(d) = st.queue.pop_front() {
                        st.running += 1;
                        break d;
                    }
                    if st.running == 0 {
                        return;
                    }
                    st = self.work.wait(st).unwrap();
                }
            };
            let l = listing(&dir);
            let subdirs: Vec<PathBuf> = match &l {
                Ok(es) => es
                    .iter()
                    .filter(|e| e.kind == 1)
                    .map(|e| e.path.clone())
                    .collect(),
                Err(_) => Vec::new(),
            };
            let mut st = self.state.lock().unwrap();
            st.done.insert(dir, l);
            for d in subdirs {
                st.queue.push_back(d);
            }
            st.running -= 1;
            drop(st);
            self.ready.notify_all();
            self.work.notify_all();
        }
    }

    /// Stage 2 waiting for one directory's listing.
    fn take(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        let mut st = self.state.lock().unwrap();
        loop {
            if let Some(l) = st.done.remove(dir) {
                return l;
            }
            st = self.ready.wait(st).unwrap();
        }
    }

    fn stop(&self) {
        let mut st = self.state.lock().unwrap();
        st.stop = true;
        drop(st);
        self.work.notify_all();
    }
}

/// What the writer replays. Everything it needs is here, so it opens nothing
/// except the files too big to prefetch.
enum Event {
    Dir,
    EndDir,
    Entry(Vec<u8>),
    EndEntry,
    Symlink(Vec<u8>),
    /// A file whose bytes arrive through the pipe.
    File {
        executable: bool,
    },
    /// A file the writer opens itself.
    Big {
        executable: bool,
        len: u64,
        path: PathBuf,
    },
}

/// Stage 2's output for the readers: the files in NAR order, grouped into runs.
struct Plan {
    state: Mutex<PlanState>,
    grew: Condvar,
}

struct PlanState {
    files: Vec<(PathBuf, u64)>,
    /// `(first file, count)` per run, in order.
    runs: Vec<(usize, usize)>,
    open_first: usize,
    open_bytes: u64,
    done: bool,
}

impl Plan {
    fn file(&self, path: PathBuf, len: u64, rb: u64, rf: usize) {
        let mut st = self.state.lock().unwrap();
        st.files.push((path, len));
        st.open_bytes += len;
        let n = st.files.len() - st.open_first;
        if n >= rf || st.open_bytes >= rb {
            let first = st.open_first;
            st.runs.push((first, n));
            st.open_first = st.files.len();
            st.open_bytes = 0;
            drop(st);
            self.grew.notify_all();
        }
    }

    /// Close the last partial run and let the readers finish.
    fn finish(&self) {
        let mut st = self.state.lock().unwrap();
        let n = st.files.len() - st.open_first;
        if n > 0 {
            let first = st.open_first;
            st.runs.push((first, n));
            st.open_first = st.files.len();
        }
        st.done = true;
        drop(st);
        self.grew.notify_all();
    }

    /// Run `k`'s files. `None` means there will be no run `k`.
    fn run(&self, k: usize) -> Option<Vec<(PathBuf, u64)>> {
        let mut st = self.state.lock().unwrap();
        loop {
            if k < st.runs.len() {
                let (first, n) = st.runs[k];
                return Some(st.files[first..first + n].to_vec());
            }
            if st.done {
                return None;
            }
            st = self.grew.wait(st).unwrap();
        }
    }
}

/// Stage 2's output for the writer. The events go in batches down their own
/// channel, because one lock and one clone per event costs more than the
/// syscalls this whole pipeline saves. The channel is unbounded on purpose:
/// the writer blocks on file contents, so a bounded one would deadlock
/// against the readers, and the whole stream is only a few events per file.
struct Sink {
    tx: std::sync::mpsc::Sender<Vec<Event>>,
    buf: std::cell::RefCell<Vec<Event>>,
}

impl Sink {
    fn push(&self, e: Event) {
        let mut b = self.buf.borrow_mut();
        b.push(e);
        if b.len() >= 256 {
            let batch = std::mem::take(&mut *b);
            let _ = self.tx.send(batch);
        }
    }

    fn flush(&self) {
        let batch = std::mem::take(&mut *self.buf.borrow_mut());
        if !batch.is_empty() {
            let _ = self.tx.send(batch);
        }
    }
}

/// Stage 2: the depth-first walk, which is NAR order.
fn plan_tree(dirs: &Dirs, plan: &Plan, sink: &Sink, root: &Path) -> io::Result<()> {
    let md = fs::symlink_metadata(root)?;
    let e = entry_of(Vec::new(), root.to_path_buf(), &md);
    let knobs = (run_bytes(), run_files(), prefetch_max());
    plan_entry(dirs, plan, sink, &e, 0, knobs)
}

fn plan_entry(
    dirs: &Dirs,
    plan: &Plan,
    sink: &Sink,
    e: &Entry,
    d: usize,
    knobs: (u64, usize, u64),
) -> io::Result<()> {
    if d > 64 {
        return Err(io::Error::other("tree too deep"));
    }
    let (rb, rf, big) = knobs;
    match e.kind {
        2 => {
            sink.push(Event::Symlink(
                fs::read_link(&e.path)?.as_os_str().as_bytes().to_vec(),
            ));
            Ok(())
        }
        0 if e.len > big => {
            sink.push(Event::Big {
                executable: e.executable,
                len: e.len,
                path: e.path.clone(),
            });
            Ok(())
        }
        0 => {
            sink.push(Event::File {
                executable: e.executable,
            });
            plan.file(e.path.clone(), e.len, rb, rf);
            Ok(())
        }
        _ => {
            let entries = dirs.take(&e.path)?;
            sink.push(Event::Dir);
            for c in &entries {
                sink.push(Event::Entry(c.name.clone()));
                plan_entry(dirs, plan, sink, c, d + 1, knobs)?;
                sink.push(Event::EndEntry);
            }
            sink.push(Event::EndDir);
            Ok(())
        }
    }
}

/// A filled run: one allocation, with each file's slice recorded.
struct Slab {
    bytes: Vec<u8>,
    at: Vec<(usize, usize)>,
}

fn fill(files: &[(PathBuf, u64)]) -> io::Result<Slab> {
    let total: u64 = files.iter().map(|(_, l)| *l).sum();
    let mut bytes = Vec::with_capacity(total as usize);
    let mut at = Vec::with_capacity(files.len());
    for (path, len) in files {
        let off = bytes.len();
        let f = fs::File::open(path)?;
        let got = (&f).take(*len).read_to_end(&mut bytes)?;
        if got as u64 != *len {
            return Err(io::Error::other(format!(
                "file '{}' changed size while being read",
                path.display()
            )));
        }
        at.push((off, got));
    }
    Ok(Slab { bytes, at })
}

struct Pipe {
    rx: Vec<Receiver<io::Result<Slab>>>,
    run: std::cell::Cell<usize>,
    held: std::cell::RefCell<Option<(Slab, usize)>>,
}

impl Pipe {
    /// Calls `f` with the next file's bytes, in NAR order. Nothing is copied
    /// or allocated per file: the bytes stay in the reader's slab.
    fn with_next<R>(&self, f: impl FnOnce(&[u8]) -> R) -> io::Result<R> {
        let mut held = self.held.borrow_mut();
        if held.is_none() {
            let k = self.run.get();
            self.run.set(k + 1);
            let slab = self.rx[k % self.rx.len()]
                .recv()
                .map_err(|_| io::Error::other("a reader stopped"))??;
            *held = Some((slab, 0));
        }
        let (slab, i) = held.as_mut().unwrap();
        let (off, len) = slab.at[*i];
        *i += 1;
        let out = f(&slab.bytes[off..off + len]);
        if *i == slab.at.len() {
            *held = None;
        }
        Ok(out)
    }
}

/// Stage 4: replay the events. This thread opens nothing but the files too big
/// to prefetch.
fn write_all(
    w: &mut TreeWriter<'_>,
    rx: &std::sync::mpsc::Receiver<Vec<Event>>,
    pipe: &Pipe,
) -> io::Result<()> {
    while let Ok(batch) = rx.recv() {
        for ev in batch {
            match ev {
                Event::Dir => w.dir()?,
                Event::EndDir => w.end_dir()?,
                Event::Entry(name) => w.entry(&name)?,
                Event::EndEntry => w.end_entry()?,
                Event::Symlink(t) => w.symlink(&t)?,
                Event::File { executable } => {
                    pipe.with_next(|b| w.regular(executable, b.len() as u64, &mut &b[..]))??
                }
                Event::Big {
                    executable,
                    len,
                    path,
                } => {
                    let mut f = fs::File::open(path)?;
                    w.regular(executable, len, &mut f)?
                }
            }
        }
    }
    Ok(())
}

fn nar_hash_prefetched(path: &Path, readers: usize) -> io::Result<(Hash, u64)> {
    let dirs = Dirs {
        state: Mutex::new(DirState {
            queue: VecDeque::new(),
            done: HashMap::new(),
            running: 0,
            stop: false,
        }),
        ready: Condvar::new(),
        work: Condvar::new(),
    };
    let plan = Plan {
        state: Mutex::new(PlanState {
            files: Vec::new(),
            runs: Vec::new(),
            open_first: 0,
            open_bytes: 0,
            done: false,
        }),
        grew: Condvar::new(),
    };
    if fs::symlink_metadata(path)?.is_dir() {
        dirs.push(path.to_path_buf());
    }
    let (ev_tx, ev_rx) = std::sync::mpsc::channel::<Vec<Event>>();
    let mut h = Hasher::new(HashAlgo::Sha256);
    let mut txs: Vec<SyncSender<io::Result<Slab>>> = Vec::with_capacity(readers);
    let mut rxs = Vec::with_capacity(readers);
    for _ in 0..readers {
        let (tx, rx) = sync_channel(depth());
        txs.push(tx);
        rxs.push(rx);
    }
    let pipe = Pipe {
        rx: rxs,
        run: std::cell::Cell::new(0),
        held: std::cell::RefCell::new(None),
    };
    let (dirs, plan) = (&dirs, &plan);
    let failed: Mutex<Option<io::Error>> = Mutex::new(None);
    let failed = &failed;
    let out = std::thread::scope(|s| {
        for _ in 0..readers {
            s.spawn(|| dirs.serve());
        }
        s.spawn(move || {
            let sink = Sink {
                tx: ev_tx,
                buf: std::cell::RefCell::new(Vec::with_capacity(256)),
            };
            if let Err(e) = plan_tree(dirs, plan, &sink, path) {
                *failed.lock().unwrap() = Some(e);
            }
            sink.flush();
            plan.finish();
            dirs.stop();
        });
        for (r, tx) in txs.into_iter().enumerate() {
            s.spawn(move || {
                let mut k = r;
                while let Some(files) = plan.run(k) {
                    if tx.send(fill(&files)).is_err() {
                        return;
                    }
                    k += readers;
                }
            });
        }
        let mut w = TreeWriter::new(&mut h);
        w.header()?;
        write_all(&mut w, &ev_rx, &pipe)?;
        w.flush()
    });
    out?;
    if let Some(e) = failed.lock().unwrap().take() {
        return Err(e);
    }
    let size = h.bytes_written();
    Ok((h.finish(), size))
}
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("nar_prefetch: <path> [reps] [readers]");
        std::process::exit(2);
    }
    let path = Path::new(&args[0]);
    let reps: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3);
    let readers: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(16);

    let (want, want_size) = nar::nar_hash_filtered(path, HashAlgo::Sha256, &mut |_, _| true)
        .unwrap_or_else(|e| panic!("serial: {e}"));
    let (got, got_size) = nar_hash_prefetched(path, readers).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(got, want, "digest");
    assert_eq!(got_size, want_size, "nar size");
    println!("# {} {want_size} readers={readers}", want.to_sri());

    let bench = |name: &str, mut f: Box<dyn FnMut()>| {
        let mut best = f64::INFINITY;
        for _ in 0..reps {
            let t = Instant::now();
            f();
            best = best.min(t.elapsed().as_secs_f64());
        }
        println!(
            "{name} {reps} {:.1} {:.0}",
            best * 1e3,
            want_size as f64 / best / 1e6
        );
    };
    bench(
        "serial",
        Box::new(|| {
            nar::nar_hash_filtered(path, HashAlgo::Sha256, &mut |_, _| true).unwrap();
        }),
    );
    bench(
        "hash_only",
        Box::new(|| {
            let mut h = Hasher::new(HashAlgo::Sha256);
            let buf = vec![0u8; 1 << 20];
            let mut left = want_size;
            while left > 0 {
                let n = left.min(1 << 20) as usize;
                io::Write::write_all(&mut h, &buf[..n]).unwrap();
                left -= n as u64;
            }
            h.finish();
        }),
    );
    bench(
        "prefetched",
        Box::new(|| {
            nar_hash_prefetched(path, readers).unwrap();
        }),
    );
}
