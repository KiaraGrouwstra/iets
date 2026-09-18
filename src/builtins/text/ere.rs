//! POSIX extended regular expressions, parsed the way libstdc++'s
//! `std::regex::extended` parses them and compiled into the same NFA
//! (`_Compiler` in `bits/regex_compiler.tcc`, gcc 15.2).
//!
//! The compiler is byte-oriented: a bracket expression collapses to a
//! 256-bit set, character classes are the C locale's ASCII ones, and ranges
//! compare as unsigned bytes, which is what libstdc++ does with `char` and
//! no collation. Outside brackets only the ERE metacharacters may be
//! escaped; anything else is an error, as in libstdc++.

use super::posix::{self, Bits, INVALID, Prog, Seq};

const CLASSES: [&str; 12] = [
    "alnum", "alpha", "blank", "cntrl", "digit", "graph", "lower", "print", "punct", "space",
    "upper", "xdigit",
];

/// Compile an ERE. `None` means libstdc++ would throw `regex_error`, which
/// callers report as "invalid regular expression".
pub fn compile(pat: &[u8]) -> Option<Prog> {
    let mut c = Compiler {
        pat,
        i: 0,
        prog: Prog::new(),
    };
    // _Compiler's ctor: the whole pattern is subexpression 0.
    let (start, sub) = c.prog.sub_begin()?;
    debug_assert_eq!(start, 0);
    let mut r = Seq { start, end: start };
    let body = c.disjunction()?;
    if c.i != c.pat.len() {
        return None; // error_paren: a token no rule consumed
    }
    c.prog.append(&mut r, body);
    let e = c.prog.sub_end(sub)?;
    c.prog.append_id(&mut r, e);
    let a = c.prog.accept()?;
    c.prog.append_id(&mut r, a);
    Some(c.prog)
}

struct Compiler<'a> {
    pat: &'a [u8],
    i: usize,
    prog: Prog,
}

