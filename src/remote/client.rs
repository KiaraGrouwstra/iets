//! The client side: Have -> Put -> Build, three round trips before the build starts.

use std::collections::HashSet;
use std::io;
use std::time::{Duration, Instant};

use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;

use crate::store::hash::Hash;
use crate::store::nar::dump_path;
use crate::store::path::StorePath;

use super::chunk::{Chunker, from_digest, sha256, to_digest};
use super::plan::{Object, Plan, SourceKind};
use super::proto::builder_client::BuilderClient;
use super::proto::{
    BuildRequest, Chunk, ContentAddress, Digest, DrvBlob, HaveRequest, Manifest, PutRequest,
    build_event, put_request,
};

pub const MAX_MESSAGE: usize = 16 << 20;

/// What the builder reported it lacks, in the two digest namespaces of `Have`.
struct Missing {
    chunks: HashSet<Hash>,
    objects: HashSet<Hash>,
}

/// Wall-clock and volume figures for one `build` call.
#[derive(Debug, Default, Clone)]
pub struct Timings {
    pub connect: Duration,
    pub have: Duration,
    pub put: Duration,
    pub build: Duration,
    pub have_digests: usize,
    pub missing: usize,
    pub put_objects: usize,
    pub put_chunks: usize,
    pub put_bytes: u64,
}

impl Timings {
    pub fn report(&self, plan: &Plan, plan_time: Duration) -> String {
        format!(
            "plan: {} sources, {} drvs, {:.3}s\nconnect: {:.3}s\nhave: {} digests, {} missing, {:.3}s\nput: {} objects, {} chunks, {} bytes, {:.3}s\nbuild: {:.3}s",
            plan.sources(),
            plan.drvs(),
            plan_time.as_secs_f64(),
            self.connect.as_secs_f64(),
            self.have_digests,
            self.missing,
            self.have.as_secs_f64(),
            self.put_objects,
            self.put_chunks,
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
/// Returns once the builder sends Done or closes the stream.
pub async fn build(
    url: &str,
    plan: &Plan,
    root: &StorePath,
    outputs: &[String],
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
    for o in &plan.objects {
        if let Object::Source { chunks, .. } = o {
            req.chunks.extend(chunks.iter().map(to_digest));
        }
        req.objects.push(to_digest(o.digest()));
    }
    t.have_digests = req.chunks.len() + req.objects.len();
    let resp = client.have(req).await?.into_inner();
    let to_set = |v: &[Digest]| -> Result<HashSet<Hash>, ClientError> {
        v.iter()
            .map(|d| from_digest(d).map_err(ClientError))
            .collect()
    };
    let missing = Missing {
        chunks: to_set(&resp.missing_chunks)?,
        objects: to_set(&resp.missing_objects)?,
    };
    t.missing = missing.chunks.len() + missing.objects.len();
    t.have = t1.elapsed();

    let t2 = Instant::now();
    if t.missing > 0 {
        let (tx, rx) = tokio::sync::mpsc::channel::<PutRequest>(16);
        let objects: Vec<Object> = plan.objects.clone();
        let producer = tokio::task::spawn_blocking(move || produce(&objects, &missing, tx));
        let ack = client.put(ReceiverStream::new(rx)).await?.into_inner();
        let stats = producer
            .await
            .map_err(|e| ClientError(format!("upload thread: {e}")))??;
        t.put_objects = stats.0;
        t.put_chunks = stats.1;
        t.put_bytes = stats.2;
        if ack.stored.len() != t.missing {
            return Err(ClientError(format!(
                "builder stored {} of {} missing objects",
                ack.stored.len(),
                t.missing
            )));
        }
    }
    t.put = t2.elapsed();

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

/// Runs on a blocking thread: re-dumps each source that needs uploading, sends its missing
/// chunks, then its manifest; sends missing drvs. Returns (objects, chunks, bytes).
fn produce(
    objects: &[Object],
    missing: &Missing,
    tx: tokio::sync::mpsc::Sender<PutRequest>,
) -> Result<(usize, usize, u64), ClientError> {
    let closed = || ClientError("builder closed the upload stream".into());
    let mut n_objects = 0;
    let mut n_chunks = 0;
    let mut bytes = 0u64;
    for o in objects {
        match o {
            Object::Source {
                path,
                local,
                kind,
                nar_hash,
                nar_size,
                chunks,
            } => {
                let need_chunks: Vec<bool> =
                    chunks.iter().map(|c| missing.chunks.contains(c)).collect();
                if need_chunks.iter().any(|&b| b) {
                    let mut i = 0;
                    let mut err = None;
                    let mut chunker = Chunker::new(|data: Vec<u8>| {
                        let need = need_chunks.get(i).copied().unwrap_or(true);
                        i += 1;
                        if need {
                            let digest = sha256(&data);
                            if chunks.get(i - 1) != Some(&digest) {
                                return Err(io::Error::other(format!(
                                    "source '{}' changed while uploading",
                                    local.display()
                                )));
                            }
                            n_chunks += 1;
                            bytes += data.len() as u64;
                            let req = PutRequest {
                                body: Some(put_request::Body::Chunk(Chunk {
                                    digest: Some(to_digest(&digest)),
                                    data,
                                })),
                            };
                            if tx.blocking_send(req).is_err() {
                                err = Some(closed());
                                return Err(io::Error::other("closed"));
                            }
                        }
                        Ok(())
                    });
                    let r = dump_path(local, &mut chunker, &mut |_, _| true)
                        .and_then(|()| chunker.finish());
                    if let Some(e) = err {
                        return Err(e);
                    }
                    r?;
                }
                if missing.objects.contains(nar_hash) {
                    n_objects += 1;
                    let (ca, references) = match kind {
                        SourceKind::Source => (ContentAddress::Source, Vec::new()),
                        SourceKind::Text { references } => (
                            ContentAddress::Text,
                            references.iter().map(|r| r.to_string()).collect(),
                        ),
                    };
                    let req = PutRequest {
                        body: Some(put_request::Body::Manifest(Manifest {
                            store_path: path.to_string(),
                            nar_hash: Some(to_digest(nar_hash)),
                            nar_size: *nar_size,
                            chunks: chunks.iter().map(to_digest).collect(),
                            ca: ca as i32,
                            references,
                        })),
                    };
                    tx.blocking_send(req).map_err(|_| closed())?;
                }
            }
            Object::Drv {
                path,
                aterm,
                digest,
            } => {
                if missing.objects.contains(digest) {
                    n_objects += 1;
                    bytes += aterm.len() as u64;
                    let req = PutRequest {
                        body: Some(put_request::Body::Drv(DrvBlob {
                            drv_path: path.to_string(),
                            aterm: aterm.clone(),
                        })),
                    };
                    tx.blocking_send(req).map_err(|_| closed())?;
                }
            }
        }
    }
    Ok((n_objects, n_chunks, bytes))
}

/// `build` on a fresh multi-thread runtime, for callers without one.
pub fn build_blocking(
    url: &str,
    plan: &Plan,
    root: &StorePath,
    outputs: &[String],
    on_event: &mut dyn FnMut(build_event::Event),
) -> Result<Timings, ClientError> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| ClientError(format!("tokio: {e}")))?;
    rt.block_on(build(url, plan, root, outputs, on_event))
}
