//! `ietsp print-dev-env`: the shell environment of a flake's devShell.

use std::collections::HashMap;
use std::sync::Arc;

use crate::cli::{EvalArgs, Input, abs_path, cwd, evaluate, parse_eval_args, report, usage};
use crate::error::Error as CrateError;
use crate::eval::{Error, Evaluator, Pos};
use crate::store::locate::{drv_roots, locate_in_store};
use crate::trace::run::Cache;

struct DevEnvArgs {
    transport: crate::remote::worker::Transport,
    /// Absolute; a symlink to the env output, registered as an indirect GC root.
    gc_root: Option<std::path::PathBuf>,
}

/// Build events on the daemon connection: log lines are printed live with `--timings`,
/// else kept and printed only when the build fails.
struct EventSink {
    live: bool,
    log: Vec<String>,
    failed: Option<String>,
    prov: crate::provenance::Provenance,
}

impl EventSink {
    fn event(&mut self, e: crate::remote::event::Event) {
        use crate::remote::event::Event;
        match e {
            Event::Log(l) => {
                let line = self.prov.rewrite_str(&String::from_utf8_lossy(&l.line));
                if self.live {
                    eprintln!("ietsp: {line}");
                } else {
                    self.log.push(line);
                }
            }
            Event::Started(s) if self.live => match self.prov.started(&s.drv_path) {
                Some(l) => eprintln!("ietsp: {l}"),
                None => eprintln!("ietsp: building {}", s.drv_path),
            },
            Event::Failed(f) => {
                self.failed = Some(match self.prov.failed(&f.drv_path, &f.reason) {
                    Some(l) => l.trim_start_matches("error: ").to_string(),
                    None => format!(
                        "build of {} failed: {}",
                        f.drv_path,
                        self.prov.rewrite_str(&f.reason)
                    ),
                });
            }
            _ => {}
        }
    }

    fn dump(&self) {
        for l in &self.log {
            eprintln!("ietsp: {l}");
        }
    }
}

/// `ietsp print-dev-env`: the bash `nix print-dev-env` prints for a flake's shell, from the
/// in-memory shell derivation through a Nix daemon. See docs/direnv.md.
pub fn cmd_print_dev_env(argv: &[String]) -> i32 {
    let mut url: Option<String> = None;
    let mut gc_root: Option<String> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut flake: Option<String> = None;
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--builder" => {
                i += 1;
                url = Some(argv.get(i).cloned().unwrap_or_else(|| usage()));
            }
            "--gc-root" => {
                i += 1;
                gc_root = Some(argv.get(i).cloned().unwrap_or_else(|| usage()));
            }
            s if s.starts_with('-') => rest.push(s.to_string()),
            s => {
                if flake.replace(s.to_string()).is_some() {
                    usage()
                }
            }
        }
        i += 1;
    }
    let mut a = parse_eval_args(&rest);
    a.input = Some(Input::Flake(flake.unwrap_or_else(|| ".".into())));
    let cfg = crate::config::Config::load(&[]).unwrap_or_else(|_| crate::config::Config::empty());
    // the environment runs here, so its inputs belong in the local store:
    // the `builder` setting does not apply
    let url = url.unwrap_or_else(|| cfg.store());
    let Some(transport) = crate::remote::worker::Transport::parse(&url) else {
        eprintln!(
            "error: print-dev-env needs a Nix daemon: --builder daemon, unix:///path or ssh-ng://host"
        );
        return 1;
    };
    let cwd = cwd();
    let gc_root = gc_root.map(|r| {
        std::path::PathBuf::from(
            <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(&abs_path(&r, &cwd)),
        )
    });
    if matches!(transport, crate::remote::worker::Transport::Grpc(_)) {
        eprintln!("error: print-dev-env needs a Nix daemon; grpc:// only builds");
        return 1;
    }
    if gc_root.is_some() && !matches!(transport, crate::remote::worker::Transport::Unix(_)) {
        eprintln!("error: --gc-root needs a local daemon (daemon or unix://)");
        return 1;
    }
    let d = DevEnvArgs { transport, gc_root };

    let t0 = std::time::Instant::now();
    let mut ev = a.evaluator();
    if let Err(e) = super::configure_lints(&mut ev, &a) {
        super::report(&ev, &e);
        return 1;
    }
    let cache = super::setup_cache(&a, "print-dev-env", &ev);
    if let Some(c) = &cache
        && c.lookup()
        && let Some(bytes) = c.store.read(&c.key, crate::trace::store::RESULT)
        && let Some(hit) = crate::devenv::CachedResult::decode(&bytes)
    {
        match dev_env_hit(&d, &hit) {
            Ok(true) => {
                use std::io::Write;
                c.replay_warnings();
                let _ = std::io::stdout().write_all(&hit.bytes);
                let _ = std::io::stdout().flush();
                if a.timings {
                    eprintln!("total: {:.3}s", t0.elapsed().as_secs_f64());
                }
                return 0;
            }
            Ok(false) => {
                if a.timings {
                    eprintln!("cache: env output '{}' is gone, rebuilding", hit.out_path);
                }
            }
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    }
    if cache.is_some() {
        ev.trace = crate::trace::Trace::enabled();
    }
    // parallel: eval and the daemon flow run as the main fiber
    crate::parallel::run(a.workers, ev, move |ev| dev_env_full(ev, a, &d, cache, t0))
}

