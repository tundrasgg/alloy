//! Files under `tests/` are test code as a whole.

fn helper() -> u16 {
    demo::port("7").unwrap()
}

#[test]
fn model() {
    assert_eq!(helper(), 7);
}
