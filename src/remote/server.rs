//! The reference builder: amber objects in a directory, a local Nix store behind it,
//! `nix-store --realise` for the build. For tests and benchmarks, not a product.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use prost::Message;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};

use crate::amber::objects::verify;
use crate::amber::tree::{Root, read_file, restore};
use crate::amber::{Key, Type};
use crate::store::drv::Derivation;
use crate::store::hash::{Hash, HashAlgo};
use crate::store::nar::{flat_hash, nar_hash_filtered};
use crate::store::path::{IngestionMethod, StorePath, make_fixed_output_path, make_text_path};

use super::proto::builder_server::{Builder, BuilderServer};
use super::proto::{
    BuildEvent, BuildRequest, Built, ContentAddress, Done, Failed, FetchRequest, HaveRequest,
    HaveResponse, Log, Manifest, NarChunk, PutRequest, PutResponse, Scheduled, Started,
    build_event, put_request,
};

const MAX_MESSAGE: usize = 64 << 20;

/// `Fetch` sends the NAR in pieces of this size.
const NAR_CHUNK: usize = 4 << 20;

#[derive(Clone)]
pub struct Builderd {
    dir: Arc<PathBuf>,
    builds: Arc<AtomicU64>,
}

impl Builderd {
    pub fn new(dir: PathBuf) -> io::Result<Builderd> {
        for sub in ["objects", "paths", "tmp"] {
            fs::create_dir_all(dir.join(sub))?;
        }
        Ok(Builderd {
            dir: Arc::new(dir),
            builds: Arc::new(AtomicU64::new(0)),
        })
    }

    fn object(&self, k: &Key) -> PathBuf {
        self.dir.join("objects").join(k.to_string())
    }

    fn by_path(&self, p: &StorePath) -> PathBuf {
        self.dir.join("paths").join(p.base_name())
    }

    /// Reads an object by key; the getter `amber::tree` restores from.
    fn get(&self, k: &Key) -> io::Result<Vec<u8>> {
        fs::read(self.object(k)).map_err(|e| io::Error::other(format!("object {k}: {e}")))
    }

    fn load_path(&self, p: &StorePath) -> Result<put_request::Body, String> {
        let bytes =
            fs::read(self.by_path(p)).map_err(|_| format!("builder does not have '{p}'"))?;
        PutRequest::decode(bytes.as_slice())
            .map_err(|e| format!("path record '{p}': {e}"))?
            .body
            .ok_or_else(|| "empty path record".into())
    }

    /// Writes `data` under `path` atomically.
    fn store(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        if path.exists() {
            return Ok(());
        }
        let tmp = self.dir.join("tmp").join(format!(
            "{}.{}",
            path.file_name().unwrap().to_string_lossy(),
            std::process::id()
        ));
        fs::write(&tmp, data)?;
        fs::rename(&tmp, path)
    }

    fn store_path_record(&self, path: &StorePath, body: put_request::Body) -> io::Result<()> {
        let bytes = PutRequest { body: Some(body) }.encode_to_vec();
        self.store(&self.by_path(path), &bytes)
    }

