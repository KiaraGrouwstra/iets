//! The reference builder: blobs in a directory, a local Nix store behind it, `nix-store
//! --realise` for the build. For tests and benchmarks, not a product.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{self, Read, Write};
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

use crate::store::drv::Derivation;
use crate::store::hash::{Hash, HashAlgo, Hasher};
use crate::store::nar::{flat_hash, restore_path};
use crate::store::path::{IngestionMethod, StorePath, make_fixed_output_path, make_text_path};

use super::chunk::{CHUNK_MAX, Chunker, from_digest, sha256, to_digest};
use super::proto::builder_server::{Builder, BuilderServer};
use super::proto::{
    BuildEvent, BuildRequest, Built, Chunk, ContentAddress, Done, DrvBlob, Failed, FetchRequest,
    HaveRequest, HaveResponse, Log, Manifest, PutRequest, PutResponse, Scheduled, Started,
    build_event, put_request,
};

const MAX_MESSAGE: usize = 16 << 20;

#[derive(Clone)]
pub struct Builderd {
    dir: Arc<PathBuf>,
    builds: Arc<AtomicU64>,
}

impl Builderd {
    pub fn new(dir: PathBuf) -> io::Result<Builderd> {
        for sub in ["blobs", "objects", "paths", "tmp"] {
            fs::create_dir_all(dir.join(sub))?;
        }
        Ok(Builderd {
            dir: Arc::new(dir),
            builds: Arc::new(AtomicU64::new(0)),
        })
    }

    fn blob(&self, h: &Hash) -> PathBuf {
        self.dir.join("blobs").join(h.to_base16())
    }

    fn object(&self, h: &Hash) -> PathBuf {
        self.dir.join("objects").join(h.to_base16())
    }

    fn by_path(&self, p: &StorePath) -> PathBuf {
        self.dir.join("paths").join(p.base_name())
    }

    fn load_object(&self, p: &StorePath) -> Result<put_request::Body, String> {
        let hex = fs::read_to_string(self.by_path(p))
            .map_err(|_| format!("builder does not have '{p}'"))?;
        let h = Hash::from_base16(HashAlgo::Sha256, hex.trim()).map_err(|e| e.0)?;
        let bytes = fs::read(self.object(&h)).map_err(|e| format!("object {hex}: {e}"))?;
        PutRequest::decode(bytes.as_slice())
            .map_err(|e| format!("object {hex}: {e}"))?
            .body
            .ok_or_else(|| "empty object".into())
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

    fn store_object(&self, h: &Hash, path: &StorePath, body: put_request::Body) -> io::Result<()> {
        let bytes = PutRequest { body: Some(body) }.encode_to_vec();
        self.store(&self.object(h), &bytes)?;
        self.store(&self.by_path(path), h.to_base16().as_bytes())
    }

    fn put_one(&self, body: put_request::Body) -> Result<Hash, Status> {
        let bad = |m: String| Status::invalid_argument(m);
        let io = |e: io::Error| Status::internal(e.to_string());
        match body {
            put_request::Body::Chunk(c) => {
                if c.data.len() > CHUNK_MAX {
                    return Err(bad("chunk larger than 4 MiB".into()));
                }
                let h = sha256(&c.data);
                if let Some(d) = &c.digest
                    && from_digest(d).map_err(bad)? != h
                {
                    return Err(bad("chunk digest does not match its data".into()));
                }
                self.store(&self.blob(&h), &c.data).map_err(io)?;
                Ok(h)
            }
            put_request::Body::Manifest(m) => {
                let path = StorePath::parse(&m.store_path).map_err(|e| bad(e.0))?;
                let nar_hash = from_digest(
                    m.nar_hash
                        .as_ref()
                        .ok_or_else(|| bad("no nar_hash".into()))?,
                )
                .map_err(bad)?;
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
                for c in &m.chunks {
                    let h = from_digest(c).map_err(bad)?;
                    if !self.blob(&h).exists() {
                        return Err(Status::failed_precondition(format!(
                            "manifest '{path}' references missing chunk {}",
                            h.to_base16()
                        )));
                    }
                }
                self.store_object(&nar_hash, &path, put_request::Body::Manifest(m))
                    .map_err(io)?;
                Ok(nar_hash)
            }
            put_request::Body::Drv(d) => {
                let path = StorePath::parse(&d.drv_path).map_err(|e| bad(e.0))?;
                let name = Derivation::name_from_path(&path).map_err(|e| bad(e.0))?;
                let text = std::str::from_utf8(&d.aterm).map_err(|e| bad(e.to_string()))?;
                let drv = Derivation::parse(text, name).map_err(|e| bad(e.0))?;
                let expect = drv.drv_path().map_err(|e| bad(e.0))?;
                if expect != path {
                    return Err(bad(format!(
                        "drv path '{path}' does not match its ATerm (expected '{expect}')"
                    )));
                }
                let h = sha256(&d.aterm);
                self.store_object(&h, &path, put_request::Body::Drv(d))
                    .map_err(io)?;
                Ok(h)
            }
        }
    }
}

#[tonic::async_trait]
impl Builder for Builderd {
    async fn have(&self, req: Request<HaveRequest>) -> Result<Response<HaveResponse>, Status> {
        let req = req.into_inner();
        let mut resp = HaveResponse::default();
        for d in &req.chunks {
            let h = from_digest(d).map_err(Status::invalid_argument)?;
            if !self.blob(&h).exists() {
                resp.missing_chunks.push(to_digest(&h));
            }
        }
        for d in &req.objects {
            let h = from_digest(d).map_err(Status::invalid_argument)?;
            if !self.object(&h).exists() {
                resp.missing_objects.push(to_digest(&h));
            }
        }
        Ok(Response::new(resp))
    }

