//! `nix_api_util.h`: the error context every call writes into, plus the
//! library-wide entry points that need no evaluator.

use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};

use ietsp::eval::{Error, Evaluator};

pub type nix_err = c_int;

pub const NIX_OK: nix_err = 0;
pub const NIX_ERR_UNKNOWN: nix_err = -1;
pub const NIX_ERR_KEY: nix_err = -3;
pub const NIX_ERR_NIX_ERROR: nix_err = -4;

const NIX_LVL_VOMIT: c_int = 7;

pub type nix_get_string_callback =
    Option<unsafe extern "C" fn(start: *const c_char, n: c_uint, user_data: *mut c_void)>;

/// The error state of one call chain. The code is the first field, so a
/// consumer may cast the pointer to `nix_err *`, as the header promises.
#[repr(C)]
pub struct nix_c_context {
    code: nix_err,
    msg: Option<CString>,
    info: Option<CString>,
    name: Option<CString>,
}

#[unsafe(no_mangle)]
pub extern "C" fn nix_c_context_create() -> *mut nix_c_context {
    Box::into_raw(Box::new(nix_c_context {
        code: NIX_OK,
        msg: None,
        info: None,
        name: None,
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_c_context_free(context: *mut nix_c_context) {
    if !context.is_null() {
        // SAFETY: the caller passes a context from nix_c_context_create that
        // it has not freed yet, and stops using it after this call.
        drop(unsafe { Box::from_raw(context) });
    }
}

fn borrow<'a>(context: *mut nix_c_context) -> Option<&'a mut nix_c_context> {
    // SAFETY: an entry point's `context` is null or a live context the caller
    // is not using from another thread at the same time (header: "make sure
    // not to use them for multiple calls simultaneously").
    unsafe { context.as_mut() }
}

/// What every entry point does first: only the code is reset, so an old
/// message stays readable until the next failure, as `nix_clear_err` does.
pub(crate) fn clear(context: *mut nix_c_context) {
    if let Some(c) = borrow(context) {
        c.code = NIX_OK;
    }
}

pub(crate) fn err(context: *mut nix_c_context, code: nix_err, msg: &str) -> nix_err {
    if let Some(c) = borrow(context) {
        c.code = code;
        c.msg = CString::new(msg).ok();
        c.info = None;
        c.name = None;
    }
    code
}

/// A failure from the evaluator itself. `nix_err_name` and
/// `nix_err_info_msg` only answer after this one.
pub(crate) fn nix_error(
    context: *mut nix_c_context,
    name: &str,
    text: &str,
    info: &str,
) -> nix_err {
    if let Some(c) = borrow(context) {
        c.code = NIX_ERR_NIX_ERROR;
        c.msg = CString::new(text).ok();
        c.info = CString::new(info).ok();
        c.name = CString::new(name).ok();
    }
    NIX_ERR_NIX_ERROR
}

/// An `ietsp` evaluation error, rendered the way the CLI renders it: the
/// full report as the message, the bare text as the info message, and the
/// stable error code of docs/cli.md as the name.
pub(crate) fn eval_error(context: *mut nix_c_context, ev: &Evaluator, e: &Error) -> nix_err {
    let r = ietsp::api::report_of(ev, e);
    nix_error(context, r.code, &r.text(), &r.raw)
}

/// A value of the wrong type where the header documents an assertion.
pub(crate) fn type_error(context: *mut nix_c_context, want: &str, got: &str) -> nix_err {
    nix_error(
        context,
        "type",
        &format!("error: expected {want} but found {got}"),
        &format!("expected {want} but found {got}"),
    )
}

pub(crate) fn give_string(
    callback: nix_get_string_callback,
    bytes: &[u8],
    user_data: *mut c_void,
) -> nix_err {
    if let Some(f) = callback {
        // SAFETY: the header says the callback borrows `start` for `n` bytes
        // and must not keep it; the slice outlives the call.
        unsafe { f(bytes.as_ptr().cast(), bytes.len() as c_uint, user_data) };
    }
    NIX_OK
}

/// A NUL-terminated copy of `s` on the C heap, for the calls whose header
/// says "free with free()".
pub(crate) fn malloc_str(s: &str) -> *const c_char {
    // SAFETY: the allocation is len + 1 bytes and exactly that many are
    // written; the caller frees it with free(), as the header says.
    unsafe {
        let p = libc::malloc(s.len() + 1) as *mut u8;
        if p.is_null() {
            return std::ptr::null();
        }
        std::ptr::copy_nonoverlapping(s.as_ptr(), p, s.len());
        *p.add(s.len()) = 0;
        p.cast()
    }
}

/// The bytes of a C string argument, or `None` when it is null.
pub(crate) fn cstr<'a>(p: *const c_char) -> Option<&'a [u8]> {
    if p.is_null() {
        return None;
    }
    // SAFETY: a non-null `const char *` argument is NUL-terminated and stays
    // valid for the call, as every one of these headers documents.
    Some(unsafe { CStr::from_ptr(p) }.to_bytes())
}

#[unsafe(no_mangle)]
pub extern "C" fn nix_libutil_init(context: *mut nix_c_context) -> nix_err {
    clear(context);
    NIX_OK
}

#[unsafe(no_mangle)]
pub extern "C" fn nix_libstore_init(context: *mut nix_c_context) -> nix_err {
    clear(context);
    NIX_OK
}

#[unsafe(no_mangle)]
pub extern "C" fn nix_libstore_init_no_load_config(context: *mut nix_c_context) -> nix_err {
    clear(context);
    NIX_OK
}

#[unsafe(no_mangle)]
pub extern "C" fn nix_libexpr_init(context: *mut nix_c_context) -> nix_err {
    clear(context);
    NIX_OK
}

/// The Nix version this ABI answers to, with the implementation behind it,
/// so a consumer that prints it sees which evaluator it loaded.
#[unsafe(no_mangle)]
pub extern "C" fn nix_version_get() -> *const c_char {
    c"2.34.8-ietsp".as_ptr()
}

/// ietsp keeps no global settings table; a state reads its own
/// configuration when it is built (docs/c-api.md).
#[unsafe(no_mangle)]
pub extern "C" fn nix_setting_get(
    context: *mut nix_c_context,
    _key: *const c_char,
    _callback: nix_get_string_callback,
    _user_data: *mut c_void,
) -> nix_err {
    clear(context);
    err(context, NIX_ERR_KEY, "Setting not found")
}

#[unsafe(no_mangle)]
pub extern "C" fn nix_setting_set(
    context: *mut nix_c_context,
    _key: *const c_char,
    _value: *const c_char,
) -> nix_err {
    clear(context);
    err(context, NIX_ERR_KEY, "Setting not found")
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_err_msg(
    context: *mut nix_c_context,
    read_context: *const nix_c_context,
    n: *mut c_uint,
) -> *const c_char {
    clear(context);
    // SAFETY: `read_context` is a live context of the caller's.
    let r = unsafe { read_context.as_ref() };
    if let Some(r) = r
        && r.code != NIX_OK
        && let Some(m) = &r.msg
    {
        if !n.is_null() {
            // SAFETY: the header says `n` is an out pointer to one unsigned int.
            unsafe { *n = m.as_bytes().len() as c_uint };
        }
        return m.as_ptr();
    }
    err(context, NIX_ERR_UNKNOWN, "No error message");
    std::ptr::null()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_err_info_msg(
    context: *mut nix_c_context,
    read_context: *const nix_c_context,
    callback: nix_get_string_callback,
    user_data: *mut c_void,
) -> nix_err {
    clear(context);
    // SAFETY: as in nix_err_msg.
    match unsafe { read_context.as_ref() } {
        Some(r) if r.code == NIX_ERR_NIX_ERROR => {
            let info = r.info.as_ref().map_or(&b""[..], |s| s.as_bytes());
            give_string(callback, info, user_data)
        }
        _ => err(context, NIX_ERR_UNKNOWN, "Last error was not a nix error"),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_err_name(
    context: *mut nix_c_context,
    read_context: *const nix_c_context,
    callback: nix_get_string_callback,
    user_data: *mut c_void,
) -> nix_err {
    clear(context);
    // SAFETY: as in nix_err_msg.
    match unsafe { read_context.as_ref() } {
        Some(r) if r.code == NIX_ERR_NIX_ERROR => {
            let name = r.name.as_ref().map_or(&b""[..], |s| s.as_bytes());
            give_string(callback, name, user_data)
        }
        _ => err(context, NIX_ERR_UNKNOWN, "Last error was not a nix error"),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_err_code(read_context: *const nix_c_context) -> nix_err {
    // SAFETY: as in nix_err_msg.
    unsafe { read_context.as_ref() }.map_or(NIX_ERR_UNKNOWN, |r| r.code)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn nix_set_err_msg(
    context: *mut nix_c_context,
    code: nix_err,
    msg: *const c_char,
) -> nix_err {
    let text = cstr(msg).map(String::from_utf8_lossy).unwrap_or_default();
    err(context, code, &text)
}

#[unsafe(no_mangle)]
pub extern "C" fn nix_clear_err(context: *mut nix_c_context) {
    clear(context);
}

/// ietsp's log has no verbosity knob, so the level is validated and
/// dropped (docs/c-api.md).
#[unsafe(no_mangle)]
pub extern "C" fn nix_set_verbosity(context: *mut nix_c_context, level: c_int) -> nix_err {
    clear(context);
    if !(0..=NIX_LVL_VOMIT).contains(&level) {
        return err(context, NIX_ERR_UNKNOWN, "Invalid verbosity level");
    }
    NIX_OK
}
