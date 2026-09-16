//! `derivationStrict` and the derivation path pipeline (primops.cc:
//! derivationStrictInternal): attrs -> `Derivation` -> output paths ->
//! drvPath. Also `placeholder` and `import` of a `.drv` store path.

use std::collections::BTreeSet;
use std::path::PathBuf;

use super::Registry;
use crate::eval::{Error, ErrorKind, EvalResult, Evaluator, Pos, bail};
use crate::print::{json_escape, lexicographic_order, print_json};
use crate::runtime::*;
use crate::store::drv::{
    Derivation, DerivationOutput, DrvError, DrvHashCache, finalize_input_addressed_outputs,
    hash_derivation_modulo, load_derivation_from_store,
};
use crate::store::hash::{Hash, HashAlgo, hash_bytes};
use crate::store::path::{
    DRV_EXTENSION, IngestionMethod, StorePath, check_name, hash_placeholder, output_path_name,
};
use crate::syntax::symbol::Sym;

pub fn register(r: &mut Registry) {
    r.op("derivationStrict", 1, derivation_strict);
    r.op("placeholder", 1, placeholder);
}

/// What later derivations need about one we created: its output names and
/// the references of its `.drv` file (for `=drv` context closures). The
/// `hashDerivationModulo` result lives in `DrvState::hash_cache`, so the full
/// `Derivation` is never needed again.
#[derive(Clone, Debug)]
pub struct DrvInfo {
    pub outputs: BTreeSet<String>,
    pub references: BTreeSet<StorePath>,
}

/// Per-evaluator derivation state (derivations.cc:drvHashes plus the
/// derivations this evaluation created).
#[derive(Default, Clone)]
pub struct DrvState {
    pub hash_cache: crate::parallel::shared::Shared<DrvHashCache>,
    pub drvs: crate::parallel::shared::SharedMap<StorePath, DrvInfo>,
    /// `ietsp eval --dump-drvs DIR`: every created derivation is written as
    /// `DIR/<drv basename>` in ATerm form.
    pub dump_dir: Option<PathBuf>,
    pub nr_derivations: u64,
    syms: Option<DrvSyms>,
    imported_drv_fn: Option<Word>,
}

#[derive(Clone, Copy)]
struct DrvSyms {
    args: Sym,
    builder: Sym,
    system: Sym,
    output_hash: Sym,
    output_hash_algo: Sym,
    output_hash_mode: Sym,
    outputs: Sym,
    structured_attrs: Sym,
    ignore_nulls: Sym,
    content_addressed: Sym,
    impure: Sym,
    json: Sym,
}

fn syms(ev: &mut Evaluator) -> DrvSyms {
    if let Some(s) = ev.drv.syms {
        return s;
    }
    let s = DrvSyms {
        args: ev.sym(b"args"),
        builder: ev.sym(b"builder"),
        system: ev.sym(b"system"),
        output_hash: ev.sym(b"outputHash"),
        output_hash_algo: ev.sym(b"outputHashAlgo"),
        output_hash_mode: ev.sym(b"outputHashMode"),
        outputs: ev.sym(b"outputs"),
        structured_attrs: ev.sym(b"__structuredAttrs"),
        ignore_nulls: ev.sym(b"__ignoreNulls"),
        content_addressed: ev.sym(b"__contentAddressed"),
        impure: ev.sym(b"__impure"),
        json: ev.sym(b"__json"),
    };
    ev.drv.syms = Some(s);
    s
}

fn drv_err(e: DrvError, pos: Pos) -> Box<Error> {
    Error::boxed(ErrorKind::Eval, e.0, pos)
}

fn utf8(bytes: &[u8], pos: Pos) -> EvalResult<String> {
    String::from_utf8(bytes.to_vec())
        .map_err(|_| Error::boxed(ErrorKind::Eval, "string is not valid UTF-8", pos))
}

fn parse_store_path(p: &[u8], pos: Pos) -> EvalResult<StorePath> {
    let s = std::str::from_utf8(p)
        .map_err(|_| Error::boxed(ErrorKind::Eval, "store path is not valid UTF-8", pos))?;
    StorePath::parse(s).map_err(|e| Error::boxed(ErrorKind::Eval, e.0, pos))
}

