//! libstdc++'s `std::regex` in POSIX `extended` mode, ported: the NFA that
//! `_Compiler` builds (`bits/regex_compiler.tcc`) and the depth-first
//! executor that walks it (`bits/regex_executor.tcc`, gcc 15.2).
//!
//! Nix compiles `builtins.match` and `builtins.split` patterns with
//! `std::regex(re, extended)`, so the answers, down to which subexpression
//! captures survive, are whatever that executor produces. It is not textbook
//! POSIX: alternation is leftmost-longest (both branches run and the longer
//! end position wins, ties going to the first in depth-first order) while a
//! quantifier stops as soon as any solution exists (`_M_handle_repeat` only
//! leaves the loop `if (!_M_has_sol)`). A second automaton cannot be talked
//! into that, so this is the automaton.
//!
//! The recursion of `_M_dfs` is an explicit frame stack here: a pattern that
//! consumes a megabyte of input in one path would otherwise need a megabyte
//! of call frames.

use std::collections::HashMap;
use std::rc::Rc;

pub type Id = u32;
pub const INVALID: Id = u32::MAX;

/// `_GLIBCXX_REGEX_STATE_LIMIT`: libstdc++ throws `error_space` once the NFA
/// holds more states than this, which Nix reports as a memory limit.
const STATE_LIMIT: usize = 100_000;

/// The state budget, for the compiler's repetition loops.
pub fn state_limit() -> u64 {
    STATE_LIMIT as u64
}

/// A byte set: a bracket expression, already collapsed to bytes.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Bits([u64; 4]);

impl Bits {
    pub fn set(&mut self, b: u8) {
        self.0[(b >> 6) as usize] |= 1 << (b & 63);
    }

    pub fn set_range(&mut self, lo: u8, hi: u8) {
        for b in lo..=hi {
            self.set(b);
        }
    }

    pub fn negate(&mut self) {
        for w in &mut self.0 {
            *w = !*w;
        }
    }

    fn has(&self, b: u8) -> bool {
        self.0[(b >> 6) as usize] >> (b & 63) & 1 != 0
    }
}

#[derive(Clone)]
enum Op {
    /// An alternation's merge point, and the empty alternative.
    Dummy,
    /// `_S_opcode_alternative`: `alt` is the left branch, `next` the right.
    Alt(Id),
    /// `_S_opcode_repeat`, always greedy: `alt` re-enters, `next` leaves.
    Repeat(Id),
    SubBegin(u32),
    SubEnd(u32),
    LineBegin,
    LineEnd,
    Byte(u8),
    /// `.`, which in POSIX mode is every byte but NUL.
    Any,
    Set(Rc<Bits>),
    Accept,
}

#[derive(Clone)]
struct State {
    op: Op,
    next: Id,
}

/// A compiled pattern. Node identity matters: `_M_rep_count` is keyed by
/// state id, so the clones that `{n,m}` makes each get their own counter.
pub struct Prog {
    states: Vec<State>,
    /// Subexpressions, counting the implicit one around the whole pattern.
    nsub: u32,
}

/// libstdc++'s `_StateSeq`: a sub-NFA with one entry and one exit.
#[derive(Clone, Copy)]
pub struct Seq {
    pub start: Id,
    pub end: Id,
}

impl Prog {
    pub fn new() -> Self {
        Prog {
            states: Vec::new(),
            nsub: 0,
        }
    }

    /// `None` once the NFA outgrows `_GLIBCXX_REGEX_STATE_LIMIT`.
    fn insert(&mut self, op: Op) -> Option<Id> {
        self.states.push(State { op, next: INVALID });
        if self.states.len() > STATE_LIMIT {
            return None;
        }
        Some(self.states.len() as Id - 1)
    }

    pub fn dummy(&mut self) -> Option<Id> {
        self.insert(Op::Dummy)
    }

    pub fn byte(&mut self, b: u8) -> Option<Id> {
        self.insert(Op::Byte(b))
    }

    pub fn any(&mut self) -> Option<Id> {
        self.insert(Op::Any)
    }

    pub fn set(&mut self, bits: Bits) -> Option<Id> {
        self.insert(Op::Set(Rc::new(bits)))
    }

