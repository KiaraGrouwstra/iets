//! `flake.lock` checks (lockfile.cc): the file must parse, and every node
//! reachable from the root must be locked. `call-flake.nix` does the rest.

use std::collections::HashSet;

use serde_json::Value;

#[derive(Debug)]
pub struct Lock {
    pub root: String,
    json: Value,
}

impl Lock {
    pub fn root_inputs(&self) -> impl Iterator<Item = &str> {
        self.json["nodes"][&self.root]["inputs"]
            .as_object()
            .into_iter()
            .flat_map(|m| m.keys().map(String::as_str))
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
) -> Result<&'a str, String> {
    if depth > 64 {
        return Err("follow cycle".into());
    }
    let mut cur = root;
    for elem in path {
        let Some(name) = elem.as_str() else {
            return Err("follows path element is not a string".into());
        };
        let Some(spec) = nodes.get(cur).and_then(|n| n["inputs"].get(name)) else {
            return Err(format!("input '{name}' of node '{cur}' is missing"));
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
) -> Result<&'a str, String> {
    match spec {
        Value::String(s) => Ok(s.as_str()),
        Value::Array(p) => resolve(nodes, root, p, depth),
        _ => Err("input spec is neither a node name nor a follows path".into()),
    }
}

pub fn check(bytes: &[u8]) -> Result<Lock, String> {
    let json: Value = serde_json::from_slice(bytes).map_err(|e| format!("cannot parse: {e}"))?;
    let version = json["version"].as_u64().unwrap_or(0);
    if !(5..=7).contains(&version) {
        return Err(format!("unsupported version {version}"));
    }
    let Some(root) = json["root"].as_str().map(str::to_owned) else {
        return Err("no root".into());
    };
    let Some(nodes) = json["nodes"].as_object() else {
        return Err("no nodes".into());
    };
    let mut todo = vec![root.as_str()];
    let mut seen: HashSet<&str> = HashSet::new();
    while let Some(key) = todo.pop() {
        if !seen.insert(key) {
            continue;
        }
        let Some(node) = nodes.get(key) else {
            return Err(format!("node '{key}' is missing"));
        };
        if key != root && !is_locked(node) {
            return Err(format!("node '{key}' is not locked"));
        }
        if let Some(inputs) = node["inputs"].as_object() {
            for spec in inputs.values() {
                todo.push(resolve_spec(nodes, &root, spec, 0)?);
            }
        }
    }
    Ok(Lock { root, json })
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
    }

    #[test]
    fn incomplete_locks_fail() {
        let missing_node = LOCK.replace(r#""a": "a", "#, r#""a": "b", "#);
        assert!(check(missing_node.as_bytes()).unwrap_err().contains("'b'"));
        let unlocked = LOCK.replace(r#", "narHash": "sha256-BBBB""#, "");
        assert!(
            check(unlocked.as_bytes())
                .unwrap_err()
                .contains("not locked")
        );
        let bad_follows = LOCK.replace(r#"["a", "nixpkgs"]"#, r#"["a", "nope"]"#);
        assert!(check(bad_follows.as_bytes()).unwrap_err().contains("nope"));
        let old = LOCK.replace(r#""version": 7"#, r#""version": 4"#);
        assert!(check(old.as_bytes()).is_err());
        assert!(check(b"{").is_err());
    }
}
