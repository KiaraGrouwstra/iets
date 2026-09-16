//! List primops.

use super::Registry;
use crate::eval::{ErrorKind, EvalResult, Evaluator, Pos, bail};
use crate::runtime::*;

pub fn register(r: &mut Registry) {
    r.op("__head", 1, head);
    r.op("__tail", 1, tail);
    r.op("__length", 1, length);
    r.op("__elem", 2, elem);
    r.op("__elemAt", 2, elem_at);
    r.op("map", 2, map);
    r.op("__filter", 2, filter);
    r.op("__foldl'", 3, foldl_strict);
    r.op("__concatLists", 1, concat_lists);
    r.op("__concatMap", 2, concat_map);
    r.op("__genList", 2, gen_list);
    r.op("__any", 2, any);
    r.op("__all", 2, all);
    r.op("__sort", 2, sort);
    r.op("__partition", 2, partition);
}

fn head(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let l = ev.force_list(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.head",
    )?;
    if l.len == 0 {
        bail!(
            ErrorKind::Eval,
            pos,
            "'builtins.head' called on an empty list"
        );
    }
    ev.force_cell(&l.cells()[0])
}

fn tail(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let l = ev.force_list(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.tail",
    )?;
    if l.len == 0 {
        bail!(
            ErrorKind::Eval,
            pos,
            "'builtins.tail' called on an empty list"
        );
    }
    let items: Vec<Word> = l.cells()[1..].iter().map(|c| c.get()).collect();
    Ok(ev.mk_list(&items))
}

fn length(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let l = ev.force_list(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.length",
    )?;
    Ok(Word::imm_int(l.len as i64))
}

fn elem(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let l = ev.force_list(
        args[1],
        pos,
        "while evaluating the second argument passed to builtins.elem",
    )?;
    for c in l.cells() {
        let v = c.get();
        if ev.eq_values(args[0], v, pos)? {
            return Ok(Word::TRUE);
        }
    }
    Ok(Word::FALSE)
}

fn elem_at(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let l = ev.force_list(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.elemAt",
    )?;
    let n = ev.force_int(
        args[1],
        pos,
        "while evaluating the second argument passed to builtins.elemAt",
    )?;
    if n < 0 || n as usize >= l.len as usize {
        bail!(
            ErrorKind::Eval,
            pos,
            "'builtins.elemAt' called with index {} on a list of size {}",
            n,
            l.len
        );
    }
    ev.force_cell(&l.cells()[n as usize])
}

fn map(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let l = ev.force_list(
        args[1],
        pos,
        "while evaluating the second argument passed to builtins.map",
    )?;
    if l.len == 0 {
        return Ok(Word::from_ptr(l));
    }
    let f = ev.force_function(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.map",
    )?;
    let out = ev.arena.alloc_list(l.len as usize);
    for (o, c) in out.cells().iter().zip(l.cells()) {
        let t = ev.arena.mk_thunk_app(f, c.get());
        o.set(t);
    }
    Ok(Word::from_ptr(out))
}

fn filter(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let l = ev.force_list(
        args[1],
        pos,
        "while evaluating the second argument passed to builtins.filter",
    )?;
    if l.len == 0 {
        return Ok(Word::from_ptr(l));
    }
    let f = ev.force_function(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.filter",
    )?;
    let mut out = Vec::with_capacity(l.len as usize);
    let mut same = true;
    for c in l.cells() {
        let v = c.get();
        let r = ev.call1(f, v, pos)?;
        if ev.force_bool(
            r,
            pos,
            "while evaluating the return value of the filtering function passed to builtins.filter",
        )? {
            out.push(v);
        } else {
            same = false;
        }
    }
    if same {
        return Ok(Word::from_ptr(l));
    }
    Ok(ev.mk_list(&out))
}

fn foldl_strict(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let f = ev.force_function(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.foldlStrict",
    )?;
    let l = ev.force_list(
        args[2],
        pos,
        "while evaluating the third argument passed to builtins.foldlStrict",
    )?;
    let mut acc = args[1];
    for c in l.cells() {
        acc = ev.call2(f, acc, c.get(), pos)?;
    }
    ev.force(acc)
}

fn concat_lists(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let l = ev.force_list(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.concatLists",
    )?;
    let mut lists = Vec::with_capacity(l.len as usize);
    for c in l.cells() {
        let v = c.get();
        lists.push(ev.force_list(
            v,
            pos,
            "while evaluating a value of the list passed to builtins.concatLists",
        )?);
    }
    Ok(ev.concat_lists(&lists))
}

fn concat_map(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let f = ev.force_function(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.concatMap",
    )?;
    let l = ev.force_list(
        args[1],
        pos,
        "while evaluating the second argument passed to builtins.concatMap",
    )?;
    let mut lists = Vec::with_capacity(l.len as usize);
    for c in l.cells() {
        let r = ev.call1(f, c.get(), pos)?;
        lists.push(ev.force_list(
            r,
            pos,
            "while evaluating the return value of the function passed to builtins.concatMap",
        )?);
    }
    Ok(ev.concat_lists(&lists))
}