    pub fn line_begin(&mut self) -> Option<Id> {
        self.insert(Op::LineBegin)
    }

    pub fn line_end(&mut self) -> Option<Id> {
        self.insert(Op::LineEnd)
    }

    pub fn accept(&mut self) -> Option<Id> {
        self.insert(Op::Accept)
    }

    /// Opens a subexpression; the index is passed back to [`Prog::sub_end`].
    pub fn sub_begin(&mut self) -> Option<(Id, u32)> {
        let n = self.nsub;
        self.nsub += 1;
        Some((self.insert(Op::SubBegin(n))?, n))
    }

    pub fn sub_end(&mut self, n: u32) -> Option<Id> {
        self.insert(Op::SubEnd(n))
    }

    pub fn alt(&mut self, next: Id, alt: Id) -> Option<Id> {
        let id = self.insert(Op::Alt(alt))?;
        self.states[id as usize].next = next;
        Some(id)
    }

    pub fn repeat(&mut self, next: Id, alt: Id) -> Option<Id> {
        let id = self.insert(Op::Repeat(alt))?;
        self.states[id as usize].next = next;
        Some(id)
    }

    /// Swap a repeat's "match more" and "match less" branches, the last step
    /// of `{n,m}` expansion.
    pub fn swap_repeat(&mut self, id: Id) {
        let st = &mut self.states[id as usize];
        if let Op::Repeat(alt) = st.op {
            st.op = Op::Repeat(st.next);
            st.next = alt;
        }
    }

    /// `_StateSeq::_M_append`.
    pub fn append_id(&mut self, seq: &mut Seq, id: Id) {
        self.states[seq.end as usize].next = id;
        seq.end = id;
    }

    pub fn append(&mut self, seq: &mut Seq, other: Seq) {
        self.states[seq.end as usize].next = other.start;
        seq.end = other.end;
    }

    /// `_StateSeq::_M_clone`: a fresh copy of the sub-NFA, so `a{2,3}` gets
    /// one repeat counter per copy.
    pub fn clone_seq(&mut self, seq: Seq) -> Option<Seq> {
        let mut map: HashMap<Id, Id> = HashMap::new();
        let mut stack = vec![seq.start];
        while let Some(u) = stack.pop() {
            if map.contains_key(&u) {
                continue;
            }
            let dup = self.states[u as usize].clone();
            let alt = match dup.op {
                Op::Alt(a) | Op::Repeat(a) => Some(a),
                _ => None,
            };
            let next = dup.next;
            let id = self.insert(dup.op)?;
            self.states[id as usize].next = next;
            map.insert(u, id);
            if let Some(a) = alt
                && a != INVALID
                && !map.contains_key(&a)
            {
                stack.push(a);
            }
            if u == seq.end {
                continue;
            }
            if next != INVALID && !map.contains_key(&next) {
                stack.push(next);
            }
        }
        let lookup = |id: Id| map.get(&id).copied();
        for &v in map.values() {
            let next = self.states[v as usize].next;
            if next != INVALID
                && let Some(n) = lookup(next)
            {
                self.states[v as usize].next = n;
            }
            let alt = match self.states[v as usize].op {
                Op::Alt(a) | Op::Repeat(a) => a,
                _ => continue,
            };
            if alt != INVALID
                && let Some(a) = lookup(alt)
            {
                self.states[v as usize].op = match self.states[v as usize].op {
                    Op::Alt(_) => Op::Alt(a),
                    _ => Op::Repeat(a),
                };
            }
        }
        Some(Seq {
            start: lookup(seq.start)?,
            end: lookup(seq.end)?,
        })
    }
}

/// One subexpression's span, libstdc++'s `sub_match`.
#[derive(Clone, Copy, Default)]
struct Span {
    first: usize,
    second: usize,
    matched: bool,
}

/// A step of the depth-first walk, or the undo that follows a finished child.
enum Frame {
    Node(Id),
    Byte,
    SubBegin(u32, usize),
    SubEnd(u32, Span),
    Rep(usize, (usize, u32)),
    /// `if (!_M_has_sol) _M_dfs(__state._M_next)` after the body ran.
    RepExit(Id),
    /// The right branch of an alternation, with `_M_has_sol` reset.
    AltRight(Id),
    /// `_M_has_sol |= __has_sol` once both branches ran.
    AltJoin(bool),
}

