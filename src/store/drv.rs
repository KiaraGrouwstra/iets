use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use super::hash::{Hash, HashAlgo, HashFormat, hash_bytes};
use super::path::{
    DRV_EXTENSION, IngestionMethod, STORE_DIR, StorePath, make_fixed_output_path, make_output_path,
    make_text_path, output_path_name,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrvError(pub String);

impl fmt::Display for DrvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DrvError {}

impl From<super::path::StorePathError> for DrvError {
    fn from(e: super::path::StorePathError) -> DrvError {
        DrvError(e.0)
    }
}

impl From<super::hash::HashError> for DrvError {
    fn from(e: super::hash::HashError) -> DrvError {
        DrvError(e.0)
    }
}

/// Mirrors derivations.hh:DerivationOutput (Impure is not supported).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DerivationOutput {
    InputAddressed(StorePath),
    Fixed {
        method: IngestionMethod,
        hash: Hash,
    },
    Deferred,
    CaFloating {
        method: IngestionMethod,
        algo: HashAlgo,
    },
}

/// The env var that carries structured attrs (parsed-derivations.hh:StructuredAttrs::envVarName).
pub const STRUCTURED_ATTRS_ENV: &str = "__json";

/// Mirrors derivations.hh:Derivation. `env` must be sorted by key, bytewise, without duplicates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Derivation {
    pub name: String,
    pub outputs: BTreeMap<String, DerivationOutput>,
    pub input_srcs: BTreeSet<StorePath>,
    pub input_drvs: BTreeMap<StorePath, BTreeSet<String>>,
    pub platform: String,
    pub builder: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub structured_attrs: Option<String>,
}

/// Mirrors derivations.hh:DrvHash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrvHash {
    pub hashes: BTreeMap<String, Hash>,
    pub kind: DrvHashKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrvHashKind {
    Regular,
    Deferred,
}

/// Memo of `hash_derivation_modulo` per .drv path (derivations.cc:drvHashes).
pub type DrvHashCache = BTreeMap<StorePath, DrvHash>;

/// Reads a .drv that is not in the cache. Mirrors Store::readInvalidDerivation.
pub type DrvLoader<'a> = dyn FnMut(&StorePath) -> Result<Derivation, DrvError> + 'a;

/// Mirrors derivations.cc:printString: ATerm string with `\"`, `\\`, `\n`, `\r`, `\t` escapes.
fn print_string(out: &mut String, s: &str) {
    out.push('"');
    let mut rest = s;
    while let Some(i) = rest
        .bytes()
        .position(|b| matches!(b, b'"' | b'\\' | b'\n' | b'\r' | b'\t'))
    {
        out.push_str(&rest[..i]);
        out.push_str(match rest.as_bytes()[i] {
            b'"' => "\\\"",
            b'\\' => "\\\\",
            b'\n' => "\\n",
            b'\r' => "\\r",
            _ => "\\t",
        });
        rest = &rest[i + 1..];
    }
    out.push_str(rest);
    out.push('"');
}

/// Mirrors derivations.cc:printUnquotedString (no escaping; for store paths and names).
fn print_unquoted(out: &mut String, s: &str) {
    out.push('"');
    out.push_str(s);
    out.push('"');
}

fn print_list<'a>(out: &mut String, items: impl Iterator<Item = &'a str>, quoted: bool) {
    out.push('[');
    for (i, s) in items.enumerate() {
        if i > 0 {
            out.push(',');
        }
        if quoted {
            print_string(out, s);
        } else {
            print_unquoted(out, s);
        }
    }
    out.push(']');
}

impl DerivationOutput {
    /// Mirrors derivations.cc:DerivationOutput::CAFixed::path.
    pub fn fixed_path(
        &self,
        drv_name: &str,
        output_name: &str,
    ) -> Result<Option<StorePath>, DrvError> {
        match self {
            DerivationOutput::Fixed { method, hash } => Ok(Some(make_fixed_output_path(
                *method,
                hash,
                &output_path_name(drv_name, output_name),
                &BTreeSet::new(),
            )?)),
            _ => Ok(None),
        }
    }

