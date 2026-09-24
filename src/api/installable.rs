//! One installable: the attribute paths a fragment names in a flake's
//! outputs, and the values they select (docs/plumbing.md).

use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos};
use crate::runtime::Word;

/// Which `getActualAttrPaths` table a fragment is tried against
/// (installable-flake.cc, with each command's own defaults and prefixes).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prefixes {
    /// `nix build`, `nix eval`: one package.
    Package,
    /// `nix develop`, `print-dev-env`: a development shell first.
    DevShell,
    /// `nix search`: the package sets, not one package.
    PackageSet,
}

impl Prefixes {
    pub fn parse(s: &str) -> Option<Prefixes> {
        match s {
            "package" => Some(Prefixes::Package),
            "dev-shell" => Some(Prefixes::DevShell),
            "package-set" => Some(Prefixes::PackageSet),
            _ => None,
        }
    }
}

/// How many candidates the caller wants: the first that is there, or every
/// one of them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Want {
    First,
    All,
}

/// A candidate that resolved: the value and the attribute path it was at.
pub struct Found {
    pub value: Word,
    pub path: Vec<String>,
}

/// The attribute paths `fragment` names, in order. A leading `.` makes the
/// rest the whole path; an empty fragment takes the defaults.
pub fn attr_paths(p: Prefixes, fragment: &str, system: &str) -> Vec<String> {
    if let Some(rest) = fragment.strip_prefix('.') {
        return vec![rest.to_string()];
    }
    if fragment.is_empty() {
        return match p {
            Prefixes::Package => vec![
                format!("packages.{system}.default"),
                format!("defaultPackage.{system}"),
            ],
            Prefixes::DevShell => vec![
                format!("devShells.{system}.default"),
                format!("devShell.{system}"),
                format!("packages.{system}.default"),
                format!("defaultPackage.{system}"),
            ],
            Prefixes::PackageSet => vec![
                format!("packages.{system}"),
                format!("legacyPackages.{system}"),
            ],
        };
    }
    let mut v = Vec::new();
    if p == Prefixes::DevShell {
        v.push(format!("devShells.{system}.{fragment}"));
    }
    v.push(format!("packages.{system}.{fragment}"));
    v.push(format!("legacyPackages.{system}.{fragment}"));
    v.push(fragment.to_string());
    v
}

/// installable-flake.cc:showAttrPaths.
pub fn show_attr_paths(paths: &[String]) -> String {
    let mut s = String::new();
    for (n, p) in paths.iter().enumerate() {
        if n > 0 {
            s.push_str(if n + 1 == paths.len() { " or " } else { ", " });
        }
        s.push('\'');
        s.push_str(p);
        s.push('\'');
    }
    s
}

/// The candidates of `fragment` selected from a flake's `outputs`. Never
/// empty: a miss is `MissingAttr` naming every candidate, as Nix reports it.
pub fn from_outputs(
    ev: &mut Evaluator,
    outputs: Word,
    loc: &str,
    prefixes: Prefixes,
    fragment: &str,
    want: Want,
) -> EvalResult<Vec<Found>> {
    let candidates = attr_paths(prefixes, fragment, ev.current_system);
    let mut found = Vec::new();
    for c in &candidates {
        let path = crate::flake::parse_attr_path(c)
            .map_err(|m| Error::boxed(ErrorKind::Eval, m.to_string(), Pos::NONE))?;
        if let Some(value) = crate::flake::select(ev, outputs, &path)? {
            found.push(Found { value, path });
            if want == Want::First {
                return Ok(found);
            }
        }
    }
    if found.is_empty() {
        return Err(Error::boxed(
            ErrorKind::MissingAttr,
            format!(
                "flake '{loc}' does not provide attribute {}",
                show_attr_paths(&candidates)
            ),
            Pos::NONE,
        ));
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_and_fragment_attr_paths() {
        assert_eq!(
            attr_paths(Prefixes::DevShell, "", "x86_64-linux"),
            [
                "devShells.x86_64-linux.default",
                "devShell.x86_64-linux",
                "packages.x86_64-linux.default",
                "defaultPackage.x86_64-linux"
            ]
        );
        assert_eq!(
            attr_paths(Prefixes::DevShell, "ci", "x86_64-linux"),
            [
                "devShells.x86_64-linux.ci",
                "packages.x86_64-linux.ci",
                "legacyPackages.x86_64-linux.ci",
                "ci"
            ]
        );
        assert_eq!(
            attr_paths(Prefixes::Package, "", "s"),
            ["packages.s.default", "defaultPackage.s"]
        );
        assert_eq!(
            attr_paths(Prefixes::Package, "hello", "s"),
            ["packages.s.hello", "legacyPackages.s.hello", "hello"]
        );
        assert_eq!(
            attr_paths(Prefixes::PackageSet, "", "s"),
            ["packages.s", "legacyPackages.s"]
        );
        assert_eq!(
            attr_paths(Prefixes::PackageSet, "sub", "s"),
            ["packages.s.sub", "legacyPackages.s.sub", "sub"]
        );
        for p in [Prefixes::Package, Prefixes::DevShell, Prefixes::PackageSet] {
            assert_eq!(attr_paths(p, ".a.b", "s"), ["a.b"]);
        }
        assert_eq!(
            show_attr_paths(&["a".into(), "b".into(), "c".into()]),
            "'a', 'b' or 'c'"
        );
    }
}
