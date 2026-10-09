//! The wire target: iets's daemon client against Nix's, both talking to one
//! scripted daemon.
//!
//! The input chooses a script: the daemon's side of the handshake at any
//! protocol version, stderr frames (log lines, activities, results, errors)
//! after `SetOptions` and after the operation, then the operation's reply,
//! with wrong store paths, hashes, signatures and content addresses now and
//! then. A thread serves the script on a Unix socket to each connection and
//! reads whatever the client sends. iets's `Connection` and Nix's
//! `RemoteStore` (`wire-oracle`) each connect and run the same operation; the
//! two answers must be the same line (nix-conformance's SPEC.md).
//!
//! Oracle frames (`wire-oracle`): request u32 length and the line
//! `SOCKET OP ARG...`, reply u32 length and the answer.

use crate::oracle::{self, Oracle};
use crate::{Divergence, Target, Verdict, known, no_panic};
use iets::remote::worker::{Connection, Options, Transport};
use libfuzzer_sys::arbitrary::Unstructured;
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub const KINDS: &[&str] = &[
    // Nix's client accepts the reply, iets's refuses it
    "nix-only",
    // iets's client accepts the reply, Nix's refuses it
    "iets-only",
    // both accept it, with different answers
    "answer",
    "iets-panic",
    // the oracle died: Nix itself crashes on the reply
    "nix-crash",
];

const MAGIC_2: u64 = 0x6478_696f;
const NEXT: u64 = 0x6f6c_6d67;
const READ: u64 = 0x6461_7461;
const LAST: u64 = 0x616c_7473;
const ERROR: u64 = 0x6378_7470;
const START: u64 = 0x5354_5254;
const STOP: u64 = 0x5354_4f50;
const RESULT: u64 = 0x5253_4c54;

/// Valid store paths for the arguments and the replies.
const PATHS: [&str; 4] = [
    "/nix/store/0c6ld7f4z8xbwgaz1mqk5c5bkwlhypzb-hello-2.12.3",
    "/nix/store/1x2kbk4wpqvw3gwhxc5fy0xyhrbqk46n-glibc-2.42",
    "/nix/store/7q6r5bxprr5bzxqyhjmzjvcg9c0sfbyk-a.drv",
    "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-x",
];
const HASH: &str = "4ab29c33e1fd9f3e3e1ebf6e0da0e8f8c3a5e3a73c92c4e9d6e8c04b1a5f2b3d";

pub struct Wire {
    oracle: Oracle,
    known: Vec<known::Entry>,
    dir: PathBuf,
    seq: u64,
}

/// One operation and its arguments, as both clients run it.
struct Op {
    name: &'static str,
    args: Vec<String>,
}

impl Wire {
    pub fn new(known: Vec<known::Entry>) -> Wire {
        let dir = std::env::temp_dir().join(format!("iets-fuzz-wire-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a scratch directory for the wire target");
        Wire {
            oracle: Oracle::from_env("IETS_WIRE_ORACLE", "wire-oracle"),
            known,
            dir,
            seq: 0,
        }
    }

    fn compare(&mut self, input: &[u8]) -> Option<Divergence> {
        let (op, script) = generate(&mut Unstructured::new(input));
        // the daemon's bytes, to replay a case against a client by hand
        if let Ok(f) = std::env::var("IETS_WIRE_SCRIPT") {
            let _ = std::fs::write(f, &script);
        }
        self.seq += 1;
        let sock = self.dir.join(format!("{}.sock", self.seq));
        let stop = serve(&sock, script);
        let iets = no_panic(|| run_iets(&sock, &op));
        let mut line = format!("{} {}", sock.display(), op.name);
        for a in &op.args {
            line.push(' ');
            line.push_str(a);
        }
        let mut frame = (line.len() as u32).to_le_bytes().to_vec();
        frame.extend_from_slice(line.as_bytes());
        let nix = self.oracle.ask(&frame, |out| {
            let n = oracle::u32_le(out)? as usize;
            Ok(String::from_utf8_lossy(&oracle::bytes(out, n)?).into_owned())
        });
        stop.store(true, Ordering::SeqCst);
        let _ = std::fs::remove_file(&sock);
        let d = |kind, nix: String, iets: String| {
            Some(Divergence {
                kind,
                nix,
                iets,
                at: std::iter::once(op.name.to_string())
                    .chain(op.args.iter().cloned())
                    .collect::<Vec<_>>()
                    .join(" "),
            })
        };
        let nix = match nix {
            Ok(n) => n,
            Err(why) => return d("nix-crash", why, String::new()),
        };
        let iets = match iets {
            Ok(i) => i,
            Err(msg) => return d("iets-panic", nix, msg),
        };
        match (nix == "error", iets == "error") {
            _ if nix == iets => None,
            (false, true) => d("nix-only", nix, iets),
            (true, false) => d("iets-only", nix, iets),
            _ => d("answer", nix, iets),
        }
    }
}

impl Target for Wire {
    fn check(&mut self, input: &[u8]) -> Verdict {
        let d = self.compare(input);
        crate::classify(&self.known, d, input)
    }
}

impl Drop for Wire {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Serves `script` to every connection until `stop`: write it, close the
/// write side, read until the client goes.
fn serve(sock: &Path, script: Vec<u8>) -> Arc<AtomicBool> {
    let listener = UnixListener::bind(sock).expect("a socket for the scripted daemon");
    listener
        .set_nonblocking(true)
        .expect("a non-blocking listener");
    let stop = Arc::new(AtomicBool::new(false));
    let done = stop.clone();
    let script = Arc::new(script);
    std::thread::spawn(move || {
        while !done.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut s, _)) => {
                    let script = script.clone();
                    std::thread::spawn(move || {
                        let _ = s.set_nonblocking(false);
                        let _ = s.write_all(&script);
                        let _ = s.shutdown(std::net::Shutdown::Write);
                        let mut sink = [0u8; 4096];
                        while matches!(s.read(&mut sink), Ok(n) if n > 0) {}
                    });
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_micros(200)),
            }
        }
    });
    stop
}

