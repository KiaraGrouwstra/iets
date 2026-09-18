//! `nix build --store grpc://… --eval-store auto`, which is what the
//! `nix-farm` wrapper runs. The outputs land in the farm's binary cache, not
//! in a local store, so an out-link needs them registered locally first.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::store::path::StorePath;

use super::super::event::Event;
use super::super::plan::{Locator, Plan};
use super::super::worker::{Connection, DEFAULT_SOCKET, Transport};
use super::{Client, GrpcConfig};

/// One installable: its derivation and the outputs to install.
pub struct Target {
    pub drv: StorePath,
    pub outputs: Vec<String>,
}

/// What `nix build` does with the result.
pub struct Links {
    /// The `-o`/`--out-link` prefix; `None` for `--no-link`.
    pub out_link: Option<String>,
    pub print_out_paths: bool,
}

/// Builds every target and returns the exit status. `on_event` sees the same
/// events the worker transport reports.
pub fn realise(
    cfg: &GrpcConfig,
    plan: &Plan,
    locate: &Locator<'_>,
    targets: &[Target],
    links: &Links,
    on_event: &mut dyn FnMut(Event),
) -> Result<i32> {
    let mut c = Client::open(cfg)?;
    let mut paths: BTreeMap<(String, String), StorePath> = BTreeMap::new();
    let mut done: BTreeMap<String, ()> = BTreeMap::new();
    for t in targets {
        let key = t.drv.to_string();
        if done.insert(key, ()).is_some() {
            continue;
        }
        let mut timings = super::build::Timings::default();
        let built = super::build::build_on(
            &mut c,
            plan,
            locate,
            &t.drv,
            &t.outputs,
            false,
            &mut timings,
            on_event,
        )?;
        let Some(built) = built else { return Ok(1) };
        for (name, path) in built {
            paths.insert((t.drv.to_string(), name), path);
        }
    }

    let wanted: Vec<StorePath> = targets
        .iter()
        .flat_map(|t| {
            t.outputs
                .iter()
                .filter_map(|o| paths.get(&(t.drv.to_string(), o.clone())).cloned())
        })
        .collect();
    if links.out_link.is_some() && !wanted.is_empty() {
        // The farm published the outputs to the cache; copy them into the
        // local store so the symlink resolves.
        let mut conn =
            Connection::open(&Transport::Unix(PathBuf::from(DEFAULT_SOCKET)), &mut |_| {})?;
        super::build::register(&mut c, &wanted, &mut conn, on_event)?;
        let cwd = crate::cli::cwd();
        for (i, t) in targets.iter().enumerate() {
            for output in &t.outputs {
                let Some(path) = paths.get(&(t.drv.to_string(), output.clone())) else {
                    continue;
                };
                let link = out_link_name(links.out_link.as_deref().expect("some"), i, output);
                let link = String::from_utf8_lossy(&crate::cli::abs_path(&link, &cwd)).into_owned();
                conn.add_perm_root(&path.to_string(), Path::new(&link))?;
            }
        }
    }
    if links.print_out_paths {
        let mut out = String::new();
        for p in &wanted {
            out.push_str(&p.to_string());
            out.push('\n');
        }
        print!("{out}");
    }
    Ok(0)
}

/// build.cc:createOutLinks: the n-th installable adds `-n` from 1 on, an
/// output other than `out` adds `-<output>`.
fn out_link_name(prefix: &str, i: usize, output: &str) -> String {
    let mut link = prefix.to_string();
    if i > 0 {
        link.push_str(&format!("-{i}"));
    }
    if output != "out" {
        link.push('-');
        link.push_str(output);
    }
    link
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_links_are_numbered_like_nix_build() {
        assert_eq!(out_link_name("result", 0, "out"), "result");
        assert_eq!(out_link_name("result", 1, "out"), "result-1");
        assert_eq!(out_link_name("result", 0, "dev"), "result-dev");
        assert_eq!(out_link_name("result", 2, "dev"), "result-2-dev");
    }
}
