/// Free function named like `Store::new`.
pub fn new() -> u32 {
    helper();
    0
}

/// Shared helper.
pub fn helper() {}

/// Generates one getter per name (defined by macro_rules!, expanded by codegraph).
macro_rules! getters {
    ($($name:ident),* $(,)?) => {
        $(pub fn $name() -> u32 { new() })*
    };
}

getters!(alpha, beta);

pub fn sum() -> u32 {
    alpha() + beta()
}

/// Named like a variant of `store::tok::Token`.
pub struct Ident;
