use std::fmt;

use super::path::{STORE_DIR, StorePath, StorePathError};

/// One string-context element. Mirrors value/context.hh:NixStringContextElem.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ContextElem {
    /// A plain store path: `<path>`.
    Opaque(StorePath),
    /// A derivation and its whole closure: `=<drvpath>`.
    DrvDeep(StorePath),
    /// One output of a derivation: `!<output>!<drvpath>`.
    Built { drv_path: StorePath, output: String },
}

impl ContextElem {
    /// Mirrors value/context.cc:NixStringContextElem::parse. The path part may be a base name
    /// (Nix's internal form) or a full `/nix/store/...` path (the `getContext` form).
    pub fn parse(s: &str) -> Result<ContextElem, StorePathError> {
        let bad = |why: &str| StorePathError(format!("Bad String Context element: {s}: {why}"));
        if s.is_empty() {
            return Err(bad(
                "String context element should never be an empty string",
            ));
        }
        match s.as_bytes()[0] {
            b'!' => {
                let rest = &s[1..];
                let (output, path) = rest.split_once('!').ok_or_else(|| {
                    bad("String content element beginning with '!' should have a second '!'")
                })?;
                if path.contains('!') {
                    return Err(bad("dynamic derivation outputs are not supported"));
                }
                Ok(ContextElem::Built {
                    drv_path: parse_path(path)?,
                    output: output.to_string(),
                })
            }
            b'=' => Ok(ContextElem::DrvDeep(parse_path(&s[1..])?)),
            _ => {
                if s.contains('!') {
                    return Err(bad(
                        "String content element not beginning with '!' should not have a second '!'",
                    ));
                }
                Ok(ContextElem::Opaque(parse_path(s)?))
            }
        }
    }

    /// The `getContext` / `appendContext` form with full store paths.
    pub fn to_full_string(&self) -> String {
        match self {
            ContextElem::Opaque(p) => p.to_string(),
            ContextElem::DrvDeep(p) => format!("={p}"),
            ContextElem::Built { drv_path, output } => format!("!{output}!{drv_path}"),
        }
    }

    /// Mirrors value/context.cc:NixStringContextElem::to_string (base names, Nix's internal form).
    pub fn to_internal_string(&self) -> String {
        match self {
            ContextElem::Opaque(p) => p.base_name().to_string(),
            ContextElem::DrvDeep(p) => format!("={}", p.base_name()),
            ContextElem::Built { drv_path, output } => {
                format!("!{output}!{}", drv_path.base_name())
            }
        }
    }
}

impl fmt::Display for ContextElem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_full_string())
    }
}

fn parse_path(s: &str) -> Result<StorePath, StorePathError> {
    if s.starts_with(STORE_DIR) {
        StorePath::parse(s)
    } else {
        StorePath::from_base_name(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // nix-instantiate --eval --strict -E 'let d = builtins.derivation { name = "ia"; system = "x86_64-linux";
    //   builder = "/bin/sh"; }; in builtins.getContext "${d.drvPath}${d.out}${builtins.toFile "f" "x"}"'
    // gives keys /nix/store/4glqg5r5alz14nppx4a1pi3657ws2d0w-ia.drv (allOutputs + outputs = ["out"])
    // and /nix/store/x93g3gvygaiq7h4b6zls3w7l5az1y526-f (path = true).
    #[test]
    fn parse_and_format() {
        let drv = "/nix/store/4glqg5r5alz14nppx4a1pi3657ws2d0w-ia.drv";
        let file = "/nix/store/x93g3gvygaiq7h4b6zls3w7l5az1y526-f";
        let deep = ContextElem::parse(&format!("={drv}")).unwrap();
        assert_eq!(deep, ContextElem::DrvDeep(StorePath::parse(drv).unwrap()));
        assert_eq!(deep.to_full_string(), format!("={drv}"));
        assert_eq!(
            deep.to_internal_string(),
            "=4glqg5r5alz14nppx4a1pi3657ws2d0w-ia.drv"
        );

        let built = ContextElem::parse(&format!("!out!{drv}")).unwrap();
        assert_eq!(
            built,
            ContextElem::Built {
                drv_path: StorePath::parse(drv).unwrap(),
                output: "out".into()
            }
        );
        assert_eq!(built.to_full_string(), format!("!out!{drv}"));
        assert_eq!(
            ContextElem::parse(&built.to_internal_string()).unwrap(),
            built
        );

        let opaque = ContextElem::parse(file).unwrap();
        assert_eq!(opaque, ContextElem::Opaque(StorePath::parse(file).unwrap()));
        assert_eq!(
            opaque.to_internal_string(),
            "x93g3gvygaiq7h4b6zls3w7l5az1y526-f"
        );
        assert_eq!(
            ContextElem::parse(&opaque.to_internal_string()).unwrap(),
            opaque
        );

        assert!(ContextElem::parse("").is_err());
        assert!(ContextElem::parse("!out").is_err());
        assert!(ContextElem::parse(&format!("out!{drv}")).is_err());
        assert!(ContextElem::parse(&format!("!out!!dev!{drv}")).is_err());
    }
}
