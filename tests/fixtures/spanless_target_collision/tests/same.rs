macro_rules! generated {
    () => {
        pub fn generated() {}
    };
}

generated!();

#[test]
fn uses_generated() {
    generated();
}
