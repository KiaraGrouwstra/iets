//! Fallback transport: the Nix worker protocol (libstore/worker-protocol.hh), spoken to a
//! local `nix-daemon` socket or to `nix-daemon --stdio` over ssh. One serial connection; every
//! op is one request and one `STDERR_*` reply loop.

pub mod build;
pub mod wire;

use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use super::proto::build_event::Event;
use super::proto::{Built, Failed, Log, Started};
use wire::{
    Counting, FramedSink, read_bytes, read_str, read_strs, read_u64, write_str, write_strs,
    write_u64,
};

const WORKER_MAGIC_1: u64 = 0x6e69_7863;
const WORKER_MAGIC_2: u64 = 0x6478_696f;

const STDERR_NEXT: u64 = 0x6f6c_6d67;
const STDERR_READ: u64 = 0x6461_7461;
const STDERR_WRITE: u64 = 0x6461_7416;
const STDERR_LAST: u64 = 0x616c_7473;
const STDERR_ERROR: u64 = 0x6378_7470;
const STDERR_START_ACTIVITY: u64 = 0x5354_5254;
const STDERR_STOP_ACTIVITY: u64 = 0x5354_4f50;
const STDERR_RESULT: u64 = 0x5253_4c54;

/// The newest protocol this client speaks (worker-protocol.cc:WorkerProto::latest).
pub const LATEST_MINOR: u64 = 38;
/// Below this the handshake and `BuildPathsWithResults` differ; not worth supporting.
pub const MIN_MINOR: u64 = 35;

/// worker-protocol.hh:WorkerProto::Op, the subset used here.
#[derive(Clone, Copy)]
#[repr(u64)]
enum Op {
    SetOptions = 19,
    QueryValidPaths = 31,
    AddIndirectRoot = 12,
    NarFromPath = 38,
    QueryDerivationOutputMap = 41,
    AddMultipleToStore = 44,
    BuildPathsWithResults = 46,
}

/// logging.hh:ActivityType and ResultType, the ones that carry build output.
const ACT_BUILD: u64 = 105;
const RES_BUILD_LOG_LINE: u64 = 101;
const RES_POST_BUILD_LOG_LINE: u64 = 107;

/// error.hh:Verbosity.
const LVL_INFO: u64 = 3;
const LVL_VOMIT: u64 = 7;

pub const DEFAULT_SOCKET: &str = "/nix/var/nix/daemon-socket/socket";

/// Where the `--builder` URL points when it names a worker-protocol endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transport {
    Unix(PathBuf),
    Ssh { user: Option<String>, host: String },
}

impl Transport {
    /// `daemon`, `unix://[/path]`, `ssh-ng://[user@]host`. `None` for every other scheme,
    /// which stays on gRPC.
    pub fn parse(url: &str) -> Option<Transport> {
        if url == "daemon" || url == "unix://" {
            return Some(Transport::Unix(PathBuf::from(DEFAULT_SOCKET)));
        }
        if let Some(path) = url.strip_prefix("unix://") {
            return Some(Transport::Unix(PathBuf::from(path)));
        }
        if let Some(rest) = url.strip_prefix("ssh-ng://") {
            let rest = rest.trim_end_matches('/');
            let (user, host) = match rest.split_once('@') {
                Some((u, h)) => (Some(u.to_string()), h.to_string()),
                None => (None, rest.to_string()),
            };
            return Some(Transport::Ssh { user, host });
        }
        None
    }
}

#[derive(Debug)]
pub struct WorkerError(pub String);

impl std::fmt::Display for WorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for WorkerError {}

impl From<io::Error> for WorkerError {
    fn from(e: io::Error) -> Self {
        WorkerError(format!("daemon connection: {e}"))
    }
}

type Result<T> = std::result::Result<T, WorkerError>;

/// Round trips and bytes on one connection, for `--timings`.
#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    /// Worker ops sent, `SetOptions` included.
    pub ops: usize,
    /// Ops plus the handshake exchanges that each wait for the daemon.
    pub round_trips: usize,
    pub bytes_sent: u64,
}

