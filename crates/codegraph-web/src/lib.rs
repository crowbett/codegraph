//! Browser bridge for codegraph (the site's docs/demo-contract.md ABI, no imports):
//! [`codegraph::webdemo`] indexes its bundled sample into an in-memory SQLite graph with
//! codegraph's real indexer and resolver and answers `sample`, `callers` and `callees`.

#[cfg(target_arch = "wasm32")]
mod sys;

use std::alloc::{alloc, dealloc, handle_alloc_error, Layout};
use std::cell::OnceCell;

use codegraph::webdemo::Demo;
use serde_json::{json, Value};

thread_local! {
    static DEMO: OnceCell<Result<Demo, String>> = const { OnceCell::new() };
}

/// Answer one call; the sample is indexed on the first.
pub fn handle(method: &str, params: &[u8]) -> Value {
    DEMO.with(
        |d| match d.get_or_init(|| Demo::new().map_err(|e| e.to_string())) {
            Ok(demo) => demo.handle(method, params),
            Err(e) => json!({ "error": format!("indexing the sample: {e}") }),
        },
    )
}

fn layout(len: usize) -> Layout {
    match Layout::from_size_align(len.max(1), 1) {
        Ok(l) => l,
        Err(_) => std::process::abort(),
    }
}

#[no_mangle]
pub extern "C" fn demo_alloc(len: usize) -> *mut u8 {
    let layout = layout(len);
    // SAFETY: the layout has a non-zero size.
    let p = unsafe { alloc(layout) };
    if p.is_null() {
        handle_alloc_error(layout);
    }
    p
}

/// # Safety
/// `ptr` and `len` must come from `demo_alloc` or `demo_call`.
#[no_mangle]
pub unsafe extern "C" fn demo_free(ptr: *mut u8, len: usize) {
    unsafe { dealloc(ptr, layout(len)) }
}

/// # Safety
/// Both ranges must be live buffers from `demo_alloc`.
#[no_mangle]
pub unsafe extern "C" fn demo_call(
    method: *const u8,
    method_len: usize,
    params: *const u8,
    params_len: usize,
) -> *mut u8 {
    let method = unsafe { std::slice::from_raw_parts(method, method_len) };
    let params = unsafe { std::slice::from_raw_parts(params, params_len) };
    let out = match std::str::from_utf8(method) {
        Ok(m) => handle(m, params).to_string(),
        Err(_) => r#"{"error":"method is not UTF-8"}"#.to_string(),
    };
    let len = out.len();
    let p = demo_alloc(len + 4);
    unsafe {
        p.copy_from_nonoverlapping((len as u32).to_le_bytes().as_ptr(), 4);
        p.add(4).copy_from_nonoverlapping(out.as_ptr(), len);
    }
    p
}
