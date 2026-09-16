//! POSIX extended regular expressions (as libstdc++'s `std::regex::extended`
//! accepts them) translated to the `regex` crate's syntax.
//!
//! The translation is byte-oriented: every literal is emitted as `\xNN`
//! inside bracket expressions, so none of the crate's class operators
//! (`&&`, `--`, `~~`, nested `[`) can fire. Outside brackets only the ERE
//! metacharacters may be escaped; anything else is an error, as in libstdc++.

use std::fmt::Write as _;

const CLASSES: [&str; 12] = [
    "alnum", "alpha", "blank", "cntrl", "digit", "graph", "lower", "print", "punct", "space",
    "upper", "xdigit",
];

fn push_byte(out: &mut String, b: u8) {
    if b.is_ascii_alphanumeric() || b == b' ' || b == b'_' || b == b'-' || b == b'/' || b == b':' {
        out.push(b as char);
    } else {
        let _ = write!(out, "\\x{b:02x}");
    }
}

/// Translate an ERE into `regex::bytes` syntax. `None` means the pattern is
/// invalid in ERE, which callers report as "invalid regular expression".
pub fn translate(pat: &[u8]) -> Option<String> {
    let mut out = String::with_capacity(pat.len() + 8);
    let mut i = 0;
    let mut atom_start: Option<usize> = None;
    let mut last_was_quant = false;
    let mut groups: Vec<usize> = Vec::new();
    while i < pat.len() {
        let c = pat[i];
        i += 1;
        match c {
            b'\\' => {
                let e = *pat.get(i)?;
                i += 1;
                if !b"^.[$()|*+?{\\".contains(&e) {
                    return None;
                }
                atom_start = Some(out.len());
                out.push('\\');
                out.push(e as char);
                last_was_quant = false;
            }
            b'[' => {
                atom_start = Some(out.len());
                i = bracket(pat, i, &mut out)?;
                last_was_quant = false;
            }
            b'(' => {
                groups.push(out.len());
                out.push('(');
                atom_start = None;
                last_was_quant = false;
            }
            b')' => {
                let start = groups.pop()?;
                out.push(')');
                atom_start = Some(start);
                last_was_quant = false;
            }
            b'|' => {
                out.push('|');
                atom_start = None;
                last_was_quant = false;
            }
            b'*' | b'+' | b'?' => {
                let start = atom_start?;
                if last_was_quant {
                    wrap(&mut out, start);
                }
                out.push(c as char);
                last_was_quant = true;
            }
            b'{' => {
                let start = atom_start?;
                let (lo, hi, next) = interval(pat, i)?;
                i = next;
                if last_was_quant {
                    wrap(&mut out, start);
                }
                match hi {
                    Some(h) => {
                        let _ = write!(out, "{{{lo},{h}}}");
                    }
                    None if pat[i - 2] == b',' => {
                        let _ = write!(out, "{{{lo},}}");
                    }
                    None => {
                        let _ = write!(out, "{{{lo}}}");
                    }
                }
                last_was_quant = true;
            }
            b'^' | b'$' => {
                out.push(c as char);
                atom_start = None;
                last_was_quant = false;
            }
            b'.' => {
                atom_start = Some(out.len());
                out.push('.');
                last_was_quant = false;
            }
            _ => {
                atom_start = Some(out.len());
                push_byte(&mut out, c);
                last_was_quant = false;
            }
        }
    }
    if !groups.is_empty() {
        return None;
    }
    Some(out)
}

/// A second quantifier in a row applies to the already quantified atom:
/// `a+?` is `(a+)?` in ERE.
fn wrap(out: &mut String, start: usize) {
    out.insert_str(start, "(?:");
    out.push(')');
}

/// Parse `n}`, `n,}`, `n,m}` after the `{`. Returns (n, m, index after `}`).
fn interval(pat: &[u8], mut i: usize) -> Option<(u32, Option<u32>, usize)> {
    fn digits(pat: &[u8], i: &mut usize) -> Option<u32> {
        let start = *i;
        while *i < pat.len() && pat[*i].is_ascii_digit() {
            *i += 1;
        }
        if start == *i {
            return None;
        }
        std::str::from_utf8(&pat[start..*i]).ok()?.parse().ok()
    }
    let lo = digits(pat, &mut i)?;
    if lo > 0x7fff {
        return None;
    }
    let hi = match *pat.get(i)? {
        b'}' => {
            i += 1;
            return Some((lo, None, i));
        }
        b',' => {
            i += 1;
            if pat.get(i) == Some(&b'}') {
                i += 1;
                return Some((lo, None, i));
            }
            digits(pat, &mut i)?
        }
        _ => return None,
    };
    if *pat.get(i)? != b'}' || hi < lo {
        return None;
    }
    if hi > 0x7fff {
        return None;
    }
    Some((lo, Some(hi), i + 1))
}