fn run_iets(sock: &Path, op: &Op) -> String {
    let answer = || -> iets::error::Result<String> {
        let mut quiet = |_| {};
        let mut c = Connection::open_with(
            &Transport::Unix(sock.to_path_buf()),
            &Options::default(),
            &mut quiet,
        )?;
        let args = &op.args;
        Ok(match op.name {
            "connect" => format!(
                "ok minor={} version={} trusted={}",
                c.minor,
                c.daemon_nix_version,
                match c.trusted {
                    Some(true) => "yes",
                    Some(false) => "no",
                    None => "unknown",
                }
            ),
            "path-info" => match c.query_path_info(&args[0], &mut quiet)? {
                None => "none".into(),
                Some(i) => format!(
                    "info deriver={} nar={} refs={} time={} size={} ultimate={} sigs={} ca={}",
                    i.deriver,
                    i.nar_hash,
                    set(i.references),
                    i.registration_time,
                    i.nar_size,
                    u8::from(i.ultimate),
                    set(i.sigs),
                    i.ca
                ),
            },
            "valid-paths" => format!("paths {}", set(c.query_valid_paths(args, &mut quiet)?)),
            "missing" => {
                let m = c.query_missing(args, &mut quiet)?;
                format!(
                    "missing build={} subst={} unknown={} download={} nar={}",
                    set(m.will_build),
                    set(m.will_substitute),
                    set(m.unknown),
                    m.download_size,
                    m.nar_size
                )
            }
            "output-map" => {
                let m = c.query_derivation_output_map(&args[0], &mut quiet)?;
                let v = m
                    .into_iter()
                    .map(|(k, p)| format!("{k}={}", p.unwrap_or_else(|| "-".into())))
                    .collect();
                format!("outputs {}", set(v))
            }
            "substitutable" => format!(
                "subst {}",
                set(c.query_substitutable_path_infos(args, &mut quiet)?)
            ),
            _ => "unsupported".into(),
        })
    };
    answer().unwrap_or_else(|_| "error".into())
}

/// A set on Nix's side: sorted and without duplicates.
fn set(mut v: Vec<String>) -> String {
    v.sort();
    v.dedup();
    v.join(",")
}

struct W(Vec<u8>);

impl W {
    fn u64(&mut self, n: u64) {
        self.0.extend_from_slice(&n.to_le_bytes());
    }
    fn str(&mut self, s: &[u8]) {
        self.u64(s.len() as u64);
        self.0.extend_from_slice(s);
        self.0.resize(self.0.len().next_multiple_of(8), 0);
    }
    fn strs(&mut self, v: &[Vec<u8>]) {
        self.u64(v.len() as u64);
        for s in v {
            self.str(s);
        }
    }
}

fn r<T>(x: Result<T, libfuzzer_sys::arbitrary::Error>, d: T) -> T {
    x.unwrap_or(d)
}