    /// Mirrors derivations.cc:DerivationOutput::path: the statically known output path, if any.
    pub fn path(&self, drv_name: &str, output_name: &str) -> Result<Option<StorePath>, DrvError> {
        match self {
            DerivationOutput::InputAddressed(p) => Ok(Some(p.clone())),
            DerivationOutput::Fixed { .. } => self.fixed_path(drv_name, output_name),
            DerivationOutput::Deferred | DerivationOutput::CaFloating { .. } => Ok(None),
        }
    }
}

/// Mirrors content-address.cc:ContentAddress::printMethodAlgo: `r:sha256` or `sha256`.
fn method_algo(method: IngestionMethod, algo: HashAlgo) -> String {
    format!("{}{}", method.prefix(), algo.name())
}

impl Derivation {
    /// Mirrors derivations.cc:Derivation::unparse without masking.
    pub fn unparse(&self) -> Result<String, DrvError> {
        self.unparse_with(false, None)
    }

    /// Mirrors derivations.cc:Derivation::unparse(store, maskOutputs, actualInputs).
    pub fn unparse_with(
        &self,
        mask_outputs: bool,
        actual_inputs: Option<&BTreeMap<String, BTreeSet<String>>>,
    ) -> Result<String, DrvError> {
        let mut s = String::with_capacity(4096);
        s.push_str("Derive([");
        for (i, (name, out)) in self.outputs.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push('(');
            print_unquoted(&mut s, name);
            match out {
                DerivationOutput::InputAddressed(p) => {
                    s.push(',');
                    let path = if mask_outputs {
                        String::new()
                    } else {
                        p.to_string()
                    };
                    print_unquoted(&mut s, &path);
                    s.push_str(",\"\",\"\"");
                }
                DerivationOutput::Fixed { method, hash } => {
                    s.push(',');
                    let path = if mask_outputs {
                        String::new()
                    } else {
                        self.fixed_output_path(name)?.to_string()
                    };
                    print_unquoted(&mut s, &path);
                    s.push(',');
                    print_unquoted(&mut s, &method_algo(*method, hash.algo));
                    s.push(',');
                    print_unquoted(&mut s, &hash.to_base16());
                }
                DerivationOutput::CaFloating { method, algo } => {
                    s.push_str(",\"\",");
                    print_unquoted(&mut s, &method_algo(*method, *algo));
                    s.push_str(",\"\"");
                }
                DerivationOutput::Deferred => s.push_str(",\"\",\"\",\"\""),
            }
            s.push(')');
        }
        s.push_str("],[");
        match actual_inputs {
            Some(inputs) => {
                for (i, (hash_hex, outputs)) in inputs.iter().enumerate() {
                    if i > 0 {
                        s.push(',');
                    }
                    s.push('(');
                    print_unquoted(&mut s, hash_hex);
                    s.push(',');
                    print_list(&mut s, outputs.iter().map(String::as_str), false);
                    s.push(')');
                }
            }
            None => {
                for (i, (drv_path, outputs)) in self.input_drvs.iter().enumerate() {
                    if i > 0 {
                        s.push(',');
                    }
                    s.push('(');
                    print_unquoted(&mut s, &drv_path.to_string());
                    s.push(',');
                    print_list(&mut s, outputs.iter().map(String::as_str), false);
                    s.push(')');
                }
            }
        }
        s.push_str("],");
        let srcs: Vec<String> = self.input_srcs.iter().map(StorePath::to_string).collect();
        print_list(&mut s, srcs.iter().map(String::as_str), false);
        s.push(',');
        print_unquoted(&mut s, &self.platform);
        s.push(',');
        print_string(&mut s, &self.builder);
        s.push(',');
        print_list(&mut s, self.args.iter().map(String::as_str), true);
        s.push_str(",[");
        let mut first = true;
        let mut emit = |s: &mut String, k: &str, v: &str| {
            if first {
                first = false;
            } else {
                s.push(',');
            }
            s.push('(');
            print_string(s, k);
            s.push(',');
            let masked = mask_outputs && self.outputs.contains_key(k);
            print_string(s, if masked { "" } else { v });
            s.push(')');
        };
        let mut json_done = self.structured_attrs.is_none();
        for (k, v) in &self.env {
            if !json_done && k.as_str() > STRUCTURED_ATTRS_ENV {
                emit(
                    &mut s,
                    STRUCTURED_ATTRS_ENV,
                    self.structured_attrs.as_deref().unwrap(),
                );
                json_done = true;
            }
            emit(&mut s, k, v);
        }
        if !json_done {
            emit(
                &mut s,
                STRUCTURED_ATTRS_ENV,
                self.structured_attrs.as_deref().unwrap(),
            );
        }
        s.push_str("])");
        Ok(s)
    }

    /// Mirrors derivations.cc:DerivationOutput::CAFixed::path for output `name`.
    pub fn fixed_output_path(&self, output_name: &str) -> Result<StorePath, DrvError> {
        match self.outputs.get(output_name) {
            Some(o) => o
                .fixed_path(&self.name, output_name)?
                .ok_or_else(|| DrvError(format!("output '{output_name}' is not fixed"))),
            None => Err(DrvError(format!("no output '{output_name}'"))),
        }
    }

    /// Mirrors derivations.cc:DerivationType::isFixed: every output is fixed.
    pub fn is_fixed(&self) -> bool {
        !self.outputs.is_empty()
            && self
                .outputs
                .values()
                .all(|o| matches!(o, DerivationOutput::Fixed { .. }))
    }

    /// Mirrors derivations.cc:parseDerivation for the traditional `Derive(...)` form.
    pub fn parse(s: &str, name: &str) -> Result<Derivation, DrvError> {
        let mut p = Parser {
            s: s.as_bytes(),
            pos: 0,
        };
        let mut drv = Derivation {
            name: name.to_string(),
            ..Default::default()
        };
        p.expect(b"Derive([")?;
        while !p.end_of_list()? {
            p.expect(b"(")?;
            let id = p.string()?;
            p.expect(b",")?;
            let path = p.string()?;
            p.expect(b",")?;
            let hash_algo = p.string()?;
            p.expect(b",")?;
            let hash = p.string()?;
            p.expect(b")")?;
            drv.outputs
                .insert(id, parse_output(&path, &hash_algo, &hash)?);
        }
        p.expect(b",[")?;
        while !p.end_of_list()? {
            p.expect(b"(")?;
            let drv_path = p.path()?;
            p.expect(b",")?;
            if p.peek() == Some(b'(') {
                return Err(DrvError(
                    "dynamic derivation inputs are not supported".into(),
                ));
            }
            let outputs = p.string_list()?;
            p.expect(b")")?;
            drv.input_drvs.insert(StorePath::parse(&drv_path)?, outputs);
        }
        p.expect(b",")?;
        for src in p.string_list()? {
            if !src.starts_with('/') {
                return Err(DrvError(format!("bad path '{src}' in derivation")));
            }
            drv.input_srcs.insert(StorePath::parse(&src)?);
        }
        p.expect(b",")?;
        drv.platform = p.string()?;
        p.expect(b",")?;
        drv.builder = p.string()?;
        p.expect(b",[")?;
        while !p.end_of_list()? {
            drv.args.push(p.string()?);
        }
        p.expect(b",[")?;
        while !p.end_of_list()? {
            p.expect(b"(")?;
            let k = p.string()?;
            p.expect(b",")?;
            let v = p.string()?;
            p.expect(b")")?;
            if k == STRUCTURED_ATTRS_ENV {
                drv.structured_attrs = Some(v);
            } else {
                drv.env.push((k, v));
            }
        }
        p.expect(b")")?;
        drv.env.sort_by(|a, b| a.0.cmp(&b.0));
        drv.env.dedup_by(|later, earlier| {
            if later.0 == earlier.0 {
                std::mem::swap(&mut earlier.1, &mut later.1);
                true
            } else {
                false
            }
        });
        Ok(drv)
    }

    /// Mirrors derivations.cc:BasicDerivation::nameFromPath: strips `.drv` from the path name.
    pub fn name_from_path(drv_path: &StorePath) -> Result<&str, DrvError> {
        drv_path
            .name()
            .strip_suffix(DRV_EXTENSION)
            .ok_or_else(|| DrvError(format!("'{drv_path}' is not a derivation path")))
    }

    /// Mirrors derivations.cc:computeStorePath: the text path of the unparsed ATerm with
    /// references = inputSrcs + inputDrvs.
    pub fn drv_path(&self) -> Result<StorePath, DrvError> {
        self.drv_path_for(&self.unparse()?)
    }

    /// `drv_path` for a caller that already holds `self.unparse()`, so the ATerm is built once.
    pub fn drv_path_for(&self, aterm: &str) -> Result<StorePath, DrvError> {
        let mut references = self.input_srcs.clone();
        references.extend(self.input_drvs.keys().cloned());
        let hash = hash_bytes(HashAlgo::Sha256, aterm.as_bytes());
        Ok(make_text_path(
            &format!("{}{DRV_EXTENSION}", self.name),
            &hash,
            &references,
        )?)
    }

    /// Looks up `key` in the sorted env.
    pub fn env_get(&self, key: &str) -> Option<&str> {
        self.env
            .binary_search_by(|(k, _)| k.as_str().cmp(key))
            .ok()
            .map(|i| self.env[i].1.as_str())
    }

    /// Inserts or replaces `key` in the sorted env.
    pub fn env_set(&mut self, key: &str, value: String) {
        match self.env.binary_search_by(|(k, _)| k.as_str().cmp(key)) {
            Ok(i) => self.env[i].1 = value,
            Err(i) => self.env.insert(i, (key.to_string(), value)),
        }
    }
}

