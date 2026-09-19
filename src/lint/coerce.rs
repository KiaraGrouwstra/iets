//! Silent coercions: path copies, reads through a copy, derivation
//! attribute coercions, `==` across types or on functions, `or` on a
//! non-attrset. Hook: the evaluator, at the coercion site.

use super::{LintId, Where};
use crate::eval::{EvalResult, Evaluator, Pos};
use crate::runtime::{Value, ValueType, Word};

fn lossy(b: &[u8]) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(b)
}

/// After a path was copied to the store by a string coercion. Inside a
/// flake root or a locked input `"${./file}"` is the normal reference,
/// so only a directory is reported there.
pub fn path_copy(ev: &mut Evaluator, path: &[u8], store_path: &[u8], pos: Pos) -> EvalResult<()> {
    if !ev.lint_on(LintId::PathCopy) {
        return Ok(());
    }
    if ev.flake_tree_of(path).is_some() {
        if !is_directory(ev, path) {
            return Ok(());
        }
        return ev.lint(LintId::PathCopy, pos, || {
            format!(
                "directory '{}' is copied to the store whole as '{}' by this string coercion; use `builtins.path {{ path = ...; filter = ...; }}` to copy the files it needs",
                lossy(path),
                lossy(store_path)
            )
        });
    }
    ev.lint(LintId::PathCopy, pos, || {
        format!(
            "path '{}' is copied to the store as '{}' by this string coercion; use `toString` for the name or `builtins.path` for an explicit copy",
            lossy(path),
            lossy(store_path)
        )
    })
}

/// Whether the (possibly virtual) store path `path` is a directory.
fn is_directory(ev: &Evaluator, path: &[u8]) -> bool {
    use crate::builtins::store_hooks::{Whence, whence};
    match whence(ev, path) {
        Some(Whence::Fs(p)) => ev.trace.stat_now(&p, true) == crate::trace::Kind::Directory,
        Some(Whence::In(entry, _, rel)) => matches!(
            entry.tree.resolve(&rel, true),
            Some(crate::flake::spool::Node::Dir(_))
        ),
        None => false,
    }
}

/// Before `path` is copied: a path that already lies under the store dir,
/// unless it is the flake root's or a locked input's virtual copy.
pub fn store_path_recopy(ev: &mut Evaluator, path: &[u8], pos: Pos) -> EvalResult<()> {
    if !ev.lint_on(LintId::StorePathRecopy) {
        return Ok(());
    }
    let mut prefix = ev.store_dir.clone();
    prefix.push(b'/');
    if !path.starts_with(&prefix) || ev.flake_tree_of(path).is_some() {
        return Ok(());
    }
    ev.lint(LintId::StorePathRecopy, pos, || {
        format!(
            "'{}' is already a store path and is copied again, which doubles the hash in its name; use `builtins.storePath` or keep it a string",
            lossy(path)
        )
    })
}

/// The store path `path` lies under, when this evaluation copied it.
fn copied_root(ev: &Evaluator, path: &[u8]) -> Option<Vec<u8>> {
    let mut prefix = ev.store_dir.clone();
    prefix.push(b'/');
    let rest = path.strip_prefix(prefix.as_slice())?;
    let end = rest.iter().position(|&b| b == b'/').unwrap_or(rest.len());
    let root = &path[..prefix.len() + end];
    ev.lints.is_copy(root).then(|| root.to_vec())
}

/// A read of `path` (readFile, readDir, import, ...) that goes through a
/// store copy this evaluation made.
pub fn read_through_copy(ev: &mut Evaluator, path: &[u8], pos: Pos) -> EvalResult<()> {
    if !ev.lint_on(LintId::ReadThroughCopy) {
        return Ok(());
    }
    let Some(root) = copied_root(ev, path) else {
        return Ok(());
    };
    let local = ev
        .src_to_store
        .iter()
        .find(|(_, sp)| *sp == root.as_slice())
        .map(|(l, _)| l);
    ev.lint(LintId::ReadThroughCopy, pos, || {
        let orig = local.map_or(String::new(), |l| format!(" (copied from '{}')", lossy(&l)));
        format!(
            "'{}' is read through the store copy '{}'{orig}; read the original path instead",
            lossy(path),
            lossy(&root)
        )
    })
}

