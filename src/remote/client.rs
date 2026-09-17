//! The client side: Have -> Put -> Build, three round trips before the build starts.

use std::collections::HashSet;
use std::io;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;

use crate::amber::tree::{Root, Sink, ingest_bytes};
use crate::amber::{Key, Object as AmberObject};
use crate::store::path::StorePath;

use super::plan::{Object, Plan, SourceKind};
use super::proto::builder_client::BuilderClient;
use super::proto::{
    BuildRequest, ContentAddress, DrvBlob, FetchRequest, HaveRequest, Manifest, ObjectBatch,
    PutRequest, build_event, put_request,
};

pub const MAX_MESSAGE: usize = 64 << 20;

/// Objects are sent in batches of about this many bytes.
const BATCH_BYTES: usize = 1 << 20;

/// What the builder reported it lacks.
struct Missing {
    keys: HashSet<Key>,
    paths: HashSet<String>,
}

/// Wall-clock and volume figures for one `build` call.
#[derive(Debug, Default, Clone)]
pub struct Timings {
    pub connect: Duration,
    pub have: Duration,
    pub put: Duration,
    pub build: Duration,
    pub have_keys: usize,
    pub have_paths: usize,
    pub missing: usize,
    pub put_objects: usize,
    pub put_paths: usize,
    pub put_bytes: u64,
}

impl Timings {
    pub fn report(&self, plan: &Plan, plan_time: Duration) -> String {
        let stats = plan.key_stats();
        format!(
            "plan: {} sources, {} drvs, {:.3}s\nobjects: {} keys, {} unique, {} shared by two or more trees\nconnect: {:.3}s\nhave: {} keys, {} paths, {} missing, {:.3}s\nput: {} objects, {} paths, {} bytes, {:.3}s\nbuild: {:.3}s",
            plan.sources(),
            plan.drvs(),
            plan_time.as_secs_f64(),
            stats.emitted,
            stats.unique,
            stats.shared,
            self.connect.as_secs_f64(),
            self.have_keys,
            self.have_paths,
            self.missing,
            self.have.as_secs_f64(),
            self.put_objects,
            self.put_paths,
            self.put_bytes,
            self.put.as_secs_f64(),
            self.build.as_secs_f64(),
        )
    }
}

#[derive(Debug)]
pub struct ClientError(pub String);

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ClientError {}

impl From<tonic::Status> for ClientError {
    fn from(s: tonic::Status) -> Self {
        ClientError(format!("builder: {}", s.message()))
    }
}

impl From<tonic::transport::Error> for ClientError {
    fn from(e: tonic::transport::Error) -> Self {
        ClientError(format!("transport: {e}"))
    }
}

impl From<io::Error> for ClientError {
    fn from(e: io::Error) -> Self {
        ClientError(e.to_string())
    }
}

