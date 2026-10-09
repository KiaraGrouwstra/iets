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
    float_digits: bool,
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
                float_digits: e
                    .get("float_digits")
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
            && (!self.float_digits || same_numbers(&d.nix, &d.iets))
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

/// Whether `a` and `b` differ only in the digits of numbers, each pair of
/// which reads back to the same double.
fn same_numbers(a: &str, b: &str) -> bool {
    let (ta, tb) = (tokens(a), tokens(b));
    ta.len() == tb.len()
        && ta.iter().zip(&tb).all(|(x, y)| match (x, y) {
            (Tok::Num(x), Tok::Num(y)) => x.parse::<f64>().ok() == y.parse::<f64>().ok(),
            (x, y) => x == y,
        })
}

#[derive(PartialEq)]
enum Tok<'a> {
    Num(&'a str),
    Text(char),
}

fn tokens(s: &str) -> Vec<Tok<'_>> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        let digits = |i: &mut usize| {
            let s = *i;
            while *i < b.len() && b[*i].is_ascii_digit() {
                *i += 1;
            }
            *i > s
        };
        if b[i] == b'-' {
            i += 1;
        }
        if digits(&mut i) {
            if i + 1 < b.len() && b[i] == b'.' && b[i + 1].is_ascii_digit() {
                i += 1;
                digits(&mut i);
            }
            if i < b.len() && matches!(b[i], b'e' | b'E') {
                let mut j = i + 1;
                if j < b.len() && matches!(b[j], b'+' | b'-') {
                    j += 1;
                }
                if digits(&mut j) {
                    i = j;
                }
            }
            out.push(Tok::Num(&s[start..i]));
        } else {
            let c = s[start..].chars().next().unwrap();
            i = start + c.len_utf8();
            out.push(Tok::Text(c));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::same_numbers;

    #[test]
    fn numbers_of_one_double() {
        assert!(same_numbers(
            "\"[4.2000000000009997e+17,1e+300]\"",
            "\"[4.200000000001e+17,1e+300]\""
        ));
        assert!(!same_numbers("[0.1]", "[0.2]"));
        assert!(!same_numbers("[0.1,1]", "[0.1,2]"));
        assert!(!same_numbers("a0.1", "b0.1"));
    }
}
