#![allow(clippy::unwrap_used, missing_docs)]
#[test]
fn dump() {
    let src = std::fs::read_to_string(std::env::var("ZZ_SRC").unwrap()).unwrap();
    let mut p = tree_sitter::Parser::new();
    p.set_language(&tree_sitter_go::LANGUAGE.into()).unwrap();
    let t = p.parse(&src, None).unwrap();
    println!("{}", t.root_node().to_sexp());
}