impl Compiler<'_> {
    fn peek(&self) -> Option<u8> {
        self.pat.get(self.i).copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            return true;
        }
        false
    }

    /// `_M_disjunction`, left-associative: `a|b|c` nests as `(a|b)|c`, and
    /// the executor walks the left branch of each alternation first.
    fn disjunction(&mut self) -> Option<Seq> {
        let mut alt1 = self.alternative()?;
        while self.eat(b'|') {
            let mut alt2 = self.alternative()?;
            let end = self.prog.dummy()?;
            self.prog.append_id(&mut alt1, end);
            self.prog.append_id(&mut alt2, end);
            let node = self.prog.alt(alt2.start, alt1.start)?;
            alt1 = Seq { start: node, end };
        }
        Some(alt1)
    }

    /// `_M_alternative`: a run of terms, then the dummy its base case makes.
    fn alternative(&mut self) -> Option<Seq> {
        let mut seq: Option<Seq> = None;
        while let Some(t) = self.term()? {
            match &mut seq {
                None => seq = Some(t),
                Some(s) => self.prog.append(s, t),
            }
        }
        let d = self.prog.dummy()?;
        match seq {
            None => Some(one(d)),
            Some(mut s) => {
                self.prog.append_id(&mut s, d);
                Some(s)
            }
        }
    }

    /// `_M_term`: an assertion, or an atom with its quantifiers. `Some(None)`
    /// means the next token starts no term.
    fn term(&mut self) -> Option<Option<Seq>> {
        // _M_assertion. It returns before _M_quantifier runs, so `^*` is an
        // error rather than a repeated anchor.
        match self.peek() {
            Some(b'^') => {
                self.i += 1;
                let id = self.prog.line_begin()?;
                return Some(Some(one(id)));
            }
            Some(b'$') => {
                self.i += 1;
                let id = self.prog.line_end()?;
                return Some(Some(one(id)));
            }
            _ => {}
        }
        let Some(mut a) = self.atom()? else {
            return Some(None);
        };
        while let Some(q) = self.quantifier(a)? {
            a = q;
        }
        Some(Some(a))
    }

    /// `_M_atom`. `Some(None)` means this is not the start of an atom.
    fn atom(&mut self) -> Option<Option<Seq>> {
        let Some(c) = self.peek() else {
            return Some(None);
        };
        let seq = match c {
            b'.' => {
                self.i += 1;
                let id = self.prog.any()?;
                one(id)
            }
            b'[' => {
                self.i += 1;
                let bits = self.bracket()?;
                let id = self.prog.set(bits)?;
                one(id)
            }
            b'(' => {
                self.i += 1;
                let (start, sub) = self.prog.sub_begin()?;
                let mut r = Seq { start, end: start };
                let body = self.disjunction()?;
                if !self.eat(b')') {
                    return None;
                }
                self.prog.append(&mut r, body);
                let e = self.prog.sub_end(sub)?;
                self.prog.append_id(&mut r, e);
                r
            }
            b'\\' => {
                let e = *self.pat.get(self.i + 1)?;
                if !b"^.[$()|*+?{\\".contains(&e) {
                    return None;
                }
                self.i += 2;
                let id = self.prog.byte(e)?;
                one(id)
            }
            b')' | b'|' | b'*' | b'+' | b'?' | b'{' => return Some(None),
            _ => {
                self.i += 1;
                let id = self.prog.byte(c)?;
                one(id)
            }
        };
        Some(Some(seq))
    }

    /// `_M_quantifier`. A second quantifier applies to the already quantified
    /// atom, so `a+?` is `(a+)?`.
    fn quantifier(&mut self, mut e: Seq) -> Option<Option<Seq>> {
        let seq = match self.peek() {
            Some(b'*') => {
                self.i += 1;
                let r = self.prog.repeat(INVALID, e.start)?;
                self.prog.append_id(&mut e, r);
                one(r)
            }
            Some(b'+') => {
                self.i += 1;
                let r = self.prog.repeat(INVALID, e.start)?;
                self.prog.append_id(&mut e, r);
                e
            }
            Some(b'?') => {
                self.i += 1;
                let end = self.prog.dummy()?;
                let r = self.prog.repeat(INVALID, e.start)?;
                self.prog.append_id(&mut e, end);
                let mut rs = one(r);
                self.prog.append_id(&mut rs, end);
                Seq { start: r, end }
            }
            Some(b'{') => {
                self.i += 1;
                self.interval(e)?
            }
            _ => return Some(None),
        };
        Some(Some(seq))
    }

    /// `{n}`, `{n,}` or `{n,m}` after the `{`: `n` copies of the atom, then
    /// either a closure or `m - n` nested optional copies.
    fn interval(&mut self, r: Seq) -> Option<Seq> {
        let lo = self.digits()?;
        let mut infinite = false;
        let mut extra = 0u64;
        if self.eat(b',') {
            if self.peek() == Some(b'}') {
                infinite = true;
            } else {
                let hi = self.digits()?;
                if hi < lo {
                    return None; // error_badbrace
                }
                extra = hi - lo;
            }
        }
        if !self.eat(b'}') {
            return None;
        }
        let d = self.prog.dummy()?;
        let mut e = one(d);
        // Each copy costs a state, so a silly count runs into the NFA's
        // state limit instead of looping.
        for _ in 0..lo.min(posix::STATE_LIMIT as u64 + 1) {
            let c = self.prog.clone_seq(r)?;
            self.prog.append(&mut e, c);
        }
        if infinite {
            let mut tmp = self.prog.clone_seq(r)?;
            let s = self.prog.repeat(INVALID, tmp.start)?;
            self.prog.append_id(&mut tmp, s);
            self.prog.append_id(&mut e, s);
        } else {
            let end = self.prog.dummy()?;
            let mut alts = Vec::new();
            for _ in 0..extra.min(posix::STATE_LIMIT as u64 + 1) {
                let tmp = self.prog.clone_seq(r)?;
                let alt = self.prog.repeat(tmp.start, end)?;
                alts.push(alt);
                self.prog.append(
                    &mut e,
                    Seq {
                        start: alt,
                        end: tmp.end,
                    },
                );
            }
            self.prog.append_id(&mut e, end);
            // "_M_alt is the match-more branch": libstdc++ builds these the
            // other way round and swaps at the end.
            for alt in alts {
                self.prog.swap_repeat(alt);
            }
        }
        Some(e)
    }

    /// One `dup_count` token: the scanner allows digits only.
    fn digits(&mut self) -> Option<u64> {
        let start = self.i;
        let mut n: u64 = 0;
        while let Some(c) = self.peek() {
            if !c.is_ascii_digit() {
                break;
            }
            n = n.saturating_mul(10).saturating_add((c - b'0') as u64);
            self.i += 1;
        }
        if start == self.i {
            return None; // error_badbrace
        }
        Some(n)
    }

    /// A bracket expression after the `[`.
    fn bracket(&mut self) -> Option<Bits> {
        let mut bits = Bits::default();
        let negate = self.eat(b'^');
        let mut first = true;
        loop {
            let c = self.peek()?;
            if c == b']' && !first {
                self.i += 1;
                break;
            }
            first = false;
            match self.bracket_item(&mut bits)? {
                Item::Class => {
                    if self.peek() == Some(b'-') && self.pat.get(self.i + 1) != Some(&b']') {
                        return None; // a class cannot start a range
                    }
                }
                Item::Byte(lo) => {
                    if self.peek() == Some(b'-')
                        && self.pat.get(self.i + 1).is_some_and(|&n| n != b']')
                    {
                        self.i += 1;
                        let Item::Byte(hi) = self.bracket_item(&mut bits)? else {
                            return None;
                        };
                        if hi < lo {
                            return None; // error_range
                        }
                        bits.set_range(lo, hi);
                    } else {
                        bits.set(lo);
                    }
                }
            }
        }
        if negate {
            bits.negate();
        }
        Some(bits)
    }

    /// One bracket item: a literal byte, `[.x.]`, `[=x=]`, or `[:class:]`.
    fn bracket_item(&mut self, bits: &mut Bits) -> Option<Item> {
        let c = self.peek()?;
        if c == b'['
            && let Some(&kind) = self.pat.get(self.i + 1)
            && matches!(kind, b':' | b'.' | b'=')
        {
            let body_start = self.i + 2;
            let mut j = body_start;
            while j + 1 < self.pat.len() && !(self.pat[j] == kind && self.pat[j + 1] == b']') {
                j += 1;
            }
            if j + 1 >= self.pat.len() {
                return None;
            }
            let body = &self.pat[body_start..j];
            self.i = j + 2;
            if kind == b':' {
                let name = std::str::from_utf8(body).ok()?;
                if !CLASSES.contains(&name) {
                    return None;
                }
                add_class(bits, name);
                return Some(Item::Class);
            }
            if body.len() != 1 {
                return None;
            }
            return Some(Item::Byte(body[0]));
        }
        self.i += 1;
        Some(Item::Byte(c))
    }
}