/// Mirrors primops.cc:prim_derivationStrict.
fn derivation_strict(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let attrs = ev.force_attrs(
        args[0],
        pos,
        "while evaluating the argument passed to builtins.derivationStrict",
    )?;
    crate::parallel::fanout_attrs_coerce(ev, attrs); // parallel
    let Some(name_attr) = attrs.get(ev.s.name) else {
        let mut e = Error::new(
            ErrorKind::MissingAttr,
            "attribute 'name' missing".into(),
            pos,
        );
        e.add_trace(
            pos,
            "in the attrset passed as argument to builtins.derivationStrict",
        );
        return Err(Box::new(e));
    };
    let name_w = name_attr.cell.get();
    let drv_name = ev
        .force_string_no_ctx(
            name_w,
            pos,
            "while evaluating the `name` attribute passed to builtins.derivationStrict",
        )
        .map_err(|mut e| {
            e.add_trace(
                name_attr.pos,
                "while evaluating the derivation attribute 'name'",
            );
            e
        })?;
    let drv_name = utf8(drv_name, pos)?;
    let s = syms(ev);
    derivation_strict_internal(ev, &drv_name, attrs, pos, &s).map_err(|mut e| {
        e.add_trace(
            Pos::NONE,
            format!(
                "while evaluating derivation '{drv_name}'\n  whose name attribute is located at {}",
                ev.describe_pos(name_attr.pos)
            ),
        );
        e
    })
}

struct Builder<'a> {
    s: &'a DrvSyms,
    drv: Derivation,
    context: Vec<Ctx>,
    ignore_nulls: bool,
    content_addressed: bool,
    output_hash: Option<String>,
    output_hash_algo: Option<HashAlgo>,
    ingestion_method: Option<IngestionMethod>,
    outputs: BTreeSet<String>,
    /// The structured-attrs JSON object under construction, when
    /// `__structuredAttrs = true`.
    json: Option<Vec<u8>>,
}

impl Builder<'_> {
    /// Mirrors the `handleHashMode` lambda.
    fn handle_hash_mode(&mut self, s: &str, pos: Pos) -> EvalResult<()> {
        match IngestionMethod::parse(s) {
            Some(m) => {
                self.ingestion_method = Some(m);
                Ok(())
            }
            None if s == "text" || s == "git" => bail!(
                ErrorKind::Eval,
                pos,
                "outputHashMode '{s}' is not supported (experimental feature)"
            ),
            None => bail!(
                ErrorKind::Eval,
                pos,
                "invalid value '{s}' for 'outputHashMode' attribute"
            ),
        }
    }

    /// Mirrors the `handleOutputs` lambda.
    fn handle_outputs<'b>(
        &mut self,
        names: impl Iterator<Item = &'b str>,
        pos: Pos,
    ) -> EvalResult<()> {
        self.outputs.clear();
        for j in names {
            if self.outputs.contains(j) {
                bail!(ErrorKind::Eval, pos, "duplicate derivation output '{j}'");
            }
            if j == "drvPath" {
                bail!(
                    ErrorKind::Eval,
                    pos,
                    "invalid derivation output name 'drvPath'"
                );
            }
            self.outputs.insert(j.to_string());
        }
        if self.outputs.is_empty() {
            bail!(
                ErrorKind::Eval,
                pos,
                "derivation cannot have an empty set of outputs"
            );
        }
        Ok(())
    }
}