/// worker-protocol.cc:BuildResult, the fields this client reads.
#[derive(Debug, Clone)]
pub struct BuildResult {
    pub path: String,
    pub status: u8,
    pub msg: String,
}

impl BuildResult {
    /// common-protocol.cc:buildResultStatusTable: Built, Substituted, AlreadyValid,
    /// ResolvesToAlreadyValid.
    pub fn success(&self) -> bool {
        matches!(self.status, 0 | 1 | 2 | 13)
    }
}

pub struct Connection {
    to: BufWriter<Counting<Box<dyn Write + Send>>>,
    from: BufReader<Box<dyn Read + Send>>,
    child: Option<Child>,
    pub minor: u64,
    pub daemon_nix_version: String,
    /// `Some(true)` when the daemon trusts us (protocol >= 1.35).
    pub trusted: Option<bool>,
    pub stats: Stats,
    /// Running builds by activity id, to attribute log lines.
    activities: HashMap<u64, String>,
}

impl Drop for Connection {
    /// The ssh child exits on EOF of its stdin, which lives inside `to`; swap it out before
    /// waiting, or the wait never returns.
    fn drop(&mut self) {
        let _ = self.to.flush();
        if let Some(mut c) = self.child.take() {
            let sink: Box<dyn Write + Send> = Box::new(io::sink());
            drop(std::mem::replace(
                &mut self.to,
                BufWriter::new(Counting {
                    inner: sink,
                    bytes: 0,
                }),
            ));
            let _ = c.wait();
        }
    }
}