/// The position the derivation-attribute lints (`drv-bool-null`,
/// `drv-list`, `impure-path-in-drv`, `absolute-path-in-drv`, `fod`) fall
/// back to when the attribute's own position is out of scope: nixpkgs's
/// `mkDerivation` and `fetchurl` bind the attributes, so the user's
/// derivation must be found elsewhere. The `derivationStrict` call, the
/// `name` attribute, then the first attribute (in the set's order) whose
/// position lies in a lint root; `Pos::NONE` when none does. The
/// evaluator keeps no call trace, so a derivation whose attribute set
/// holds no position from the root (a nixpkgs fetcher call) stays silent.
pub fn drv_fallback(
    ev: &mut Evaluator,
    attrs: &'static crate::runtime::AttrsObj,
    pos: Pos,
    name_pos: Pos,
) -> Pos {
    if !ev.lints.config.scoped() {
        return Pos::NONE;
    }
    for p in [pos, name_pos] {
        if ev.where_of(p) == Where::Root {
            return p;
        }
    }
    for a in attrs.iter() {
        if ev.where_of(a.pos) == Where::Root {
            return a.pos;
        }
    }
    Pos::NONE
}

/// The site a derivation-attribute lint reports at: the attribute's own
/// position when the scope keeps it, else `fallback`.
pub fn drv_attr_pos(ev: &mut Evaluator, attr_pos: Pos, fallback: Pos) -> Pos {
    if !ev.lints.config.scoped() || ev.where_of(attr_pos) != Where::Elsewhere {
        attr_pos
    } else {
        fallback
    }
}

/// A derivation attribute value, forced, about to be coerced into the
/// environment.
pub fn drv_value(ev: &mut Evaluator, key: &str, w: Word, attr_pos: Pos) -> EvalResult<()> {
    match w.view() {
        Value::Bool(b) => ev.lint(LintId::DrvBoolNull, attr_pos, || {
            format!(
                "derivation attribute '{key}' is the Boolean {b}, which becomes \"{}\" in the environment",
                if b { "1" } else { "" }
            )
        }),
        Value::Null => ev.lint(LintId::DrvBoolNull, attr_pos, || {
            format!(
                "derivation attribute '{key}' is null, which becomes \"\" in the environment; set `__ignoreNulls` to drop it"
            )
        }),
        Value::List(_) if key != "outputs" => ev.lint(LintId::DrvList, attr_pos, || {
            format!(
                "derivation attribute '{key}' is a list, which is space-joined into one environment string"
            )
        }),
        _ => Ok(()),
    }
}

/// `==` or `!=` on the forced operands `a` and `b`.
pub fn eq(ev: &mut Evaluator, a: Word, b: Word, pos: Pos) -> EvalResult<()> {
    let (ta, tb) = (a.value_type(), b.value_type());
    if ta == ValueType::Function || tb == ValueType::Function {
        ev.lint(LintId::EqFunction, pos, || {
            "`==` on a function is never true: functions compare by identity of one temporary"
                .to_string()
        })?;
    }
    let numeric = |t| matches!(t, ValueType::Int | ValueType::Float);
    if ta != tb && !(numeric(ta) && numeric(tb)) {
        ev.lint(LintId::EqTypeMismatch, pos, || {
            format!(
                "`==` compares {} with {}: values of different types are never equal",
                ta.name(),
                tb.name()
            )
        })?;
    }
    Ok(())
}

/// `x.a or d` reached a subject that is not an attrset.
pub fn or_non_attrs(ev: &mut Evaluator, v: Word, pos: Pos) -> EvalResult<()> {
    ev.lint(LintId::OrNonAttrs, pos, || {
        format!(
            "`or` falls back because the subject is {}, not an attrset",
            v.value_type().name()
        )
    })
}