/// One iteration of the attribute loop in derivationStrictInternal. `key`
/// is the attribute name; attributes arrive in lexicographic order, so
/// pushing onto `drv.env` keeps it sorted.
fn process_attr(
    ev: &mut Evaluator,
    b: &mut Builder<'_>,
    sym: Sym,
    key: &str,
    w: Word,
    pos: Pos,
) -> EvalResult<()> {
    let s = *b.s;
    if b.ignore_nulls && ev.force(w)? == Word::NULL {
        return Ok(());
    }
    if sym == s.content_addressed {
        if ev.force_bool(w, pos, "")? {
            b.content_addressed = true;
        }
        return Ok(());
    }
    if sym == s.impure {
        if ev.force_bool(w, pos, "")? {
            bail!(
                ErrorKind::Eval,
                pos,
                "impure derivations are not supported (experimental feature)"
            );
        }
        return Ok(());
    }
    if sym == s.args {
        let l = ev.force_list(w, pos, "")?;
        for c in l.cells() {
            let st = ev.coerce_to_string(
                c.get(),
                &mut b.context,
                pos,
                "while evaluating an element of the argument list",
                true,
                true,
                true,
            )?;
            b.drv.args.push(utf8(&st, pos)?);
        }
        return Ok(());
    }
    if let Some(mut json) = b.json.take() {
        if sym == s.structured_attrs {
            b.json = Some(json);
            return Ok(());
        }
        if json.len() > 1 {
            json.push(b',');
        }
        json_escape(&mut json, key.as_bytes())?;
        json.push(b':');
        let r = print_json(ev, w, &mut json, &mut b.context, true, pos);
        b.json = Some(json);
        r?;
        if sym == s.builder {
            let st = ev.force_string(w, pos, "")?;
            b.context.extend_from_slice(st.ctx);
            b.drv.builder = utf8(st.bytes, pos)?;
        } else if sym == s.system {
            let st = ev.force_string_no_ctx(w, pos, "")?;
            b.drv.platform = utf8(st, pos)?;
        } else if sym == s.output_hash {
            let st = ev.force_string_no_ctx(w, pos, "")?;
            b.output_hash = Some(utf8(st, pos)?);
        } else if sym == s.output_hash_algo {
            let st = ev.force_string_no_ctx(w, pos, "")?;
            b.output_hash_algo = HashAlgo::parse(&utf8(st, pos)?);
        } else if sym == s.output_hash_mode {
            let st = ev.force_string_no_ctx(w, pos, "")?;
            let st = utf8(st, pos)?;
            b.handle_hash_mode(&st, pos)?;
        } else if sym == s.outputs {
            let l = ev.force_list(w, pos, "")?;
            let mut names = Vec::with_capacity(l.len as usize);
            for c in l.cells() {
                let st = ev.force_string_no_ctx(c.get(), pos, "")?;
                names.push(utf8(st, pos)?);
            }
            b.handle_outputs(names.iter().map(String::as_str), pos)?;
        }
        return Ok(());
    }
    let st = ev.coerce_to_string(w, &mut b.context, pos, "", true, true, true)?;
    let st = utf8(&st, pos)?;
    if sym == s.json {
        b.drv.structured_attrs = Some(st);
        return Ok(());
    }
    if sym == s.builder {
        b.drv.builder = st.clone();
    } else if sym == s.system {
        b.drv.platform = st.clone();
    } else if sym == s.output_hash {
        b.output_hash = Some(st.clone());
    } else if sym == s.output_hash_algo {
        b.output_hash_algo = HashAlgo::parse(&st);
    } else if sym == s.output_hash_mode {
        b.handle_hash_mode(&st, pos)?;
    } else if sym == s.outputs {
        b.handle_outputs(
            st.split([' ', '\t', '\n', '\r']).filter(|t| !t.is_empty()),
            pos,
        )?;
    }
    b.drv.env.push((key.to_string(), st));
    Ok(())
}