struct Exec<'a> {
    prog: &'a Prog,
    hay: &'a [u8],
    begin: usize,
    cur: usize,
    results: Vec<Span>,
    cur_results: Vec<Span>,
    rep_count: Vec<(usize, u32)>,
    has_sol: bool,
    sol_pos: Option<usize>,
    exact: bool,
    not_null: bool,
    continuous: bool,
    prev_avail: bool,
    frames: Vec<Frame>,
}

impl<'a> Exec<'a> {
    fn new(prog: &'a Prog, hay: &'a [u8], begin: usize) -> Self {
        let n = prog.nsub as usize;
        Exec {
            prog,
            hay,
            begin,
            cur: begin,
            results: vec![Span::default(); n],
            cur_results: vec![Span::default(); n],
            rep_count: vec![(0, 0); prog.states.len()],
            has_sol: false,
            sol_pos: None,
            exact: false,
            not_null: false,
            continuous: false,
            prev_avail: false,
            frames: Vec::new(),
        }
    }

    /// `_M_main_dispatch(__match_mode, __dfs)`.
    fn main(&mut self) -> bool {
        self.has_sol = false;
        self.sol_pos = None;
        self.cur_results.copy_from_slice(&self.results);
        self.cur = self.begin;
        self.frames.push(Frame::Node(0));
        while let Some(f) = self.frames.pop() {
            match f {
                Frame::Node(id) => self.step(id),
                Frame::Byte => self.cur -= 1,
                Frame::SubBegin(n, first) => self.cur_results[n as usize].first = first,
                Frame::SubEnd(n, span) => self.cur_results[n as usize] = span,
                Frame::Rep(i, back) => self.rep_count[i] = back,
                Frame::RepExit(next) => {
                    if !self.has_sol && next != INVALID {
                        self.frames.push(Frame::Node(next));
                    }
                }
                Frame::AltRight(next) => {
                    let saved = self.has_sol;
                    self.has_sol = false;
                    self.frames.push(Frame::AltJoin(saved));
                    self.frames.push(Frame::Node(next));
                }
                Frame::AltJoin(saved) => self.has_sol |= saved,
            }
        }
        self.has_sol
    }

    fn step(&mut self, id: Id) {
        let st = &self.prog.states[id as usize];
        let next = st.next;
        match &st.op {
            Op::Dummy => self.follow(next),
            Op::Alt(alt) => {
                let alt = *alt;
                self.frames.push(Frame::AltRight(next));
                self.follow(alt);
            }
            Op::Repeat(alt) => {
                let alt = *alt;
                self.frames.push(Frame::RepExit(next));
                self.rep_once_more(id as usize, alt);
            }
            Op::SubBegin(n) => {
                let n = *n;
                let res = &mut self.cur_results[n as usize];
                self.frames.push(Frame::SubBegin(n, res.first));
                res.first = self.cur;
                self.follow(next);
            }
            Op::SubEnd(n) => {
                let n = *n;
                let cur = self.cur;
                let res = &mut self.cur_results[n as usize];
                self.frames.push(Frame::SubEnd(n, *res));
                res.second = cur;
                res.matched = true;
                self.follow(next);
            }
            Op::LineBegin => {
                if self.cur == self.begin && !self.prev_avail {
                    self.follow(next);
                }
            }
            Op::LineEnd => {
                if self.cur == self.hay.len() {
                    self.follow(next);
                }
            }
            Op::Byte(b) => {
                let b = *b;
                if self.hay.get(self.cur) == Some(&b) {
                    self.advance(next);
                }
            }
            Op::Any => {
                if matches!(self.hay.get(self.cur), Some(&c) if c != 0) {
                    self.advance(next);
                }
            }
            Op::Set(bits) => {
                if matches!(self.hay.get(self.cur), Some(&c) if bits.has(c)) {
                    self.advance(next);
                }
            }
            Op::Accept => self.accept(),
        }
    }

