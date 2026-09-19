//! Deprecated and legacy constructs: URL literals, `let { body = ...; }`,
//! `__overrides`, and references to deprecated or unsafe builtins. Hooks:
//! the parser notes URL literals and legacy lets; the compiler reports
//! them and checks every global or `builtins.x` reference.

use super::LintId;

pub fn url_literal(text: &[u8], error: bool) -> String {
    let text = String::from_utf8_lossy(text);
    if error {
        format!("URL literals are disallowed. Consider using a string literal \"{text}\" instead")
    } else {
        format!("URL literals are discouraged. Consider using a string literal \"{text}\" instead")
    }
}

pub fn legacy_let() -> String {
    "`let { body = ...; }` is a legacy form; use `let ... in` or `rec { }`".into()
}

pub fn overrides() -> String {
    "`__overrides` in a rec set is a legacy feature; use `//` or a fixed point".into()
}

/// Cheap prefilter for `builtin_reference`: every global reference passes
/// here, so no allocation before this says yes.
#[inline]
pub fn may_lint(name: &[u8]) -> bool {
    let bare = name.strip_prefix(b"__").unwrap_or(name);
    name.starts_with(b"__") || bare.starts_with(b"unsafe") || matches!(bare, b"isNull" | b"toPath")
}

/// The lints a reference to builtin `name` (without the `__` prefix when
/// reached through `builtins.`, with it for a bare global) triggers.
/// Returns `(lint, message)` pairs; one name can trigger both.
pub fn builtin_reference(name: &str) -> Vec<(LintId, String)> {
    let mut out = Vec::new();
    let bare = name.strip_prefix("__").unwrap_or(name);
    if bare.starts_with("unsafe") {
        out.push((
            LintId::UnsafeBuiltin,
            format!("`{name}` is an unsafe builtin: its result is not tracked by the evaluator"),
        ));
    }
    let deprecated = match bare {
        "isNull" => Some("use `x == null`"),
        "toPath" => Some("use a path literal or `/. + x`"),
        "unsafeDiscardOutputDependency" => Some("its semantics are being removed"),
        _ => None,
    };
    if let Some(hint) = deprecated {
        out.push((
            LintId::DeprecatedBuiltin,
            format!("`{name}` is deprecated: {hint}"),
        ));
    } else if name.starts_with("__") && name != "__curPos" {
        out.push((
            LintId::DeprecatedBuiltin,
            format!("`{name}` is an internal alias: use `builtins.{bare}`"),
        ));
    }
    out
}
