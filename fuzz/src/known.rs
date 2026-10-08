//! The known divergences of `fuzz/divergences.toml`: one array of tables
//! per target, `[[parse]]`, `[[nar]]`.

use crate::Divergence;

pub struct Entry {
    pub id: String,
    kind: Option<String>,
    input: Option<Vec<u8>>,
    nix: Option<String>,
    iets: Option<String>,
    names_differ: bool,
}

pub fn load(text: &str, target: &str, kinds: &[&str]) -> Result<Vec<Entry>, String> {
    let table: toml::Table = text.parse().map_err(|e| format!("divergences.toml: {e}"))?;
    let Some(list) = table.get(target) else {
        return Ok(vec![]);
    };
    let list = list.as_array().ok_or(format!(
        "divergences.toml: `{target}` is not an array of tables"
    ))?;
    list.iter()
        .map(|e| {
            let field = |k: &str| e.get(k).and_then(|v| v.as_str()).map(str::to_string);
            let id = field("id").ok_or("divergences.toml: an entry has no `id`")?;
            if field("doc").is_none() {
                return Err(format!("divergences.toml: `{id}` has no `doc`"));
            }
            let kind = field("kind");
            if let Some(k) = &kind
                && !kinds.contains(&k.as_str())
            {
                return Err(format!(
                    "divergences.toml: `{id}` has the unknown kind `{k}`"
                ));
            }
            Ok(Entry {
                id,
                kind,
                input: field("input").map(String::into_bytes),
                nix: field("nix"),
                iets: field("iets"),
                names_differ: e
                    .get("names_differ")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            })
        })
        .collect()
}

impl Entry {
    pub fn matches(&self, d: &Divergence, src: &[u8]) -> bool {
        self.kind.as_deref().is_none_or(|k| k == d.kind)
            && self.input.as_deref().is_none_or(|i| contains(src, i))
            && self.nix.as_deref().is_none_or(|n| d.nix.contains(n))
            && self.iets.as_deref().is_none_or(|n| d.iets.contains(n))
            && (!self.names_differ || quoted(&d.nix) != quoted(&d.iets))
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || hay.windows(needle.len()).any(|w| w == needle)
}

/// The `target` entries of `$IETS_FUZZ_DIVERGENCES`, else of the list next
/// to this crate, compiled in so a fuzz binary needs no path.
pub fn from_env(target: &str, kinds: &[&str]) -> Result<Vec<Entry>, String> {
    match std::env::var("IETS_FUZZ_DIVERGENCES") {
        Ok(p) => load(
            &std::fs::read_to_string(&p).map_err(|e| format!("{p}: {e}"))?,
            target,
            kinds,
        ),
        Err(_) => load(include_str!("../divergences.toml"), target, kinds),
    }
}

/// The first `'...'` of a message: the name an error is about.
fn quoted(msg: &str) -> Option<&str> {
    let (_, rest) = msg.split_once('\'')?;
    Some(rest.split_once('\'')?.0)
}