fn gen_list(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let n = ev.force_int(
        args[1],
        pos,
        "while evaluating the second argument passed to builtins.genList",
    )?;
    if n < 0 {
        bail!(ErrorKind::Eval, pos, "cannot create list of size {}", n);
    }
    let f = ev.force_function(
        args[0],
        Pos::NONE,
        "while evaluating the first argument passed to builtins.genList",
    )?;
    let out = ev.arena.alloc_list(n as usize);
    for (i, c) in out.cells().iter().enumerate() {
        let t = ev.arena.mk_thunk_app(f, Word::imm_int(i as i64));
        c.set(t);
    }
    Ok(Word::from_ptr(out))
}

fn any(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let f = ev.force_function(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.any",
    )?;
    let l = ev.force_list(
        args[1],
        pos,
        "while evaluating the second argument passed to builtins.any",
    )?;
    for c in l.cells() {
        let r = ev.call1(f, c.get(), pos)?;
        if ev.force_bool(
            r,
            pos,
            "while evaluating the return value of the function passed to builtins.any",
        )? {
            return Ok(Word::TRUE);
        }
    }
    Ok(Word::FALSE)
}

fn all(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let f = ev.force_function(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.all",
    )?;
    let l = ev.force_list(
        args[1],
        pos,
        "while evaluating the second argument passed to builtins.all",
    )?;
    for c in l.cells() {
        let r = ev.call1(f, c.get(), pos)?;
        if !ev.force_bool(
            r,
            pos,
            "while evaluating the return value of the function passed to builtins.all",
        )? {
            return Ok(Word::FALSE);
        }
    }
    Ok(Word::TRUE)
}

/// Stable merge sort that tolerates inconsistent comparators.
fn sort(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let l = ev.force_list(
        args[1],
        pos,
        "while evaluating the second argument passed to builtins.sort",
    )?;
    if l.len == 0 {
        return Ok(Word::from_ptr(l));
    }
    let f = ev.force_function(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.sort",
    )?;
    let mut items: Vec<Word> = Vec::with_capacity(l.len as usize);
    for c in l.cells() {
        items.push(ev.force_cell(c)?);
    }
    let is_less_than = matches!(f.view(), Value::PrimOp(p) if p.name == "__lessThan");
    let mut less = |ev: &mut Evaluator, a: Word, b: Word| -> EvalResult<bool> {
        if is_less_than {
            return ev.less_than(a, b, pos);
        }
        let r = ev.call2(f, a, b, pos)?;
        ev.force_bool(
            r,
            pos,
            "while evaluating the return value of the sorting function passed to builtins.sort",
        )
    };
    let mut buf = items.clone();
    merge_sort(ev, &mut items, &mut buf, &mut less)?;
    Ok(ev.mk_list(&items))
}

fn merge_sort(
    ev: &mut Evaluator,
    v: &mut [Word],
    buf: &mut [Word],
    less: &mut impl FnMut(&mut Evaluator, Word, Word) -> EvalResult<bool>,
) -> EvalResult<()> {
    let n = v.len();
    if n <= 1 {
        return Ok(());
    }
    let mid = n / 2;
    {
        let (a, b) = v.split_at_mut(mid);
        let (ba, bb) = buf.split_at_mut(mid);
        merge_sort(ev, a, ba, less)?;
        merge_sort(ev, b, bb, less)?;
    }
    let (mut i, mut j, mut k) = (0, mid, 0);
    while i < mid && j < n {
        // take from the right only when strictly less: keeps the sort stable
        if less(ev, v[j], v[i])? {
            buf[k] = v[j];
            j += 1;
        } else {
            buf[k] = v[i];
            i += 1;
        }
        k += 1;
    }
    while i < mid {
        buf[k] = v[i];
        i += 1;
        k += 1;
    }
    while j < n {
        buf[k] = v[j];
        j += 1;
        k += 1;
    }
    v.copy_from_slice(&buf[..n]);
    Ok(())
}

fn partition(ev: &mut Evaluator, args: &[Word], pos: Pos) -> EvalResult<Word> {
    let f = ev.force_function(
        args[0],
        pos,
        "while evaluating the first argument passed to builtins.partition",
    )?;
    let l = ev.force_list(
        args[1],
        pos,
        "while evaluating the second argument passed to builtins.partition",
    )?;
    let mut right = Vec::new();
    let mut wrong = Vec::new();
    for c in l.cells() {
        let v = ev.force_cell(c)?;
        let r = ev.call1(f, v, pos)?;
        if ev.force_bool(r, pos, "while evaluating the return value of the partition function passed to builtins.partition")? {
            right.push(v);
        } else {
            wrong.push(v);
        }
    }
    let r = ev.mk_list(&right);
    let w = ev.mk_list(&wrong);
    let mut entries = vec![(ev.s.right, Pos::NONE, r), (ev.s.wrong, Pos::NONE, w)];
    ev.mk_attrs(&mut entries, pos)
}