    async fn put(
        &self,
        req: Request<Streaming<PutRequest>>,
    ) -> Result<Response<PutResponse>, Status> {
        let mut stream = req.into_inner();
        let mut stored = Vec::new();
        while let Some(msg) = stream.message().await? {
            let Some(body) = msg.body else {
                return Err(Status::invalid_argument("empty PutRequest"));
            };
            stored.push(to_digest(&self.put_one(body)?));
        }
        Ok(Response::new(PutResponse { stored }))
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

    type FetchStream = ReceiverStream<Result<Chunk, Status>>;

    async fn fetch(
        &self,
        req: Request<FetchRequest>,
    ) -> Result<Response<Self::FetchStream>, Status> {
        let path = StorePath::parse(&req.into_inner().store_path)
            .map_err(|e| Status::invalid_argument(e.0))?;
        let (tx, rx) = mpsc::channel(4);
        tokio::task::spawn_blocking(move || {
            let mut chunker = Chunker::new(|data: Vec<u8>| {
                let c = Chunk {
                    digest: Some(to_digest(&sha256(&data))),
                    data,
                };
                tx.blocking_send(Ok(c))
                    .map_err(|_| io::Error::other("client went away"))
            });
            let r = crate::store::nar::dump_path(
                Path::new(&path.to_string()),
                &mut chunker,
                &mut |_, _| true,
            )
            .and_then(|()| chunker.finish());
            if let Err(e) = r {
                let _ = tx.blocking_send(Err(Status::not_found(e.to_string())));
            }
        });
        Ok(Response::new(ReceiverStream::new(rx)))
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

/// Reads the chunks of a manifest as one NAR stream while hashing it.
struct ChunkReader {
    files: std::vec::IntoIter<PathBuf>,
    cur: Option<fs::File>,
    hasher: Hasher,
}

impl Read for ChunkReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if let Some(f) = &mut self.cur {
                let n = f.read(buf)?;
                if n > 0 {
                    self.hasher.update(&buf[..n]);
                    return Ok(n);
                }
                self.cur = None;
            }
            match self.files.next() {
                Some(p) => self.cur = Some(fs::File::open(p)?),
                None => return Ok(0),
            }
        }
    }
}

/// Restores a source tree from its chunks at `dest`, checks the NAR hash and size, and
/// checks that the content yields `store_path` under the manifest's addressing.
fn materialize_source(d: &Builderd, m: &Manifest, dest: &Path) -> Result<(), String> {
    let path = StorePath::parse(&m.store_path).map_err(|e| e.0)?;
    let nar_hash = from_digest(m.nar_hash.as_ref().ok_or("no nar_hash")?)?;
    let files: Vec<PathBuf> = m
        .chunks
        .iter()
        .map(|c| from_digest(c).map(|h| d.blob(&h)))
        .collect::<Result<_, _>>()?;
    let mut r = ChunkReader {
        files: files.into_iter(),
        cur: None,
        hasher: Hasher::new(HashAlgo::Sha256),
    };
    restore_path(&mut r, dest).map_err(|e| format!("restoring '{}': {e}", m.store_path))?;
    let mut rest = Vec::new();
    r.read_to_end(&mut rest).map_err(|e| e.to_string())?;
    if !rest.is_empty() {
        return Err(format!("'{}': trailing bytes after NAR", m.store_path));
    }
    let size = r.hasher.bytes_written();
    let got = r.hasher.finish();
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
    let put_request::Body::Manifest(m) = d.load_object(s)? else {
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
    let mut drvs: Vec<(StorePath, Derivation, DrvBlob)> = Vec::new();
    let mut seen: HashSet<StorePath> = HashSet::new();
    let mut stack: Vec<(StorePath, bool)> = vec![(root.clone(), false)];
    while let Some((p, expanded)) = stack.pop() {
        if expanded {
            let put_request::Body::Drv(blob) = d.load_object(&p)? else {
                return Err(format!("'{p}' is not a derivation"));
            };
            let name = Derivation::name_from_path(&p).map_err(|e| e.0)?;
            let text = std::str::from_utf8(&blob.aterm).map_err(|e| e.to_string())?;
            let drv = Derivation::parse(text, name).map_err(|e| e.0)?;
            drvs.push((p, drv, blob));
            continue;
        }
        if !seen.insert(p.clone()) {
            continue;
        }
        let put_request::Body::Drv(blob) = d.load_object(&p)? else {
            return Err(format!("'{p}' is not a derivation"));
        };
        let name = Derivation::name_from_path(&p).map_err(|e| e.0)?;
        let text = std::str::from_utf8(&blob.aterm).map_err(|e| e.to_string())?;
        let drv = Derivation::parse(text, name).map_err(|e| e.0)?;
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
        for (p, drv, blob) in &drvs {
            if !invalid.contains(&p.to_string()) {
                continue;
            }
            let dest = tmp.join(p.base_name());
            fs::write(&dest, &blob.aterm).map_err(|e| e.to_string())?;
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