enum Item {
    Byte(u8),
    Class,
}

/// One bracket item: a literal byte, `[.x.]`, `[=x=]`, or `[:class:]`.
fn bracket_item(pat: &[u8], i: &mut usize, out: &mut String) -> Option<Item> {
    let c = *pat.get(*i)?;
    if c == b'['
        && let Some(&kind) = pat.get(*i + 1)
        && matches!(kind, b':' | b'.' | b'=')
    {
        let body_start = *i + 2;
        let mut j = body_start;
        while j + 1 < pat.len() && !(pat[j] == kind && pat[j + 1] == b']') {
            j += 1;
        }
        if j + 1 >= pat.len() {
            return None;
        }
        let body = &pat[body_start..j];
        *i = j + 2;
        if kind == b':' {
            let name = std::str::from_utf8(body).ok()?;
            if !CLASSES.contains(&name) {
                return None;
            }
            let _ = write!(out, "[:{name}:]");
            return Some(Item::Class);
        }
        if body.len() != 1 {
            return None;
        }
        return Some(Item::Byte(body[0]));
    }
    *i += 1;
    Some(Item::Byte(c))
}

/// Parse a bracket expression after the `[`; returns the index after `]`.
fn bracket(pat: &[u8], mut i: usize, out: &mut String) -> Option<usize> {
    out.push('[');
    if pat.get(i) == Some(&b'^') {
        out.push('^');
        i += 1;
    }
    let mut first = true;
    loop {
        let c = *pat.get(i)?;
        if c == b']' && !first {
            i += 1;
            break;
        }
        first = false;
        match bracket_item(pat, &mut i, out)? {
            Item::Class => {
                if pat.get(i) == Some(&b'-') && pat.get(i + 1) != Some(&b']') {
                    return None;
                }
            }
            Item::Byte(lo) => {
                if pat.get(i) == Some(&b'-') && pat.get(i + 1).is_some_and(|&n| n != b']') {
                    i += 1;
                    let hi = match bracket_item(pat, &mut i, out)? {
                        Item::Byte(h) => h,
                        Item::Class => return None,
                    };
                    if hi < lo {
                        return None;
                    }
                    let _ = write!(out, "\\x{lo:02x}-\\x{hi:02x}");
                } else {
                    let _ = write!(out, "\\x{lo:02x}");
                }
            }
        }
    }
    out.push(']');
    Some(i)
}

#[cfg(test)]
mod tests {
    use super::translate;

    #[test]
    fn brackets_and_escapes() {
        assert_eq!(translate(b"[\\.]").unwrap(), "[\\x5c\\x2e]");
        assert_eq!(translate(b"[]a]").unwrap(), "[\\x5d\\x61]");
        assert_eq!(translate(b"[^]a]").unwrap(), "[^\\x5d\\x61]");
        assert_eq!(translate(b"[a-]").unwrap(), "[\\x61\\x2d]");
        assert_eq!(translate(b"[!--]").unwrap(), "[\\x21-\\x2d]");
        assert_eq!(translate(b"[[:alpha:]_]").unwrap(), "[[:alpha:]\\x5f]");
        assert_eq!(translate(b"a\\.b").unwrap(), "a\\.b");
        assert!(translate(b"\\n").is_none());
        assert!(translate(b"\\/").is_none());
        assert!(translate(b"[[:foo:]]").is_none());
        assert!(translate(b"[z-a]").is_none());
        assert!(translate(b"[[:alpha:]-z]").is_none());
    }

    #[test]
    fn quantifiers() {
        assert_eq!(translate(b"a**").unwrap(), "(?:a*)*");
        assert_eq!(translate(b"(a+?)").unwrap(), "((?:a+)?)");
        assert_eq!(translate(b"a{1,2}{2}").unwrap(), "(?:a{1,2}){2}");
        assert_eq!(translate(b"a{1,}").unwrap(), "a{1,}");
        assert_eq!(translate(b"a{3}").unwrap(), "a{3}");
        assert!(translate(b"*a").is_none());
        assert!(translate(b"(*a)").is_none());
        assert!(translate(b"a|*b").is_none());
        assert!(translate(b"^*a").is_none());
        assert!(translate(b"a{").is_none());
        assert!(translate(b"a{,2}").is_none());
        assert!(translate(b"a{2,1}").is_none());
        assert!(translate(b"a{99999}").is_none());
        assert!(translate(b"(?:a)").is_none());
        assert!(translate(b"(a").is_none());
        assert!(translate(b"a)").is_none());
    }
}