/// A cache hit is usable when the env output (and the files the script names) still exist;
/// the GC root is refreshed when it does not point at the output.
fn dev_env_hit(d: &DevEnvArgs, hit: &crate::devenv::CachedResult) -> crate::error::Result<bool> {
    use crate::remote::worker::{Connection, Transport};
    let present = match &d.transport {
        Transport::Unix(_) => std::path::Path::new(&hit.out_path).exists(),
        Transport::Ssh { .. } | Transport::Grpc(_) => {
            let mut conn = Connection::open(&d.transport, &mut |_| {})?;
            !conn
                .query_valid_paths(std::slice::from_ref(&hit.out_path), &mut |_| {})?
                .is_empty()
        }
    };
    if !present || !hit.files.iter().all(|f| f.exists()) {
        return Ok(false);
    }
    if let Some(root) = &d.gc_root {
        ensure_gc_root(&d.transport, root, &hit.out_path, None)?;
    }
    Ok(true)
}

/// indirect-root-store.cc:addPermRoot: a symlink at `root` to `out_path`, then
/// `AddIndirectRoot`. Nothing happens when the link already points there.
fn ensure_gc_root(
    transport: &crate::remote::worker::Transport,
    root: &std::path::Path,
    out_path: &str,
    conn: Option<&mut crate::remote::worker::Connection>,
) -> crate::error::Result<()> {
    use crate::remote::worker::Connection;
    use crate::store::path::STORE_DIR;
    if std::fs::read_link(root).ok().as_deref() == Some(std::path::Path::new(out_path)) {
        return Ok(());
    }
    if root.starts_with(STORE_DIR) {
        return Err(CrateError::Usage(format!(
            "creating a garbage collector root ({}) in the Nix store is forbidden",
            root.display()
        )));
    }
    if let Ok(m) = std::fs::symlink_metadata(root)
        && (!m.file_type().is_symlink()
            || !std::fs::read_link(root)
                .map(|t| t.starts_with(STORE_DIR))
                .unwrap_or(false))
    {
        return Err(CrateError::Usage(format!(
            "cannot create symlink {}; already exists",
            root.display()
        )));
    }
    crate::remote::worker::replace_symlink(out_path, root)
        .map_err(|e| CrateError::io(format!("cannot create {}", root.display()), e))?;
    let root_s = root
        .to_str()
        .ok_or_else(|| CrateError::Usage("gc root path is not UTF-8".into()))?;
    let mut opened;
    let conn = match conn {
        Some(c) => c,
        None => {
            opened = Connection::open(transport, &mut |_| {})?;
            &mut opened
        }
    };
    conn.add_indirect_root(root_s, &mut |_| {})
}

/// What the `-env` build produced: the environment `get-env.sh` dumped, the output that
/// holds it (`nix develop` makes it `NIX_GCROOT`), and the shell derivation's builder.
pub struct DevEnv {
    pub env: crate::devenv::BuildEnvironment,
    pub out_path: String,
    /// `<store path>/bin/bash`: the bash the shell was built with.
    pub builder: String,
    /// When the daemon phase started, for the caller's `--timings` line.
    t1: std::time::Instant,
}

/// `print-dev-env` without the printing: the build environment of the flake installable
/// `a.input` names, on `ev`. `nix develop` (src/cli/classic/develop.rs) uses this.
pub fn dev_env(
    ev: Evaluator,
    a: EvalArgs,
    transport: crate::remote::worker::Transport,
) -> Result<DevEnv, i32> {
    let d = DevEnvArgs {
        transport,
        gc_root: None,
    };
    crate::parallel::run(a.workers, ev, move |ev| dev_env_build(ev, a, &d))
}

