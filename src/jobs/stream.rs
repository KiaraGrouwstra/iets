//! A sink for the `eval-jobs` walk that hands finished attributes to another
//! thread instead of printing them, plus typed readers for the fields that
//! sink's consumer needs. `jobs::run` already takes any `FnMut(Line)`, so
//! this is a hook, not a second walk: the emitter and this share the walk.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use super::line::{Job, Line};

/// The sender half of the stream; hand `sink` to `jobs::run`.
pub struct Stream {
    tx: Sender<Line>,
}

impl Stream {
    /// A stream and the receiver its consumer drains.
    pub fn new() -> (Stream, Receiver<Line>) {
        let (tx, rx) = channel();
        (Stream { tx }, rx)
    }

    /// The `jobs::run` sink. Dropping the returned closure closes the stream,
    /// which is how the consumer learns the walk is over.
    pub fn sink(self) -> impl FnMut(Line) {
        let tx = self.tx;
        move |line: Line| {
            let _ = tx.send(line);
        }
    }
}

/// Everything about one job the dispatcher looks at. The raw `Job` keeps the
/// rest as JSON for the emitter.
pub struct Info {
    pub attr: String,
    pub drv_path: crate::store::path::StorePath,
    /// Output name to store path, the outputs eval knows.
    pub outputs: Vec<(String, String)>,
    pub system: Option<String>,
    /// `--check-cache-status`: `local`, `cached` or `notBuilt`.
    pub cache_status: Option<String>,
}

/// `extra[key]` as a JSON string.
fn json_string(job: &Job, key: &str) -> Option<String> {
    serde_json::from_slice::<String>(job.extra.get(key)?).ok()
}

impl Info {
    pub fn of(job: &Job) -> Info {
        let cache_status = json_string(job, "cacheStatus").or_else(|| {
            (job.extra.get("isCached").map(Vec::as_slice) == Some(b"true"))
                .then(|| "cached".to_string())
        });
        Info {
            attr: job.attr.clone(),
            drv_path: job.drv_path.clone(),
            outputs: job
                .outputs
                .iter()
                .filter_map(|(n, p)| p.as_ref().map(|p| (n.clone(), p.to_string())))
                .collect(),
            system: json_string(job, "system"),
            cache_status,
        }
    }
}

/// Reads the stream in batches: the first line blocks, then everything else
/// that already arrived joins it, up to `max`. `None` when the walk is over.
///
/// This is what overlaps evaluation with building: the batch is whatever
/// finished while the last build op ran, and that whole batch becomes one
/// `BuildPathsWithResults`.
pub struct Batcher {
    rx: Receiver<Line>,
    max: usize,
    /// How long to wait for more lines after the first one, so a fast walk
    /// fills a batch instead of sending one op per attribute.
    settle: Duration,
}

impl Batcher {
    pub fn new(rx: Receiver<Line>, max: usize) -> Batcher {
        Batcher {
            rx,
            max: max.max(1),
            settle: Duration::from_millis(20),
        }
    }

    pub fn next_batch(&mut self) -> Option<Vec<Line>> {
        let first = self.rx.recv().ok()?;
        let mut batch = vec![first];
        let deadline = Instant::now() + self.settle;
        while batch.len() < self.max {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(l) => batch.push(l),
                Err(_) => break,
            }
        }
        Some(batch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::path::StorePath;
    use std::collections::BTreeMap;

    fn job(attr: &str, extra: &[(&str, &str)]) -> Job {
        Job {
            attr: attr.into(),
            attr_path: vec![attr.into()],
            drv_path: StorePath::parse("/nix/store/00000000000000000000000000000000-x.drv")
                .unwrap(),
            outputs: BTreeMap::from([(
                "out".to_string(),
                Some(StorePath::parse("/nix/store/00000000000000000000000000000000-x").unwrap()),
            )]),
            constituents: vec![],
            named_constituents: None,
            glob_constituents: false,
            extra: extra
                .iter()
                .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
                .collect(),
        }
    }

    #[test]
    fn info_reads_system_and_cache_status() {
        let i = Info::of(&job(
            "a",
            &[
                ("system", "\"x86_64-linux\""),
                ("cacheStatus", "\"notBuilt\""),
            ],
        ));
        assert_eq!(i.system.as_deref(), Some("x86_64-linux"));
        assert_eq!(i.cache_status.as_deref(), Some("notBuilt"));
        assert_eq!(i.outputs.len(), 1);

        // nix-eval-jobs's older shape: isCached without cacheStatus
        let i = Info::of(&job("a", &[("isCached", "true")]));
        assert_eq!(i.cache_status.as_deref(), Some("cached"));
        let i = Info::of(&job("a", &[("isCached", "false")]));
        assert_eq!(i.cache_status, None);
    }

    #[test]
    fn batcher_groups_what_already_arrived() {
        let (s, rx) = Stream::new();
        let mut sink = s.sink();
        sink(Line::Job(job("a", &[])));
        sink(Line::Job(job("b", &[])));
        sink(Line::Job(job("c", &[])));
        drop(sink);
        let mut b = Batcher::new(rx, 2);
        assert_eq!(b.next_batch().map(|v| v.len()), Some(2));
        assert_eq!(b.next_batch().map(|v| v.len()), Some(1));
        assert!(b.next_batch().is_none());
    }
}
