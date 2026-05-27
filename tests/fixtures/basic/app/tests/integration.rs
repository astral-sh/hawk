pub fn test_binary_helper() {}

#[test]
fn exercises_test_support_api() {
    library::integration_test_support();
    test_support::entry();
    test_binary_helper();
}
