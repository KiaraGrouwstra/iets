//! Copying a closure from one worker-protocol store into another. A remote
//! `builder` leaves its outputs on its own store, so an out-link or a GC root
//! on the local daemon needs them registered locally first (docs/classic.md).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{self, Read, Write};

use crate::error::{Error, Result};
use crate::store::path::StorePath;

use super::event::Event;
use super::grpc::drvwire::PathInfo;
use super::worker::Connection;
use super::worker::wire::write_u64;

/// Registers the closure of `paths`, read from `from`, into `to`, references
/// first. Returns how many paths were added; what `to` has already is left
/// alone. The signature check is waived, which the daemon grants a trusted
/// client only, so an unsigned output of a remote builder still lands.
pub fn closure(
    from: &mut Connection,
    to: &mut Connection,
    paths: &[StorePath],
    on_event: &mut dyn FnMut(Event),
) -> Result<usize> {
    let infos = remote_closure(from, paths, on_event)?;
    let all: Vec<String> = infos.keys().map(StorePath::to_string).collect();
    let valid: HashSet<String> = to.query_valid_paths(&all, on_event)?.into_iter().collect();
    let order: Vec<StorePath> = topo_sort(&infos)
        .into_iter()
        .filter(|p| !valid.contains(&p.to_string()))
        .collect();
    if order.is_empty() {
        return Ok(0);
    }
    let n = order.len();
    let infos = &infos;
    let order = &order;
    let mut body = |w: &mut dyn Write| -> io::Result<()> {
        write_u64(w, n as u64)?;
        for p in order {
            let info = &infos[p];
            info.write(w)?;
            let size = info.nar_size;
            from.nar_from_path(&p.to_string(), &mut |_| {}, &mut |r| {
                let got = io::copy(&mut r.take(size), w)?;
                if got != size {
                    return Err(io::Error::other(format!(
                        "'{p}': the NAR was {got} bytes, not the {size} its path info promised"
                    )));
                }
                Ok(())
            })
            .map_err(io::Error::other)?;
        }
        Ok(())
    };
    to.add_multiple_to_store(true, &mut body, on_event)?;
    Ok(n)
}

/// The closure of `paths` on `from`, as path infos. `ultimate` is cleared, as
/// store-api.cc:copyStorePath does: the copy was not built here.
fn remote_closure(
    from: &mut Connection,
    paths: &[StorePath],
    on_event: &mut dyn FnMut(Event),
) -> Result<HashMap<StorePath, PathInfo>> {
    let mut infos: HashMap<StorePath, PathInfo> = HashMap::new();
    let mut queue: BTreeSet<StorePath> = paths.iter().cloned().collect();
    while let Some(p) = queue.pop_first() {
        if infos.contains_key(&p) {
            continue;
        }
        let Some(mut info) = from.query_path_info(&p.to_string(), on_event)? else {
            return Err(Error::Protocol(format!(
                "'{p}' is not valid on the builder, so it cannot be copied back"
            )));
        };
        info.ultimate = false;
        for r in &info.references {
            let rp = StorePath::parse(r)?;
            if !infos.contains_key(&rp) {
                queue.insert(rp);
            }
        }
        infos.insert(p, info);
    }
    Ok(infos)
}

/// `paths` with every reference ahead of the path that names it.
pub fn topo_sort(infos: &HashMap<StorePath, PathInfo>) -> Vec<StorePath> {
    let mut out = Vec::with_capacity(infos.len());
    let mut seen: HashSet<StorePath> = HashSet::new();
    let mut order: Vec<&StorePath> = infos.keys().collect();
    order.sort();
    for root in order {
        let mut stack = vec![(root.clone(), false)];
        while let Some((p, expanded)) = stack.pop() {
            if expanded {
                out.push(p);
                continue;
            }
            if !seen.insert(p.clone()) {
                continue;
            }
            stack.push((p.clone(), true));
            if let Some(info) = infos.get(&p) {
                for r in &info.references {
                    if let Ok(rp) = StorePath::parse(r)
                        && rp != p
                        && !seen.contains(&rp)
                    {
                        stack.push((rp, false));
                    }
                }
            }
        }
    }
    out
}
