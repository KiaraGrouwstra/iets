//! `ietsp-shim [--latency-ms L] [--cpu-log FILE] -- CMD ARGS...`: runs CMD with its stdin
//! and stdout relayed through this process. Every chunk in each direction is held for L ms
//! before it is forwarded, so one round trip costs 2L (rate limiters such as `pv -L` change
//! bandwidth, not latency). When CMD exits, its own user+system CPU time is read from its
//! `/proc/<pid>/stat` while it is still a zombie (the child's own fields, not the builders
//! it reaped) and appended to FILE as `<pid> <seconds>`.
//!
//! The scheduling benchmarks run `nix-daemon --stdio` under it, from an `ssh` shim and from
//! a `socat` listener, so the daemon's cost per connection is measured the same way on both
//! transports.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn relay(mut from: Box<dyn Read + Send>, mut to: Box<dyn Write + Send>, delay: Duration) {
    if delay.is_zero() {
        let mut buf = vec![0u8; 1 << 16];
        loop {
            match from.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if to.write_all(&buf[..n]).and_then(|_| to.flush()).is_err() {
                        break;
                    }
                }
            }
        }
        return;
    }
    let (tx, rx) = mpsc::channel::<Option<(Instant, Vec<u8>)>>();
    let writer = std::thread::spawn(move || {
        let mut queue: VecDeque<(Instant, Vec<u8>)> = VecDeque::new();
        loop {
            let next = if let Some((due, _)) = queue.front() {
                match rx.recv_timeout(due.saturating_duration_since(Instant::now())) {
                    Ok(m) => Some(m),
                    Err(mpsc::RecvTimeoutError::Timeout) => None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            } else {
                match rx.recv() {
                    Ok(m) => Some(m),
                    Err(_) => break,
                }
            };
            match next {
                Some(Some(item)) => queue.push_back(item),
                Some(None) => break,
                None => {}
            }
            while let Some((due, _)) = queue.front() {
                if *due > Instant::now() {
                    break;
                }
                let (_, data) = queue.pop_front().unwrap();
                if to.write_all(&data).and_then(|_| to.flush()).is_err() {
                    return;
                }
            }
        }
        while let Some((due, data)) = queue.pop_front() {
            std::thread::sleep(due.saturating_duration_since(Instant::now()));
            if to.write_all(&data).and_then(|_| to.flush()).is_err() {
                return;
            }
        }
    });
    let mut buf = vec![0u8; 1 << 16];
    loop {
        match from.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if tx
                    .send(Some((Instant::now() + delay, buf[..n].to_vec())))
                    .is_err()
                {
                    break;
                }
            }
        }
    }
    let _ = tx.send(None);
    let _ = writer.join();
}

/// utime + stime of `pid` in seconds, from `/proc/<pid>/stat` (fields 14 and 15 after the
/// parenthesised command name), and whether the process is a zombie.
fn cpu_of(pid: u32) -> io::Result<(f64, bool)> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let rest = &s[s.rfind(')').unwrap_or(0) + 1..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    let ticks = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    // rest[0] is the state; utime and stime are stat fields 14 and 15, so rest[11], rest[12]
    let clk = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    Ok(((ticks(11) + ticks(12)) / clk, f.first() == Some(&"Z")))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut latency = Duration::ZERO;
    let mut cpu_log: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--latency-ms" => {
                i += 1;
                latency = Duration::from_millis(args[i].parse().expect("--latency-ms N"));
            }
            "--cpu-log" => {
                i += 1;
                cpu_log = Some(args[i].clone());
            }
            "--" => {
                i += 1;
                break;
            }
            other => {
                eprintln!("ietsp-shim: unknown option {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    let cmd = &args[i..];
    if cmd.is_empty() {
        eprintln!("usage: ietsp-shim [--latency-ms L] [--cpu-log FILE] -- CMD ARGS...");
        std::process::exit(2);
    }
    let mut child = Command::new(&cmd[0])
        .args(&cmd[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| {
            eprintln!("ietsp-shim: cannot run {}: {e}", cmd[0]);
            std::process::exit(1)
        });
    let pid = child.id();
    let child_in: Box<dyn Write + Send> = Box::new(child.stdin.take().unwrap());
    let child_out: Box<dyn Read + Send> = Box::new(child.stdout.take().unwrap());
    let up = std::thread::spawn(move || relay(Box::new(io::stdin()), child_in, latency));
    let down = std::thread::spawn(move || relay(child_out, Box::new(io::stdout()), latency));
    let _ = down.join();
    // The daemon exits on stdin EOF; once its stdout closed it is done or about to be.
    let mut cpu = 0.0;
    for _ in 0..2000 {
        match cpu_of(pid) {
            Ok((c, zombie)) => {
                cpu = c;
                if zombie {
                    break;
                }
            }
            Err(_) => break,
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let status = child.wait().map(|s| s.code().unwrap_or(1)).unwrap_or(1);
    let _ = up.join();
    if let Some(f) = cpu_log
        && let Ok(mut l) = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(f)
    {
        let _ = writeln!(l, "{pid} {cpu:.3}");
    }
    std::process::exit(status);
}