    /// Stores one message of the `Put` stream; returns (objects, paths) stored.
    fn put_one(&self, body: put_request::Body) -> Result<(u64, u64), Status> {
        let bad = |m: String| Status::invalid_argument(m);
        let io = |e: io::Error| Status::internal(e.to_string());
        match body {
            put_request::Body::Objects(batch) => {
                let n = batch.objects.len() as u64;
                for o in batch.objects {
                    let k = Key::parse(&o.key).map_err(bad)?;
                    verify(&k, &o.data).map_err(bad)?;
                    self.store(&self.object(&k), &o.data).map_err(io)?;
                }
                Ok((n, 0))
            }
            put_request::Body::Manifest(m) => {
                let path = StorePath::parse(&m.store_path).map_err(|e| bad(e.0))?;
                let nar_hash =
                    Hash::from_bytes(HashAlgo::Sha256, &m.nar_hash).map_err(|e| bad(e.0))?;
                match ContentAddress::try_from(m.ca) {
                    Ok(ContentAddress::Source) => {
                        // Cheap consistency check now; the NAR itself is verified at build time.
                        let expect = make_fixed_output_path(
                            IngestionMethod::Recursive,
                            &nar_hash,
                            path.name(),
                            &BTreeSet::new(),
                        )
                        .map_err(|e| bad(e.0))?;
                        if expect != path {
                            return Err(bad(format!(
                                "manifest path '{path}' does not match NAR hash (expected '{expect}')"
                            )));
                        }
                    }
                    Ok(ContentAddress::Text) => {
                        for r in &m.references {
                            StorePath::parse(r).map_err(|e| bad(e.0))?;
                        }
                    }
                    Err(_) => return Err(bad(format!("unknown content address kind {}", m.ca))),
                }
                manifest_root(&m).map_err(bad)?;
                for k in &m.keys {
                    let k = Key::parse(k).map_err(bad)?;
                    if !self.object(&k).exists() {
                        return Err(Status::failed_precondition(format!(
                            "manifest '{path}' needs missing object {k}"
                        )));
                    }
                }
                self.store_path_record(&path, put_request::Body::Manifest(m))
                    .map_err(io)?;
                Ok((0, 1))
            }
            put_request::Body::Drv(d) => {
                let path = StorePath::parse(&d.drv_path).map_err(|e| bad(e.0))?;
                let name = Derivation::name_from_path(&path).map_err(|e| bad(e.0))?;
                let content = Key::parse(&d.content).map_err(bad)?;
                let get = |k: &Key| self.get(k);
                let aterm = read_file(&get, &content)
                    .map_err(|e| Status::failed_precondition(format!("drv '{path}': {e}")))?;
                let text = std::str::from_utf8(&aterm).map_err(|e| bad(e.to_string()))?;
                let drv = Derivation::parse(text, name).map_err(|e| bad(e.0))?;
                let expect = drv.drv_path().map_err(|e| bad(e.0))?;
                if expect != path {
                    return Err(bad(format!(
                        "drv path '{path}' does not match its ATerm (expected '{expect}')"
                    )));
                }
                self.store_path_record(&path, put_request::Body::Drv(d))
                    .map_err(io)?;
                Ok((0, 1))
            }
        }
    }

    /// The ATerm of a stored drv, parsed.
    fn load_drv(&self, p: &StorePath) -> Result<(Derivation, Vec<u8>), String> {
        let put_request::Body::Drv(blob) = self.load_path(p)? else {
            return Err(format!("'{p}' is not a derivation"));
        };
        let content = Key::parse(&blob.content)?;
        let get = |k: &Key| self.get(k);
        let aterm = read_file(&get, &content).map_err(|e| e.to_string())?;
        let name = Derivation::name_from_path(p).map_err(|e| e.0)?;
        let text = std::str::from_utf8(&aterm).map_err(|e| e.to_string())?;
        let drv = Derivation::parse(text, name).map_err(|e| e.0)?;
        Ok((drv, aterm))
    }
}

/// The top-level node a manifest describes, checked against its key type.
fn manifest_root(m: &Manifest) -> Result<Root, String> {
    if !m.symlink_target.is_empty() {
        if !m.root.is_empty() {
            return Err("a symlink manifest carries no root key".into());
        }
        return Ok(Root::Symlink(m.symlink_target.as_bytes().to_vec()));
    }
    let key = Key::parse(&m.root)?;
    match key.type_() {
        Type::DirLeaf | Type::DirNode => Ok(Root::Dir(key)),
        Type::Blob | Type::FileNode => Ok(Root::File {
            key,
            executable: m.executable,
        }),
        t => Err(format!("root key type {t} cannot be a store path")),
    }
}