/// The cache-miss path: evaluate the shell, derive and build its `-env` derivation through
/// the daemon, read the environment, print it, and remember the bytes.
fn dev_env_full(
    ev: &mut Evaluator,
    a: EvalArgs,
    d: &DevEnvArgs,
    cache: Option<Cache>,
    t0: std::time::Instant,
) -> i32 {
    let (timings, json) = (a.timings, a.json);
    let cwd = cwd();
    let built = match dev_env_build(ev, a, d) {
        Ok(b) => b,
        Err(code) => return code,
    };
    let r = (|| -> crate::error::Result<Vec<u8>> {
        let (mut output, files) = if json {
            (
                serde_json::to_vec(&built.env.to_json())
                    .map_err(|e| CrateError::Protocol(e.to_string()))?,
                Vec::new(),
            )
        } else {
            let attrs_dir = match &d.gc_root {
                Some(r) => std::path::PathBuf::from(format!("{}-attrs", r.display())),
                None => std::env::temp_dir().join(format!("ietsp-dev-env-{}", std::process::id())),
            };
            let outputs_dir = std::path::Path::new(
                <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(&cwd),
            )
            .join("outputs");
            let rc = crate::devenv::make_rc_script(&built.env, &attrs_dir, &outputs_dir)?;
            (rc.script.into_bytes(), rc.files)
        };
        output.push(b'\n');
        if let Some(c) = &cache {
            c.write(
                ev,
                &crate::devenv::CachedResult {
                    out_path: built.out_path.clone(),
                    files,
                    bytes: output.clone(),
                }
                .encode(),
                false,
            );
        }
        if timings {
            eprintln!("daemon: {:.3}s", built.t1.elapsed().as_secs_f64());
        }
        Ok(output)
    })();
    match r {
        Ok(out) => {
            use std::io::Write;
            let _ = std::io::stdout().write_all(&out);
            let _ = std::io::stdout().flush();
            if timings {
                eprintln!("total: {:.3}s", t0.elapsed().as_secs_f64());
            }
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

/// Evaluates the shell, derives and builds its `-env` derivation through the daemon and
/// reads the environment back. The error is already reported; `Err` is the exit code.
fn dev_env_build(ev: &mut Evaluator, a: EvalArgs, d: &DevEnvArgs) -> Result<DevEnv, i32> {
    use crate::build::backend::{Backend, Batch, Job, Worker};
    use crate::build::upload::{PlanUpload, Uploader};
    use crate::builtins::derivation::{DrvInfo, load_drv_traced};
    use crate::devenv;
    use crate::eval::ErrorKind;
    use crate::remote::worker::{Options, failed_event};
    use crate::store::drv::Derivation;
    use crate::store::path::StorePath;
    use crate::store::source::SourceOrigin;
    use std::collections::BTreeSet;

    let t0 = std::time::Instant::now();
    let timings = a.timings;
    let f = match &a.input {
        Some(Input::Flake(f)) => f.clone(),
        _ => usage(),
    };
    let (loc, frag) = f.split_once('#').unwrap_or((f.as_str(), ""));
    let candidates = devenv::attr_paths(frag, ev.current_system);
    let mut roots = Vec::new();
    let r = (|| {
        let outputs = evaluate(
            ev,
            EvalArgs {
                input: Some(Input::Flake(loc.to_string())),
                ..a
            },
        )?;
        for c in &candidates {
            let path = crate::flake::parse_attr_path(c)
                .map_err(|m| Error::boxed(ErrorKind::Eval, m.to_string(), Pos::NONE))?;
            if let Some(v) = crate::flake::select(ev, outputs, &path)? {
                return drv_roots(ev, v, &mut roots);
            }
        }
        Err(Error::boxed(
            ErrorKind::MissingAttr,
            format!(
                "flake '{loc}' does not provide attribute {}",
                devenv::show_attr_paths(&candidates)
            ),
            Pos::NONE,
        ))
    })();
    if let Err(e) = r {
        report(ev, &e);
        return Err(1);
    }
    if roots.len() != 1 {
        eprintln!(
            "error: '{f}' needs to evaluate to a single derivation, but it evaluated to {} derivations",
            roots.len()
        );
        return Err(1);
    }
    let shell_path = roots[0].0.clone();
    let eval_time = t0.elapsed();

    let r = (|| -> crate::error::Result<DevEnv> {
        let shell = match ev.drv.drvs.get(&shell_path).and_then(|i| i.aterm) {
            Some(aterm) => Derivation::parse(
                std::str::from_utf8(&aterm).map_err(|e| CrateError::Store(e.to_string()))?,
                Derivation::name_from_path(&shell_path)?,
            ),
            None => load_drv_traced(&ev.trace, &shell_path),
        }?;
        let trace = ev.trace.clone();
        let mut loader = move |p: &StorePath| load_drv_traced(&trace, p);
        let env_drv =
            devenv::env_derivation(&shell, &mut ev.drv.hash_cache.lock().unwrap(), &mut loader)?;
        let text = env_drv.unparse()?;
        let env_path = env_drv.drv_path_for(&text)?;
        let mut references = env_drv.input_srcs.clone();
        references.extend(env_drv.input_drvs.keys().cloned());
        ev.drv.drvs.insert(
            env_path.clone(),
            DrvInfo {
                outputs: env_drv.outputs.keys().cloned().collect(),
                references,
                aterm: Some(Arc::new(text.into_bytes())),
                pos: None,
                attr: None,
            },
        );
        ev.sources.insert(
            devenv::get_env_sh_path(),
            SourceOrigin::text(devenv::GET_ENV_SH.as_bytes().to_vec(), BTreeSet::new()),
        );
        let mut out_paths = Vec::new();
        for (name, out) in &env_drv.outputs {
            match out.path(&env_drv.name, name)? {
                Some(p) => out_paths.push(p.to_string()),
                None => {
                    return Err(CrateError::Store(
                        "content-addressed shells are not supported".into(),
                    ));
                }
            }
        }
        if timings {
            eprintln!(
                "eval: {} derivations, {:.3}s\nenv drv: {env_path}",
                ev.drv.nr_derivations,
                eval_time.as_secs_f64()
            );
        }

        let t1 = std::time::Instant::now();
        let mut sink = EventSink {
            live: timings,
            log: Vec::new(),
            failed: None,
            prov: crate::provenance::Provenance::snapshot(ev),
        };
        let mut w = Worker::open(&d.transport, &Options::default(), &mut |e| sink.event(e))?;
        let valid = w
            .conn
            .query_valid_paths(&out_paths, &mut |e| sink.event(e))?;
        if valid.len() != out_paths.len() {
            let t2 = std::time::Instant::now();
            let no_overrides = HashMap::new();
            let locate = |paths: &[StorePath]| locate_in_store(paths, &no_overrides);
            let plan = {
                let drvs = ev.drv.drvs.lock();
                let sources = ev.source_origins();
                let inputs = crate::remote::plan::Inputs {
                    drvs: &drvs,
                    sources: &sources,
                };
                crate::remote::plan::plan(std::slice::from_ref(&env_path), inputs)?
            };
            let plan_time = t2.elapsed();
            PlanUpload::new(&plan, &locate).upload(
                &mut w,
                std::slice::from_ref(&env_path),
                &mut |e| sink.event(e),
            )?;
            let job = Job {
                drv: env_path.clone(),
                outputs: Vec::new(),
            };
            let batch = Batch {
                jobs: std::slice::from_ref(&job),
                drvs: &|_| Ok(Vec::new()),
            };
            for s in w.submit(&batch, &mut |e| sink.event(e))?.values() {
                if !s.ok {
                    sink.event(failed_event(&env_path.to_string(), &s.reason));
                }
            }
            if let Some(why) = &sink.failed {
                sink.dump();
                return Err(CrateError::Protocol(why.clone()));
            }
            if timings {
                eprintln!("{}", w.report(&plan, plan_time));
            }
        } else if timings {
            eprintln!(
                "daemon: env output already valid, {:.3}s",
                t1.elapsed().as_secs_f64()
            );
        }

        let (out_path, bytes) = read_env_output(&d.transport, &mut w.conn, &out_paths, &mut sink)?;
        let env = devenv::BuildEnvironment::parse(&bytes)?;
        if let Some(root) = &d.gc_root {
            ensure_gc_root(&d.transport, root, &out_path, Some(&mut w.conn))?;
        }
        drop(w);
        Ok(DevEnv {
            env,
            out_path,
            builder: shell.builder.clone(),
            t1,
        })
    })();
    r.map_err(|e| {
        eprintln!("error: {e}");
        1
    })
}

/// develop.cc:getDerivationEnvironment, after the build: the first output (by name) whose
/// file is not empty holds the JSON. A local daemon's output is read from disk; a remote one
/// arrives as a NAR.
fn read_env_output(
    transport: &crate::remote::worker::Transport,
    conn: &mut crate::remote::worker::Connection,
    out_paths: &[String],
    sink: &mut EventSink,
) -> crate::error::Result<(String, Vec<u8>)> {
    use crate::remote::worker::Transport;
    use crate::store::path::StorePath;
    for p in out_paths {
        let bytes = match transport {
            Transport::Unix(_) => {
                std::fs::read(p).map_err(|e| CrateError::io(format!("cannot read '{p}'"), e))?
            }
            Transport::Ssh { .. } | Transport::Grpc(_) => {
                let dir =
                    std::env::temp_dir().join(format!("ietsp-dev-env-{}", std::process::id()));
                std::fs::create_dir_all(&dir)?;
                let dest = dir.join(StorePath::parse(p)?.base_name());
                let _ = std::fs::remove_file(&dest);
                conn.nar_from_path(p, &mut |e| sink.event(e), &mut |r| {
                    crate::store::nar::restore_path(r, &dest)
                })?;
                let bytes = std::fs::read(&dest)?;
                let _ = std::fs::remove_dir_all(&dir);
                bytes
            }
        };
        if !bytes.is_empty() {
            return Ok((p.clone(), bytes));
        }
    }
    Err(CrateError::Protocol(
        "get-env.sh failed to produce an environment".into(),
    ))
}
