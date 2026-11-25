//! C interface used by the SwiftUI app. Strings returned by this library must
//! be released with `smk_free_string`.

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use smk_core::embed;

struct Callback(Option<extern "C" fn(*mut c_void)>, *mut c_void);
// The context pointer is only handed back to the caller's own callback.
unsafe impl Send for Callback {}
unsafe impl Sync for Callback {}

static CALLBACK: Mutex<Option<Callback>> = Mutex::new(None);

fn fire_callback() {
    if let Some(Callback(Some(f), ctx)) = CALLBACK.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        f(*ctx);
    }
}

unsafe fn string(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    CStr::from_ptr(p).to_string_lossy().into_owned()
}

fn owned(s: String) -> *mut c_char {
    CString::new(s.replace('\0', "")).map(CString::into_raw).unwrap_or(std::ptr::null_mut())
}

/// Register the function called (on an engine thread) whenever the state changed.
#[no_mangle]
pub extern "C" fn smk_set_callback(callback: Option<extern "C" fn(*mut c_void)>, context: *mut c_void) {
    *CALLBACK.lock().unwrap_or_else(|e| e.into_inner()) = Some(Callback(callback, context));
}

/// Screen size in points, used when other machines enter this one.
#[no_mangle]
pub extern "C" fn smk_set_screen(width: u32, height: u32) {
    smk_core::input::set_screen_size(width, height);
}

/// Returns an error message, or an empty string on success.
#[no_mangle]
pub unsafe extern "C" fn smk_start(config_dir: *const c_char, download_dir: *const c_char) -> *mut c_char {
    let _ = tracing_subscriber::fmt().with_env_filter("info").try_init();
    let on_change: smk_core::engine::ChangeCallback = Arc::new(fire_callback);
    let result = embed::start(PathBuf::from(string(config_dir)), PathBuf::from(string(download_dir)), false, on_change);
    owned(result.err().map(|e| format!("{e:#}")).unwrap_or_default())
}

#[no_mangle]
pub extern "C" fn smk_state() -> *mut c_char {
    owned(embed::state_json())
}

/// Returns an error message, or an empty string on success.
#[no_mangle]
pub unsafe extern "C" fn smk_command(name: *const c_char, a: *const c_char, b: *const c_char) -> *mut c_char {
    owned(embed::command(&string(name), &string(a), &string(b)))
}

/// kind: 0 motion(dx, dy), 1 button(index, down), 2 scroll(dx, dy), 3 key(evdev code, down).
#[no_mangle]
pub extern "C" fn smk_input(kind: i32, a: f64, b: f64) {
    embed::input(kind, a, b);
}

#[no_mangle]
pub unsafe extern "C" fn smk_free_string(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}
