//! git. The tree is what git.cc:getAccessorFromWorkdir copies: the index
//! entries that still exist, no `.git`, no submodules. Every command is a
//! `Cmd` observation.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use super::{Scm, Snapshot};
use crate::trace::{Kind, Trace};

const NULL_REV: &str = "0000000000000000000000000000000000000000";

pub struct Git;

fn run(trace: &Trace, root: &Path, args: &[&str]) -> Result<(i32, Vec<u8>), String> {
    let mut argv: Vec<Vec<u8>> = vec![b"git".to_vec()];
    argv.extend(args.iter().map(|a| a.as_bytes().to_vec()));
    trace
        .cmd(root, &argv)
        .map_err(|e| format!("ietsp: cannot run git: {e}"))
}

fn git(trace: &Trace, root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let (status, out) = run(trace, root, args)?;
    if status != 0 {
        return Err(format!(
            "ietsp: `git {}` failed in '{}' (exit {status})",
            args.join(" "),
            root.display()
        ));
    }
    Ok(out)
}

fn number(out: &[u8]) -> u64 {
    String::from_utf8_lossy(out).trim().parse().unwrap_or(0)
}

impl Scm for Git {
    fn name(&self) -> &'static str {
        "git"
    }

    fn title(&self) -> &'static str {
        "Git"
    }

    fn is_root(&self, dir: &Path) -> bool {
        dir.join(".git").exists()
    }

    /// git-utils.cc:getWorkdirInfo: index entries minus deleted ones; dirty
    /// when any tracked file differs from HEAD or the index.
    fn snapshot(&self, trace: &Trace, root: &Path) -> Result<Snapshot, String> {
        let ls = git(trace, root, &["ls-files", "-z", "--cached", "--stage"])?;
        let mut files: HashSet<PathBuf> = HashSet::new();
        for entry in ls.split(|&b| b == 0).filter(|e| !e.is_empty()) {
            let Some(tab) = entry.iter().position(|&b| b == b'\t') else {
                continue;
            };
            let mode = entry[..tab].split(|&b| b == b' ').next().unwrap_or(b"");
            if mode == b"160000" {
                continue;
            }
            files.insert(root.join(OsStr::from_bytes(&entry[tab + 1..])));
        }
        let status = git(
            trace,
            root,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=no",
                "--no-renames",
                "--ignore-submodules=all",
            ],
        )?;
        let mut dirty = false;
        for entry in status.split(|&b| b == 0).filter(|e| e.len() > 3) {
            dirty = true;
            if entry[0] == b'D' || entry[1] == b'D' {
                files.remove(&root.join(OsStr::from_bytes(&entry[3..])));
            }
        }
        let head = run(trace, root, &["rev-parse", "--verify", "-q", "HEAD"])?;
        let head = (head.0 == 0).then(|| String::from_utf8_lossy(&head.1).trim().to_string());
        let last_modified = match &head {
            Some(_) => number(&git(trace, root, &["log", "-1", "--format=%ct", "HEAD"])?),
            None => 0,
        };
        let shallow = || {
            let marker = root.join(".git/shallow");
            trace.stat_now(marker.as_os_str().as_bytes(), true) != Kind::Missing
        };
        let rev_count = match &head {
            _ if dirty || shallow() => None,
            Some(_) => Some(number(&git(trace, root, &["rev-list", "--count", "HEAD"])?)),
            None => Some(0),
        };
        Ok(Snapshot {
            files,
            dirty,
            // git.cc gives a clean tree without a commit the null revision
            rev: head.or_else(|| (!dirty).then(|| NULL_REV.to_string())),
            last_modified,
            rev_count,
        })
    }

    fn untracked(&self, root: &Path, rel: &OsStr) -> Option<String> {
        let tracked = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["ls-files", "--error-unmatch", "--"])
            .arg(rel)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?;
        (!tracked.success()).then(|| {
            format!(
                "git -C \"{}\" add \"{}\"",
                root.display(),
                rel.to_string_lossy()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::flake::parse_flake_ref;
    use crate::source::SourceRef;

    #[test]
    fn a_directory_in_a_repository_is_found() {
        let d = crate::store::nar::tests::fixture_tree("git-root");
        std::fs::write(d.join("sub/flake.nix"), b"{ outputs = _: {}; }").unwrap();
        std::fs::create_dir_all(d.join(".git")).unwrap();
        let r = parse_flake_ref(&format!("{}/sub#x", d.display()), b"/", None).unwrap();
        let url = format!("git+file://{}?dir=sub", d.display());
        assert_eq!(crate::api::flake::url(&r), url);
        let r = parse_flake_ref(&format!("path:{}/sub", d.display()), b"/", None).unwrap();
        assert_eq!(r.source, SourceRef::Path(d.join("sub")));
    }
}
