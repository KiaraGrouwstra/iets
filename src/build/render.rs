//! The progress renderer. Everything goes to stderr, so stdout stays what
//! the command already printed.
//!
//! One buffer per build, flushed as one block when that build stops, so the
//! logs of concurrent builds never interleave (ci_renderer.py:34-197). A
//! failure prints the last `FAILURE_TAIL` lines. On Actions-style CI the
//! successes are folded with `::group::` markers. A heartbeat thread names
//! the running builds every `HEARTBEAT`, and a build silent for `STALL`
//! switches to live streaming.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::time::{Duration, Instant};

use super::event::{FastBuildEvent, Kind, fmt_duration};

/// Unfolded tail of a failure log.
pub const FAILURE_TAIL: usize = 200;
/// Lines kept per build; the oldest rotate out.
pub const BUFFER_LINES: usize = 10_000;
/// Lines shown when a build is escalated to live streaming.
const STALL_TAIL: usize = 5;
pub const HEARTBEAT: Duration = Duration::from_secs(30);
pub const STALL: Duration = Duration::from_secs(300);

const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A terminal: one line per state change, logs only on failure.
    Tty,
    /// No terminal: the whole log of every build, in one block.
    Ci,
}

/// The renderer's settings. `fold` is on when the CI is Actions-shaped and
/// `--no-fold` was not given.
#[derive(Debug, Clone, Copy)]
pub struct Style {
    pub mode: Mode,
    pub color: bool,
    pub fold: bool,
    pub quiet: bool,
}

impl Style {
    /// What the environment asks for: a terminal on stderr means `Tty`,
    /// `GITHUB_ACTIONS`/`GITEA_ACTIONS`/`FORGEJO_ACTIONS` mean folding.
    pub fn detect() -> Style {
        Style {
            mode: if is_tty() { Mode::Tty } else { Mode::Ci },
            color: is_tty() && std::env::var_os("NO_COLOR").is_none(),
            fold: actions_ci(),
            quiet: false,
        }
    }
}

pub fn is_tty() -> bool {
    unsafe { libc::isatty(libc::STDERR_FILENO) == 1 }
}

/// The CIs whose logs understand `::group::` markers.
pub fn actions_ci() -> bool {
    ["GITHUB_ACTIONS", "GITEA_ACTIONS", "FORGEJO_ACTIONS"]
        .iter()
        .any(|v| std::env::var_os(v).is_some_and(|s| !s.is_empty()))
}

/// One build's buffered output.
struct Build {
    label: String,
    started: Instant,
    last_output: Instant,
    lines: VecDeque<String>,
    dropped: bool,
    phase: Option<String>,
    /// Escalated by the stall detector: lines go out as they arrive and the
    /// final block is skipped.
    streaming: bool,
}

impl Build {
    fn tail(&self, n: usize) -> Vec<String> {
        let skip = self.lines.len().saturating_sub(n);
        self.lines.iter().skip(skip).cloned().collect()
    }
}

pub struct Renderer {
    style: Style,
    out: Box<dyn Write + Send>,
    running: HashMap<String, Build>,
    /// Buffers of builds that already stopped, for a failure reported after
    /// the activity ended. Bounded: only the most recent are kept.
    finished: VecDeque<(String, Vec<String>)>,
}

impl Renderer {
    pub fn new(style: Style, out: Box<dyn Write + Send>) -> Renderer {
        Renderer {
            style,
            out,
            running: HashMap::new(),
            finished: VecDeque::new(),
        }
    }

    fn sgr(&self, code: &str, s: &str) -> String {
        if self.style.color {
            format!("{code}{s}{RESET}")
        } else {
            s.to_string()
        }
    }

    fn print(&mut self, lines: &[String]) {
        for l in lines {
            let _ = writeln!(self.out, "{l}");
        }
        let _ = self.out.flush();
    }

    fn line(&mut self, l: String) {
        self.print(&[l]);
    }

    /// One line, outside any build: a plan note, a daemon message.
    pub fn note(&mut self, msg: &str) {
        self.line(msg.to_string());
    }

