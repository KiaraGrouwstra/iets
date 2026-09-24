//! One build dispatch. Every command that realises a derivation goes through
//! [`Dispatch`]: the builder URL, the upload plan, the connection, the upload
//! and the build are one object, and a root's output path is one lookup
//! (docs/plumbing.md).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use crate::build::render::Style;
use crate::build::upload::PlanUpload;
use crate::build::{Dispatcher, Outcome, Target};
use crate::builtins::derivation::DrvInfo;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::eval::Evaluator;
use crate::provenance::Provenance;
use crate::remote::plan::{Inputs, Plan};
use crate::remote::worker::{Missing, Options, Transport};
use crate::store::locate::locate_in_store;
use crate::store::path::StorePath;
use crate::store::source::SourceOrigin;

/// One derivation and one of its outputs a command asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    pub drv: StorePath,
    pub output: String,
}

impl Root {
    pub fn paths(roots: &[Root]) -> Vec<StorePath> {
        roots.iter().map(|r| r.drv.clone()).collect()
    }
}

/// The Nix settings the native commands read: they take no `--option`, and an
/// unreadable nix.conf leaves the defaults.
pub fn config() -> Config {
    Config::load(&[]).unwrap_or_else(|_| Config::empty())
}

/// Where the builds go: the flag, else the `builder` setting, else nix.conf's
/// `store` (docs/config.md).
pub fn transport(url: Option<&str>, cfg: &Config) -> Result<Transport> {
    let url = crate::settings::builder(url, cfg);
    Transport::parse_checked(&url, &cfg.system())
        .map_err(|e| Error::Usage(e.to_string()))?
        .ok_or_else(|| {
            Error::Usage(format!(
                "unsupported builder '{url}': expected daemon, unix:///path, ssh-ng://[user@]host or grpc://host:port"
            ))
        })
}

/// The upload plan for `roots`, from the derivations and sources this
/// evaluation made.
pub fn plan(ev: &Evaluator, roots: &[StorePath]) -> Result<Plan> {
    let drvs = ev.drv.drvs.lock();
    let sources = ev.known.origins();
    plan_with(roots, &drvs, &sources)
}

/// [`plan`] over a derivation table the caller holds: a cache hit, a rewritten
/// fixed-output derivation, or nothing at all, in which case every input drv
/// is read from `/nix/store`.
pub fn plan_with(
    roots: &[StorePath],
    drvs: &HashMap<StorePath, DrvInfo>,
    sources: &HashMap<StorePath, SourceOrigin>,
) -> Result<Plan> {
    crate::remote::plan::plan(roots, Inputs { drvs, sources })
}

/// The plan's locator with no `--source` overrides: for a command that drives
/// the upload on a connection of its own because it goes on to speak the
/// worker protocol itself (`nix-shell`, `print-dev-env`).
pub fn locate_store(paths: &[StorePath]) -> Result<Vec<Option<SourceOrigin>>> {
    locate_in_store(paths, &HashMap::new())
}

/// A planned closure, the connection that carries it, and the builds.
pub struct Dispatch {
    plan: Plan,
    /// `--source STOREPATH=LOCAL`: read those paths from a local tree.
    overrides: HashMap<String, PathBuf>,
    pub d: Dispatcher,
}

impl Dispatch {
    pub fn open(
        transport: &Transport,
        options: &Options,
        plan: Plan,
        prov: Provenance,
        style: Style,
    ) -> Result<Dispatch> {
        Ok(Dispatch::with(
            Dispatcher::open(transport, options, prov, style)?,
            plan,
        ))
    }

    /// A dispatcher the caller made itself, for a command that owns its
    /// reporter.
    pub fn with(d: Dispatcher, plan: Plan) -> Dispatch {
        Dispatch {
            plan,
            overrides: HashMap::new(),
            d,
        }
    }

    pub fn overrides(mut self, sources: HashMap<String, PathBuf>) -> Dispatch {
        self.overrides = sources;
        self
    }

    pub fn plan(&self) -> &Plan {
        &self.plan
    }

    /// Uploads what `batch` needs and builds it in one op.
    pub fn submit(&mut self, batch: Vec<Target>) -> Result<Vec<Outcome>> {
        let locate = |paths: &[StorePath]| locate_in_store(paths, &self.overrides);
        let mut up = PlanUpload::new(&self.plan, &locate);
        self.d.submit(&mut up, batch)
    }

    /// `--no-build`: register the closure and stop.
    pub fn upload_only(&mut self, roots: &[StorePath]) -> Result<()> {
        let locate = |paths: &[StorePath]| locate_in_store(paths, &self.overrides);
        let mut up = PlanUpload::new(&self.plan, &locate);
        self.d.upload_only(&mut up, roots)
    }

    /// `--dry-run`: what building `targets` would build and fetch.
    pub fn missing(&mut self, targets: &[Target]) -> Result<Missing> {
        let locate = |paths: &[StorePath]| locate_in_store(paths, &self.overrides);
        let mut up = PlanUpload::new(&self.plan, &locate);
        self.d.missing(&mut up, targets)
    }

    /// Builds every root in one op. Roots that share a derivation become one
    /// target carrying both outputs; `name` says what a derivation is called.
    pub fn realise(
        &mut self,
        roots: &[Root],
        name: &dyn Fn(&StorePath) -> String,
    ) -> Result<Built> {
        let mut batch: Vec<Target> = Vec::new();
        for r in roots {
            match batch.iter_mut().find(|t| t.drv == r.drv) {
                Some(t) if t.outputs.contains(&r.output) => {}
                Some(t) => t.outputs.push(r.output.clone()),
                None => batch.push(Target::new(
                    &name(&r.drv),
                    r.drv.clone(),
                    vec![r.output.clone()],
                )),
            }
        }
        Ok(Built(
            self.submit(batch)?
                .into_iter()
                .map(|o| (o.drv.to_string(), o.outputs))
                .collect(),
        ))
    }
}

/// What a build put in the store, by derivation and output name.
pub struct Built(HashMap<String, BTreeMap<String, String>>);

impl Built {
    /// One output path per root, in the order the roots were given.
    pub fn paths(&self, roots: &[Root]) -> Result<Vec<String>> {
        roots
            .iter()
            .map(|r| {
                let drv = r.drv.to_string();
                (self.0.get(&drv).and_then(|m| m.get(&r.output)).cloned()).ok_or_else(|| {
                    Error::Store(format!("derivation '{drv}' has no output '{}'", r.output))
                })
            })
            .collect()
    }
}