/// Mirrors primops.cc:derivationStrictInternal.
fn derivation_strict_internal(
    ev: &mut Evaluator,
    drv_name: &str,
    attrs: &'static AttrsObj,
    pos: Pos,
    s: &DrvSyms,
) -> EvalResult<Word> {
    if let Err(e) = check_name(drv_name) {
        bail!(
            ErrorKind::Eval,
            pos,
            "invalid derivation name: {}. Please pass a different 'name'.",
            e.0
        );
    }
    let json_mode = match attrs.get(s.structured_attrs) {
        Some(a) => ev.force_bool(
            a.cell.get(),
            pos,
            "while evaluating the `__structuredAttrs` attribute passed to builtins.derivationStrict",
        )?,
        None => false,
    };
    let ignore_nulls = match attrs.get(s.ignore_nulls) {
        Some(a) => ev.force_bool(
            a.cell.get(),
            pos,
            "while evaluating the `__ignoreNulls` attribute passed to builtins.derivationStrict",
        )?,
        None => false,
    };

    let mut b = Builder {
        s,
        drv: Derivation {
            name: drv_name.to_string(),
            ..Default::default()
        },
        context: Vec::new(),
        ignore_nulls,
        content_addressed: false,
        output_hash: None,
        output_hash_algo: None,
        ingestion_method: None,
        outputs: ["out".to_string()].into_iter().collect(),
        json: json_mode.then(|| vec![b'{']),
    };

    for attr in lexicographic_order(ev, attrs) {
        if attr.sym == s.ignore_nulls {
            continue;
        }
        let key = utf8(ev.interner.name(attr.sym), pos)?;
        process_attr(ev, &mut b, attr.sym, &key, attr.cell.get(), pos).map_err(|mut e| {
            e.add_trace(
                attr.pos,
                format!("while evaluating attribute '{key}' of derivation '{drv_name}'"),
            );
            e
        })?;
    }

    let Builder {
        mut drv,
        mut context,
        content_addressed,
        output_hash,
        output_hash_algo,
        ingestion_method,
        outputs,
        json,
        ..
    } = b;

    if let Some(mut json) = json {
        json.push(b'}');
        drv.structured_attrs = Some(utf8(&json, pos)?);
    }

    context.sort();
    context.dedup();
    for c in &context {
        match c {
            Ctx::Opaque(p) => {
                drv.input_srcs.insert(parse_store_path(p, pos)?);
            }
            Ctx::Built { drv: d, output } => {
                drv.input_drvs
                    .entry(parse_store_path(d, pos)?)
                    .or_default()
                    .insert(utf8(output, pos)?);
            }
            Ctx::DrvDeep(d) => {
                let d = parse_store_path(d, pos)?;
                for (j, outputs) in drv_closure(ev, &d, pos)? {
                    drv.input_srcs.insert(j.clone());
                    if let Some(outputs) = outputs {
                        drv.input_drvs.insert(j, outputs);
                    }
                }
            }
        }
    }

    if drv.builder.is_empty() {
        bail!(ErrorKind::Eval, pos, "required attribute 'builder' missing");
    }
    if drv.platform.is_empty() {
        bail!(ErrorKind::Eval, pos, "required attribute 'system' missing");
    }
    if drv_name.ends_with(DRV_EXTENSION) {
        bail!(
            ErrorKind::Eval,
            pos,
            "derivation names are allowed to end in '{DRV_EXTENSION}' only if they produce a single derivation file"
        );
    }

    let mut loader = load_derivation_from_store;
    if let Some(h) = output_hash {
        if outputs.len() != 1 || !outputs.contains("out") {
            bail!(
                ErrorKind::Eval,
                pos,
                "multiple outputs are not supported in fixed-output derivations"
            );
        }
        let hash = if h.is_empty() {
            let Some(algo) = output_hash_algo else {
                bail!(
                    ErrorKind::Eval,
                    pos,
                    "empty hash requires explicit hash algorithm"
                );
            };
            let zero = Hash::from_bytes(algo, &vec![0u8; algo.size()])
                .map_err(|e| Error::boxed(ErrorKind::Eval, e.0, pos))?;
            eprintln!("warning: found empty hash, assuming '{}'", zero.to_sri());
            zero
        } else {
            Hash::parse_any(&h, output_hash_algo)
                .map_err(|e| Error::boxed(ErrorKind::Eval, e.0, pos))?
        };
        let out = DerivationOutput::Fixed {
            method: ingestion_method.unwrap_or(IngestionMethod::Flat),
            hash,
        };
        let path = out
            .fixed_path(drv_name, "out")
            .map_err(|e| drv_err(e, pos))?
            .expect("fixed output has a path");
        drv.env_set("out", path.to_string());
        drv.outputs.insert("out".into(), out);
    } else if content_addressed {
        let algo = output_hash_algo.unwrap_or(HashAlgo::Sha256);
        let method = ingestion_method.unwrap_or(IngestionMethod::Recursive);
        for o in &outputs {
            drv.env_set(o, hash_placeholder(o));
            drv.outputs
                .insert(o.clone(), DerivationOutput::CaFloating { method, algo });
        }
    } else {
        for o in &outputs {
            drv.env_set(o, String::new());
            drv.outputs.insert(o.clone(), DerivationOutput::Deferred);
        }
        finalize_input_addressed_outputs(&mut drv, &mut ev.drv.hash_cache.lock(), &mut loader)
            .map_err(|e| drv_err(e, pos))?;
    }

    let drv_path = drv.drv_path().map_err(|e| drv_err(e, pos))?;
    let h = hash_derivation_modulo(&drv, false, &mut ev.drv.hash_cache.lock(), &mut loader)
        .map_err(|e| drv_err(e, pos))?;
    ev.drv.hash_cache.lock().insert(drv_path.clone(), h);
    ev.drv.nr_derivations += 1;

    if let Some(dir) = &ev.drv.dump_dir {
        let text = drv.unparse().map_err(|e| drv_err(e, pos))?;
        let file = dir.join(drv_path.base_name());
        std::fs::write(&file, text).map_err(|e| {
            Error::boxed(
                ErrorKind::Io,
                format!("writing '{}': {e}", file.display()),
                pos,
            )
        })?;
    }

    let mut references: BTreeSet<StorePath> = drv.input_srcs.clone();
    references.extend(drv.input_drvs.keys().cloned());
    ev.drv.drvs.insert(
        drv_path.clone(),
        DrvInfo {
            outputs: drv.outputs.keys().cloned().collect(),
            references,
        },
    );

    let drv_path_s = drv_path.to_string();
    let drv_path_bytes = ev.arena.alloc_bytes(drv_path_s.as_bytes());
    let mut entries = Vec::with_capacity(1 + drv.outputs.len());
    let dp = ev
        .arena
        .mk_string_ctx(drv_path_bytes, &[Ctx::DrvDeep(drv_path_bytes)]);
    entries.push((ev.s.drv_path, Pos::NONE, dp));
    for (name, out) in &drv.outputs {
        entries.push((
            ev.sym(name.as_bytes()),
            Pos::NONE,
            output_string(ev, &drv_path, drv_path_bytes, drv_name, name, out, pos)?,
        ));
    }
    ev.mk_attrs(&mut entries, pos)
}

