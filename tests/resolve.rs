//! Resolution beyond names: macro arguments, typed receivers, trait dispatch, implicit
//! interfaces. One fixture per language under tests/fixtures/<lang>_infer.
#![allow(clippy::unwrap_used, missing_docs)]

mod common;

use common::{edge, index_fixture};

fn assert_edge(ix: &common::Indexed, file: &str, line: u32, name: &str, qname: &str, conf: &str) {
    let got = edge(&ix.store, file, line, name);
    assert_eq!(
        got,
        Some((qname.to_string(), conf.to_string())),
        "{file}:{line} {name}"
    );
}

#[test]
fn rust_calls_inside_macro_arguments() {
    let ix = index_fixture("rust_infer");
    // assert_eq!(short(n as u32), "0") / vec![short(1), short(2)]
    assert_edge(&ix, "src/lib.rs", 15, "short", "short", "exact");
    assert_edge(&ix, "src/lib.rs", 16, "short", "short", "exact");
    // Turbofish inside macro tokens: `parse::<u32>(..)` and `r.cast::<u8>()`.
    assert_edge(&ix, "src/lib.rs", 42, "parse", "parse", "exact");
    assert_edge(&ix, "src/lib.rs", 44, "cast", "repo::Repo::cast", "exact");
}

#[test]
fn rust_typed_receivers_and_trait_dispatch() {
    let ix = index_fixture("rust_infer");
    let lib = "src/lib.rs";
    // `let repo = Repo::open(..)?` -> Result<Self, _> unwrapped -> Repo.
    assert_edge(&ix, lib, 14, "count", "repo::Repo::count", "exact");
    // Repo::name, not the trait default Backend::name.
    assert_edge(&ix, lib, 17, "name", "repo::Repo::name", "exact");
    // Concrete type from `Type::default()` -> its impl of the trait method.
    assert_edge(&ix, lib, 19, "list", "backend::MemBackend::list", "exact");
    // `&dyn Backend`, a method returning `&dyn Backend`, `B: Backend`, `impl Backend`.
    assert_edge(&ix, lib, 21, "list", "backend::Backend::list", "exact");
    assert_edge(&ix, lib, 22, "list", "backend::Backend::list", "exact");
    assert_edge(&ix, lib, 26, "list", "backend::Backend::list", "exact");
    assert_edge(&ix, lib, 30, "list", "backend::Backend::list", "exact");
    // `self.backend.list()` through a `Box<dyn Backend>` field.
    assert_edge(
        &ix,
        "src/repo.rs",
        14,
        "list",
        "backend::Backend::list",
        "exact",
    );
    // `len()` on a Vec<String> is std: resolves to nothing rather than a guess.
    assert_eq!(edge(&ix.store, lib, 26, "len"), None);
}

#[test]
fn ts_workspace_packages_and_typed_receivers() {
    let ix = index_fixture("ts_infer");
    let app = "packages/app/src/main.ts";
    // `this.store` declared by a constructor parameter property.
    assert_edge(&ix, app, 7, "get", "src.Store.get", "exact");
    // `makeStore()` return type, imported from a sibling workspace package.
    assert_edge(&ix, app, 13, "get", "src.Store.get", "exact");
    // `new Map()` is a global type: no guess.
    assert_eq!(edge(&ix.store, app, 15, "get"), None);
    // `new Service(s).lookup(..)` and an interface-typed parameter.
    assert_edge(&ix, app, 16, "lookup", "main.Service.lookup", "exact");
    assert_edge(&ix, app, 17, "run", "src.Tool.run", "exact");
    // Through a barrel: `export * from`, `export { a as b } from`, a namespace import of
    // the barrel, and `function f(this: T)`.
    let user = "packages/app/src/user.ts";
    assert_edge(&ix, user, 4, "ext", "ann.Ann.ext", "exact");
    assert_edge(&ix, user, 7, "annFn", "ann.annFn", "exact");
    assert_edge(&ix, user, 8, "aliasHelper", "helpers.helper", "exact");
    assert_edge(&ix, user, 9, "annFn", "ann.annFn", "exact");
    // `export *` passes on exported names only.
    assert_eq!(edge(&ix.store, user, 10, "hidden"), None);
}