impl Connection {
    /// Opens the transport and runs the handshake and `SetOptions`, as
    /// remote-store.cc:initConnection does.
    pub fn open(transport: &Transport, on_event: &mut dyn FnMut(Event)) -> Result<Connection> {
        let (to, from, child): (Box<dyn Write + Send>, Box<dyn Read + Send>, Option<Child>) =
            match transport {
                Transport::Unix(path) => {
                    let s = UnixStream::connect(path).map_err(|e| {
                        WorkerError(format!("cannot connect to '{}': {e}", path.display()))
                    })?;
                    let r = s.try_clone()?;
                    (Box::new(s), Box::new(r), None)
                }
                Transport::Ssh { user, host } => {
                    let mut cmd = Command::new("ssh");
                    if let Some(u) = user {
                        cmd.arg("-l").arg(u);
                    }
                    cmd.arg("-x")
                        .arg(host)
                        .args(["nix-daemon", "--stdio"])
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::inherit());
                    let mut c = cmd
                        .spawn()
                        .map_err(|e| WorkerError(format!("cannot run ssh: {e}")))?;
                    let stdin = c.stdin.take().expect("piped");
                    let stdout = c.stdout.take().expect("piped");
                    (Box::new(stdin), Box::new(stdout), Some(c))
                }
            };
        let mut conn = Connection {
            to: BufWriter::with_capacity(
                1 << 20,
                Counting {
                    inner: to,
                    bytes: 0,
                },
            ),
            from: BufReader::with_capacity(1 << 20, from),
            child,
            minor: 0,
            daemon_nix_version: String::new(),
            trusted: None,
            stats: Stats::default(),
            activities: HashMap::new(),
        };
        conn.handshake()?;
        conn.process_stderr(on_event)?;
        conn.set_options(on_event)?;
        Ok(conn)
    }

    pub fn bytes_sent(&self) -> u64 {
        self.to.get_ref().bytes
    }

    fn sync_stats(&mut self) {
        self.stats.bytes_sent = self.bytes_sent();
    }

    fn w(&mut self) -> &mut dyn Write {
        &mut self.to
    }

    fn r(&mut self) -> &mut dyn Read {
        &mut self.from
    }

    /// worker-protocol-connection.cc:BasicClientConnection::handshake and postHandshake.
    fn handshake(&mut self) -> Result<()> {
        write_u64(self.w(), WORKER_MAGIC_1)?;
        write_u64(self.w(), (1 << 8) | LATEST_MINOR)?;
        self.to.flush()?;
        if read_u64(self.r())? != WORKER_MAGIC_2 {
            return Err(WorkerError("nix-daemon protocol mismatch".into()));
        }
        let daemon = read_u64(self.r())?;
        if (daemon & 0xff00) >> 8 != 1 {
            return Err(WorkerError(
                "Nix daemon protocol version not supported".into(),
            ));
        }
        self.minor = (daemon & 0xff).min(LATEST_MINOR);
        self.stats.round_trips += 1;
        if self.minor < MIN_MINOR {
            return Err(WorkerError(format!(
                "the Nix daemon speaks protocol 1.{}; this client needs 1.{MIN_MINOR} or newer",
                self.minor
            )));
        }
        if self.minor >= 38 {
            write_strs::<&str>(self.w(), &[])?;
            self.to.flush()?;
            read_strs(self.r())?;
            self.stats.round_trips += 1;
        }
        write_u64(self.w(), 0)?;
        write_u64(self.w(), 0)?;
        self.to.flush()?;
        self.daemon_nix_version = read_str(self.r())?;
        self.trusted = match read_u64(self.r())? {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        };
        self.stats.round_trips += 1;
        Ok(())
    }

    fn op(&mut self, op: Op) -> io::Result<()> {
        self.stats.ops += 1;
        self.stats.round_trips += 1;
        write_u64(self.w(), op as u64)
    }

    /// remote-store.cc:setOptions. Sends the same twelve fields plus an empty override map.
    /// `max-jobs` is the CPU count, what `max-jobs = auto` gives; `0` would forbid local
    /// builds.
    fn set_options(&mut self, on_event: &mut dyn FnMut(Event)) -> Result<()> {
        let jobs = std::thread::available_parallelism().map_or(1, |n| n.get() as u64);
        self.op(Op::SetOptions)?;
        for v in [
            0,         // keep-failed
            0,         // keep-going
            0,         // try-fallback
            LVL_INFO,  // verbosity
            jobs,      // max-jobs
            0,         // max-silent-time
            1,         // obsolete use-build-hook
            LVL_VOMIT, // build verbosity: lvlVomit means verboseBuild = false
            0,         // obsolete log type
            0,         // obsolete print build trace
            0,         // cores
            1,         // substitute
        ] {
            write_u64(self.w(), v)?;
        }
        write_u64(self.w(), 0)?;
        self.process_stderr(on_event)
    }

    /// worker-protocol-connection.cc:processStderrReturn: drains the daemon's log stream until
    /// `STDERR_LAST` (the reply follows) or `STDERR_ERROR`.
    fn process_stderr(&mut self, on_event: &mut dyn FnMut(Event)) -> Result<()> {
        self.to.flush()?;
        loop {
            match read_u64(self.r())? {
                STDERR_LAST => {
                    self.sync_stats();
                    return Ok(());
                }
                STDERR_NEXT => {
                    let mut line = read_bytes(self.r(), wire::MAX_STRING)?;
                    while line.last() == Some(&b'\n') {
                        line.pop();
                    }
                    on_event(Event::Log(Log {
                        drv_path: String::new(),
                        line,
                    }));
                }
                STDERR_START_ACTIVITY => {
                    let act = read_u64(self.r())?;
                    let _lvl = read_u64(self.r())?;
                    let ty = read_u64(self.r())?;
                    let _s = read_str(self.r())?;
                    let fields = self.read_fields()?;
                    let _parent = read_u64(self.r())?;
                    if ty == ACT_BUILD
                        && let Some(Field::Str(drv)) = fields.first()
                    {
                        self.activities.insert(act, drv.clone());
                        on_event(Event::Started(Started {
                            drv_path: drv.clone(),
                        }));
                    }
                }
                STDERR_STOP_ACTIVITY => {
                    let act = read_u64(self.r())?;
                    self.activities.remove(&act);
                }
                STDERR_RESULT => {
                    let act = read_u64(self.r())?;
                    let ty = read_u64(self.r())?;
                    let fields = self.read_fields()?;
                    if matches!(ty, RES_BUILD_LOG_LINE | RES_POST_BUILD_LOG_LINE)
                        && let Some(Field::Str(line)) = fields.first()
                    {
                        on_event(Event::Log(Log {
                            drv_path: self.activities.get(&act).cloned().unwrap_or_default(),
                            line: line.clone().into_bytes(),
                        }));
                    }
                }
                STDERR_ERROR => {
                    let e = self.read_error()?;
                    self.sync_stats();
                    return Err(e);
                }
                STDERR_READ | STDERR_WRITE => {
                    return Err(WorkerError(
                        "daemon asked for a data stream this client does not provide".into(),
                    ));
                }
                other => {
                    return Err(WorkerError(format!(
                        "got unknown message type {other:#x} from Nix daemon"
                    )));
                }
            }
        }
    }

    /// worker-protocol-connection.cc:readFields.
    fn read_fields(&mut self) -> Result<Vec<Field>> {
        let n = read_u64(self.r())?;
        let mut fields = Vec::with_capacity(n.min(16) as usize);
        for _ in 0..n {
            let f = match read_u64(self.r())? {
                0 => {
                    read_u64(self.r())?;
                    Field::Int
                }
                1 => Field::Str(read_str(self.r())?),
                t => {
                    return Err(WorkerError(format!(
                        "got unsupported field type {t:#x} from Nix daemon"
                    )));
                }
            };
            fields.push(f);
        }
        Ok(fields)
    }

    /// serialise.cc:readError (protocol >= 1.26).
    fn read_error(&mut self) -> Result<WorkerError> {
        let ty = read_str(self.r())?;
        if ty != "Error" {
            return Err(WorkerError(format!("unexpected error type '{ty}'")));
        }
        let _level = read_u64(self.r())?;
        let _name = read_str(self.r())?;
        let mut msg = read_str(self.r())?;
        let _have_pos = read_u64(self.r())?;
        let traces = read_u64(self.r())?;
        for _ in 0..traces {
            let _have_pos = read_u64(self.r())?;
            let hint = read_str(self.r())?;
            msg.push_str("\n       … ");
            msg.push_str(&hint);
        }
        Ok(WorkerError(msg))
    }

    /// `QueryValidPaths` with substitution off; returns the valid subset.
    pub fn query_valid_paths(
        &mut self,
        paths: &[String],
        on_event: &mut dyn FnMut(Event),
    ) -> Result<Vec<String>> {
        self.op(Op::QueryValidPaths)?;
        write_strs(self.w(), paths)?;
        write_u64(self.w(), 0)?;
        self.process_stderr(on_event)?;
        Ok(read_strs(self.r())?)
    }

    /// `AddMultipleToStore`: `body` writes the count, then each `ValidPathInfo` followed by its
    /// NAR (store-api.cc:Store::addMultipleToStore(Source &)), into one framed stream.
    pub fn add_multiple_to_store(
        &mut self,
        body: &mut dyn FnMut(&mut dyn Write) -> io::Result<()>,
        on_event: &mut dyn FnMut(Event),
    ) -> Result<()> {
        self.op(Op::AddMultipleToStore)?;
        write_u64(self.w(), 0)?;
        write_u64(self.w(), 0)?;
        self.to.flush()?;
        let mut sink = FramedSink::new(&mut self.to);
        body(&mut sink)?;
        sink.finish()?;
        self.process_stderr(on_event)
    }

    /// `BuildPathsWithResults` in normal mode; `paths` are legacy derived paths
    /// (`/nix/store/x.drv!out1,out2` or `!*`), the encoding for protocol >= 1.30.
    pub fn build_paths_with_results(
        &mut self,
        paths: &[String],
        on_event: &mut dyn FnMut(Event),
    ) -> Result<Vec<BuildResult>> {
        self.op(Op::BuildPathsWithResults)?;
        write_strs(self.w(), paths)?;
        write_u64(self.w(), 0)?;
        self.process_stderr(on_event)?;
        let n = read_u64(self.r())?;
        let mut results = Vec::new();
        for _ in 0..n {
            let path = read_str(self.r())?;
            let status = read_u64(self.r())?;
            let msg = read_str(self.r())?;
            for _ in 0..4 {
                read_u64(self.r())?;
            }
            if self.minor >= 37 {
                for _ in 0..2 {
                    if read_u64(self.r())? == 1 {
                        read_u64(self.r())?;
                    }
                }
            }
            let outputs = read_u64(self.r())?;
            for _ in 0..outputs {
                read_str(self.r())?;
                read_str(self.r())?;
            }
            results.push(BuildResult {
                path,
                status: status as u8,
                msg,
            });
        }
        Ok(results)
    }

    /// `QueryDerivationOutputMap`: output name to store path when the daemon knows it.
    pub fn query_derivation_output_map(
        &mut self,
        drv: &str,
        on_event: &mut dyn FnMut(Event),
    ) -> Result<BTreeMap<String, Option<String>>> {
        self.op(Op::QueryDerivationOutputMap)?;
        write_str(self.w(), drv.as_bytes())?;
        self.process_stderr(on_event)?;
        let n = read_u64(self.r())?;
        let mut map = BTreeMap::new();
        for _ in 0..n {
            let name = read_str(self.r())?;
            let path = read_str(self.r())?;
            map.insert(name, if path.is_empty() { None } else { Some(path) });
        }
        Ok(map)
    }

    /// `AddIndirectRoot` (uds-remote-store.cc:addIndirectRoot): registers `path`, an
    /// existing symlink into the store that the caller created, as a GC root. The daemon
    /// links `/nix/var/nix/gcroots/auto/<hash>` to it.
    pub fn add_indirect_root(&mut self, path: &str, on_event: &mut dyn FnMut(Event)) -> Result<()> {
        self.op(Op::AddIndirectRoot)?;
        write_str(self.w(), path.as_bytes())?;
        self.process_stderr(on_event)?;
        read_u64(self.r())?;
        Ok(())
    }

    /// `NarFromPath`: `sink` reads exactly the NAR from the connection.
    pub fn nar_from_path(
        &mut self,
        path: &str,
        on_event: &mut dyn FnMut(Event),
        sink: &mut dyn FnMut(&mut dyn Read) -> io::Result<()>,
    ) -> Result<()> {
        self.op(Op::NarFromPath)?;
        write_str(self.w(), path.as_bytes())?;
        self.process_stderr(on_event)?;
        sink(&mut self.from)?;
        Ok(())
    }

    /// True when the daemon has sent bytes we have not consumed; a protocol bug otherwise.
    pub fn has_pending(&mut self) -> bool {
        !self.from.buffer().is_empty()
    }
}

enum Field {
    Int,
    Str(String),
}

pub fn built_event(drv_path: &str, output: &str, store_path: &str) -> Event {
    Event::Built(Built {
        drv_path: drv_path.to_string(),
        output: output.to_string(),
        store_path: store_path.to_string(),
        signature: String::new(),
    })
}

pub fn failed_event(drv_path: &str, reason: &str) -> Event {
    Event::Failed(Failed {
        drv_path: drv_path.to_string(),
        reason: reason.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_from_url() {
        assert_eq!(
            Transport::parse("daemon"),
            Some(Transport::Unix(PathBuf::from(DEFAULT_SOCKET)))
        );
        assert_eq!(
            Transport::parse("unix:///tmp/s"),
            Some(Transport::Unix(PathBuf::from("/tmp/s")))
        );
        assert_eq!(
            Transport::parse("ssh-ng://bob@bld1"),
            Some(Transport::Ssh {
                user: Some("bob".into()),
                host: "bld1".into()
            })
        );
        assert_eq!(
            Transport::parse("ssh-ng://bld1/"),
            Some(Transport::Ssh {
                user: None,
                host: "bld1".into()
            })
        );
        assert_eq!(Transport::parse("http://127.0.0.1:9455"), None);
    }
}