/// Mirrors eval.cc:EvalState::mkOutputString: the output path, or a
/// downstream placeholder when it is not statically known, with `!out!drv`
/// context.
fn output_string(
    ev: &mut Evaluator,
    drv_path: &StorePath,
    drv_path_bytes: &'static [u8],
    drv_name: &str,
    output: &str,
    out: &DerivationOutput,
    pos: Pos,
) -> EvalResult<Word> {
    let s = match out.path(drv_name, output).map_err(|e| drv_err(e, pos))? {
        Some(p) => p.to_string(),
        None => downstream_placeholder(drv_path, drv_name, output),
    };
    let output = ev.arena.alloc_bytes(output.as_bytes());
    let bytes = ev.arena.alloc_bytes(s.as_bytes());
    Ok(ev.arena.mk_string_ctx(
        bytes,
        &[Ctx::Built {
            drv: drv_path_bytes,
            output,
        }],
    ))
}

/// Mirrors downstream-placeholder.cc:DownstreamPlaceholder::unknownCaOutput.
fn downstream_placeholder(drv_path: &StorePath, drv_name: &str, output: &str) -> String {
    let clear = format!(
        "nix-upstream-output:{}:{}",
        drv_path.hash_part(),
        output_path_name(drv_name, output)
    );
    format!(
        "/{}",
        hash_bytes(HashAlgo::Sha256, clear.as_bytes()).to_nix32()
    )
}

/// Output names and `.drv` references of `p`: from this evaluation's table,
/// else parsed from `/nix/store`.
fn drv_info(ev: &mut Evaluator, p: &StorePath, pos: Pos) -> EvalResult<DrvInfo> {
    if let Some(i) = ev.drv.drvs.get(p) {
        return Ok(i);
    }
    let drv = load_derivation_from_store(p).map_err(|e| drv_err(e, pos))?;
    let mut references: BTreeSet<StorePath> = drv.input_srcs.clone();
    references.extend(drv.input_drvs.keys().cloned());
    let info = DrvInfo {
        outputs: drv.outputs.keys().cloned().collect(),
        references,
    };
    ev.drv.drvs.insert(p.clone(), info.clone());
    Ok(info)
}