/// The operation and the daemon's whole side of the conversation.
fn generate(u: &mut Unstructured) -> (Op, Vec<u8>) {
    let pick = |u: &mut Unstructured| PATHS[r(u.choose_index(PATHS.len()), 0)].to_string();
    let op = match r(u.int_in_range(0u8..=5), 0) {
        0 => Op {
            name: "connect",
            args: vec![],
        },
        1 => Op {
            name: "path-info",
            args: vec![pick(u)],
        },
        2 => Op {
            name: "valid-paths",
            args: vec![pick(u), pick(u)],
        },
        3 => Op {
            name: "missing",
            args: vec![format!("{}!out", PATHS[2])],
        },
        4 => Op {
            name: "output-map",
            args: vec![PATHS[2].to_string()],
        },
        _ => Op {
            name: "substitutable",
            args: vec![pick(u)],
        },
    };
    let mut w = W(Vec::new());
    w.u64(if r(u.ratio(1u8, 64), false) {
        0
    } else {
        MAGIC_2
    });
    let major = if r(u.ratio(1u8, 64), false) { 2 } else { 1 };
    // iets speaks 1.35 and newer only, on purpose (docs/reference/divergences.md)
    let minor: u64 = if r(u.ratio(1u8, 4), false) {
        r(u.int_in_range(35..=40), 38)
    } else {
        38
    };
    w.u64((major << 8) | minor);
    let proto = minor.min(38);
    if proto >= 38 {
        let n = r(u.int_in_range(0u8..=2), 0);
        let feats: Vec<Vec<u8>> = (0..n).map(|_| bytes(u, b"f")).collect();
        w.strs(&feats);
    }
    if proto >= 33 {
        w.str(&bytes(u, b"2.34.8"));
    }
    if proto >= 35 {
        w.u64(r(u.choose(&[0, 1, 2, 3]), &0).to_owned());
    }
    // after the handshake (remote-store.cc:initConnection), after SetOptions,
    // then after the operation
    stderr(u, &mut w, proto);
    stderr(u, &mut w, proto);
    if op.name != "connect" {
        stderr(u, &mut w, proto);
        reply(u, &mut w, &op);
    }
    if r(u.ratio(1u8, 16), false) {
        let cut = r(u.int_in_range(0..=w.0.len()), w.0.len());
        w.0.truncate(cut);
    }
    (op, w.0)
}

fn bytes(u: &mut Unstructured, default: &[u8]) -> Vec<u8> {
    if r(u.ratio(3u8, 4), true) {
        default.to_vec()
    } else {
        r(u.arbitrary::<&[u8]>(), default).to_vec()
    }
}

/// A store path, now and then a wrong one.
fn path(u: &mut Unstructured) -> Vec<u8> {
    match r(u.int_in_range(0u8..=15), 0) {
        0 => b"/nix/store/not-a-store-path".to_vec(),
        1 => b"/etc/passwd".to_vec(),
        // never empty: an empty store path makes Nix's canonPath assert (nix-conformance
        // nix-bugs/); an empty deriver is "none" and is made by the caller
        2 => match r(u.arbitrary::<&[u8]>(), b"") {
            b"" => b"x".to_vec(),
            b => b.to_vec(),
        },
        _ => PATHS[r(u.choose_index(PATHS.len()), 0)].as_bytes().to_vec(),
    }
}

fn paths(u: &mut Unstructured) -> Vec<Vec<u8>> {
    (0..r(u.int_in_range(0u8..=3), 0))
        .map(|_| path(u))
        .collect()
}

/// Log lines, activities and results, ended by `LAST` or by an error.
fn stderr(u: &mut Unstructured, w: &mut W, proto: u64) {
    for _ in 0..r(u.int_in_range(0u8..=4), 0) {
        match r(u.int_in_range(0u8..=5), 0) {
            0 => {
                w.u64(NEXT);
                w.str(&bytes(u, b"building\n"));
            }
            1 => {
                w.u64(START);
                w.u64(r(u.int_in_range(1u64..=3), 1));
                w.u64(3);
                w.u64(r(u.choose(&[100, 105, 0, 999]), &105).to_owned());
                w.str(&bytes(u, b"building '/nix/store/x.drv'"));
                fields(u, w, 0);
                w.u64(r(u.int_in_range(0u64..=2), 0));
            }
            2 => {
                w.u64(STOP);
                w.u64(r(u.int_in_range(1u64..=3), 1));
            }
            3 => {
                w.u64(RESULT);
                w.u64(r(u.int_in_range(1u64..=3), 1));
                let ty = r(u.choose(&[101, 104, 107, 0]), &101).to_owned();
                w.u64(ty);
                // Nix's SimpleLogger takes the first field of a log line
                // unchecked and crashes without one (nix-conformance nix-bugs/)
                fields(u, w, if matches!(ty, 101 | 107) { 1 } else { 0 });
            }
            4 => {
                w.u64(READ);
                w.u64(8);
            }
            _ => w.u64(r(u.arbitrary::<u64>(), 0)),
        }
    }
    if r(u.ratio(1u8, 8), false) {
        w.u64(ERROR);
        if proto >= 26 {
            // any other type makes Nix's readError assert (nix-conformance nix-bugs/)
            w.str(b"Error");
            w.u64(0);
            w.str(b"Error");
            w.str(&bytes(u, b"path '/nix/store/x' is not valid"));
            w.u64(0);
            let n = r(u.int_in_range(0u8..=2), 0);
            w.u64(n.into());
            for _ in 0..n {
                w.u64(0);
                w.str(&bytes(u, b"while doing it"));
            }
        } else {
            w.str(&bytes(u, b"failed"));
            w.u64(1);
        }
    } else {
        w.u64(LAST);
    }
}

