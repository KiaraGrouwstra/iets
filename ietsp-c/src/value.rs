//! `nix_api_value.h`: reading a value, building one, and the two builders.

use std::ffi::{c_char, c_int, c_uint, c_void};

use ietsp::api::value::{Detached, Eval, Value, ValueKind};

use crate::ctx::*;
use crate::state::*;

pub type ValueType = c_int;

pub const NIX_TYPE_THUNK: ValueType = 0;
pub const NIX_TYPE_INT: ValueType = 1;
pub const NIX_TYPE_FLOAT: ValueType = 2;
pub const NIX_TYPE_BOOL: ValueType = 3;
pub const NIX_TYPE_STRING: ValueType = 4;
pub const NIX_TYPE_PATH: ValueType = 5;
pub const NIX_TYPE_NULL: ValueType = 6;
pub const NIX_TYPE_ATTRS: ValueType = 7;
pub const NIX_TYPE_LIST: ValueType = 8;
pub const NIX_TYPE_FUNCTION: ValueType = 9;

fn type_of(k: ValueKind) -> ValueType {
    match k {
        ValueKind::Thunk => NIX_TYPE_THUNK,
        ValueKind::Int => NIX_TYPE_INT,
        ValueKind::Float => NIX_TYPE_FLOAT,
        ValueKind::Bool => NIX_TYPE_BOOL,
        ValueKind::String => NIX_TYPE_STRING,
        ValueKind::Path => NIX_TYPE_PATH,
        ValueKind::Null => NIX_TYPE_NULL,
        ValueKind::Attrs => NIX_TYPE_ATTRS,
        ValueKind::List => NIX_TYPE_LIST,
        ValueKind::Function => NIX_TYPE_FUNCTION,
    }
}

/// The words `nix::showType` uses, which is what `nix_get_typename` and
/// the evaluator's own type errors print.
fn show(k: ValueKind) -> &'static str {
    match k {
        ValueKind::Thunk => "a thunk",
        ValueKind::Int => "an integer",
        ValueKind::Float => "a float",
        ValueKind::Bool => "a Boolean",
        ValueKind::String => "a string",
        ValueKind::Path => "a path",
        ValueKind::Null => "null",
        ValueKind::Attrs => "a set",
        ValueKind::List => "a list",
        ValueKind::Function => "a function",
    }
}