#[tonic::async_trait]
impl Builder for Builderd {
    async fn have(&self, req: Request<HaveRequest>) -> Result<Response<HaveResponse>, Status> {
        let req = req.into_inner();
        let mut resp = HaveResponse::default();
        for k in req.keys {
            let key = Key::parse(&k).map_err(Status::invalid_argument)?;
            if !self.object(&key).exists() {
                resp.missing_keys.push(k);
            }
        }
        for p in req.paths {
            let path = StorePath::parse(&p).map_err(|e| Status::invalid_argument(e.0))?;
            if !self.by_path(&path).exists() {
                resp.missing_paths.push(p);
            }
        }
        Ok(Response::new(resp))
    }

    async fn put(
        &self,
        req: Request<Streaming<PutRequest>>,
    ) -> Result<Response<PutResponse>, Status> {
        let mut stream = req.into_inner();
        let mut resp = PutResponse::default();
        while let Some(msg) = stream.message().await? {
            let Some(body) = msg.body else {
                return Err(Status::invalid_argument("empty PutRequest"));
            };
            let (objects, paths) = self.put_one(body)?;
            resp.objects += objects;
            resp.paths += paths;
        }
        Ok(Response::new(resp))
    }

    type BuildStream = ReceiverStream<Result<BuildEvent, Status>>;

