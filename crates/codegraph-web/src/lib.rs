//! Browser bridge for codegraph: indexes bundled in-memory sources with the real
//! tree-sitter extractor and answers callers (docs/demo-contract.md ABI).

use codegraph::extract::extract;
use codegraph::lang::Lang;
use codegraph::model::EdgeKind;
use serde_json::{json, Value};
use std::alloc::{alloc, dealloc, handle_alloc_error, Layout};

const SAMPLE: &[(&str, &str)] = &[
    (
        "main.go",
        "package main\n\nfunc main() {\n\trun()\n}\n\nfunc run() {\n\tparse()\n\tparse()\n}\n",
    ),
    (
        "parse.go",
        "package main\n\nfunc parse() int {\n\treturn tokenize()\n}\n\nfunc tokenize() int {\n\treturn 1\n}\n",
    ),
];

/// Name-based callers over `extract` output (the SQL resolver is not in this build).
pub fn handle(method: &str, params: &[u8]) -> Value {
    match method {
        "callers" => {
            let p: Value = serde_json::from_slice(params).unwrap_or(Value::Null);
            let Some(target) = p["symbol"].as_str() else {
                return json!({ "error": "params: symbol required" });
            };
            let mut out = Vec::new();
            for (path, src) in SAMPLE {
                let Some(lang) = Lang::from_path(std::path::Path::new(path)) else {
                    continue;
                };
                let facts = match extract(lang, src.as_bytes()) {
                    Ok(f) => f,
                    Err(e) => return json!({ "error": format!("{path}: {e}") }),
                };
                for r in &facts.refs {
                    if r.kind == EdgeKind::Call && r.name == target {
                        let from = r.from.map(|i| facts.symbols[i].local_qname.clone());
                        out.push(json!({ "from": from, "file": path, "line": r.line }));
                    }
                }
            }
            json!({ "symbol": target, "callers": out })
        }
        _ => json!({ "error": format!("unknown method {method}") }),
    }
}

#[no_mangle]
pub extern "C" fn demo_alloc(len: usize) -> *mut u8 {
    let layout = Layout::from_size_align(len.max(1), 1).unwrap();
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
    unsafe { dealloc(ptr, Layout::from_size_align(len.max(1), 1).unwrap()) }
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