/// A sub-NFA of one state.
fn one(id: u32) -> Seq {
    Seq { start: id, end: id }
}

enum Item {
    Byte(u8),
    Class,
}

/// The C locale's classes, which is what `std::ctype<char>` reports.
fn add_class(bits: &mut Bits, name: &str) {
    let ranges: &[(u8, u8)] = match name {
        "alnum" => &[(b'0', b'9'), (b'A', b'Z'), (b'a', b'z')],
        "alpha" => &[(b'A', b'Z'), (b'a', b'z')],
        "blank" => &[(b'\t', b'\t'), (b' ', b' ')],
        "cntrl" => &[(0, 0x1f), (0x7f, 0x7f)],
        "digit" => &[(b'0', b'9')],
        "graph" => &[(0x21, 0x7e)],
        "lower" => &[(b'a', b'z')],
        "print" => &[(0x20, 0x7e)],
        "punct" => &[(0x21, 0x2f), (0x3a, 0x40), (0x5b, 0x60), (0x7b, 0x7e)],
        "space" => &[(0x09, 0x0d), (b' ', b' ')],
        "upper" => &[(b'A', b'Z')],
        _ => &[(b'0', b'9'), (b'A', b'F'), (b'a', b'f')],
    };
    for &(lo, hi) in ranges {
        bits.set_range(lo, hi);
    }
}

#[cfg(test)]
mod tests {
    use super::compile;

    fn ok(pat: &str) -> bool {
        compile(pat.as_bytes()).is_some()
    }

    #[test]
    fn brackets_and_escapes() {
        assert!(ok("[\\.]"));
        assert!(ok("[]a]"));
        assert!(ok("[^]a]"));
        assert!(ok("[a-]"));
        assert!(ok("[!--]"));
        assert!(ok("[[:alpha:]_]"));
        assert!(ok("a\\.b"));
        assert!(!ok("\\n"));
        assert!(!ok("\\/"));
        assert!(!ok("[[:foo:]]"));
        assert!(!ok("[z-a]"));
        assert!(!ok("[[:alpha:]-z]"));
        assert!(!ok("[a-c"));
        assert!(!ok("[["));
    }

    #[test]
    fn quantifiers() {
        assert!(ok("a**"));
        assert!(ok("(a+?)"));
        assert!(ok("a{1,2}{2}"));
        assert!(ok("a{1,}"));
        assert!(ok("a{3}"));
        assert!(ok("a{40000}"));
        assert!(!ok("*a"));
        assert!(!ok("(*a)"));
        assert!(!ok("a|*b"));
        assert!(!ok("^*a"));
        assert!(!ok("a{"));
        assert!(!ok("a{,2}"));
        assert!(!ok("a{2,1}"));
        assert!(!ok("a{2x}"));
        assert!(!ok("a{99999}"));
        assert!(!ok("(?:a)"));
        assert!(!ok("(a"));
        assert!(!ok("a)"));
        assert!(!ok("(1)\\1"));
    }
}