    async fn build(
        &self,
        req: Request<BuildRequest>,
    ) -> Result<Response<Self::BuildStream>, Status> {
        let req = req.into_inner();
        let root =
            StorePath::parse(&req.root_drv_path).map_err(|e| Status::invalid_argument(e.0))?;
        let (tx, rx) = mpsc::channel(256);
        let this = self.clone();
        let id = self.builds.fetch_add(1, Ordering::Relaxed);
        tokio::spawn(async move {
            let events = Events(tx);
            let ok = match run_build(&this, id, &root, &req.outputs, &events).await {
                Ok(()) => true,
                Err(reason) => {
                    events
                        .send(build_event::Event::Failed(Failed {
                            drv_path: root.to_string(),
                            reason,
                        }))
                        .await;
                    false
                }
            };
            events
                .send(build_event::Event::Done(Done { success: ok }))
                .await;
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }

    type FetchStream = ReceiverStream<Result<NarChunk, Status>>;

    async fn fetch(
        &self,
        req: Request<FetchRequest>,
    ) -> Result<Response<Self::FetchStream>, Status> {
        let path = StorePath::parse(&req.into_inner().store_path)
            .map_err(|e| Status::invalid_argument(e.0))?;
        let (tx, rx) = mpsc::channel(4);
        tokio::task::spawn_blocking(move || {
            let mut splitter = NarSplitter {
                buf: Vec::with_capacity(NAR_CHUNK),
                tx: &tx,
            };
            let r = crate::store::nar::dump_path(
                Path::new(&path.to_string()),
                &mut splitter,
                &mut |_, _| true,
            )
            .and_then(|()| splitter.finish());
            if let Err(e) = r {
                let _ = tx.blocking_send(Err(Status::not_found(e.to_string())));
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

/// Cuts the NAR stream into `NAR_CHUNK` pieces for `Fetch`.
struct NarSplitter<'a> {
    buf: Vec<u8>,
    tx: &'a mpsc::Sender<Result<NarChunk, Status>>,
}

impl NarSplitter<'_> {
    fn send(&mut self) -> io::Result<()> {
        let data = std::mem::replace(&mut self.buf, Vec::with_capacity(NAR_CHUNK));
        self.tx
            .blocking_send(Ok(NarChunk { data }))
            .map_err(|_| io::Error::other("client went away"))
    }

    fn finish(mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        self.send()
    }
}

impl Write for NarSplitter<'_> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let n = (NAR_CHUNK - self.buf.len()).min(data.len());
        self.buf.extend_from_slice(&data[..n]);
        if self.buf.len() == NAR_CHUNK {
            self.send()?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Events(mpsc::Sender<Result<BuildEvent, Status>>);

impl Events {
    async fn send(&self, ev: build_event::Event) {
        let _ = self.0.send(Ok(BuildEvent { event: Some(ev) })).await;
    }

    async fn log(&self, drv_path: String, line: Vec<u8>) {
        self.send(build_event::Event::Log(Log { drv_path, line }))
            .await;
    }
}

/// Restores a source tree from its objects at `dest`, checks the NAR hash and size, and
/// checks that the content yields `store_path` under the manifest's addressing.
fn materialize_source(d: &Builderd, m: &Manifest, dest: &Path) -> Result<(), String> {
    let path = StorePath::parse(&m.store_path).map_err(|e| e.0)?;
    let nar_hash = Hash::from_bytes(HashAlgo::Sha256, &m.nar_hash).map_err(|e| e.0)?;
    let root = manifest_root(m)?;
    let get = |k: &Key| d.get(k);
    restore(&get, &root, dest).map_err(|e| format!("restoring '{}': {e}", m.store_path))?;
    let (got, size) =
        nar_hash_filtered(dest, HashAlgo::Sha256, &mut |_, _| true).map_err(|e| e.to_string())?;
    if got != nar_hash || size != m.nar_size {
        return Err(format!(
            "'{}': NAR hash mismatch (got {}, {size} bytes)",
            m.store_path,
            got.to_base16()
        ));
    }
    let expect = match ContentAddress::try_from(m.ca) {
        Ok(ContentAddress::Source) => make_fixed_output_path(
            IngestionMethod::Recursive,
            &nar_hash,
            path.name(),
            &BTreeSet::new(),
        )
        .map_err(|e| e.0)?,
        Ok(ContentAddress::Text) => {
            let meta = fs::symlink_metadata(dest).map_err(|e| e.to_string())?;
            if !meta.is_file() || meta.permissions().mode() & 0o100 != 0 {
                return Err(format!(
                    "'{path}': a text path must be one regular, non-executable file"
                ));
            }
            let refs: BTreeSet<StorePath> = m
                .references
                .iter()
                .map(|r| StorePath::parse(r).map_err(|e| e.0))
                .collect::<Result<_, _>>()?;
            let h = flat_hash(dest, HashAlgo::Sha256).map_err(|e| e.to_string())?;
            make_text_path(path.name(), &h, &refs).map_err(|e| e.0)?
        }
        Err(_) => return Err(format!("unknown content address kind {}", m.ca)),
    };
    if expect != path {
        return Err(format!(
            "'{path}': content does not yield this store path (expected '{expect}')"
        ));
    }
    Ok(())
}

/// The Nix expression that registers one materialized object at its planned path.
/// `builtins.path` recomputes a source path; `builtins.toFile` with appended context
/// recomputes a text path (drv files included) with its references.
fn register_expr(path: &StorePath, file: &Path, ca: ContentAddress, refs: &[String]) -> String {
    match ca {
        ContentAddress::Source => format!(
            "  (builtins.path {{ path = \"{}\"; name = \"{}\"; }})\n",
            file.display(),
            path.name()
        ),
        ContentAddress::Text => {
            let ctx: String = refs
                .iter()
                .map(|r| format!("\"{r}\" = {{ path = true; }}; "))
                .collect();
            format!(
                "  (builtins.toFile \"{}\" (builtins.appendContext (builtins.readFile \"{}\") {{ {ctx}}}))\n",
                path.name(),
                file.display()
            )
        }
    }
}

/// Loads `s` after the sources its manifest references: a `toFile` text
/// may point at a tree no derivation lists, and registration needs the
/// references valid first.
fn collect_source(
    d: &Builderd,
    s: &StorePath,
    seen: &mut HashSet<StorePath>,
    sources: &mut Vec<(StorePath, Manifest)>,
) -> Result<(), String> {
    if !seen.insert(s.clone()) {
        return Ok(());
    }
    let put_request::Body::Manifest(m) = d.load_path(s)? else {
        return Err(format!("'{s}' is not a source tree"));
    };
    for r in &m.references {
        let r = StorePath::parse(r).map_err(|e| e.0)?;
        if !r.is_derivation() {
            collect_source(d, &r, seen, sources)?;
        }
    }
    sources.push((s.clone(), m));
    Ok(())
}

async fn run_build(
    d: &Builderd,
    id: u64,
    root: &StorePath,
    _outputs: &[String],
    events: &Events,
) -> Result<(), String> {
    // 1. Closure of the root from the object store: sources first, then drvs in
    //    dependency order.
    let mut sources: Vec<(StorePath, Manifest)> = Vec::new();
    let mut drvs: Vec<(StorePath, Derivation, Vec<u8>)> = Vec::new();
    let mut seen: HashSet<StorePath> = HashSet::new();
    let mut stack: Vec<(StorePath, bool)> = vec![(root.clone(), false)];
    while let Some((p, expanded)) = stack.pop() {
        if expanded {
            let (drv, aterm) = d.load_drv(&p)?;
            drvs.push((p, drv, aterm));
            continue;
        }
        if !seen.insert(p.clone()) {
            continue;
        }
        let (drv, _) = d.load_drv(&p)?;
        for s in &drv.input_srcs {
            collect_source(d, s, &mut seen, &mut sources)?;
        }
        stack.push((p.clone(), true));
        for i in drv.input_drvs.keys() {
            if !seen.contains(i) {
                stack.push((i.clone(), false));
            }
        }
    }

    // 2. Which of them does the Nix store lack? One call.
    let all: Vec<String> = sources
        .iter()
        .map(|(p, _)| p.to_string())
        .chain(drvs.iter().map(|(p, _, _)| p.to_string()))
        .collect();
    let out = tokio::process::Command::new("nix-store")
        .arg("--check-validity")
        .arg("--print-invalid")
        .args(&all)
        .output()
        .await
        .map_err(|e| format!("nix-store --check-validity: {e}"))?;
    let invalid: HashSet<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();

    // 3. Materialize the missing ones and register them through one `nix eval`.
    if !invalid.is_empty() {
        let tmp = d
            .dir
            .join("tmp")
            .join(format!("build-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
        let mut expr = String::from("[\n");
        let mut expected = Vec::new();
        for (p, m) in &sources {
            if !invalid.contains(&p.to_string()) {
                continue;
            }
            let dest = tmp.join(p.base_name());
            let (d2, m2, dest2) = (d.clone(), m.clone(), dest.clone());
            tokio::task::spawn_blocking(move || materialize_source(&d2, &m2, &dest2))
                .await
                .map_err(|e| e.to_string())??;
            let ca = ContentAddress::try_from(m.ca).map_err(|e| e.to_string())?;
            expr.push_str(&register_expr(p, &dest, ca, &m.references));
            expected.push(p.to_string());
        }
        for (p, drv, aterm) in &drvs {
            if !invalid.contains(&p.to_string()) {
                continue;
            }
            let dest = tmp.join(p.base_name());
            fs::write(&dest, aterm).map_err(|e| e.to_string())?;
            let refs: Vec<String> = drv
                .input_srcs
                .iter()
                .chain(drv.input_drvs.keys())
                .map(|r| r.to_string())
                .collect();
            expr.push_str(&register_expr(p, &dest, ContentAddress::Text, &refs));
            expected.push(p.to_string());
        }
        expr.push_str("]\n");
        let expr_path = tmp.join("register.nix");
        fs::write(&expr_path, expr).map_err(|e| e.to_string())?;
        events
            .log(
                String::new(),
                format!("registering {} paths in the Nix store", expected.len()).into_bytes(),
            )
            .await;
        let out = tokio::process::Command::new("nix")
            .args([
                "--extra-experimental-features",
                "nix-command",
                "eval",
                "--impure",
                "--json",
                "--file",
            ])
            .arg(&expr_path)
            .output()
            .await
            .map_err(|e| format!("nix eval: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "registering paths failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        let got: Vec<String> =
            serde_json::from_slice(&out.stdout).map_err(|e| format!("nix eval output: {e}"))?;
        if got != expected {
            return Err(format!(
                "registered paths differ from the plan: got {got:?}, expected {expected:?}"
            ));
        }
        let _ = fs::remove_dir_all(&tmp);
    }

    // 4. Build.
    events
        .send(build_event::Event::Scheduled(Scheduled {
            drv_paths: vec![root.to_string()],
        }))
        .await;
    let mut child = tokio::process::Command::new("nix-store")
        .args(["--realise", "--log-format", "internal-json"])
        .arg(root.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("nix-store --realise: {e}"))?;
    let mut lines = BufReader::new(child.stderr.take().unwrap()).split(b'\n');
    let mut activities: HashMap<u64, String> = HashMap::new();
    let mut tail: Vec<String> = Vec::new();
    while let Some(line) = lines.next_segment().await.map_err(|e| e.to_string())? {
        match parse_log_line(&line, &mut activities) {
            LogLine::Started(drv) => {
                events
                    .send(build_event::Event::Started(Started { drv_path: drv }))
                    .await
            }
            LogLine::Log(drv, text) => {
                if tail.len() == 20 {
                    tail.remove(0);
                }
                tail.push(String::from_utf8_lossy(&text).into_owned());
                events.log(drv, text).await
            }
            LogLine::Skip => {}
        }
    }
    let status = child.wait().await.map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("nix-store --realise failed:\n{}", tail.join("\n")));
    }

    // 5. Outputs: `nix-store -q --outputs` lists them in output-name order.
    let (_, root_drv, _) = drvs.last().ok_or("no root")?;
    let out = tokio::process::Command::new("nix-store")
        .args(["-q", "--outputs"])
        .arg(root.to_string())
        .output()
        .await
        .map_err(|e| e.to_string())?;
    let paths: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    for (name, path) in root_drv.outputs.keys().zip(paths) {
        events
            .send(build_event::Event::Built(Built {
                drv_path: root.to_string(),
                output: name.clone(),
                store_path: path,
                signature: String::new(),
            }))
            .await;
    }
    Ok(())
}

enum LogLine {
    Started(String),
    Log(String, Vec<u8>),
    Skip,
}

/// Maps one `--log-format internal-json` stderr line to an event. Activity type 105 is
/// actBuild; result type 101 is resBuildLogLine (logging.hh).
fn parse_log_line(line: &[u8], activities: &mut HashMap<u64, String>) -> LogLine {
    let Some(json) = line.strip_prefix(b"@nix ") else {
        return LogLine::Log(String::new(), line.to_vec());
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(json) else {
        return LogLine::Log(String::new(), line.to_vec());
    };
    let id = v["id"].as_u64().unwrap_or(0);
    match v["action"].as_str() {
        Some("start") if v["type"].as_u64() == Some(105) => {
            let drv = v["fields"][0].as_str().unwrap_or("").to_string();
            activities.insert(id, drv.clone());
            LogLine::Started(drv)
        }
        Some("result") if v["type"].as_u64() == Some(101) => {
            let drv = activities.get(&id).cloned().unwrap_or_default();
            LogLine::Log(
                drv,
                v["fields"][0].as_str().unwrap_or("").as_bytes().to_vec(),
            )
        }
        Some("msg") => LogLine::Log(
            String::new(),
            v["msg"].as_str().unwrap_or("").as_bytes().to_vec(),
        ),
        Some("stop") => {
            activities.remove(&id);
            LogLine::Skip
        }
        _ => LogLine::Skip,
    }
}

/// Serves on `listen` until the process ends.
pub async fn serve(
    listen: std::net::SocketAddr,
    dir: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let svc = BuilderServer::new(Builderd::new(dir)?)
        .max_decoding_message_size(MAX_MESSAGE)
        .max_encoding_message_size(MAX_MESSAGE);
    let _ = writeln!(io::stderr(), "ietsp-builderd listening on {listen}");
    tonic::transport::Server::builder()
        .initial_stream_window_size(8 << 20)
        .initial_connection_window_size(32 << 20)
        .add_service(svc)
        .serve(listen)
        .await?;
    Ok(())
}
