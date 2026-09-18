//! The Nix worker protocol (libstore/worker-protocol.hh), spoken to a
//! local `nix-daemon` socket or to `nix-daemon --stdio` over ssh. One serial connection; every
//! op is one request and one `STDERR_*` reply loop.

pub mod build;
pub mod wire;

use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use super::event::{Built, Event, Failed, Log, Phase, Started, Stopped};
use crate::config::Config;
use crate::error::{Error, Result};
use wire::{Counting, FramedSink};

/// What `SetOptions` sends (remote-store.cc:setOptions). `Default` is what this
/// client has always sent; `TryFrom<&Config>` takes the values from the user's
/// nix.conf instead, and fails the way Nix does on a bad setting value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// `0` would forbid local builds, so the default is the CPU count, what
    /// `max-jobs = auto` gives.
    pub max_jobs: u64,
    /// `0` means all cores.
    pub cores: u64,
    pub keep_going: bool,
    pub keep_failed: bool,
    pub substitutes: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            max_jobs: std::thread::available_parallelism().map_or(1, |n| n.get() as u64),
            cores: 0,
            keep_going: false,
            keep_failed: false,
            substitutes: true,
        }
    }
}

impl TryFrom<&Config> for Options {
    type Error = Error;

    fn try_from(c: &Config) -> Result<Options> {
        Ok(Options {
            max_jobs: c.max_jobs()?,
            cores: c.cores()?,
            keep_going: c.keep_going()?,
            keep_failed: c.keep_failed()?,
            substitutes: c.bool("substitute")?.unwrap_or(true),
        })
    }
}

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
    QuerySubstitutablePathInfos = 30,
    NarFromPath = 38,
    QueryDerivationOutputMap = 41,
    AddMultipleToStore = 44,
    BuildPathsWithResults = 46,
}

/// logging.hh:ActivityType and ResultType, the ones that carry build output.
const ACT_BUILD: u64 = 105;
const RES_BUILD_LOG_LINE: u64 = 101;
const RES_POST_BUILD_LOG_LINE: u64 = 107;
const RES_SET_PHASE: u64 = 104;

/// error.hh:Verbosity.
const LVL_INFO: u64 = 3;
const LVL_VOMIT: u64 = 7;

pub const DEFAULT_SOCKET: &str = "/nix/var/nix/daemon-socket/socket";

/// Where the `--builder` or `--store` URL points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transport {
    Unix(PathBuf),
    /// nix-grpc-store, which does not speak the worker protocol; see
    /// `crate::remote::grpc`.
    Grpc(Box<super::grpc::GrpcConfig>),
    Ssh {
        user: Option<String>,
        host: String,
        /// The command run on the host, `nix-daemon --stdio` unless the URL's
        /// `remote-program` query parameter says otherwise (as for Nix's ssh-ng store).
        remote_program: Vec<String>,
    },
}

impl Transport {
    /// `daemon`, `unix://[/path]`, `ssh-ng://[user@]host[?remote-program=CMD]`,
    /// `grpc://host:port[?…]`. `None` for every other scheme. A malformed
    /// `grpc://` URL is `None` too; `parse_checked` reports why.
    pub fn parse(url: &str) -> Option<Transport> {
        Transport::parse_checked(url, crate::config::DEFAULT_SYSTEM).ok()?
    }

    /// `parse` with the `grpc://` parameter errors kept, and the `system=`
    /// default the caller's `system` setting implies.
    pub fn parse_checked(
        url: &str,
        default_system: &str,
    ) -> crate::error::Result<Option<Transport>> {
        if let Some(cfg) = super::grpc::GrpcConfig::parse(url, default_system) {
            return Ok(Some(Transport::Grpc(Box::new(cfg?))));
        }
        Ok(Transport::parse_worker(url))
    }

