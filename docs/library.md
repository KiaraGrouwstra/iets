# Using ietsp as a library

The crate is a library with a thin `ietsp` binary on top. A frontend —
another CLI, an editor server, a build service — links the library and
calls the entry points below. `examples/eval.rs`, `examples/build.rs` and
`examples/drv_info.rs` are three working frontends; they use `pub` items
only and never touch `ietsp::cli`.

**Rule: frontends never reach into evaluator internals.** No `cli`
module, no private field, no copied helper. When something a frontend
needs lives only in `cli`, move it into the library (`crate::api`, or the
module it belongs to) and let `cli` call it there.

## Entry points

| What | Where |
|:---|:---|
| Create an evaluator | `eval::Evaluator::new()` |
| Environment settings, read once | `settings::get() -> &'static Settings` (docs/settings.md) |
| Arguments of one evaluation | `api::EvalArgs` (`EvalArgs::new()` for the defaults) |
| What to evaluate | `api::Input::{Expr, File, Flake}`, `api::ArgKind` for `--arg` |
| `-I` entries, `NIX_PATH` | `api::parse_include`, `api::full_includes` |
| Evaluate it | `api::evaluate(&mut Evaluator, EvalArgs) -> Result<Word, Box<Error>>` |
| Run on worker threads | `parallel::try_run(workers, ev, f)`; `parallel::default_workers()` |
| Force a value | `Evaluator::force`, `force_deep`, `force_attrs` |
| Print a value | `print::print_json`, `print::print_ambiguous`; `Evaluator::coerce_to_string` for `--raw` |
| Report an error | `api::report(&Evaluator, &Error)`; positions rewritten to local trees |
| Error types | `eval::{Error, ErrorKind, EvalResult}`; `error::Error` for store and io |
| Derivations this run created | `Evaluator::drv.drvs` (`StorePath -> builtins::derivation::DrvInfo`) |
| A drv's attribute path and position | `DrvInfo::attr`, `DrvInfo::pos`; `DrvState::set_attr` records the walk |
| Position to file:line:col | `provenance::describe_pos`, else `Evaluator::describe_pos` |
| Store paths back to local trees | `provenance::Provenance::snapshot(ev, cfg)` |
| Sources this run copied in | `Evaluator::source_origins()` |
| Derivation roots of a value | `store::locate::drv_roots` |
| Outputs to build | `api::build_targets(wanted, roots)` |
| Upload plan | `remote::plan::plan(roots, Inputs { drvs, sources }, &locate)`; `store::locate::locate_in_store` |
| Worker connection | `remote::worker::Transport::parse("daemon" \| "unix:///…" \| "ssh-ng://…")` |
| Build | `build::Dispatcher::open(&transport, &options, prov, style)`, then `submit(&mut build::upload::PlanUpload::new(&plan, &locate), targets)` |
| Build targets and results | `build::Target::new(attr, drv, outputs)`; `build::Outcome` per target |
| Daemon events | `remote::event::Event::{Started, Log, Phase, Stopped, Failed}` |
| Nix configuration | `config::Config::load(&[])` |
| Evaluation cache | `api::setup_cache(&args, subcommand, &ev)`, `trace::run::Cache` |
| Flake references | `api::flake_ref`, `flake::{eval_flake, eval_flake_value}` |
| `eval-jobs` style walks | `jobs::run`, `jobs::Options` |

## Shape of a frontend

1. Build an `EvalArgs`.
2. `parallel::try_run(workers, Evaluator::new(), move |ev| …)`: everything
   inside runs as the main fiber, so `force` may park while helpers work.
3. `api::evaluate(ev, args)` for the value, then force and print it, or
   `drv_roots` + `plan` + `build` for a build.
4. Report errors with `api::report`, which rewrites store paths and
   positions to the local trees they came from.