/// Uploads what the builder lacks and builds `root`. Events reach `on_event` as they arrive.
/// Returns once the builder sends Done or closes the stream. With `upload_only` the call
/// stops after `Put`.
pub async fn build(
    url: &str,
    plan: &Plan,
    root: &StorePath,
    outputs: &[String],
    upload_only: bool,
    on_event: &mut dyn FnMut(build_event::Event),
) -> Result<Timings, ClientError> {
    let mut t = Timings::default();
    let t0 = Instant::now();
    let channel = Channel::from_shared(url.to_string())
        .map_err(|e| ClientError(format!("bad url '{url}': {e}")))?
        .initial_stream_window_size(8 << 20)
        .initial_connection_window_size(32 << 20)
        .connect()
        .await?;
    let mut client = BuilderClient::new(channel)
        .max_decoding_message_size(MAX_MESSAGE)
        .max_encoding_message_size(MAX_MESSAGE);
    t.connect = t0.elapsed();

    let t1 = Instant::now();
    let mut req = HaveRequest::default();
    let mut seen: HashSet<Key> = HashSet::new();
    for o in &plan.objects {
        for k in &o.tree().keys {
            if seen.insert(*k) {
                req.keys.push(k.as_bytes().to_vec());
            }
        }
        req.paths.push(o.store_path().to_string());
    }
    t.have_keys = req.keys.len();
    t.have_paths = req.paths.len();
    let resp = client.have(req).await?.into_inner();
    let missing = Missing {
        keys: resp
            .missing_keys
            .iter()
            .map(|k| Key::parse(k).map_err(ClientError))
            .collect::<Result<_, _>>()?,
        paths: resp.missing_paths.into_iter().collect(),
    };
    t.missing = missing.keys.len() + missing.paths.len();
    t.have = t1.elapsed();

    let t2 = Instant::now();
    if t.missing > 0 {
        let (tx, rx) = tokio::sync::mpsc::channel::<PutRequest>(16);
        let objects: Vec<Object> = plan.objects.clone();
        let producer = tokio::task::spawn_blocking(move || produce(&objects, missing, tx));
        let ack = client.put(ReceiverStream::new(rx)).await?.into_inner();
        let stats = producer
            .await
            .map_err(|e| ClientError(format!("upload thread: {e}")))??;
        t.put_objects = stats.objects;
        t.put_paths = stats.paths;
        t.put_bytes = stats.bytes;
        if ack.objects as usize + ack.paths as usize != t.missing {
            return Err(ClientError(format!(
                "builder stored {} objects and {} paths of {} missing",
                ack.objects, ack.paths, t.missing
            )));
        }
    }
    t.put = t2.elapsed();
    if upload_only {
        return Ok(t);
    }

    let t3 = Instant::now();
    let mut events = client
        .build(BuildRequest {
            root_drv_path: root.to_string(),
            outputs: outputs.to_vec(),
            keep_going: false,
            priority: 0,
        })
        .await?
        .into_inner();
    while let Some(ev) = events.next().await {
        let ev = ev?;
        let Some(ev) = ev.event else { continue };
        let done = matches!(ev, build_event::Event::Done(_));
        on_event(ev);
        if done {
            break;
        }
    }
    t.build = t3.elapsed();
    Ok(t)
}

#[derive(Default)]
struct PutStats {
    objects: usize,
    paths: usize,
    bytes: u64,
}

/// Sends the objects a tree walk emits, those still missing on the builder, in batches. A
/// key is sent once: the first tree that emits it removes it from `pending`.
struct Uploader<'a> {
    tx: &'a tokio::sync::mpsc::Sender<PutRequest>,
    state: Mutex<UploadState>,
}

struct UploadState {
    pending: HashSet<Key>,
    batch: Vec<super::proto::Object>,
    batch_bytes: usize,
    stats: PutStats,
    closed: bool,
}

impl Uploader<'_> {
    fn flush_locked(st: &mut UploadState, tx: &tokio::sync::mpsc::Sender<PutRequest>) {
        if st.batch.is_empty() || st.closed {
            return;
        }
        let req = PutRequest {
            body: Some(put_request::Body::Objects(ObjectBatch {
                objects: std::mem::take(&mut st.batch),
            })),
        };
        st.batch_bytes = 0;
        if tx.blocking_send(req).is_err() {
            st.closed = true;
        }
    }

    fn flush(&self) {
        let mut st = self.state.lock().unwrap();
        Self::flush_locked(&mut st, self.tx);
    }

    fn send(&self, req: PutRequest) -> Result<(), ClientError> {
        let mut st = self.state.lock().unwrap();
        Self::flush_locked(&mut st, self.tx);
        if st.closed || self.tx.blocking_send(req).is_err() {
            st.closed = true;
            return Err(closed());
        }
        Ok(())
    }
}

impl Sink for Uploader<'_> {
    fn emit(&self, o: AmberObject) {
        let mut st = self.state.lock().unwrap();
        if !st.pending.remove(&o.key) {
            return;
        }
        st.stats.objects += 1;
        st.stats.bytes += o.bytes.len() as u64;
        st.batch_bytes += o.bytes.len();
        st.batch.push(super::proto::Object {
            key: o.key.as_bytes().to_vec(),
            data: o.bytes,
        });
        if st.batch_bytes >= BATCH_BYTES {
            Self::flush_locked(&mut st, self.tx);
        }
    }
}

fn closed() -> ClientError {
    ClientError("builder closed the upload stream".into())
}