#[test]
fn rust_loop_vars_callbacks_reexports_and_test_modules() {
    let ix = index_fixture("rust_infer");
    let w = "src/walk.rs";
    // `pub use inner::compact` in store/mod.rs.
    assert_edge(&ix, w, 4, "compact", "store::inner::compact", "exact");
    // for b in &[MemBackend] / .map(|b| ..) / if let Some(m) = Option<&MemBackend>.
    assert_edge(&ix, w, 6, "list", "backend::MemBackend::list", "exact");
    assert_edge(&ix, w, 8, "list", "backend::MemBackend::list", "exact");
    assert_edge(&ix, w, 10, "list", "backend::MemBackend::list", "exact");
    // `mod common;` in an integration test.
    assert_edge(&ix, "tests/it.rs", 5, "setup", "setup", "exact");
}

#[test]
fn rust_extension_traits_on_std_types() {
    let ix = index_fixture("rust_infer");
    // `impl Put for Vec<u8>` with `Put` in scope: the impl's method, or the trait's
    // provided one.
    assert_edge(
        &ix,
        "tests/ext_use.rs",
        6,
        "put_one",
        "ext::Vec::put_one",
        "exact",
    );
    assert_edge(
        &ix,
        "tests/ext_use.rs",
        7,
        "twice",
        "ext::Put::twice",
        "exact",
    );
    // Without the trait in scope Rust would not compile the call: no guess.
    assert_eq!(edge(&ix.store, "tests/ext_unused.rs", 5, "put_one"), None);
    // std's own method stays external.
    assert_eq!(edge(&ix.store, "tests/ext_unused.rs", 4, "push"), None);
}

#[test]
fn rust_path_attribute_modules() {
    let ix = index_fixture("rust_infer");
    // tests/lex.rs: `#[path = "lex/cases.rs"] mod cases;` whose `crate::walk` is the
    // `#[path = "../src/walk.rs"] mod walk;` of the same test crate.
    assert_edge(&ix, "tests/lex/cases.rs", 4, "walk", "walk::walk", "exact");
    // A `#[cfg]` twin (`#[path] mod imp;` next to `use std::io as imp;`) is not followed.
    assert_eq!(edge(&ix.store, "tests/lex.rs", 14, "stub"), None);
}

#[test]
fn rust_std_macro_names_as_functions() {
    let ix = index_fixture("rust_infer");
    // `write("p", ..)` with `use crate::io_util::write` is a call, not the std macro...
    assert_edge(&ix, "src/uses.rs", 7, "write", "io_util::write", "exact");
    // ...while `write!(..)` stays the macro.
    assert_eq!(edge(&ix.store, "src/uses.rs", 6, "write"), None);
}

#[test]
fn rust_items_nested_in_functions_stay_local() {
    let ix = index_fixture("rust_infer");
    // `type Value = u8;` of an impl inside a test fn does not shadow the imported Value.
    assert_edge(
        &ix,
        "tests/assoc.rs",
        17,
        "pointer",
        "value::Value::pointer",
        "exact",
    );
}

#[test]
fn rust_paths_through_reexports_and_inline_modules() {
    let ix = index_fixture("rust_infer");
    // Field typed `super::Buf` where buf/mod.rs has `pub use self::buf_impl::Buf`.
    assert_edge(
        &ix,
        "src/buf/chain.rs",
        7,
        "remaining",
        "buf::buf_impl::Buf::remaining",
        "exact",
    );
    // `&infer::buf::Buf` from an integration test, through the same re-export.
    assert_edge(
        &ix,
        "tests/reexp.rs",
        2,
        "remaining",
        "buf::buf_impl::Buf::remaining",
        "exact",
    );
    // `super::from_ns` inside an inline `mod imp` is the file's own function.
    assert_edge(&ix, "src/sysx.rs", 7, "from_ns", "sysx::from_ns", "exact");
    // `walker::scan()` is the module function, not the method `State::scan`.
    assert_edge(&ix, "src/scan_use.rs", 4, "scan", "walker::scan", "exact");
}

#[test]
fn rust_trait_overloads_on_typed_receivers() {
    let ix = index_fixture("rust_infer");
    // `Extend<u8>` and `Extend<u16>` for Acc: the argument type decides, not the first impl.
    assert_eq!(edge(&ix.store, "src/overload.rs", 18, "extend"), None);
}