    fn follow(&mut self, next: Id) {
        if next != INVALID {
            self.frames.push(Frame::Node(next));
        }
    }

    fn advance(&mut self, next: Id) {
        self.cur += 1;
        self.frames.push(Frame::Byte);
        self.follow(next);
    }

    /// `_M_rep_once_more`: a repeat may run twice at one position, once for
    /// the match and once more so an empty iteration can set its captures.
    fn rep_once_more(&mut self, id: usize, alt: Id) {
        let back = self.rep_count[id];
        if back.1 == 0 || back.0 != self.cur {
            self.rep_count[id] = (self.cur, 1);
        } else if back.1 < 2 {
            self.rep_count[id].1 = back.1 + 1;
        } else {
            return;
        }
        self.frames.push(Frame::Rep(id, back));
        self.follow(alt);
    }

    /// `_M_handle_accept`, POSIX half: keep the strictly longer solution, so
    /// a tie leaves the first one found in depth-first order in place.
    fn accept(&mut self) {
        self.has_sol = if self.exact {
            self.cur == self.hay.len()
        } else {
            true
        };
        if self.cur == self.begin && self.not_null {
            self.has_sol = false;
        }
        if self.has_sol && self.sol_pos.is_none_or(|p| p < self.cur) {
            self.sol_pos = Some(self.cur);
            self.results.copy_from_slice(&self.cur_results);
        }
    }

    /// `_Executor::_M_search`.
    fn search(&mut self) -> bool {
        if self.main() {
            return true;
        }
        if self.continuous {
            return false;
        }
        self.prev_avail = true;
        while self.begin != self.hay.len() {
            self.begin += 1;
            if self.main() {
                return true;
            }
        }
        false
    }
}

/// The groups of `re` against the whole of `hay`, or `None` for no match.
/// `std::regex_match`, so `_Match_mode::_Exact` from position zero.
pub fn match_exact(prog: &Prog, hay: &[u8]) -> Option<Vec<Option<(usize, usize)>>> {
    let mut ex = Exec::new(prog, hay, 0);
    ex.exact = true;
    if !ex.main() {
        return None;
    }
    Some(groups(&ex))
}

/// One match of `re` in `hay` at or after `from`: the whole match's span and
/// the groups. `std::regex_search`, `_Match_mode::_Prefix`.
pub struct Found {
    pub start: usize,
    pub end: usize,
    pub groups: Vec<Option<(usize, usize)>>,
}

/// `std::cregex_iterator` over `hay`, which is what `builtins.split` walks.
pub struct Matches<'a> {
    prog: &'a Prog,
    hay: &'a [u8],
    /// Span of the match just returned; `None` before the first one.
    last: Option<(usize, usize)>,
    prev_avail: bool,
    done: bool,
}

pub fn matches<'a>(prog: &'a Prog, hay: &'a [u8]) -> Matches<'a> {
    Matches {
        prog,
        hay,
        last: None,
        prev_avail: false,
        done: false,
    }
}

impl Matches<'_> {
    fn search(&mut self, begin: usize, anchored_non_empty: bool) -> Option<Found> {
        let mut ex = Exec::new(self.prog, self.hay, begin);
        ex.prev_avail = self.prev_avail;
        ex.not_null = anchored_non_empty;
        ex.continuous = anchored_non_empty;
        if !ex.search() {
            return None;
        }
        Some(Found {
            start: ex.results[0].first,
            end: ex.results[0].second,
            groups: groups(&ex),
        })
    }

    fn record(&mut self, found: Option<Found>) -> Option<Found> {
        match found {
            Some(f) => {
                self.last = Some((f.start, f.end));
                Some(f)
            }
            None => {
                self.done = true;
                None
            }
        }
    }
}

impl Iterator for Matches<'_> {
    type Item = Found;

    /// `regex_iterator::operator++`: after an empty match, retry the same
    /// position for a non-empty match before stepping one byte on.
    fn next(&mut self) -> Option<Found> {
        if self.done {
            return None;
        }
        let Some((first, second)) = self.last else {
            let f = self.search(0, false);
            return self.record(f);
        };
        let mut start = second;
        if first == second {
            if start == self.hay.len() {
                self.done = true;
                return None;
            }
            if let Some(f) = self.search(start, true) {
                return self.record(Some(f));
            }
            start += 1;
        }
        self.prev_avail = true;
        let f = self.search(start, false);
        self.record(f)
    }
}

