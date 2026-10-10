//! `flake.lock` checks (lockfile.cc): the file must parse, and every node
//! reachable from the root must be locked. `call-flake.nix` does the rest.

use std::collections::HashSet;

use serde_json::Value;

use crate::error::{Error, Result};

/// lockfile.cc:LockFile::to_string of a lock without inputs, which Nix
/// uses for a flake that declares none and has no `flake.lock`.
pub const EMPTY: &str = r#"{"nodes":{"root":{}},"root":"root","version":7}"#;

#[derive(Debug)]
pub struct Lock {
    pub root: String,
    json: Value,
    /// The nodes the root reaches, itself included.
    reached: Vec<String>,
}

impl Lock {
    pub fn root_inputs(&self) -> impl Iterator<Item = &str> {
        self.json["nodes"][&self.root]["inputs"]
            .as_object()
            .into_iter()
            .flat_map(|m| m.keys().map(String::as_str))
    }

    /// `(node name, locked attrs)` of every node but the root that the root
    /// reaches, as `nix flake prefetch-inputs` visits them.
    pub fn reached_inputs(&self) -> impl Iterator<Item = (&str, &serde_json::Map<String, Value>)> {
        self.reached
            .iter()
            .filter(|n| **n != self.root)
            .filter_map(|n| Some((n.as_str(), self.json["nodes"][n]["locked"].as_object()?)))
    }

    /// `(narHash, node name)` of every locked node, for messages that name
    /// an input by what `fetchTree` receives.
    pub fn nar_hash_names(&self) -> impl Iterator<Item = (&str, &str)> {
        self.json["nodes"]
            .as_object()
            .into_iter()
            .flat_map(|m| m.iter())
            .filter_map(|(name, node)| {
                let h = node["locked"]["narHash"].as_str()?;
                Some((h, name.as_str()))
            })
    }
}

/// A node is locked when it has a `narHash` (fetchers.cc:isLocked) or is a
/// relative `path:` input resolved through its parent.
fn is_locked(node: &Value) -> bool {
    let Some(locked) = node.get("locked").and_then(Value::as_object) else {
        return false;
    };
    if !locked.get("type").is_some_and(Value::is_string) {
        return false;
    }
    if locked.get("narHash").is_some_and(Value::is_string) {
        return true;
    }
    locked.get("type").and_then(Value::as_str) == Some("path")
        && locked
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|p| !p.starts_with('/'))
}

/// call-flake.nix:getInputByPath: follow a `follows` path from the root.
fn resolve<'a>(
    nodes: &'a serde_json::Map<String, Value>,
    root: &'a str,
    path: &[Value],
    depth: usize,
) -> Result<&'a str> {
    if depth > 64 {
        return Err(Error::Flake("follow cycle".into()));
    }
    let mut cur = root;
    for elem in path {
        let Some(name) = elem.as_str() else {
            return Err(Error::Flake("follows path element is not a string".into()));
        };
        let Some(spec) = nodes.get(cur).and_then(|n| n["inputs"].get(name)) else {
            return Err(Error::Flake(format!(
                "input '{name}' of node '{cur}' is missing"
            )));
        };
        cur = resolve_spec(nodes, root, spec, depth + 1)?;
    }
    Ok(cur)
}

fn resolve_spec<'a>(
    nodes: &'a serde_json::Map<String, Value>,
    root: &'a str,
    spec: &'a Value,
    depth: usize,
) -> Result<&'a str> {
    match spec {
        Value::String(s) => Ok(s.as_str()),
        Value::Array(p) => resolve(nodes, root, p, depth),
        _ => Err(Error::Flake(
            "input spec is neither a node name nor a follows path".into(),
        )),
    }
}