/// Mirrors derivations.cc:parseDerivationOutput(store, pathS, hashAlgoStr, hashS).
fn parse_output(path: &str, hash_algo: &str, hash: &str) -> Result<DerivationOutput, DrvError> {
    if hash_algo.is_empty() {
        if path.is_empty() {
            return Ok(DerivationOutput::Deferred);
        }
        return Ok(DerivationOutput::InputAddressed(StorePath::parse(path)?));
    }
    let (method, algo_name) = match hash_algo.strip_prefix("r:") {
        Some(rest) => (IngestionMethod::Recursive, rest),
        None => {
            if hash_algo.starts_with("git:") || hash_algo.starts_with("text:") {
                return Err(DrvError(format!(
                    "unsupported output hash method '{hash_algo}'"
                )));
            }
            (IngestionMethod::Flat, hash_algo)
        }
    };
    let algo = HashAlgo::parse(algo_name)
        .ok_or_else(|| DrvError(format!("unknown hash algorithm '{algo_name}'")))?;
    if hash == "impure" {
        return Err(DrvError("impure derivations are not supported".into()));
    }
    if hash.is_empty() {
        if !path.is_empty() {
            return Err(DrvError(
                "content-addressing derivation output should not specify output path".into(),
            ));
        }
        return Ok(DerivationOutput::CaFloating { method, algo });
    }
    if !path.starts_with('/') {
        return Err(DrvError(format!("bad path '{path}' in derivation")));
    }
    let hash = Hash::parse_non_sri_unprefixed(hash, algo)?;
    Ok(DerivationOutput::Fixed { method, hash })
}

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn expect(&mut self, lit: &[u8]) -> Result<(), DrvError> {
        if self.s[self.pos..].starts_with(lit) {
            self.pos += lit.len();
            Ok(())
        } else {
            Err(DrvError(format!(
                "expected string '{}' at offset {}",
                String::from_utf8_lossy(lit),
                self.pos
            )))
        }
    }

    /// Mirrors derivations.cc:endOfList.
    fn end_of_list(&mut self) -> Result<bool, DrvError> {
        match self.peek() {
            Some(b',') => {
                self.pos += 1;
                Ok(false)
            }
            Some(b']') => {
                self.pos += 1;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Mirrors derivations.cc:parseString.
    fn string(&mut self) -> Result<String, DrvError> {
        self.expect(b"\"")?;
        let mut out = Vec::new();
        loop {
            let c = *self
                .s
                .get(self.pos)
                .ok_or_else(|| DrvError("unterminated string in derivation".into()))?;
            self.pos += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = *self
                        .s
                        .get(self.pos)
                        .ok_or_else(|| DrvError("unterminated string in derivation".into()))?;
                    self.pos += 1;
                    out.push(match e {
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b't' => b'\t',
                        e => e,
                    });
                }
                c => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| DrvError("non-UTF-8 string in derivation".into()))
    }

    /// Mirrors derivations.cc:parsePath.
    fn path(&mut self) -> Result<String, DrvError> {
        let s = self.string()?;
        if !s.starts_with('/') {
            return Err(DrvError(format!("bad path '{s}' in derivation")));
        }
        Ok(s)
    }

    /// Mirrors derivations.cc:parseStrings.
    fn string_list(&mut self) -> Result<BTreeSet<String>, DrvError> {
        let mut res = BTreeSet::new();
        self.expect(b"[")?;
        while !self.end_of_list()? {
            res.insert(self.string()?);
        }
        Ok(res)
    }
}

/// Mirrors derivations.cc:hashDerivationModulo. `loader` reads .drv files that are not cached.
pub fn hash_derivation_modulo(
    drv: &Derivation,
    mask_outputs: bool,
    cache: &mut DrvHashCache,
    loader: &mut DrvLoader<'_>,
) -> Result<DrvHash, DrvError> {
    if drv.is_fixed() {
        let mut hashes = BTreeMap::new();
        for (name, out) in &drv.outputs {
            let DerivationOutput::Fixed { method, hash } = out else {
                unreachable!()
            };
            let payload = format!(
                "fixed:out:{}:{}:{}",
                method_algo(*method, hash.algo),
                hash.to_base16(),
                drv.fixed_output_path(name)?
            );
            hashes.insert(
                name.clone(),
                hash_bytes(HashAlgo::Sha256, payload.as_bytes()),
            );
        }
        return Ok(DrvHash {
            hashes,
            kind: DrvHashKind::Regular,
        });
    }

    let mut kind = if drv
        .outputs
        .values()
        .any(|o| matches!(o, DerivationOutput::CaFloating { .. }))
    {
        DrvHashKind::Deferred
    } else {
        DrvHashKind::Regular
    };

    let mut inputs2: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (drv_path, outputs) in &drv.input_drvs {
        let res = path_derivation_modulo(drv_path, cache, loader)?;
        if res.kind == DrvHashKind::Deferred {
            kind = DrvHashKind::Deferred;
        }
        for output in outputs {
            let h = res.hashes.get(output).ok_or_else(|| {
                DrvError(format!(
                    "no hash for output '{output}' of derivation '{}'",
                    drv.name
                ))
            })?;
            inputs2
                .entry(h.to_string_with(HashFormat::Base16, false))
                .or_default()
                .insert(output.clone());
        }
    }

    let hash = hash_bytes(
        HashAlgo::Sha256,
        drv.unparse_with(mask_outputs, Some(&inputs2))?.as_bytes(),
    );
    let hashes = drv.outputs.keys().map(|k| (k.clone(), hash)).collect();
    Ok(DrvHash { hashes, kind })
}

/// Mirrors derivations.cc:pathDerivationModulo (memoized by .drv path).
pub fn path_derivation_modulo(
    drv_path: &StorePath,
    cache: &mut DrvHashCache,
    loader: &mut DrvLoader<'_>,
) -> Result<DrvHash, DrvError> {
    if let Some(h) = cache.get(drv_path) {
        return Ok(h.clone());
    }
    let drv = loader(drv_path)?;
    let h = hash_derivation_modulo(&drv, false, cache, loader)?;
    cache.insert(drv_path.clone(), h.clone());
    Ok(h)
}

/// Reads and parses `/nix/store/<hash>-<name>.drv`. Mirrors Store::readInvalidDerivation for a
/// local store; the default `DrvLoader`.
pub fn load_derivation_from_store(drv_path: &StorePath) -> Result<Derivation, DrvError> {
    let name = Derivation::name_from_path(drv_path)?;
    let full = format!("{STORE_DIR}/{}", drv_path.base_name());
    let text = std::fs::read_to_string(&full)
        .map_err(|e| DrvError(format!("cannot read derivation '{full}': {e}")))?;
    Derivation::parse(&text, name)
}

/// Mirrors derivations.cc:Derivation::fillInOutputPaths as used by primops.cc:derivationStrict:
/// Deferred outputs become input-addressed paths from the masked hash modulo, and the env
/// entries named after the outputs are set to the paths (fixed outputs included).
pub fn finalize_input_addressed_outputs(
    drv: &mut Derivation,
    cache: &mut DrvHashCache,
    loader: &mut DrvLoader<'_>,
) -> Result<(), DrvError> {
    let mut hashes_modulo: Option<DrvHash> = None;
    let names: Vec<String> = drv.outputs.keys().cloned().collect();
    for name in names {
        let out = drv.outputs[&name].clone();
        let actual = match out {
            DerivationOutput::Fixed { .. } => drv.fixed_output_path(&name)?,
            DerivationOutput::InputAddressed(_) | DerivationOutput::Deferred => {
                if hashes_modulo.is_none() {
                    hashes_modulo = Some(hash_derivation_modulo(drv, true, cache, loader)?);
                }
                let hm = hashes_modulo.as_ref().unwrap();
                match hm.kind {
                    DrvHashKind::Deferred => {
                        if let DerivationOutput::InputAddressed(p) = out {
                            return Err(DrvError(format!(
                                "derivation has incorrect output '{p}', should be deferred"
                            )));
                        }
                        continue;
                    }
                    DrvHashKind::Regular => {
                        let h = hm.hashes.get(&name).ok_or_else(|| {
                            DrvError(format!("derivation produced no hash for output '{name}'"))
                        })?;
                        let path = make_output_path(&name, h, &drv.name)?;
                        if let DerivationOutput::InputAddressed(p) = &out
                            && *p != path
                        {
                            return Err(DrvError(format!(
                                "derivation has incorrect output '{p}', should be '{path}'"
                            )));
                        }
                        drv.outputs
                            .insert(name.clone(), DerivationOutput::InputAddressed(path.clone()));
                        path
                    }
                }
            }
            DerivationOutput::CaFloating { .. } => continue,
        };
        let actual = actual.to_string();
        match drv.env_get(&name) {
            None | Some("") => drv.env_set(&name, actual),
            Some(v) if v != actual => {
                return Err(DrvError(format!(
                    "derivation has incorrect environment variable '{name}', should be '{actual}' but is actually '{v}'"
                )));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn fixture_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/drv")
    }

    /// Copies of /nix/store/*.drv produced by Nix 2.34.8 (`cp /nix/store/<name> tests/fixtures/drv/`).
    /// The file name is the store path, so `drv_path()` is checked against it.
    fn fixtures() -> Vec<(StorePath, String)> {
        let mut v: Vec<(StorePath, String)> = std::fs::read_dir(fixture_dir())
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                let base = e.file_name().into_string().unwrap();
                (
                    StorePath::from_base_name(&base).unwrap(),
                    std::fs::read_to_string(e.path()).unwrap(),
                )
            })
            .collect();
        v.sort();
        assert!(
            v.len() >= 30,
            "expected at least 30 fixtures, got {}",
            v.len()
        );
        v
    }

    fn fixture_loader(drv_path: &StorePath) -> Result<Derivation, DrvError> {
        let local = fixture_dir().join(drv_path.base_name());
        if local.exists() {
            let text = std::fs::read_to_string(local).unwrap();
            return Derivation::parse(&text, Derivation::name_from_path(drv_path)?);
        }
        load_derivation_from_store(drv_path)
    }

    #[test]
    fn fixtures_roundtrip_byte_exact() {
        let mut fixed = 0;
        let mut multi = 0;
        let mut json = 0;
        for (path, text) in fixtures() {
            let drv = Derivation::parse(&text, Derivation::name_from_path(&path).unwrap()).unwrap();
            assert_eq!(drv.unparse().unwrap(), text, "unparse mismatch for {path}");
            fixed += usize::from(drv.is_fixed());
            multi += usize::from(drv.outputs.len() > 1);
            json += usize::from(drv.structured_attrs.is_some());
        }
        assert!(fixed >= 8, "fixed-output fixtures: {fixed}");
        assert!(multi >= 10, "multi-output fixtures: {multi}");
        assert!(json >= 3, "structured-attrs fixtures: {json}");
    }

    #[test]
    fn fixtures_drv_path_matches_store_path() {
        for (path, text) in fixtures() {
            let drv = Derivation::parse(&text, Derivation::name_from_path(&path).unwrap()).unwrap();
            assert_eq!(
                drv.drv_path().unwrap(),
                path,
                "drv_path mismatch for {path}"
            );
        }
    }

    /// Resets every input-addressed output to Deferred with an empty env value, as
    /// primops.cc:derivationStrict does, then checks that finalize reproduces the original.
    /// Needs the input .drv closure from /nix/store; fixtures that lack it are skipped.
    #[test]
    fn fixtures_output_paths_recompute() {
        let mut cache = DrvHashCache::new();
        let mut loader = fixture_loader;
        let mut checked = 0;
        for (path, text) in fixtures() {
            let original =
                Derivation::parse(&text, Derivation::name_from_path(&path).unwrap()).unwrap();
            let closure_present = original.input_drvs.keys().all(|p| {
                Path::new(&p.to_string()).exists() || fixture_dir().join(p.base_name()).exists()
            });
            if !closure_present {
                eprintln!("skipping {path}: input .drv closure not in /nix/store");
                continue;
            }
            let mut drv = original.clone();
            for (name, out) in drv.outputs.iter_mut() {
                if matches!(out, DerivationOutput::InputAddressed(_)) {
                    *out = DerivationOutput::Deferred;
                    drv.env
                        .iter_mut()
                        .find(|(k, _)| k == name)
                        .unwrap()
                        .1
                        .clear();
                }
            }
            finalize_input_addressed_outputs(&mut drv, &mut cache, &mut loader).unwrap();
            assert_eq!(drv, original, "output paths differ for {path}");
            checked += 1;
        }
        assert!(checked >= 20, "recomputed only {checked} derivations");
    }

    fn simple(name: &str, outputs: &[&str]) -> Derivation {
        let mut drv = Derivation {
            name: name.into(),
            platform: "x86_64-linux".into(),
            builder: "/bin/sh".into(),
            ..Default::default()
        };
        for o in outputs {
            drv.outputs
                .insert(o.to_string(), DerivationOutput::Deferred);
            drv.env_set(o, String::new());
        }
        drv.env_set("builder", "/bin/sh".into());
        drv.env_set("name", name.into());
        drv.env_set("system", "x86_64-linux".into());
        drv
    }

    // nix-instantiate drvs.nix -A ia, with
    //   ia = builtins.derivation { name = "ia"; system = "x86_64-linux"; builder = "/bin/sh";
    //     args = ["-c" "x\"y\\z\n"]; outputs = ["out" "dev"]; FOO = "b\tar"; };
    #[test]
    fn input_addressed_multi_output_from_scratch() {
        let mut drv = simple("ia", &["out", "dev"]);
        drv.args = vec!["-c".into(), "x\"y\\z\n".into()];
        drv.env_set("FOO", "b\tar".into());
        drv.env_set("outputs", "out dev".into());
        let mut cache = DrvHashCache::new();
        finalize_input_addressed_outputs(&mut drv, &mut cache, &mut |p| {
            Err(DrvError(format!("unexpected load of {p}")))
        })
        .unwrap();
        assert_eq!(
            drv.drv_path().unwrap().to_string(),
            "/nix/store/73as87hfqgllqymcyhn1bi8iman2wwqx-ia.drv"
        );
        assert_eq!(
            drv.outputs["out"],
            DerivationOutput::InputAddressed(
                StorePath::parse("/nix/store/0rzxv3xkkgply9jd76fdiwfm6k1gyj6p-ia").unwrap()
            )
        );
        assert_eq!(
            drv.outputs["dev"],
            DerivationOutput::InputAddressed(
                StorePath::parse("/nix/store/nx6zsacidiymqhs3yji2j0i0srcnnvkc-ia-dev").unwrap()
            )
        );
        assert_eq!(
            drv.env_get("dev"),
            Some("/nix/store/nx6zsacidiymqhs3yji2j0i0srcnnvkc-ia-dev")
        );
        let expected =
            std::fs::read_to_string(fixture_dir().join("73as87hfqgllqymcyhn1bi8iman2wwqx-ia.drv"))
                .unwrap();
        assert_eq!(drv.unparse().unwrap(), expected);
    }

    // nix-instantiate drvs.nix -A fr, with
    //   fr = builtins.derivation { name = "fr"; system = "x86_64-linux"; builder = "/bin/sh";
    //     outputHash = "ba78...15ad"; outputHashAlgo = "sha256"; outputHashMode = "recursive"; };
    #[test]
    fn fixed_output_from_scratch() {
        let hex = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let mut drv = simple("fr", &[]);
        drv.outputs.insert(
            "out".into(),
            DerivationOutput::Fixed {
                method: IngestionMethod::Recursive,
                hash: Hash::parse_any(hex, Some(HashAlgo::Sha256)).unwrap(),
            },
        );
        drv.env_set("outputHash", hex.into());
        drv.env_set("outputHashAlgo", "sha256".into());
        drv.env_set("outputHashMode", "recursive".into());
        let mut cache = DrvHashCache::new();
        finalize_input_addressed_outputs(&mut drv, &mut cache, &mut |p| {
            Err(DrvError(format!("unexpected load of {p}")))
        })
        .unwrap();
        assert_eq!(
            drv.env_get("out"),
            Some("/nix/store/s05rl80bpix1186wb5f471k7fxwwp7lf-fr")
        );
        assert_eq!(
            drv.drv_path().unwrap().to_string(),
            "/nix/store/sqjiq38d04j09g4dws6a9d644618zf21-fr.drv"
        );
        let expected =
            std::fs::read_to_string(fixture_dir().join("sqjiq38d04j09g4dws6a9d644618zf21-fr.drv"))
                .unwrap();
        assert_eq!(drv.unparse().unwrap(), expected);
    }

    // nix-instantiate drvs.nix -A dep2, with
    //   dep = builtins.derivation { name = "dep"; system = "x86_64-linux"; builder = "/bin/sh";
    //     f = fx; src = builtins.toFile "a" "x"; };
    //   dep2 = builtins.derivation { name = "dep2"; system = "x86_64-linux"; builder = "/bin/sh";
    //     d = ia.dev; e = "${dep}"; };
    // This exercises the fixed-output special case (fx) and the recursion through the cache.
    #[test]
    fn input_drvs_hash_modulo_from_scratch() {
        let ia = StorePath::parse("/nix/store/73as87hfqgllqymcyhn1bi8iman2wwqx-ia.drv").unwrap();
        let dep = StorePath::parse("/nix/store/wgj4nv00ip0zkjhgs6bq3f6yrfikw0m5-dep.drv").unwrap();
        let mut drv = simple("dep2", &["out"]);
        drv.env_set(
            "d",
            "/nix/store/nx6zsacidiymqhs3yji2j0i0srcnnvkc-ia-dev".into(),
        );
        drv.env_set(
            "e",
            "/nix/store/pzjq1hdf1x9qs1dxcqiba82dyl1274aa-dep".into(),
        );
        drv.input_drvs
            .insert(ia, ["dev".to_string()].into_iter().collect());
        drv.input_drvs
            .insert(dep.clone(), ["out".to_string()].into_iter().collect());
        let mut cache = DrvHashCache::new();
        finalize_input_addressed_outputs(&mut drv, &mut cache, &mut fixture_loader).unwrap();
        assert_eq!(
            drv.drv_path().unwrap().to_string(),
            "/nix/store/0fdrgjkjrlf9vi5xgyxfl6s8ihav2idi-dep2.drv"
        );
        assert_eq!(
            drv.env_get("out"),
            Some("/nix/store/f1lfacbx1gq8g6pw0290cijhli2n3nls-dep2")
        );
        assert!(cache.contains_key(&dep));
        assert!(cache.contains_key(
            &StorePath::parse("/nix/store/pckkdf72dvcpi4l4cbg668xbfvyg4ps8-fx.drv").unwrap()
        ));
    }

    #[test]
    fn string_escapes_roundtrip() {
        let mut s = String::new();
        print_string(&mut s, "a\"b\\c\nd\re\tf$g");
        assert_eq!(s, "\"a\\\"b\\\\c\\nd\\re\\tf$g\"");
        let mut p = Parser {
            s: s.as_bytes(),
            pos: 0,
        };
        assert_eq!(p.string().unwrap(), "a\"b\\c\nd\re\tf$g");
        assert!(
            Parser {
                s: b"\"abc",
                pos: 0
            }
            .string()
            .is_err()
        );
        assert!(
            Parser {
                s: b"\"abc\\",
                pos: 0
            }
            .string()
            .is_err()
        );
    }

    #[test]
    fn masked_unparse_blanks_output_paths_and_env() {
        let text =
            std::fs::read_to_string(fixture_dir().join("73as87hfqgllqymcyhn1bi8iman2wwqx-ia.drv"))
                .unwrap();
        let drv = Derivation::parse(&text, "ia").unwrap();
        let masked = drv.unparse_with(true, None).unwrap();
        assert!(masked.starts_with("Derive([(\"dev\",\"\",\"\",\"\"),(\"out\",\"\",\"\",\"\")],"));
        assert!(masked.contains("(\"dev\",\"\")"));
        assert!(masked.contains("(\"out\",\"\")"));
        assert!(!masked.contains("/nix/store/0rzxv3xkkgply9jd76fdiwfm6k1gyj6p-ia"));
    }
}
