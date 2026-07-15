macro_rules! generated {
    () => {
        pub fn generated() {
            dead_api();
        }
    };
}

generated!();

pub fn dead_api() {}
pub fn product_api() {}