    pub fn event(&mut self, e: &FastBuildEvent) {
        match &e.kind {
            Kind::Started => self.started(e),
            Kind::Phase(p) => self.phase(e, p),
            Kind::Log(bytes) => self.log(e, bytes),
            Kind::Built { .. } => self.verdict(e, true, None),
            Kind::Failed { reason, log_tail } => self.failure(e, reason, log_tail),
            Kind::Cached => {
                if !self.style.quiet {
                    let l = self.sgr(DIM, &format!("· {} cached", e.attr));
                    self.line(l);
                }
            }
            Kind::Skipped { why } => {
                if !self.style.quiet {
                    let l = self.sgr(DIM, &format!("· {} skipped: {why}", e.attr));
                    self.line(l);
                }
            }
        }
    }

    fn started(&mut self, e: &FastBuildEvent) {
        let now = Instant::now();
        self.running.insert(
            e.drv.clone(),
            Build {
                label: e.attr.clone(),
                started: now,
                last_output: now,
                lines: VecDeque::new(),
                dropped: false,
                phase: None,
                streaming: false,
            },
        );
        if !self.style.quiet {
            let l = format!("▶ building {}", e.attr);
            self.line(l);
        }
    }

    fn phase(&mut self, e: &FastBuildEvent, phase: &str) {
        let (label, elapsed) = match self.running.get_mut(&e.drv) {
            Some(b) => {
                b.phase = Some(phase.to_string());
                b.last_output = Instant::now();
                b.lines.push_back(format!("@ phase {phase}"));
                (b.label.clone(), b.started.elapsed())
            }
            None => (e.attr.clone(), e.elapsed),
        };
        if self.style.mode == Mode::Tty && !self.style.quiet {
            let l = self.sgr(
                DIM,
                &format!("  {label}: {phase} ({})", fmt_duration(elapsed)),
            );
            self.line(l);
        }
    }