/// A value read without an evaluator: every getter below that the header
/// gives no `EvalState` needs exactly this.
fn value_of<'e>(
    context: *mut nix_c_context,
    value: *const nix_value,
) -> Result<Value<'e>, nix_err> {
    let d = read(context, value)?;
    let st = owner_of(context, value)?;
    let e = Eval::new(&mut st.ev);
    // SAFETY: `d` was read from a cell that names `st` as its state, so it
    // is a value of that evaluator, which lives until nix_state_free.
    Ok(unsafe { e.attach(d) })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_get_type(
    context: *mut nix_c_context,
    value: *const nix_value,
) -> ValueType {
    clear(context);
    match value_of(context, value) {
        Ok(v) => type_of(v.kind()),
        Err(_) => NIX_TYPE_NULL,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_get_typename(
    context: *mut nix_c_context,
    value: *const nix_value,
) -> *const c_char {
    clear(context);
    let Ok(v) = value_of(context, value) else {
        return std::ptr::null();
    };
    if v.kind() == ValueKind::String && v.context().len() > 0 {
        return malloc_str("a string with context");
    }
    malloc_str(show(v.kind()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_get_bool(
    context: *mut nix_c_context,
    value: *const nix_value,
) -> bool {
    clear(context);
    let Ok(v) = value_of(context, value) else {
        return false;
    };
    match v.as_bool() {
        Some(b) => b,
        None => {
            type_error(context, "a Boolean", show(v.kind()));
            false
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_get_int(context: *mut nix_c_context, value: *const nix_value) -> i64 {
    clear(context);
    let Ok(v) = value_of(context, value) else {
        return 0;
    };
    match v.as_int() {
        Some(i) => i,
        None => {
            type_error(context, "an integer", show(v.kind()));
            0
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_get_float(
    context: *mut nix_c_context,
    value: *const nix_value,
) -> f64 {
    clear(context);
    let Ok(v) = value_of(context, value) else {
        return 0.0;
    };
    // an integer is not a float here: `as_float` widens, the header does not
    match (v.kind(), v.as_float()) {
        (ValueKind::Float, Some(f)) => f,
        _ => {
            type_error(context, "a float", show(v.kind()));
            0.0
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_get_string(
    context: *mut nix_c_context,
    value: *const nix_value,
    callback: nix_get_string_callback,
    user_data: *mut c_void,
) -> nix_err {
    clear(context);
    let v = match value_of(context, value) {
        Ok(v) => v,
        Err(e) => return e,
    };
    match v.as_bytes() {
        Some(b) => give_string(callback, b, user_data),
        None => type_error(context, "a string", show(v.kind())),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_get_path_string(
    context: *mut nix_c_context,
    value: *const nix_value,
) -> *const c_char {
    clear(context);
    let Ok(v) = value_of(context, value) else {
        return std::ptr::null();
    };
    let Some(p) = v.as_path() else {
        type_error(context, "a path", show(v.kind()));
        return std::ptr::null();
    };
    let p = p.to_vec();
    match owner_of(context, value) {
        Ok(st) => st.intern(&p),
        Err(_) => std::ptr::null(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_get_list_size(
    context: *mut nix_c_context,
    value: *const nix_value,
) -> c_uint {
    clear(context);
    let Ok(v) = value_of(context, value) else {
        return 0;
    };
    match v.list_len() {
        Some(n) => n as c_uint,
        None => {
            type_error(context, "a list", show(v.kind()));
            0
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_get_attrs_size(
    context: *mut nix_c_context,
    value: *const nix_value,
) -> c_uint {
    clear(context);
    let Ok(v) = value_of(context, value) else {
        return 0;
    };
    match v.attrs_len() {
        Some(n) => n as c_uint,
        None => {
            type_error(context, "a set", show(v.kind()));
            0
        }
    }
}

fn list_element(
    context: *mut nix_c_context,
    value: *const nix_value,
    state: *mut EvalState,
    ix: c_uint,
    force: bool,
) -> *mut nix_value {
    clear(context);
    let d = match read(context, value) {
        Ok(d) => d,
        Err(_) => return std::ptr::null_mut(),
    };
    let st = match state_of(context, state) {
        Ok(st) => st,
        Err(_) => return std::ptr::null_mut(),
    };
    let out = {
        let _entered = st.session.enter();
        let mut e = Eval::new(&mut st.ev);
        // SAFETY: `d` was read from a cell of this state; its evaluator lives
        // until nix_state_free.
        let v = unsafe { e.attach(d) };
        let Some(n) = v.list_len() else {
            return {
                type_error(context, "a list", show(v.kind()));
                std::ptr::null_mut()
            };
        };
        if ix as usize >= n {
            err(context, NIX_ERR_KEY, "list index out of bounds");
            return std::ptr::null_mut();
        }
        let el = v.list_get(ix as usize).expect("in range");
        if !force {
            el.detach()
        } else {
            match e.force(el) {
                Ok(el) => el.detach(),
                Err(er) => {
                    eval_error(context, e.evaluator(), &er);
                    return std::ptr::null_mut();
                }
            }
        }
    };
    st.alloc(Some(out))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_get_list_byidx(
    context: *mut nix_c_context,
    value: *const nix_value,
    state: *mut EvalState,
    ix: c_uint,
) -> *mut nix_value {
    list_element(context, value, state, ix, true)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_get_list_byidx_lazy(
    context: *mut nix_c_context,
    value: *const nix_value,
    state: *mut EvalState,
    ix: c_uint,
) -> *mut nix_value {
    list_element(context, value, state, ix, false)
}

fn attr_by_name(
    context: *mut nix_c_context,
    value: *const nix_value,
    state: *mut EvalState,
    name: *const c_char,
    force: bool,
) -> *mut nix_value {
    clear(context);
    let Some(name) = cstr(name) else {
        err(context, NIX_ERR_UNKNOWN, "attribute name is null");
        return std::ptr::null_mut();
    };
    let name = name.to_vec();
    let d = match read(context, value) {
        Ok(d) => d,
        Err(_) => return std::ptr::null_mut(),
    };
    let st = match state_of(context, state) {
        Ok(st) => st,
        Err(_) => return std::ptr::null_mut(),
    };
    let out = {
        let _entered = st.session.enter();
        let mut e = Eval::new(&mut st.ev);
        // SAFETY: as in list_element.
        let v = unsafe { e.attach(d) };
        match e.attr(v, &name) {
            Err(er) => {
                eval_error(context, e.evaluator(), &er);
                return std::ptr::null_mut();
            }
            Ok(None) => {
                err(context, NIX_ERR_KEY, "missing attribute");
                return std::ptr::null_mut();
            }
            Ok(Some(a)) if !force => a.detach(),
            Ok(Some(a)) => match e.force(a) {
                Ok(a) => a.detach(),
                Err(er) => {
                    eval_error(context, e.evaluator(), &er);
                    return std::ptr::null_mut();
                }
            },
        }
    };
    st.alloc(Some(out))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_get_attr_byname(
    context: *mut nix_c_context,
    value: *const nix_value,
    state: *mut EvalState,
    name: *const c_char,
) -> *mut nix_value {
    attr_by_name(context, value, state, name, true)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_get_attr_byname_lazy(
    context: *mut nix_c_context,
    value: *const nix_value,
    state: *mut EvalState,
    name: *const c_char,
) -> *mut nix_value {
    attr_by_name(context, value, state, name, false)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_has_attr_byname(
    context: *mut nix_c_context,
    value: *const nix_value,
    state: *mut EvalState,
    name: *const c_char,
) -> bool {
    clear(context);
    let Some(name) = cstr(name) else {
        err(context, NIX_ERR_UNKNOWN, "attribute name is null");
        return false;
    };
    let name = name.to_vec();
    let Ok(d) = read(context, value) else {
        return false;
    };
    let Ok(st) = state_of(context, state) else {
        return false;
    };
    let _entered = st.session.enter();
    let mut e = Eval::new(&mut st.ev);
    // SAFETY: as in list_element.
    let v = unsafe { e.attach(d) };
    match e.has_attr(v, &name) {
        Ok(b) => b,
        Err(er) => {
            eval_error(context, e.evaluator(), &er);
            false
        }
    }
}

/// The name and the value of attribute `i`, or `None` after the error is
/// already in the context.
fn attr_at(
    context: *mut nix_c_context,
    value: *const nix_value,
    st: &mut EvalState,
    i: c_uint,
    force: bool,
) -> Option<(Vec<u8>, Detached)> {
    let d = read(context, value).ok()?;
    let _entered = st.session.enter();
    let mut e = Eval::new(&mut st.ev);
    // SAFETY: as in list_element.
    let v = unsafe { e.attach(d) };
    match e.attr_at(v, i as usize) {
        Err(er) => {
            eval_error(context, e.evaluator(), &er);
            None
        }
        Ok(None) => {
            err(context, NIX_ERR_KEY, "attribute index out of bounds");
            None
        }
        Ok(Some((name, a))) => {
            let name = name.to_vec();
            if !force {
                return Some((name, a.detach()));
            }
            match e.force(a) {
                Ok(a) => Some((name, a.detach())),
                Err(er) => {
                    eval_error(context, e.evaluator(), &er);
                    None
                }
            }
        }
    }
}

fn attr_by_idx(
    context: *mut nix_c_context,
    value: *mut nix_value,
    state: *mut EvalState,
    i: c_uint,
    name: *mut *const c_char,
    force: bool,
) -> *mut nix_value {
    clear(context);
    let st = match state_of(context, state) {
        Ok(st) => st,
        Err(_) => return std::ptr::null_mut(),
    };
    let Some((n, d)) = attr_at(context, value, st, i, force) else {
        return std::ptr::null_mut();
    };
    if !name.is_null() {
        let p = st.intern(&n);
        // SAFETY: the header says `name` is an out pointer to one `const char *`.
        unsafe { *name = p };
    }
    st.alloc(Some(d))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_get_attr_byidx(
    context: *mut nix_c_context,
    value: *mut nix_value,
    state: *mut EvalState,
    i: c_uint,
    name: *mut *const c_char,
) -> *mut nix_value {
    attr_by_idx(context, value, state, i, name, true)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_get_attr_byidx_lazy(
    context: *mut nix_c_context,
    value: *mut nix_value,
    state: *mut EvalState,
    i: c_uint,
    name: *mut *const c_char,
) -> *mut nix_value {
    attr_by_idx(context, value, state, i, name, false)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_get_attr_name_byidx(
    context: *mut nix_c_context,
    value: *mut nix_value,
    state: *mut EvalState,
    i: c_uint,
) -> *const c_char {
    clear(context);
    let st = match state_of(context, state) {
        Ok(st) => st,
        Err(_) => return std::ptr::null(),
    };
    match attr_at(context, value, st, i, false) {
        Some((n, _)) => st.intern(&n),
        None => std::ptr::null(),
    }
}

// ---- initializers -------------------------------------------------------

/// The shape every initializer shares: the state that owns the out value,
/// an `Eval` on it, and the write-once rule of the header.
fn init(
    context: *mut nix_c_context,
    value: *mut nix_value,
    f: impl FnOnce(&mut Eval<'_>) -> Result<Detached, nix_err>,
) -> nix_err {
    clear(context);
    let st = match owner_of(context, value) {
        Ok(st) => st,
        Err(e) => return e,
    };
    let d = {
        let _entered = st.session.enter();
        let mut e = Eval::new(&mut st.ev);
        match f(&mut e) {
            Ok(d) => d,
            Err(e) => return e,
        }
    };
    write_once(context, value, d)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_init_bool(
    context: *mut nix_c_context,
    value: *mut nix_value,
    b: bool,
) -> nix_err {
    init(context, value, |e| Ok(e.boolean(b).detach()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_init_int(
    context: *mut nix_c_context,
    value: *mut nix_value,
    i: i64,
) -> nix_err {
    init(context, value, |e| Ok(e.int(i).detach()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_init_float(
    context: *mut nix_c_context,
    value: *mut nix_value,
    d: f64,
) -> nix_err {
    init(context, value, |e| Ok(e.float(d).detach()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_init_null(
    context: *mut nix_c_context,
    value: *mut nix_value,
) -> nix_err {
    init(context, value, |e| Ok(e.null().detach()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_init_string(
    context: *mut nix_c_context,
    value: *mut nix_value,
    str_: *const c_char,
) -> nix_err {
    let Some(s) = cstr(str_) else {
        clear(context);
        return err(context, NIX_ERR_UNKNOWN, "string is null");
    };
    let s = s.to_vec();
    init(context, value, |e| Ok(e.string(&s).detach()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_init_path_string(
    context: *mut nix_c_context,
    _state: *mut EvalState,
    value: *mut nix_value,
    str_: *const c_char,
) -> nix_err {
    let Some(s) = cstr(str_) else {
        clear(context);
        return err(context, NIX_ERR_UNKNOWN, "path is null");
    };
    let s = s.to_vec();
    init(context, value, |e| Ok(e.path(&s).detach()))
}

/// The header takes the value as `check_value_not_null`, not as an
/// uninitialized out value, so this one may overwrite.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_init_apply(
    context: *mut nix_c_context,
    value: *mut nix_value,
    fn_: *mut nix_value,
    arg: *mut nix_value,
) -> nix_err {
    clear(context);
    let f = match read(context, fn_) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let a = match read(context, arg) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let st = match owner_of(context, value) {
        Ok(st) => st,
        Err(e) => return e,
    };
    let d = {
        let _entered = st.session.enter();
        let mut e = Eval::new(&mut st.ev);
        // SAFETY: both come from cells of this state, whose evaluator lives
        // until nix_state_free.
        let (f, a) = unsafe { (e.attach(f), e.attach(a)) };
        e.apply(f, a).detach()
    };
    write(context, value, d)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_copy_value(
    context: *mut nix_c_context,
    value: *mut nix_value,
    source: *const nix_value,
) -> nix_err {
    clear(context);
    match read(context, source) {
        Ok(d) => write_once(context, value, d),
        Err(e) => e,
    }
}

// ---- builders -----------------------------------------------------------

/// A list under construction. `nix_list_builder_insert` addresses it by
/// index, so the slots exist from the start.
pub struct ListBuilder {
    state: *mut EvalState,
    items: Vec<Option<Detached>>,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_make_list_builder(
    context: *mut nix_c_context,
    state: *mut EvalState,
    capacity: usize,
) -> *mut ListBuilder {
    clear(context);
    if state_of(context, state).is_err() {
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(ListBuilder {
        state,
        items: vec![None; capacity],
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_list_builder_insert(
    context: *mut nix_c_context,
    list_builder: *mut ListBuilder,
    index: c_uint,
    value: *mut nix_value,
) -> nix_err {
    clear(context);
    let d = match read(context, value) {
        Ok(d) => d,
        Err(e) => return e,
    };
    // SAFETY: a live builder of the caller's, not yet used by nix_make_list.
    let Some(b) = (unsafe { list_builder.as_mut() }) else {
        return err(context, NIX_ERR_UNKNOWN, "ListBuilder is null");
    };
    match b.items.get_mut(index as usize) {
        Some(slot) => {
            *slot = Some(d);
            NIX_OK
        }
        None => err(context, NIX_ERR_KEY, "list index out of bounds"),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_list_builder_free(list_builder: *mut ListBuilder) {
    if !list_builder.is_null() {
        // SAFETY: a builder from nix_make_list_builder, freed once.
        drop(unsafe { Box::from_raw(list_builder) });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_make_list(
    context: *mut nix_c_context,
    list_builder: *mut ListBuilder,
    value: *mut nix_value,
) -> nix_err {
    clear(context);
    // SAFETY: a live builder of the caller's.
    let Some(b) = (unsafe { list_builder.as_mut() }) else {
        return err(context, NIX_ERR_UNKNOWN, "ListBuilder is null");
    };
    let items = std::mem::take(&mut b.items);
    if items.iter().any(Option::is_none) {
        return err(context, NIX_ERR_UNKNOWN, "ListBuilder has an unset element");
    }
    let st = match state_of(context, b.state) {
        Ok(st) => st,
        Err(e) => return e,
    };
    let d = {
        let _entered = st.session.enter();
        let mut e = Eval::new(&mut st.ev);
        let items: Vec<_> = items
            .iter()
            // SAFETY: every element came from a cell of this state.
            .map(|d| unsafe { e.attach(d.expect("checked above")) })
            .collect();
        e.list(&items).detach()
    };
    write_once(context, value, d)
}

/// An attribute set under construction, in insertion order.
pub struct BindingsBuilder {
    state: *mut EvalState,
    entries: Vec<(Vec<u8>, Detached)>,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_make_bindings_builder(
    context: *mut nix_c_context,
    state: *mut EvalState,
    capacity: usize,
) -> *mut BindingsBuilder {
    clear(context);
    if state_of(context, state).is_err() {
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(BindingsBuilder {
        state,
        entries: Vec::with_capacity(capacity),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_bindings_builder_insert(
    context: *mut nix_c_context,
    builder: *mut BindingsBuilder,
    name: *const c_char,
    value: *mut nix_value,
) -> nix_err {
    clear(context);
    let Some(name) = cstr(name) else {
        return err(context, NIX_ERR_UNKNOWN, "attribute name is null");
    };
    let name = name.to_vec();
    let d = match read(context, value) {
        Ok(d) => d,
        Err(e) => return e,
    };
    // SAFETY: a live builder of the caller's, not yet used by nix_make_attrs.
    match unsafe { builder.as_mut() } {
        Some(b) => {
            b.entries.push((name, d));
            NIX_OK
        }
        None => err(context, NIX_ERR_UNKNOWN, "BindingsBuilder is null"),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_bindings_builder_free(builder: *mut BindingsBuilder) {
    if !builder.is_null() {
        // SAFETY: a builder from nix_make_bindings_builder, freed once.
        drop(unsafe { Box::from_raw(builder) });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_make_attrs(
    context: *mut nix_c_context,
    value: *mut nix_value,
    builder: *mut BindingsBuilder,
) -> nix_err {
    clear(context);
    // SAFETY: a live builder of the caller's.
    let Some(b) = (unsafe { builder.as_mut() }) else {
        return err(context, NIX_ERR_UNKNOWN, "BindingsBuilder is null");
    };
    let entries = std::mem::take(&mut b.entries);
    let st = match state_of(context, b.state) {
        Ok(st) => st,
        Err(e) => return e,
    };
    let d = {
        let _entered = st.session.enter();
        let mut e = Eval::new(&mut st.ev);
        let entries: Vec<(&[u8], _)> = entries
            .iter()
            // SAFETY: every value came from a cell of this state.
            .map(|(n, d)| (&n[..], unsafe { e.attach(*d) }))
            .collect();
        match e.attrs(&entries) {
            Ok(a) => a.detach(),
            Err(er) => return eval_error(context, e.evaluator(), &er),
        }
    };
    write_once(context, value, d)
}