/// Runs on a blocking thread: re-walks each tree with missing objects and sends them, then
/// the tree's manifest or drv when the builder lacks the path.
fn produce(
    objects: &[Object],
    missing: Missing,
    tx: tokio::sync::mpsc::Sender<PutRequest>,
) -> Result<PutStats, ClientError> {
    let up = Uploader {
        tx: &tx,
        state: Mutex::new(UploadState {
            pending: missing.keys,
            batch: Vec::new(),
            batch_bytes: 0,
            stats: PutStats::default(),
            closed: false,
        }),
    };
    for o in objects {
        let tree = o.tree();
        let needs_objects = {
            let st = up.state.lock().unwrap();
            tree.keys.iter().any(|k| st.pending.contains(k))
        };
        if needs_objects {
            let root = match o {
                Object::Source { data, .. } => data.ingest(&up)?,
                Object::Drv { aterm, .. } => Root::File {
                    key: ingest_bytes(aterm, &up),
                    executable: false,
                },
            };
            if root != tree.root {
                return Err(ClientError(format!(
                    "'{}' changed while uploading",
                    o.store_path()
                )));
            }
            if up.state.lock().unwrap().closed {
                return Err(closed());
            }
        }
        if !missing.paths.contains(&o.store_path().to_string()) {
            continue;
        }
        let body = match o {
            Object::Source {
                path,
                kind,
                nar_hash,
                nar_size,
                tree,
                ..
            } => {
                let (ca, references) = match kind {
                    SourceKind::Source => (ContentAddress::Source, Vec::new()),
                    SourceKind::Text { references } => (
                        ContentAddress::Text,
                        references.iter().map(|r| r.to_string()).collect(),
                    ),
                };
                let (root, executable, symlink_target) = match &tree.root {
                    Root::Dir(k) => (k.as_bytes().to_vec(), false, String::new()),
                    Root::File { key, executable } => {
                        (key.as_bytes().to_vec(), *executable, String::new())
                    }
                    Root::Symlink(t) => {
                        (Vec::new(), false, String::from_utf8_lossy(t).into_owned())
                    }
                };
                put_request::Body::Manifest(Manifest {
                    store_path: path.to_string(),
                    nar_hash: nar_hash.bytes().to_vec(),
                    nar_size: *nar_size,
                    ca: ca as i32,
                    references,
                    root,
                    executable,
                    symlink_target,
                    keys: tree.keys.iter().map(|k| k.as_bytes().to_vec()).collect(),
                })
            }
            Object::Drv { path, tree, .. } => put_request::Body::Drv(DrvBlob {
                drv_path: path.to_string(),
                content: tree
                    .root
                    .key()
                    .expect("a drv is a file")
                    .as_bytes()
                    .to_vec(),
            }),
        };
        up.send(PutRequest { body: Some(body) })?;
        up.state.lock().unwrap().stats.paths += 1;
    }
    up.flush();
    let st = up.state.into_inner().unwrap();
    if st.closed {
        return Err(closed());
    }
    Ok(st.stats)
}

/// `build` on a fresh multi-thread runtime, for callers without one.
pub fn build_blocking(
    url: &str,
    plan: &Plan,
    root: &StorePath,
    outputs: &[String],
    upload_only: bool,
    on_event: &mut dyn FnMut(build_event::Event),
) -> Result<Timings, ClientError> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| ClientError(format!("tokio: {e}")))?;
    rt.block_on(build(url, plan, root, outputs, upload_only, on_event))
}

/// `Fetch`: restores the NAR of `store_path` from the builder into `dest`, which must not
/// exist. The pieces are collected in memory first; outputs are small next to the closure.
pub async fn fetch(
    url: &str,
    store_path: &str,
    dest: &std::path::Path,
) -> Result<u64, ClientError> {
    let channel = Channel::from_shared(url.to_string())
        .map_err(|e| ClientError(format!("bad url '{url}': {e}")))?
        .connect()
        .await?;
    let mut client = BuilderClient::new(channel).max_decoding_message_size(MAX_MESSAGE);
    let mut chunks = client
        .fetch(FetchRequest {
            store_path: store_path.to_string(),
        })
        .await?
        .into_inner();
    let mut nar = Vec::new();
    while let Some(c) = chunks.next().await {
        nar.extend_from_slice(&c?.data);
    }
    let size = nar.len() as u64;
    crate::store::nar::restore_path(&mut nar.as_slice(), dest)?;
    Ok(size)
}

pub fn fetch_blocking(
    url: &str,
    store_path: &str,
    dest: &std::path::Path,
) -> Result<u64, ClientError> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| ClientError(format!("tokio: {e}")))?;
    rt.block_on(fetch(url, store_path, dest))
}
