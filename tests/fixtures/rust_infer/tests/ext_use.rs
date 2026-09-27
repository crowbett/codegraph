use infer::ext::Put;

#[test]
fn puts() {
    let mut v = Vec::with_capacity(4);
    v.put_one(1);
    v.twice(2);
}
