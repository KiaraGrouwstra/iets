//! `nix_api_expr.h`: the store handle, the state builder, the evaluator
//! state, the value cells it owns, and the calls that evaluate.

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::{CString, c_char, c_void};
use std::sync::Arc;

use ietsp::api::value::{Detached, Eval};
use ietsp::eval::{DEFAULT_SYSTEM, Evaluator};
use ietsp::session::Session;

use crate::ctx::*;

/// A store URI, remembered so `nix_store_get_uri` can answer. ietsp
/// resolves the store itself when it needs one (docs/c-api.md).
pub struct Store {
    uri: CString,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_store_open(
    context: *mut nix_c_context,
    uri: *const c_char,
    _params: *const *const *const c_char,
) -> *mut Store {
    clear(context);
    let uri = cstr(uri).unwrap_or(b"auto");
    let Ok(uri) = CString::new(uri) else {
        err(context, NIX_ERR_UNKNOWN, "store URI contains a NUL byte");
        return std::ptr::null_mut();
    };
    Box::into_raw(Box::new(Store { uri }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_store_free(store: *mut Store) {
    if !store.is_null() {
        // SAFETY: a store from nix_store_open that the caller has not freed.
        drop(unsafe { Box::from_raw(store) });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_store_get_uri(
    context: *mut nix_c_context,
    store: *mut Store,
    callback: nix_get_string_callback,
    user_data: *mut c_void,
) -> nix_err {
    clear(context);
    // SAFETY: a live store of the caller's.
    let Some(store) = (unsafe { store.as_ref() }) else {
        return err(context, NIX_ERR_UNKNOWN, "Store is null");
    };
    give_string(callback, store.uri.as_bytes(), user_data)
}

#[unsafe(no_mangle)]
pub extern "C" fn nix_store_get_storedir(
    context: *mut nix_c_context,
    _store: *mut Store,
    callback: nix_get_string_callback,
    user_data: *mut c_void,
) -> nix_err {
    clear(context);
    give_string(callback, b"/nix/store", user_data)
}

/// The settings of a state that is not built yet.
pub struct nix_eval_state_builder {
    lookup_path: Vec<Vec<u8>>,
    load: bool,
}

#[unsafe(no_mangle)]
pub extern "C" fn nix_eval_state_builder_new(
    context: *mut nix_c_context,
    _store: *mut Store,
) -> *mut nix_eval_state_builder {
    clear(context);
    Box::into_raw(Box::new(nix_eval_state_builder {
        lookup_path: Vec::new(),
        load: false,
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_eval_state_builder_free(builder: *mut nix_eval_state_builder) {
    if !builder.is_null() {
        // SAFETY: a builder from nix_eval_state_builder_new, freed once.
        drop(unsafe { Box::from_raw(builder) });
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_eval_state_builder_load(
    context: *mut nix_c_context,
    builder: *mut nix_eval_state_builder,
) -> nix_err {
    clear(context);
    // SAFETY: a live builder of the caller's.
    match unsafe { builder.as_mut() } {
        Some(b) => {
            b.load = true;
            NIX_OK
        }
        None => err(context, NIX_ERR_UNKNOWN, "Builder is null"),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_eval_state_builder_set_lookup_path(
    context: *mut nix_c_context,
    builder: *mut nix_eval_state_builder,
    lookup_path: *const *const c_char,
) -> nix_err {
    clear(context);
    // SAFETY: a live builder of the caller's.
    let Some(b) = (unsafe { builder.as_mut() }) else {
        return err(context, NIX_ERR_UNKNOWN, "Builder is null");
    };
    b.lookup_path.clear();
    if lookup_path.is_null() {
        return NIX_OK;
    }
    let mut i = 0;
    loop {
        // SAFETY: the header says the array is NULL-terminated, so reading up
        // to and including the null entry stays inside it.
        let entry = unsafe { *lookup_path.add(i) };
        let Some(bytes) = cstr(entry) else {
            return NIX_OK;
        };
        b.lookup_path.push(bytes.to_vec());
        i += 1;
    }
}

/// One evaluator, everything it needs, and everything handed to C that
/// must outlive a single call: the value cells and the strings returned as
/// `const char *`. All of it dies with `nix_state_free`.
pub struct EvalState {
    pub(crate) session: Arc<Session>,
    pub(crate) ev: Evaluator,
    cells: Vec<*mut nix_value>,
    strings: HashMap<Vec<u8>, CString>,
}

impl EvalState {
    /// A fresh cell, owned by this state.
    pub(crate) fn alloc(&mut self, v: Option<Detached>) -> *mut nix_value {
        let p = Box::into_raw(Box::new(nix_value {
            v: Cell::new(v),
            state: self,
        }));
        self.cells.push(p);
        p
    }

    /// `bytes` as a NUL-terminated string that lives until the state is
    /// freed, which is what the header promises for a name or a path.
    pub(crate) fn intern(&mut self, bytes: &[u8]) -> *const c_char {
        if let Some(s) = self.strings.get(bytes) {
            return s.as_ptr();
        }
        let Ok(s) = CString::new(bytes) else {
            return std::ptr::null();
        };
        self.strings.entry(bytes.to_vec()).or_insert(s).as_ptr()
    }
}

impl Drop for EvalState {
    fn drop(&mut self) {
        for p in self.cells.drain(..) {
            // SAFETY: every cell came from EvalState::alloc, is owned here,
            // and is dropped once.
            drop(unsafe { Box::from_raw(p) });
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_eval_state_build(
    context: *mut nix_c_context,
    builder: *mut nix_eval_state_builder,
) -> *mut EvalState {
    clear(context);
    // SAFETY: a live builder of the caller's, which it frees afterwards.
    let Some(b) = (unsafe { builder.as_ref() }) else {
        err(context, NIX_ERR_UNKNOWN, "Builder is null");
        return std::ptr::null_mut();
    };
    let session = Session::new();
    if b.load
        && let Err(e) = session.load_settings(&[])
    {
        err(context, NIX_ERR_UNKNOWN, &e.to_string());
        return std::ptr::null_mut();
    }
    let entered = session.enter();
    let mut ev = Evaluator::in_session(session.clone(), DEFAULT_SYSTEM, false);
    if !b.lookup_path.is_empty() {
        let includes: Vec<_> = b
            .lookup_path
            .iter()
            .map(|e| ietsp::api::parse_include(&String::from_utf8_lossy(e)))
            .collect();
        ietsp::builtins::set_search_path(&mut ev, includes);
    }
    drop(entered);
    Box::into_raw(Box::new(EvalState {
        session,
        ev,
        cells: Vec::new(),
        strings: HashMap::new(),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_state_create(
    context: *mut nix_c_context,
    lookup_path: *const *const c_char,
    store: *mut Store,
) -> *mut EvalState {
    let builder = nix_eval_state_builder_new(context, store);
    if builder.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the builder was just created here and is freed below.
    let state = unsafe {
        if nix_eval_state_builder_load(context, builder) != NIX_OK
            || nix_eval_state_builder_set_lookup_path(context, builder, lookup_path) != NIX_OK
        {
            std::ptr::null_mut()
        } else {
            nix_eval_state_build(context, builder)
        }
    };
    // SAFETY: as above.
    unsafe { nix_eval_state_builder_free(builder) };
    state
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_state_free(state: *mut EvalState) {
    if !state.is_null() {
        // SAFETY: a state from nix_eval_state_build, freed once. Every value
        // the caller still holds becomes invalid here (docs/c-api.md).
        drop(unsafe { Box::from_raw(state) });
    }
}

/// One value handed to C. It holds the evaluator's word, so forcing a
/// value the caller already has is visible to the caller, and a pointer
/// back to the state, for the initializers the header gives no state.
#[repr(C)]
pub struct nix_value {
    v: Cell<Option<Detached>>,
    state: *mut EvalState,
}

pub(crate) fn state_of<'a>(
    context: *mut nix_c_context,
    state: *mut EvalState,
) -> Result<&'a mut EvalState, nix_err> {
    // SAFETY: a live state of the caller's; the header forbids using one
    // state from two calls at the same time.
    unsafe { state.as_mut() }.ok_or_else(|| err(context, NIX_ERR_UNKNOWN, "EvalState is null"))
}

/// The state that allocated `value`, for the calls the header gives none.
pub(crate) fn owner_of<'a>(
    context: *mut nix_c_context,
    value: *const nix_value,
) -> Result<&'a mut EvalState, nix_err> {
    // SAFETY: a live cell of the caller's, from nix_alloc_value or a getter.
    let cell = unsafe { value.as_ref() }
        .ok_or_else(|| err(context, NIX_ERR_UNKNOWN, "nix_value is null"))?;
    state_of(context, cell.state)
}

/// The value in a cell. An unset cell is the header's "Uninitialized
/// nix_value".
pub(crate) fn read(
    context: *mut nix_c_context,
    value: *const nix_value,
) -> Result<Detached, nix_err> {
    // SAFETY: as in owner_of.
    let cell = unsafe { value.as_ref() }
        .ok_or_else(|| err(context, NIX_ERR_UNKNOWN, "nix_value is null"))?;
    cell.v
        .get()
        .ok_or_else(|| err(context, NIX_ERR_UNKNOWN, "Uninitialized nix_value"))
}

pub(crate) fn write(context: *mut nix_c_context, value: *mut nix_value, v: Detached) -> nix_err {
    // SAFETY: as in owner_of.
    match unsafe { value.as_ref() } {
        Some(cell) => {
            cell.v.set(Some(v));
            NIX_OK
        }
        None => err(context, NIX_ERR_UNKNOWN, "nix_value is null"),
    }
}

/// The out parameter of an initializer: a cell that holds nothing yet.
/// Nix values are immutable, so a second initialization is an error.
pub(crate) fn write_once(
    context: *mut nix_c_context,
    value: *mut nix_value,
    v: Detached,
) -> nix_err {
    // SAFETY: as in owner_of.
    match unsafe { value.as_ref() } {
        Some(cell) if cell.v.get().is_some() => err(
            context,
            NIX_ERR_UNKNOWN,
            "nix_value already initialized. Variables are immutable",
        ),
        Some(cell) => {
            cell.v.set(Some(v));
            NIX_OK
        }
        None => err(context, NIX_ERR_UNKNOWN, "nix_value is null"),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_alloc_value(
    context: *mut nix_c_context,
    state: *mut EvalState,
) -> *mut nix_value {
    clear(context);
    match state_of(context, state) {
        Ok(st) => st.alloc(None),
        Err(_) => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_expr_eval_from_string(
    context: *mut nix_c_context,
    state: *mut EvalState,
    expr: *const c_char,
    path: *const c_char,
    value: *mut nix_value,
) -> nix_err {
    clear(context);
    let Some(src) = cstr(expr) else {
        return err(context, NIX_ERR_UNKNOWN, "expression is null");
    };
    let base = cstr(path).unwrap_or(b"/").to_vec();
    let st = match state_of(context, state) {
        Ok(st) => st,
        Err(e) => return e,
    };
    let src = String::from_utf8_lossy(src).into_owned();
    let _entered = st.session.enter();
    let mut e = Eval::new(&mut st.ev);
    match e.eval_expr(&src, &base).and_then(|v| e.force(v)) {
        Ok(v) => write(context, value, v.detach()),
        Err(er) => eval_error(context, e.evaluator(), &er),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_value_force(
    context: *mut nix_c_context,
    state: *mut EvalState,
    value: *mut nix_value,
) -> nix_err {
    clear(context);
    let d = match read(context, value) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let st = match state_of(context, state) {
        Ok(st) => st,
        Err(e) => return e,
    };
    let _entered = st.session.enter();
    let mut e = Eval::new(&mut st.ev);
    // SAFETY: `d` was read from a cell of this state, whose evaluator lives
    // until nix_state_free.
    let v = unsafe { e.attach(d) };
    match e.force(v) {
        Ok(v) => write(context, value, v.detach()),
        Err(er) => eval_error(context, e.evaluator(), &er),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_value_force_deep(
    context: *mut nix_c_context,
    state: *mut EvalState,
    value: *mut nix_value,
) -> nix_err {
    clear(context);
    let d = match read(context, value) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let st = match state_of(context, state) {
        Ok(st) => st,
        Err(e) => return e,
    };
    let _entered = st.session.enter();
    let mut e = Eval::new(&mut st.ev);
    // SAFETY: as in nix_value_force.
    let v = unsafe { e.attach(d) };
    match e.force_deep(v) {
        Ok(v) => write(context, value, v.detach()),
        Err(er) => eval_error(context, e.evaluator(), &er),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_value_call(
    context: *mut nix_c_context,
    state: *mut EvalState,
    fn_: *mut nix_value,
    arg: *mut nix_value,
    value: *mut nix_value,
) -> nix_err {
    let mut one = [arg];
    // SAFETY: one argument is the multi-argument call with nargs = 1, and
    // `one` holds exactly that one pointer.
    unsafe { nix_value_call_multi(context, state, fn_, 1, one.as_mut_ptr(), value) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_value_call_multi(
    context: *mut nix_c_context,
    state: *mut EvalState,
    fn_: *mut nix_value,
    nargs: usize,
    args: *mut *mut nix_value,
    value: *mut nix_value,
) -> nix_err {
    clear(context);
    let mut ds = match read(context, fn_) {
        Ok(d) => vec![d],
        Err(e) => return e,
    };
    for i in 0..nargs {
        // SAFETY: the header says `args` holds `nargs` value pointers.
        let arg = unsafe { *args.add(i) };
        match read(context, arg) {
            Ok(d) => ds.push(d),
            Err(e) => return e,
        }
    }
    let st = match state_of(context, state) {
        Ok(st) => st,
        Err(e) => return e,
    };
    let _entered = st.session.enter();
    let mut e = Eval::new(&mut st.ev);
    // SAFETY: as in nix_value_force.
    let mut f = unsafe { e.attach(ds[0]) };
    for d in &ds[1..] {
        // SAFETY: as above.
        let arg = unsafe { e.attach(*d) };
        f = match e.call(f, arg).and_then(|r| e.force(r)) {
            Ok(r) => r,
            Err(er) => return eval_error(context, e.evaluator(), &er),
        };
    }
    write(context, value, f.detach())
}

/// ietsp's arena lives as long as the evaluator and is never collected, so
/// reference counting has nothing to do: a value stays alive while the
/// caller holds it, and until `nix_state_free` after that (docs/c-api.md).
#[unsafe(no_mangle)]
pub extern "C" fn nix_value_incref(context: *mut nix_c_context, _value: *mut nix_value) -> nix_err {
    clear(context);
    NIX_OK
}

#[unsafe(no_mangle)]
pub extern "C" fn nix_value_decref(context: *mut nix_c_context, _value: *mut nix_value) -> nix_err {
    clear(context);
    NIX_OK
}

#[unsafe(no_mangle)]
pub extern "C" fn nix_gc_incref(context: *mut nix_c_context, _object: *const c_void) -> nix_err {
    clear(context);
    NIX_OK
}

#[unsafe(no_mangle)]
pub extern "C" fn nix_gc_decref(context: *mut nix_c_context, _object: *const c_void) -> nix_err {
    clear(context);
    NIX_OK
}

#[unsafe(no_mangle)]
pub extern "C" fn nix_gc_now() {}

/// There is no collector to run the finalizer, so it never runs
/// (docs/c-api.md).
#[unsafe(no_mangle)]
pub extern "C" fn nix_gc_register_finalizer(
    _obj: *mut c_void,
    _cd: *mut c_void,
    _finalizer: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
) {
}
