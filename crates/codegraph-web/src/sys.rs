//! What SQLite needs from an OS on wasm32-unknown-unknown, which has none: the libc
//! functions tree-sitter's wasm libc lacks (declared in `libc/include`) and a VFS that
//! opens no files, since the graph is `:memory:` and temp storage is memory
//! (`SQLITE_TEMP_STORE=3`). Deterministic: the clock is fixed at the Unix time
//! `codegraph::clock` reports there (one second) and randomness is all zeros.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::ptr;

use rusqlite::ffi;

fn has(set: *const c_char, c: c_char) -> bool {
    // SAFETY: callers pass SQLite's NUL-terminated strings.
    unsafe { CStr::from_ptr(set) }
        .to_bytes()
        .contains(&(c as u8))
}

/// # Safety
/// `s` and `accept` are NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn strspn(s: *const c_char, accept: *const c_char) -> usize {
    let mut n = 0;
    while unsafe { *s.add(n) } != 0 && has(accept, unsafe { *s.add(n) }) {
        n += 1;
    }
    n
}

/// # Safety
/// `s` and `reject` are NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn strcspn(s: *const c_char, reject: *const c_char) -> usize {
    let mut n = 0;
    while unsafe { *s.add(n) } != 0 && !has(reject, unsafe { *s.add(n) }) {
        n += 1;
    }
    n
}

/// # Safety
/// `s` is NUL-terminated.
#[no_mangle]
pub unsafe extern "C" fn strrchr(s: *const c_char, c: c_int) -> *mut c_char {
    let c = c as c_char;
    let mut last = ptr::null_mut();
    let mut p = s.cast_mut();
    loop {
        let ch = unsafe { *p };
        if ch == c {
            last = p;
        }
        if ch == 0 {
            return last;
        }
        p = unsafe { p.add(1) };
    }
}

/// Insertion sort (SQLite calls it only from FTS3, which codegraph does not use).
///
/// # Safety
/// `base` holds `n` elements of `size` bytes that `cmp` compares.
#[no_mangle]
pub unsafe extern "C" fn qsort(
    base: *mut c_void,
    n: usize,
    size: usize,
    cmp: extern "C" fn(*const c_void, *const c_void) -> c_int,
) {
    let base = base.cast::<u8>();
    for i in 1..n {
        let mut j = i;
        while j > 0 {
            let (a, b) = unsafe { (base.add((j - 1) * size), base.add(j * size)) };
            if cmp(a.cast(), b.cast()) <= 0 {
                break;
            }
            unsafe { ptr::swap_nonoverlapping(a, b, size) };
            j -= 1;
        }
    }
}

unsafe extern "C" fn open(
    _: *mut ffi::sqlite3_vfs,
    _: ffi::sqlite3_filename,
    _: *mut ffi::sqlite3_file,
    _: c_int,
    _: *mut c_int,
) -> c_int {
    ffi::SQLITE_CANTOPEN
}

unsafe extern "C" fn delete(_: *mut ffi::sqlite3_vfs, _: *const c_char, _: c_int) -> c_int {
    ffi::SQLITE_OK
}

unsafe extern "C" fn access(
    _: *mut ffi::sqlite3_vfs,
    _: *const c_char,
    _: c_int,
    out: *mut c_int,
) -> c_int {
    unsafe { *out = 0 };
    ffi::SQLITE_OK
}

unsafe extern "C" fn full_pathname(
    _: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    n: c_int,
    out: *mut c_char,
) -> c_int {
    let src = unsafe { CStr::from_ptr(name) }.to_bytes_with_nul();
    if src.len() > n as usize {
        return ffi::SQLITE_CANTOPEN;
    }
    unsafe { ptr::copy_nonoverlapping(src.as_ptr().cast(), out, src.len()) };
    ffi::SQLITE_OK
}

unsafe extern "C" fn randomness(_: *mut ffi::sqlite3_vfs, n: c_int, out: *mut c_char) -> c_int {
    unsafe { ptr::write_bytes(out, 0, n as usize) };
    n
}

unsafe extern "C" fn sleep(_: *mut ffi::sqlite3_vfs, _: c_int) -> c_int {
    0
}

/// The Unix epoch as a Julian day number in milliseconds, plus one second.
const NOW_JULIAN_MS: i64 = 210_866_760_000_000 + 1_000;

unsafe extern "C" fn current_time(_: *mut ffi::sqlite3_vfs, out: *mut f64) -> c_int {
    unsafe { *out = NOW_JULIAN_MS as f64 / 86_400_000.0 };
    ffi::SQLITE_OK
}

unsafe extern "C" fn current_time_ms(
    _: *mut ffi::sqlite3_vfs,
    out: *mut ffi::sqlite3_int64,
) -> c_int {
    unsafe { *out = NOW_JULIAN_MS };
    ffi::SQLITE_OK
}

unsafe extern "C" fn last_error(_: *mut ffi::sqlite3_vfs, _: c_int, _: *mut c_char) -> c_int {
    0
}

static mut VFS: ffi::sqlite3_vfs = ffi::sqlite3_vfs {
    iVersion: 2,
    szOsFile: size_of::<ffi::sqlite3_file>() as c_int,
    mxPathname: 512,
    pNext: ptr::null_mut(),
    zName: c"memory".as_ptr(),
    pAppData: ptr::null_mut(),
    xOpen: Some(open),
    xDelete: Some(delete),
    xAccess: Some(access),
    xFullPathname: Some(full_pathname),
    xDlOpen: None,
    xDlError: None,
    xDlSym: None,
    xDlClose: None,
    xRandomness: Some(randomness),
    xSleep: Some(sleep),
    xCurrentTime: Some(current_time),
    xGetLastError: Some(last_error),
    xCurrentTimeInt64: Some(current_time_ms),
    xSetSystemCall: None,
    xGetSystemCall: None,
    xNextSystemCall: None,
};

/// Called by `sqlite3_initialize` (SQLite is built with `SQLITE_OS_OTHER`).
#[no_mangle]
pub extern "C" fn sqlite3_os_init() -> c_int {
    // SAFETY: single-threaded; SQLite keeps the pointer for the life of the module.
    unsafe { ffi::sqlite3_vfs_register(&raw mut VFS, 1) }
}

/// Called by `sqlite3_shutdown`.
#[no_mangle]
pub extern "C" fn sqlite3_os_end() -> c_int {
    ffi::SQLITE_OK
}
