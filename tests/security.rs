//! Security regressions: `def`/`refs` only show source from the file that was
//! indexed (SEC-4), never from a symlink swapped in afterwards or a changed file.
#![allow(clippy::unwrap_used, missing_docs)]

mod common;

use std::path::Path;

use codegraph::query::{self, Filter, STALE_NOTE};
use common::index_dir;

const A: &str = "package a\n\n// Hello says hi.\nfunc Hello() int { return 1 }\n";
const B: &str = "package a\n\nfunc Use() int { return Hello() }\n";
const SECRET: &str = "SECRET_TOKEN=hunter2\nSECRET_LINE_2\nSECRET_LINE_3\nSECRET_LINE_4\n";

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.go"), A).unwrap();
    std::fs::write(dir.path().join("b.go"), B).unwrap();
    dir
}

/// Replace `path` with a symlink to `target`; `false` if symlinks are not permitted.
fn swap_for_symlink(target: &Path, path: &Path) -> bool {
    std::fs::remove_file(path).unwrap();
    #[cfg(unix)]
    let r = std::os::unix::fs::symlink(target, path);
    #[cfg(windows)]
    let r = std::os::windows::fs::symlink_file(target, path);
    match r {
        Ok(()) => true,
        Err(e) => {
            eprintln!("skipping symlink case: {e}");
            false
        }
    }
}

#[test]
fn def_and_refs_show_indexed_source() {
    let src = repo();
    let ix = index_dir(src.path());
    let defs = query::def(&ix.store, "Hello", &Filter::default(), 40, 5).unwrap();
    assert_eq!(defs.len(), 1);
    assert!(defs[0].source.contains("func Hello()"), "{defs:?}");
    assert!(defs[0].note.is_none());
    // Backward-compatible JSON: no `note` key when the source was read.
    let v = serde_json::to_value(&defs[0]).unwrap();
    assert!(v.get("note").is_none(), "{v}");
    let r = query::refs(&ix.store, "Hello", &Filter::default(), None, 50).unwrap();
    assert_eq!(r.refs.len(), 1, "{r:?}");
    assert!(r.refs[0].text.contains("return Hello()"), "{r:?}");
    assert!(r.refs[0].note.is_none());
}

#[test]
fn changed_file_is_reported_stale() {
    let src = repo();
    let ix = index_dir(src.path());
    // Same length, different bytes: only the hash can tell.
    std::fs::write(src.path().join("a.go"), A.replace("hi.", "yo.")).unwrap();
    // Grown file: rejected by size before hashing.
    std::fs::write(src.path().join("b.go"), format!("{B}// more\n")).unwrap();
    let defs = query::def(&ix.store, "Hello", &Filter::default(), 40, 5).unwrap();
    assert_eq!(defs[0].source, "");
    assert_eq!(defs[0].note.as_deref(), Some(STALE_NOTE));
    let v = serde_json::to_value(&defs[0]).unwrap();
    assert_eq!(v["note"], STALE_NOTE);
    let r = query::refs(&ix.store, "Hello", &Filter::default(), None, 50).unwrap();
    assert_eq!(r.refs[0].text, "");
    assert_eq!(r.refs[0].note.as_deref(), Some(STALE_NOTE));
}

#[test]
fn symlink_swapped_in_after_indexing_is_not_followed() {
    let src = repo();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret.env");
    std::fs::write(&secret, SECRET).unwrap();
    let ix = index_dir(src.path());
    if !swap_for_symlink(&secret, &src.path().join("a.go"))
        || !swap_for_symlink(&secret, &src.path().join("b.go"))
    {
        return;
    }
    let defs = query::def(&ix.store, "Hello", &Filter::default(), 40, 5).unwrap();
    let out = format!("{defs:?}");
    assert!(!out.contains("SECRET"), "{out}");
    assert_eq!(defs[0].note.as_deref(), Some(STALE_NOTE));
    let r = query::refs(&ix.store, "Hello", &Filter::default(), None, 50).unwrap();
    let out = format!("{r:?}");
    assert!(!out.contains("SECRET"), "{out}");
    assert_eq!(r.refs[0].note.as_deref(), Some(STALE_NOTE));
    // The rendered text says why instead of printing the secret.
    let text = codegraph::render::defs(&defs, codegraph::output::Style::plain());
    assert!(
        !text.contains("SECRET") && text.contains("re-index"),
        "{text}"
    );
}

#[test]
fn symlink_to_a_file_with_the_same_content_is_still_refused() {
    // Even byte-identical content behind a link is refused: the check is on the
    // file type, not just the hash.
    let src = repo();
    let outside = tempfile::tempdir().unwrap();
    let copy = outside.path().join("a.go");
    std::fs::write(&copy, A).unwrap();
    let ix = index_dir(src.path());
    if !swap_for_symlink(&copy, &src.path().join("a.go")) {
        return;
    }
    let defs = query::def(&ix.store, "Hello", &Filter::default(), 40, 5).unwrap();
    assert_eq!(defs[0].source, "");
    assert_eq!(defs[0].note.as_deref(), Some(STALE_NOTE));
}
