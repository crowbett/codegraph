use infer::value::Value;

trait Visitor {
    type Value;
}

struct V;

#[test]
fn visits() {
    impl Visitor for V {
        type Value = u8;
    }
}

fn check(data: &Value) -> usize {
    data.pointer()
}