    fn log(&mut self, e: &FastBuildEvent, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes).into_owned();
        let Some(b) = self.running.get_mut(&e.drv) else {
            // Not attributed to a build: the daemon's own message.
            if !self.style.quiet {
                self.line(text);
            }
            return;
        };
        b.last_output = Instant::now();
        if b.lines.len() == BUFFER_LINES {
            b.lines.pop_front();
            b.dropped = true;
        }
        b.lines.push_back(text.clone());
        if b.streaming {
            let prefix = b.label.clone();
            let l = self.sgr(DIM, &format!("{prefix}>"));
            self.line(format!("{l} {text}"));
        }
    }

    /// A build stopped without a verdict yet: flush its block and keep the
    /// tail in case a failure names it later.
    pub fn stopped(&mut self, drv: &str) {
        let Some(b) = self.running.remove(drv) else {
            return;
        };
        let tail = b.tail(FAILURE_TAIL);
        self.retain(drv, tail);
        self.verdict_block(&b, true, None);
    }

    fn retain(&mut self, drv: &str, tail: Vec<String>) {
        self.finished.push_back((drv.to_string(), tail));
        while self.finished.len() > 64 {
            self.finished.pop_front();
        }
    }

    fn verdict(&mut self, e: &FastBuildEvent, ok: bool, reason: Option<&str>) {
        match self.running.remove(&e.drv) {
            Some(b) => {
                let tail = b.tail(FAILURE_TAIL);
                self.retain(&e.drv, tail);
                self.verdict_block(&b, ok, reason);
            }
            None if !ok => {
                let l = self.sgr(
                    RED,
                    &format!(
                        "✘  {} failed after {}: {}",
                        e.attr,
                        fmt_duration(e.elapsed),
                        reason.unwrap_or("")
                    ),
                );
                self.line(l);
            }
            None => {}
        }
    }

    fn failure(&mut self, e: &FastBuildEvent, reason: &str, tail: &[String]) {
        if self.running.contains_key(&e.drv) {
            self.verdict(e, false, Some(reason));
            return;
        }
        let head = self.sgr(
            RED,
            &format!(
                "✘  {} failed after {} ({reason})",
                e.attr,
                fmt_duration(e.elapsed)
            ),
        );
        let prefix = self.sgr(DIM, &format!("{}>", e.attr));
        let mut out = vec![head];
        out.extend(tail.iter().map(|l| format!("{prefix} {l}")));
        out.push(self.sgr(
            RED,
            &format!("✘ end of log for {} · nix log {}", e.attr, e.drv),
        ));
        self.print(&out);
    }

    fn verdict_block(&mut self, b: &Build, ok: bool, reason: Option<&str>) {
        let duration = fmt_duration(b.started.elapsed());
        let verdict = if ok {
            self.sgr(GREEN, &format!("✔  {} ({duration})", b.label))
        } else {
            self.sgr(
                RED,
                &format!(
                    "✘  {} failed after {duration}{}",
                    b.label,
                    match reason {
                        Some(r) => format!(" ({r})"),
                        None => String::new(),
                    }
                ),
            )
        };
        if b.streaming {
            // The log is already on screen.
            self.line(verdict);
            return;
        }
        if self.style.quiet || (ok && self.style.mode == Mode::Tty) {
            self.line(verdict);
            return;
        }
        let prefix = self.sgr(DIM, &format!("{}>", b.label));
        let body: Vec<String> = if ok {
            b.lines.iter().map(|l| format!("{prefix} {l}")).collect()
        } else {
            b.tail(FAILURE_TAIL)
                .iter()
                .map(|l| format!("{prefix} {l}"))
                .collect()
        };
        let mut out = Vec::with_capacity(body.len() + 3);
        if ok && self.style.fold {
            out.push(format!("::group::{verdict}"));
            out.extend(body);
            out.push("::endgroup::".into());
        } else if ok {
            out.push(verdict);
            out.extend(body);
        } else {
            let dropped = if b.dropped {
                format!(" (oldest lines dropped, buffer={BUFFER_LINES})")
            } else {
                String::new()
            };
            out.push(format!("{verdict}{dropped}"));
            out.extend(body);
            out.push(self.sgr(RED, &format!("✘ end of log for {}", b.label)));
        }
        self.print(&out);
    }

    /// The tail kept for a build that already stopped.
    pub fn retained_tail(&self, drv: &str) -> Vec<String> {
        self.finished
            .iter()
            .rev()
            .find(|(d, _)| d == drv)
            .map(|(_, t)| t.clone())
            .unwrap_or_default()
    }

    /// The live buffer of a running build.
    pub fn live_tail(&self, drv: &str) -> Vec<String> {
        self.running
            .get(drv)
            .map(|b| b.tail(FAILURE_TAIL))
            .unwrap_or_default()
    }

    /// One heartbeat: escalate the stalled builds, then name what is running.
    pub fn heartbeat(&mut self) {
        self.check_stalls();
        if self.running.is_empty() || self.style.quiet {
            return;
        }
        let mut builds: Vec<(String, Duration, Option<String>, bool)> = self
            .running
            .values()
            .map(|b| {
                (
                    b.label.clone(),
                    b.started.elapsed(),
                    b.phase.clone(),
                    b.streaming,
                )
            })
            .collect();
        builds.sort_by_key(|b| std::cmp::Reverse(b.1));
        let parts: Vec<String> = builds
            .iter()
            .map(|(label, el, phase, streaming)| {
                let mut detail = fmt_duration(*el);
                if let Some(p) = phase {
                    detail.push_str(&format!(", {p}"));
                }
                format!(
                    "{label} ({detail}){}",
                    if *streaming { " ⚠stalled" } else { "" }
                )
            })
            .collect();
        let l = self.sgr(
            DIM,
            &format!("⏵ {} building: {}", builds.len(), parts.join(", ")),
        );
        self.line(l);
    }

    fn check_stalls(&mut self) {
        let stalled: Vec<String> = self
            .running
            .iter()
            .filter(|(_, b)| !b.streaming && b.last_output.elapsed() >= STALL)
            .map(|(d, _)| d.clone())
            .collect();
        for drv in stalled {
            let (label, silent, tail) = {
                let b = &self.running[&drv];
                (b.label.clone(), b.last_output.elapsed(), b.tail(STALL_TAIL))
            };
            let prefix = self.sgr(DIM, &format!("{label}>"));
            let mut out = vec![self.sgr(
                BOLD,
                &format!(
                    "⚠ {label}: no output for {}, last lines:",
                    fmt_duration(silent)
                ),
            )];
            out.extend(tail.iter().map(|l| format!("{prefix} {l}")));
            out.push(self.sgr(BOLD, &format!("⚠ {label}: streaming further output live")));
            self.print(&out);
            if let Some(b) = self.running.get_mut(&drv) {
                b.streaming = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn ci(fold: bool) -> (Renderer, Sink) {
        let s = Sink::default();
        let style = Style {
            mode: Mode::Ci,
            color: false,
            fold,
            quiet: false,
        };
        (Renderer::new(style, Box::new(s.clone())), s)
    }

    fn text(s: &Sink) -> String {
        String::from_utf8(s.0.lock().unwrap().clone()).unwrap()
    }

    fn ev(kind: Kind) -> FastBuildEvent {
        FastBuildEvent::new("checks.x.a", "/nix/store/a.drv", kind, Duration::ZERO)
    }

    #[test]
    fn ci_folds_a_success_and_never_a_failure() {
        let (mut r, s) = ci(true);
        r.event(&ev(Kind::Started));
        r.event(&ev(Kind::Log(b"line one".to_vec())));
        r.stopped("/nix/store/a.drv");
        let t = text(&s);
        assert!(t.contains("::group::✔  checks.x.a"), "{t}");
        assert!(t.contains("checks.x.a> line one"));
        assert!(t.contains("::endgroup::"));

        let (mut r, s) = ci(true);
        r.event(&ev(Kind::Started));
        r.event(&ev(Kind::Log(b"boom".to_vec())));
        r.event(&ev(Kind::Failed {
            reason: "status 1".into(),
            log_tail: vec![],
        }));
        let t = text(&s);
        assert!(!t.contains("::group::"), "{t}");
        assert!(t.contains("✘  checks.x.a failed after 0s (status 1)"));
        assert!(t.contains("checks.x.a> boom"));
    }

    #[test]
    fn logs_of_two_builds_do_not_interleave() {
        let (mut r, s) = ci(false);
        let a = FastBuildEvent::new("a", "/nix/store/a.drv", Kind::Started, Duration::ZERO);
        let b = FastBuildEvent::new("b", "/nix/store/b.drv", Kind::Started, Duration::ZERO);
        r.event(&a);
        r.event(&b);
        r.event(&FastBuildEvent::new(
            "a",
            "/nix/store/a.drv",
            Kind::Log(b"a1".to_vec()),
            Duration::ZERO,
        ));
        r.event(&FastBuildEvent::new(
            "b",
            "/nix/store/b.drv",
            Kind::Log(b"b1".to_vec()),
            Duration::ZERO,
        ));
        r.event(&FastBuildEvent::new(
            "a",
            "/nix/store/a.drv",
            Kind::Log(b"a2".to_vec()),
            Duration::ZERO,
        ));
        r.stopped("/nix/store/a.drv");
        r.stopped("/nix/store/b.drv");
        let t = text(&s);
        let a1 = t.find("a> a1").unwrap();
        let a2 = t.find("a> a2").unwrap();
        let b1 = t.find("b> b1").unwrap();
        assert!(a1 < a2 && a2 < b1, "{t}");
    }

    #[test]
    fn tty_keeps_successful_logs_off_the_screen() {
        let s = Sink::default();
        let style = Style {
            mode: Mode::Tty,
            color: false,
            fold: false,
            quiet: false,
        };
        let mut r = Renderer::new(style, Box::new(s.clone()));
        r.event(&ev(Kind::Started));
        r.event(&ev(Kind::Log(b"noise".to_vec())));
        r.event(&ev(Kind::Phase("build".into())));
        r.stopped("/nix/store/a.drv");
        let t = text(&s);
        assert!(t.contains("▶ building checks.x.a"));
        assert!(t.contains("checks.x.a: build (0s)"));
        assert!(t.contains("✔  checks.x.a (0s)"));
        assert!(!t.contains("noise"), "{t}");
    }

    #[test]
    fn heartbeat_names_running_builds() {
        let (mut r, s) = ci(false);
        r.event(&ev(Kind::Started));
        r.event(&ev(Kind::Phase("configure".into())));
        r.heartbeat();
        assert!(text(&s).contains("⏵ 1 building: checks.x.a (0s, configure)"));
    }
}