/// Mirrors `computeFSClosure` on a `.drv` path as used for `DrvDeep` context:
/// the drv, its inputs, and their closures. Only `.drv` files have known
/// references here; other store paths are leaves.
fn drv_closure(
    ev: &mut Evaluator,
    start: &StorePath,
    pos: Pos,
) -> EvalResult<Vec<(StorePath, Option<BTreeSet<String>>)>> {
    let mut seen: BTreeSet<StorePath> = BTreeSet::new();
    let mut todo = vec![start.clone()];
    let mut out = Vec::new();
    while let Some(p) = todo.pop() {
        if !seen.insert(p.clone()) {
            continue;
        }
        if p.is_derivation() {
            let info = drv_info(ev, &p, pos)?;
            todo.extend(info.references);
            out.push((p, Some(info.outputs)));
        } else {
            out.push((p, None));
        }
    }
    Ok(out)
}

/// Mirrors primops.cc:prim_placeholder.
fn placeholder(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let s = ev.force_string_no_ctx(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.placeholder",
    )?;
    let s = utf8(s, pos)?;
    Ok(ev.mk_string(hash_placeholder(&s).as_bytes()))
}

/// Mirrors primops.cc:derivationToValue for `import /nix/store/...drv`:
/// returns `None` when `path` is not an existing `.drv` store path.
pub fn import_derivation(ev: &mut Evaluator, path: &[u8], pos: Pos) -> EvalResult<Option<Word>> {
    if !path.ends_with(DRV_EXTENSION.as_bytes()) {
        return Ok(None);
    }
    let Ok(sp) = std::str::from_utf8(path).map(StorePath::parse) else {
        return Ok(None);
    };
    let Ok(sp) = sp else {
        return Ok(None);
    };
    if !std::path::Path::new(&sp.to_string()).is_file() {
        return Ok(None);
    }
    let drv = load_derivation_from_store(&sp).map_err(|e| drv_err(e, pos))?;
    let drv_name = Derivation::name_from_path(&sp)
        .map_err(|e| drv_err(e, pos))?
        .to_string();
    let path_bytes = ev.arena.alloc_bytes(path);
    let mut entries = Vec::with_capacity(3 + drv.outputs.len());
    let dp = ev
        .arena
        .mk_string_ctx(path_bytes, &[Ctx::DrvDeep(path_bytes)]);
    entries.push((ev.s.drv_path, Pos::NONE, dp));
    let name = ev.mk_string(drv.env_get("name").unwrap_or("").as_bytes());
    entries.push((ev.s.name, Pos::NONE, name));
    let mut names = Vec::with_capacity(drv.outputs.len());
    for (name, out) in &drv.outputs {
        let w = output_string(ev, &sp, path_bytes, &drv_name, name, out, pos)?;
        entries.push((ev.sym(name.as_bytes()), Pos::NONE, w));
        names.push(ev.mk_string(name.as_bytes()));
    }
    let outputs_sym = syms(ev).outputs;
    let list = ev.mk_list(&names);
    entries.push((outputs_sym, Pos::NONE, list));
    let attrs = ev.mk_attrs(&mut entries, pos)?;
    let f = match ev.drv.imported_drv_fn {
        Some(f) => f,
        None => {
            let ir = ev.compile_source(
                b"\xc2\xabimported-drv-to-derivation\xc2\xbb",
                IMPORTED_DRV_TO_DERIVATION.to_vec(),
                b"/",
                None,
            )?;
            let root = ev.root_env;
            let f = ev.eval(root, ir)?;
            ev.drv.imported_drv_fn = Some(f);
            f
        }
    };
    Ok(Some(ev.call1(f, attrs, pos)?))
}

/// Nix 2.34.8 src/libexpr/imported-drv-to-derivation.nix.
const IMPORTED_DRV_TO_DERIVATION: &[u8] = b"attrs@{ drvPath, outputs, name, ... }:
let
  commonAttrs = (builtins.listToAttrs outputsList) // {
    all = map (x: x.value) outputsList;
    inherit drvPath name;
    type = \"derivation\";
  };
  outputToAttrListElement = outputName: {
    name = outputName;
    value = commonAttrs // {
      outPath = builtins.getAttr outputName attrs;
      inherit outputName;
    };
  };
  outputsList = map outputToAttrListElement outputs;
in
(builtins.head outputsList).value
";
