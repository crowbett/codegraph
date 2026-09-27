#[test]
fn no_trait_in_scope() {
    let mut v: Vec<u8> = Vec::new();
    v.push(1);
    v.put_one(1);
}