fn fields(u: &mut Unstructured, w: &mut W, min: u8) {
    let n = r(u.int_in_range(min..=3), min);
    w.u64(n.into());
    for _ in 0..n {
        match r(u.int_in_range(0u8..=8), 0) {
            0 => w.u64(7),
            1..=4 => {
                w.u64(0);
                w.u64(r(u.arbitrary::<u64>(), 0));
            }
            _ => {
                w.u64(1);
                w.str(&bytes(u, b"/nix/store/x.drv"));
            }
        }
    }
}

fn reply(u: &mut Unstructured, w: &mut W, op: &Op) {
    match op.name {
        "path-info" => {
            w.u64(r(u.choose(&[1, 1, 1, 0, 2]), &1).to_owned());
            w.str(&if r(u.ratio(1u8, 3), false) {
                path(u)
            } else {
                Vec::new()
            });
            let nar: Vec<u8> = match r(u.int_in_range(0u8..=7), 0) {
                0 => format!("sha256:{HASH}").into_bytes(),
                1 => b"1b8m03r63zqhnjf7l5wnldhh7c134ap5vpj0850ymkq1iyzicy5s".to_vec(),
                2 => r(u.arbitrary::<&[u8]>(), b"").to_vec(),
                _ => HASH.as_bytes().to_vec(),
            };
            w.str(&nar);
            w.strs(&paths(u));
            w.u64(r(u.arbitrary::<u64>(), 0));
            w.u64(r(u.arbitrary::<u64>(), 0));
            w.u64(r(u.choose(&[0, 1, 2]), &0).to_owned());
            let sigs: Vec<Vec<u8>> = (0..r(u.int_in_range(0u8..=2), 0))
                .map(|_| match r(u.int_in_range(0u8..=3), 0) {
                    0 => b"no-colon".to_vec(),
                    1 => r(u.arbitrary::<&[u8]>(), b"").to_vec(),
                    _ => b"cache.nixos.org-1:dGVzdA==".to_vec(),
                })
                .collect();
            w.strs(&sigs);
            let ca: Vec<u8> = match r(u.int_in_range(0u8..=5), 0) {
                0 => format!("fixed:r:sha256:{HASH}").into_bytes(),
                1 => format!("text:sha256:{HASH}").into_bytes(),
                2 => r(u.arbitrary::<&[u8]>(), b"").to_vec(),
                _ => Vec::new(),
            };
            w.str(&ca);
        }
        "valid-paths" => w.strs(&paths(u)),
        "missing" => {
            for _ in 0..3 {
                w.strs(&paths(u));
            }
            w.u64(r(u.arbitrary::<u64>(), 0));
            w.u64(r(u.arbitrary::<u64>(), 0));
        }
        "output-map" => {
            let n = r(u.int_in_range(0u8..=3), 0);
            w.u64(n.into());
            for _ in 0..n {
                w.str(&bytes(u, b"out"));
                w.str(&if r(u.ratio(1u8, 4), false) {
                    Vec::new()
                } else {
                    path(u)
                });
            }
        }
        "substitutable" => {
            let n = r(u.int_in_range(0u8..=2), 0);
            w.u64(n.into());
            for _ in 0..n {
                w.str(&path(u));
                w.str(&if r(u.ratio(1u8, 2), false) {
                    path(u)
                } else {
                    Vec::new()
                });
                w.strs(&paths(u));
                w.u64(r(u.arbitrary::<u64>(), 0));
                w.u64(r(u.arbitrary::<u64>(), 0));
            }
        }
        _ => {}
    }
}