#[test]
fn rust_impls_for_references_and_slices() {
    let ix = index_fixture("rust_infer");
    // `impl<'a> Show for &'a Point`: the methods belong to Point.
    assert_edge(
        &ix,
        "src/slices.rs",
        27,
        "show",
        "slices::Point::show",
        "exact",
    );
    // `impl PutBytes for &mut [u8]` on a `&mut [u8]` receiver.
    assert_edge(
        &ix,
        "src/slices.rs",
        26,
        "put_bytes",
        "slices::[u8]::put_bytes",
        "exact",
    );
}

#[test]
fn rust_generic_parameters_bind_to_their_bound() {
    let ix = index_fixture("rust_infer");
    // Field `formatter: F` read inside `impl<W, F: Formatter>`.
    assert_edge(
        &ix,
        "src/gen.rs",
        14,
        "write_null",
        "gen::Formatter::write_null",
        "exact",
    );
    // `R::discard(r)` with `R: Reader`.
    assert_edge(
        &ix,
        "src/gen.rs",
        23,
        "discard",
        "gen::Reader::discard",
        "exact",
    );
}

#[test]
fn rust_trait_overloads_picked_by_argument_type() {
    let ix = index_fixture("rust_infer");
    let f = "src/conv.rs";
    // `&b"abc"[..]` -> `From<&'static [u8]>` (line 6), `vec![..]` -> `From<Vec<u8>>`
    // (12), a `const LONG: &[u8]` -> the slice impl, and `Vec::from(c)` -> the in-repo
    // `From<Buf3> for Vec<u8>` (18).
    for (line, def) in [(26, 6), (27, 12), (28, 6), (30, 18)] {
        let got: Option<(u32, String)> = ix
            .store
            .conn()
            .query_row(
                "SELECT t.start_line, e.confidence FROM edges e JOIN files f ON f.id = e.file_id
                 JOIN symbols t ON t.id = e.to_symbol
                 WHERE f.path = ?1 AND e.line = ?2 AND e.name = 'from'",
                rusqlite::params![f, line],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        assert_eq!(got, Some((def, "exact".to_string())), "{f}:{line}");
    }
}

#[test]
fn rust_pattern_bindings_are_typed() {
    let ix = index_fixture("rust_infer");
    let f = "src/pat.rs";
    // `let (f, _ok) = pair()` with `-> (Num, bool)`.
    assert_edge(&ix, f, 22, "as_f64", "pat::Num::as_f64", "exact");
    // `Val::Number(n)`: payload 0 of the variant; `Val::Map { inner }`: its field.
    assert_edge(&ix, f, 24, "as_f64", "pat::Num::as_f64", "exact");
    assert_edge(&ix, f, 25, "as_f64", "pat::Num::as_f64", "exact");
}

#[test]
fn rust_turbofish_calls_in_macro_tokens() {
    let ix = index_fixture("rust_infer");
    // `load::<BTreeMap<u128, ()>>(..)`, `load::<Vec<Vec<u8>>>(..)` and a qualified
    // `crate::turbo::load::<Option<u8>>(..)` inside macro arguments.
    for line in [8, 9, 10] {
        assert_edge(&ix, "src/turbo.rs", line, "load", "turbo::load", "exact");
    }
    // `Vec::<u8>::new()`: a type path, not a call of `Vec`.
    assert_eq!(edge(&ix.store, "src/turbo.rs", 11, "Vec"), None);
}

#[test]
fn rust_associated_types_are_not_module_items() {
    let ix = index_fixture("rust_infer");
    // `type Num = u8;` inside `impl Mk for Cnt` does not shadow the imported `Num`.
    assert_edge(&ix, "src/assoc2.rs", 13, "Num", "pat::Num", "exact");
    assert_edge(
        &ix,
        "src/assoc2.rs",
        14,
        "as_f64",
        "pat::Num::as_f64",
        "exact",
    );
}

#[test]
fn rust_std_wrappers_primitives_and_provided_methods() {
    let ix = index_fixture("rust_infer");
    let f = "src/prim.rs";
    // `ManuallyDrop::new(Unit).go()`, a unit struct value, `(*p).go()` with `*mut Unit`.
    assert_edge(&ix, f, 36, "go", "prim::Unit::go", "exact");
    assert_edge(&ix, f, 37, "go", "prim::Unit::go", "exact");
    assert_edge(&ix, f, 40, "go", "prim::Unit::go", "exact");
    // `Big::hi(..)`: provided by `Math`, which Big implements without overriding it.
    assert_edge(&ix, f, 38, "hi", "prim::Math::hi", "likely");
    // `f64::exponent_limit()`: the in-repo `impl Float for f64`.
    assert_edge(
        &ix,
        f,
        41,
        "exponent_limit",
        "prim::f64::exponent_limit",
        "exact",
    );
}

#[test]
fn rust_arrays_unsize_and_mut_references_keep_types() {
    let ix = index_fixture("rust_infer");
    // `[0u8; 4]` gets the methods of `impl Put for &mut [u8]`.
    assert_edge(&ix, "src/arr.rs", 19, "put1", "arr::[u8]::put1", "exact");
    // `let r = &mut Wide;` (the `mut` is not the referenced value).
    assert_edge(&ix, "src/arr.rs", 21, "go", "arr::Wide::go", "exact");
}

#[test]
fn rust_macro_tokens_default_calls() {
    let ix = index_fixture("rust_infer");
    // `assert!(Kp::default().is_empty())`: `default` is a keyword token; the chain still
    // types the receiver, and `Kp` is a path segment, not a call of `Kp`.
    assert_edge(
        &ix,
        "src/tmacro.rs",
        17,
        "is_empty",
        "tmacro::Kp::is_empty",
        "exact",
    );
    let kind: String = ix
        .store
        .conn()
        .query_row(
            "SELECT e.kind FROM edges e JOIN files f ON f.id = e.file_id
             WHERE f.path = 'src/tmacro.rs' AND e.line = 17 AND e.name = 'Kp'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kind, "ref");
}

#[test]
fn rust_supertrait_methods_on_generic_bounds() {
    let ix = index_fixture("rust_infer");
    // `F: Float` with `trait Float: AsCast`: `F::as_cast` is AsCast's.
    assert_edge(
        &ix,
        "src/sup.rs",
        10,
        "as_cast",
        "sup::AsCast::as_cast",
        "likely",
    );
}

#[test]
fn rust_self_default_is_the_impl_type() {
    let ix = index_fixture("rust_infer");
    // `let mut v = Self::default();` with a derived Default.
    assert_edge(
        &ix,
        "src/selfdef.rs",
        13,
        "bump",
        "selfdef::Acc2::bump",
        "exact",
    );
}

fn impls(ix: &common::Indexed, name: &str) -> Vec<String> {
    let mut st = ix
        .store
        .conn()
        .prepare(
            "SELECT f.qname FROM edges e JOIN symbols f ON f.id = e.from_symbol
             JOIN symbols t ON t.id = e.to_symbol
             WHERE e.kind = 'impl' AND t.name = ?1 ORDER BY f.qname",
        )
        .unwrap();
    st.query_map([name], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn go_implicit_interfaces_and_typed_receivers() {
    let ix = index_fixture("go_infer");
    // Store has Get with the right shape; Cache gets it by embedding; Wrong's Get differs.
    assert_eq!(impls(&ix, "Getter"), ["store.Cache", "store.Store"]);
    // ReadGetter = io.Reader (stdlib, known) + Getter.
    assert_eq!(impls(&ix, "ReadGetter"), ["store.Cache", "store.Store"]);
    let a = "api/api.go";
    assert_edge(&ix, a, 16, "Get", "store.Store.Get", "exact");
    assert_edge(&ix, a, 17, "Get", "store.Store.Get", "exact");
    // Interface-typed field: the interface method.
    assert_edge(&ix, a, 18, "Get", "store.Getter.Get", "exact");
    // http.ResponseWriter is stdlib: nothing, not a guess.
    assert_eq!(edge(&ix.store, a, 19, "Set"), None);
    // Promoted through the embedded *Store, from a range element.
    assert_edge(&ix, a, 21, "Get", "store.Store.Get", "likely");
}

#[test]
fn rust_receivers_inside_macros_and_smart_pointers() {
    let ix = index_fixture("rust_infer");
    let f = "src/shared.rs";
    // Receiver chain rebuilt from macro tokens: repo.backend().list().
    assert_edge(&ix, f, 7, "backend", "repo::Repo::backend", "exact");
    assert_edge(&ix, f, 7, "list", "backend::Backend::list", "exact");
    // Arc::new(r) derefs to Repo.
    assert_edge(&ix, f, 8, "count", "repo::Repo::count", "exact");
}

#[test]
fn callers_include_dynamic_dispatch() {
    use codegraph::query::{call_tree, Dir, Filter};
    let ix = index_fixture("rust_infer");
    let t = call_tree(
        &ix.store,
        "MemBackend::list",
        &Filter::default(),
        Dir::Callers,
        1,
        false,
    )
    .unwrap();
    let kids = &t.roots[0].children;
    let direct: Vec<&str> = kids
        .iter()
        .filter(|c| c.via.is_none())
        .filter_map(|c| c.site.as_deref())
        .collect();
    let via: Vec<&str> = kids
        .iter()
        .filter(|c| c.via.is_some())
        .filter_map(|c| c.site.as_deref())
        .collect();
    // `mem.list()` on the concrete type; `b.list()` on `&dyn Backend` etc. via the trait.
    assert!(
        direct.iter().any(|s| s.ends_with("src/lib.rs:19")),
        "{direct:?}"
    );
    assert!(via.iter().any(|s| s.ends_with("src/lib.rs:21")), "{via:?}");
    assert!(via.iter().any(|s| s.ends_with("src/repo.rs:14")), "{via:?}");
    assert_eq!(
        kids.iter().find_map(|c| c.via.clone()).as_deref(),
        Some("backend::Backend::list")
    );
}

// Regressions from the overnight review (each was a confirmed wrong or missing edge).
#[test]
fn review_regressions_rust() {
    let ix = index_fixture("review/rs");
    let f = "src/lib.rs";
    // H1: the receiver's own `read` wins over the std passthrough shortcut.
    assert_edge(&ix, f, 28, "get", "Snap::get", "exact");
    // H2: a tuple closure parameter shadows the outer `repo`.
    assert_ne!(
        edge(&ix.store, f, 32, "save").map(|e| e.0),
        Some("Db::save".to_string())
    );
    assert_edge(&ix, f, 33, "save", "Db::save", "exact");
    // M1: `if let Some(node) = node.parent()` binds after its initializer.
    assert_edge(&ix, f, 37, "parent", "Snap::parent", "exact");
    assert_edge(&ix, f, 38, "save", "Snap::save", "exact");
    // H3: `?Sized` / marker bounds are skipped.
    assert_edge(&ix, f, 43, "put", "Put::put", "exact");
    assert_edge(&ix, f, 47, "put", "Put::put", "exact");
    // H4: same-named types in two modules keep their own methods.
    assert_edge(&ix, f, 51, "only_x", "x::Config::only_x", "exact");
}

#[test]
fn review_regressions_go_and_ts() {
    use codegraph::query::{call_tree, Dir, Filter};
    let ix = index_fixture("review/gox");
    // H3: a type parameter constrained by an interface.
    assert_edge(&ix, "a/a.go", 8, "Get", "a.Getter.Get", "exact");
    // H4: a.Server.Close is not reached through b.OpenCloser.
    let t = call_tree(
        &ix.store,
        "a.Server.Close",
        &Filter::default(),
        Dir::Callers,
        1,
        false,
    )
    .unwrap();
    assert!(
        t.roots
            .iter()
            .all(|r| r.children.iter().all(|c| c.via.is_none())),
        "{:?}",
        t.roots
    );
    let ix = index_fixture("review/ts");
    assert_edge(&ix, "main.ts", 23, "run", "main.Base.run", "exact");
    // H1 (TS): Bag.values is the repo's own method, not Array.prototype.values.
    assert_edge(&ix, "main.ts", 27, "go", "main.Item.go", "exact");
}

#[test]
fn python_dotted_import_and_subscripted_bases() {
    let ix = index_fixture("review/py");
    // `import pkg.utils` binds `pkg`; `pkg.echo` is re-exported by pkg/__init__.py.
    assert_edge(&ix, "main.py", 11, "echo", "pkg.utils.echo", "exact");
    // `class Child(Base[int])` inherits `fail`.
    assert_edge(&ix, "main.py", 7, "fail", "pkg.utils.Base.fail", "likely");
}