    fn parse_worker(url: &str) -> Option<Transport> {
        if url == "daemon" || url == "unix://" {
            return Some(Transport::Unix(PathBuf::from(DEFAULT_SOCKET)));
        }
        if let Some(path) = url.strip_prefix("unix://") {
            return Some(Transport::Unix(PathBuf::from(path)));
        }
        if let Some(rest) = url.strip_prefix("ssh-ng://") {
            let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
            let rest = rest.trim_end_matches('/');
            let (user, host) = match rest.split_once('@') {
                Some((u, h)) => (Some(u.to_string()), h.to_string()),
                None => (None, rest.to_string()),
            };
            let mut remote_program = vec!["nix-daemon".to_string(), "--stdio".to_string()];
            for kv in query.split('&') {
                if let Some(v) = kv.strip_prefix("remote-program=") {
                    remote_program = percent_decode(v)
                        .split_whitespace()
                        .map(str::to_string)
                        .collect();
                }
            }
            return Some(Transport::Ssh {
                user,
                host,
                remote_program,
            });
        }
        None
    }
}

/// `%XX` and `+` in a URL query value.
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = if b[i] == b'%' && i + 2 < b.len() {
            std::str::from_utf8(&b[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        } else {
            None
        };
        match hex {
            Some(c) => {
                out.push(c);
                i += 3;
            }
            None => {
                out.push(if b[i] == b'+' { b' ' } else { b[i] });
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Every failure of the connection's own reads and writes reports the same way.
fn conn(e: io::Error) -> Error {
    Error::io("daemon connection", e)
}

fn write_u64(w: &mut dyn Write, n: u64) -> Result<()> {
    wire::write_u64(w, n).map_err(conn)
}

fn write_str(w: &mut dyn Write, s: &[u8]) -> Result<()> {
    wire::write_str(w, s).map_err(conn)
}

fn write_strs<S: AsRef<[u8]>>(w: &mut dyn Write, items: &[S]) -> Result<()> {
    wire::write_strs(w, items).map_err(conn)
}

fn read_u64(r: &mut dyn Read) -> Result<u64> {
    wire::read_u64(r).map_err(conn)
}

fn read_bytes(r: &mut dyn Read, max: u64) -> Result<Vec<u8>> {
    wire::read_bytes(r, max).map_err(conn)
}

fn read_str(r: &mut dyn Read) -> Result<String> {
    wire::read_str(r).map_err(conn)
}

fn read_strs(r: &mut dyn Read) -> Result<Vec<String>> {
    wire::read_strs(r).map_err(conn)
}

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
    /// Opens the transport with the options this client has always sent.
    pub fn open(transport: &Transport, on_event: &mut dyn FnMut(Event)) -> Result<Connection> {
        Connection::open_with(transport, &Options::default(), on_event)
    }

    /// Opens the transport and runs the handshake and `SetOptions`, as
    /// remote-store.cc:initConnection does.
    pub fn open_with(
        transport: &Transport,
        options: &Options,
        on_event: &mut dyn FnMut(Event),
    ) -> Result<Connection> {
        let (to, from, child): (Box<dyn Write + Send>, Box<dyn Read + Send>, Option<Child>) =
            match transport {
                Transport::Grpc(cfg) => {
                    return Err(Error::Usage(format!(
                        "'{}' is a gRPC store, not a worker-protocol daemon",
                        cfg.url()
                    )));
                }
                Transport::Unix(path) => {
                    let s = UnixStream::connect(path).map_err(|e| {
                        Error::io(format!("cannot connect to '{}'", path.display()), e)
                    })?;
                    let r = s.try_clone().map_err(conn)?;
                    (Box::new(s), Box::new(r), None)
                }
                Transport::Ssh {
                    user,
                    host,
                    remote_program,
                } => {
                    let mut cmd = Command::new("ssh");
                    if let Some(u) = user {
                        cmd.arg("-l").arg(u);
                    }
                    // the same knob Nix's ssh.cc reads, so ControlMaster and friends can
                    // be set per run
                    if let Ok(opts) = std::env::var("NIX_SSHOPTS") {
                        cmd.args(opts.split_whitespace());
                    }
                    cmd.arg("-x")
                        .arg(host)
                        .args(remote_program)
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::inherit());
                    let mut c = cmd.spawn().map_err(|e| Error::io("cannot run ssh", e))?;
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
        conn.set_options(options, on_event)?;
        Ok(conn)
    }

    pub fn bytes_sent(&self) -> u64 {
        self.to.get_ref().bytes
    }

    fn flush(&mut self) -> Result<()> {
        self.to.flush().map_err(conn)
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
        self.flush()?;
        if read_u64(self.r())? != WORKER_MAGIC_2 {
            return Err(Error::Protocol("nix-daemon protocol mismatch".into()));
        }
        let daemon = read_u64(self.r())?;
        if (daemon & 0xff00) >> 8 != 1 {
            return Err(Error::Protocol(
                "Nix daemon protocol version not supported".into(),
            ));
        }
        self.minor = (daemon & 0xff).min(LATEST_MINOR);
        self.stats.round_trips += 1;
        if self.minor < MIN_MINOR {
            return Err(Error::Protocol(format!(
                "the Nix daemon speaks protocol 1.{}; this client needs 1.{MIN_MINOR} or newer",
                self.minor
            )));
        }
        if self.minor >= 38 {
            write_strs::<&str>(self.w(), &[])?;
            self.flush()?;
            read_strs(self.r())?;
            self.stats.round_trips += 1;
        }
        write_u64(self.w(), 0)?;
        write_u64(self.w(), 0)?;
        self.flush()?;
        self.daemon_nix_version = read_str(self.r())?;
        self.trusted = match read_u64(self.r())? {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        };
        self.stats.round_trips += 1;
        Ok(())
    }

    fn op(&mut self, op: Op) -> Result<()> {
        self.stats.ops += 1;
        self.stats.round_trips += 1;
        write_u64(self.w(), op as u64)
    }

    /// remote-store.cc:setOptions. Sends the same twelve fields plus an empty override map.
    fn set_options(&mut self, o: &Options, on_event: &mut dyn FnMut(Event)) -> Result<()> {
        self.op(Op::SetOptions)?;
        for v in [
            o.keep_failed.into(), // keep-failed
            o.keep_going.into(),  // keep-going
            0,                    // try-fallback
            LVL_INFO,             // verbosity
            o.max_jobs,           // max-jobs
            0,                    // max-silent-time
            1,                    // obsolete use-build-hook
            LVL_VOMIT,            // build verbosity: lvlVomit means verboseBuild = false
            0,                    // obsolete log type
            0,                    // obsolete print build trace
            o.cores,              // cores
            o.substitutes.into(), // substitute
        ] {
            write_u64(self.w(), v)?;
        }
        write_u64(self.w(), 0)?;
        self.process_stderr(on_event)
    }

    /// worker-protocol-connection.cc:processStderrReturn: drains the daemon's log stream until
    /// `STDERR_LAST` (the reply follows) or `STDERR_ERROR`.
    fn process_stderr(&mut self, on_event: &mut dyn FnMut(Event)) -> Result<()> {
        self.flush()?;
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
                    if let Some(drv) = self.activities.remove(&act) {
                        on_event(Event::Stopped(Stopped { drv_path: drv }));
                    }
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
                    if ty == RES_SET_PHASE
                        && let Some(Field::Str(phase)) = fields.first()
                        && let Some(drv) = self.activities.get(&act)
                    {
                        on_event(Event::Phase(Phase {
                            drv_path: drv.clone(),
                            phase: phase.clone(),
                        }));
                    }
                }
                STDERR_ERROR => {
                    let e = self.read_error()?;
                    self.sync_stats();
                    return Err(e);
                }
                STDERR_READ | STDERR_WRITE => {
                    return Err(Error::Protocol(
                        "daemon asked for a data stream this client does not provide".into(),
                    ));
                }
                other => {
                    return Err(Error::Protocol(format!(
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
                    return Err(Error::Protocol(format!(
                        "got unsupported field type {t:#x} from Nix daemon"
                    )));
                }
            };
            fields.push(f);
        }
        Ok(fields)
    }

    /// serialise.cc:readError (protocol >= 1.26).
    fn read_error(&mut self) -> Result<Error> {
        let ty = read_str(self.r())?;
        if ty != "Error" {
            return Err(Error::Protocol(format!("unexpected error type '{ty}'")));
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
        Ok(Error::Protocol(msg))
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
        read_strs(self.r())
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
        self.flush()?;
        let mut sink = FramedSink::new(&mut self.to);
        body(&mut sink).map_err(conn)?;
        sink.finish().map_err(conn)?;
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

    /// `QuerySubstitutablePathInfos` (the protocol >= 1.22 shape, no content
    /// addresses): the subset of `paths` some substituter has.
    pub fn query_substitutable_path_infos(
        &mut self,
        paths: &[String],
        on_event: &mut dyn FnMut(Event),
    ) -> Result<Vec<String>> {
        self.op(Op::QuerySubstitutablePathInfos)?;
        write_u64(self.w(), paths.len() as u64)?;
        for p in paths {
            write_str(self.w(), p.as_bytes())?;
            write_str(self.w(), b"")?;
        }
        self.process_stderr(on_event)?;
        let n = read_u64(self.r())?;
        let mut out = Vec::with_capacity(n.min(1 << 16) as usize);
        for _ in 0..n {
            let path = read_str(self.r())?;
            read_str(self.r())?; // deriver
            read_strs(self.r())?; // references
            read_u64(self.r())?; // download size
            read_u64(self.r())?; // NAR size
            out.push(path);
        }
        Ok(out)
    }

    /// indirect-root-store.cc:addPermRoot: a symlink at `root` to
    /// `store_path`, then `AddIndirectRoot`.
    pub fn add_perm_root(&mut self, store_path: &str, root: &Path) -> Result<()> {
        if root.starts_with(super::super::store::path::STORE_DIR) {
            return Err(Error::Protocol(format!(
                "creating a garbage collector root ({}) in the Nix store is forbidden",
                root.display()
            )));
        }
        replace_symlink(store_path, root)
            .map_err(|e| Error::io(format!("cannot create {}", root.display()), e))?;
        let root_s = root
            .to_str()
            .ok_or_else(|| Error::Usage("gc root path is not UTF-8".into()))?;
        self.add_indirect_root(root_s, &mut |_| {})
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
        sink(&mut self.from).map_err(conn)?;
        Ok(())
    }

    /// True when the daemon has sent bytes we have not consumed; a protocol bug otherwise.
    pub fn has_pending(&mut self) -> bool {
        !self.from.buffer().is_empty()
    }
}

/// file-system.cc:replaceSymlink: a temporary link renamed over `link`.
pub fn replace_symlink(target: &str, link: &Path) -> io::Result<()> {
    let parent = link.parent().unwrap_or(Path::new("/"));
    std::fs::create_dir_all(parent)?;
    let name = link.file_name().unwrap_or_default().to_string_lossy();
    for n in 0.. {
        let tmp = parent.join(format!(".{n}_{name}"));
        match std::os::unix::fs::symlink(target, &tmp) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
        return std::fs::rename(&tmp, link).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        });
    }
    unreachable!()
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
            Transport::parse("ssh-ng://bob@builder.example"),
            Some(Transport::Ssh {
                user: Some("bob".into()),
                host: "builder.example".into(),
                remote_program: vec!["nix-daemon".into(), "--stdio".into()],
            })
        );
        assert_eq!(
            Transport::parse("ssh-ng://builder.example/"),
            Some(Transport::Ssh {
                user: None,
                host: "builder.example".into(),
                remote_program: vec!["nix-daemon".into(), "--stdio".into()],
            })
        );
        assert_eq!(
            Transport::parse(
                "ssh-ng://builder.example?remote-program=nix-daemon%20--store%20local%3Froot%3D/tmp/s+--stdio"
            ),
            Some(Transport::Ssh {
                user: None,
                host: "builder.example".into(),
                remote_program: vec![
                    "nix-daemon".into(),
                    "--store".into(),
                    "local?root=/tmp/s".into(),
                    "--stdio".into()
                ],
            })
        );
        assert_eq!(Transport::parse("http://127.0.0.1:9455"), None);
    }
}
