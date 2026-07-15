#[path = "../src/shared.rs"]
mod inner;
#[path = "../src/shared.rs"]
mod private;

pub use inner::Shared;

#[test]
fn shared_source_keeps_test_visibility() {
    let value = Shared { value: 1 };
    assert_eq!(value.value, 1);
    private::exercise();
}