pub fn check(bytes: &[u8]) -> Result<Lock> {
    let json: Value =
        serde_json::from_slice(bytes).map_err(|e| Error::Flake(format!("cannot parse: {e}")))?;
    let version = json["version"].as_u64().unwrap_or(0);
    if !(5..=7).contains(&version) {
        return Err(Error::Flake(format!("unsupported version {version}")));
    }
    let Some(root) = json["root"].as_str().map(str::to_owned) else {
        return Err(Error::Flake("no root".into()));
    };
    let Some(nodes) = json["nodes"].as_object() else {
        return Err(Error::Flake("no nodes".into()));
    };
    // a follows target is reached through its owner too, so only direct
    // edges extend the input path the hint names
    let mut todo = vec![(root.as_str(), String::new())];
    let mut seen: HashSet<&str> = HashSet::new();
    while let Some((key, path)) = todo.pop() {
        if !seen.insert(key) {
            continue;
        }
        let Some(node) = nodes.get(key) else {
            return Err(Error::Flake(format!("node '{key}' is missing")));
        };
        if key != root && !is_locked(node) {
            let e = Error::Flake(format!("node '{key}' is not locked"));
            let fix = format!(
                "input '{path}' has no 'narHash' in flake.lock; run 'nix flake update {path}' to lock it again"
            );
            return Err(Error::Hint(Box::new(e), fix));
        }
        if let Some(inputs) = node["inputs"].as_object() {
            for (name, spec) in inputs {
                let to = resolve_spec(nodes, &root, spec, 0)?;
                if spec.is_string() {
                    let sep = if path.is_empty() { "" } else { "/" };
                    todo.push((to, format!("{path}{sep}{name}")));
                }
            }
        }
    }
    let mut reached: Vec<String> = seen.into_iter().map(str::to_owned).collect();
    reached.sort();
    Ok(Lock {
        root,
        json,
        reached,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCK: &str = r#"{
      "version": 7, "root": "root",
      "nodes": {
        "root": { "inputs": { "a": "a", "nixpkgs": ["a", "nixpkgs"], "p": "p" } },
        "a": { "inputs": { "nixpkgs": "nixpkgs" },
               "locked": { "type": "github", "owner": "x", "repo": "y", "rev": "0", "narHash": "sha256-AAAA" },
               "original": { "type": "github", "owner": "x", "repo": "y" } },
        "nixpkgs": { "locked": { "type": "github", "owner": "NixOS", "repo": "nixpkgs", "rev": "1", "narHash": "sha256-BBBB" },
                     "original": { "type": "indirect", "id": "nixpkgs" } },
        "p": { "locked": { "type": "path", "path": "./sub" }, "original": { "type": "path", "path": "./sub" }, "parent": [] }
      }
    }"#;

    #[test]
    fn complete_lock_passes() {
        let l = check(LOCK.as_bytes()).unwrap();
        assert_eq!(l.root, "root");
        let mut names: Vec<&str> = l.root_inputs().collect();
        names.sort();
        assert_eq!(names, ["a", "nixpkgs", "p"]);
        let reached: Vec<&str> = l.reached_inputs().map(|(n, _)| n).collect();
        assert_eq!(reached, ["a", "nixpkgs", "p"]);
        let stale = LOCK.replace(
            r#""nodes": {"#,
            r#""nodes": { "x": { "locked": { "type": "github", "narHash": "sha256-C" } },"#,
        );
        assert_eq!(check(stale.as_bytes()).unwrap().reached_inputs().count(), 3);
    }

    #[test]
    fn empty_lock_has_no_inputs() {
        let l = check(EMPTY.as_bytes()).unwrap();
        assert_eq!(l.root, "root");
        assert_eq!(l.root_inputs().count(), 0);
        assert_eq!(l.reached_inputs().count(), 0);
    }

    #[test]
    fn incomplete_locks_fail() {
        let missing_node = LOCK.replace(r#""a": "a", "#, r#""a": "b", "#);
        assert!(
            check(missing_node.as_bytes())
                .unwrap_err()
                .to_string()
                .contains("'b'")
        );
        let unlocked = LOCK.replace(r#", "narHash": "sha256-BBBB""#, "");
        let e = check(unlocked.as_bytes()).unwrap_err();
        assert_eq!(e.to_string(), "node 'nixpkgs' is not locked");
        assert_eq!(
            e.to_report().hint.as_deref(),
            Some(
                "input 'a/nixpkgs' has no 'narHash' in flake.lock; run 'nix flake update a/nixpkgs' to lock it again"
            )
        );
        let bad_follows = LOCK.replace(r#"["a", "nixpkgs"]"#, r#"["a", "nope"]"#);
        assert!(
            check(bad_follows.as_bytes())
                .unwrap_err()
                .to_string()
                .contains("nope")
        );
        let old = LOCK.replace(r#""version": 7"#, r#""version": 4"#);
        assert!(check(old.as_bytes()).is_err());
        assert!(check(b"{").is_err());
    }
}