fn groups(ex: &Exec<'_>) -> Vec<Option<(usize, usize)>> {
    ex.results[1..]
        .iter()
        .map(|s| s.matched.then_some((s.first, s.second)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::ere::compile;
    use super::*;

    /// `builtins.match`, as a list of group texts, `~` for an unmatched group.
    fn m(re: &str, s: &str) -> Option<Vec<String>> {
        let prog = compile(re.as_bytes()).expect("valid ERE");
        let g = match_exact(&prog, s.as_bytes())?;
        Some(spans(&g, s))
    }

    /// `builtins.split`, flattened: text, then the groups of each match.
    fn sp(re: &str, s: &str) -> Vec<String> {
        let prog = compile(re.as_bytes()).expect("valid ERE");
        let mut out = Vec::new();
        let mut prev = 0;
        for f in matches(&prog, s.as_bytes()) {
            out.push(s[prev..f.start].to_string());
            out.push(spans(&f.groups, s).join(","));
            prev = f.end;
        }
        out.push(s[prev..].to_string());
        out
    }

    fn spans(g: &[Option<(usize, usize)>], s: &str) -> Vec<String> {
        g.iter()
            .map(|c| match c {
                Some((a, b)) => s[*a..*b].to_string(),
                None => "~".to_string(),
            })
            .collect()
    }

    #[test]
    fn alternation_is_leftmost_longest() {
        // The longer alternative wins even when an earlier one also matches.
        assert_eq!(m("(a|ab)", "ab"), Some(vec!["ab".into()]));
        assert_eq!(m("(a)|(ab)", "ab"), Some(vec!["~".into(), "ab".into()]));
        assert_eq!(
            m("(a|ab)(c|bcd)", "abcd"),
            Some(vec!["a".into(), "bcd".into()])
        );
        // ... and in split, where the match length shows.
        assert_eq!(sp("x*|ab", "ab"), vec!["", "", "", "", ""]);
    }

    #[test]
    fn a_starred_group_runs_one_empty_iteration() {
        assert_eq!(m("(a*)*", "aa"), Some(vec!["".into()]));
        assert_eq!(m("(a*)+", "aa"), Some(vec!["".into()]));
        assert_eq!(m("(a|)*", "aa"), Some(vec!["".into()]));
        assert_eq!(m("(a*)*", ""), Some(vec!["".into()]));
    }

    #[test]
    fn anchors_bind_to_the_whole_input() {
        assert_eq!(m("^a$", "a"), Some(vec![]));
        assert_eq!(m("a^b", "ab"), None);
        assert_eq!(m("a$b", "ab"), None);
        // `^` cannot match again once the search has moved on (match_prev_avail).
        assert_eq!(sp("^a", "aa"), vec!["", "", "a"]);
    }

    #[test]
    fn empty_matches_step_one_byte() {
        assert_eq!(sp("x*", "ab"), vec!["", "", "a", "", "b", "", ""]);
        assert_eq!(
            sp("b*", "abbbc"),
            vec!["", "", "a", "", "", "", "c", "", ""]
        );
    }

    #[test]
    fn intervals_clone_the_atom() {
        assert_eq!(m("(a){1,3}", "aaa"), Some(vec!["a".into()]));
        assert_eq!(m("a{2,3}", "aaaa"), None);
        assert_eq!(m("(ab|a){1,2}", "aba"), Some(vec!["a".into()]));
        // The state limit stands in for libstdc++'s error_space.
        assert!(compile(b"a{40000}").is_some());
        assert!(compile(b"a{99999}").is_none());
    }

    #[test]
    fn classes_are_ascii_and_bytes_are_bytes() {
        assert_eq!(m("[[:alpha:]]+", "az"), Some(vec![]));
        assert_eq!(m("[[:alpha:]]", "\u{e9}"), None);
        assert_eq!(m(".", "\n"), Some(vec![]));
        assert_eq!(m("[^a]", "\n"), Some(vec![]));
    }
}
