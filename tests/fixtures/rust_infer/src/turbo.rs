use std::collections::BTreeMap;

pub fn load<T: Default>(_s: &str) -> T {
    T::default()
}

pub fn check() {
    assert_eq!(load::<BTreeMap<u128, ()>>("x").len(), 0);
    assert!(load::<Vec<Vec<u8>>>("y").is_empty());
    assert_eq!(crate::turbo::load::<Option<u8>>("z"), None);
    assert!(Vec::<u8>::new().is_empty());
}
