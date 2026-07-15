macro_rules! production_generated {
    () => {
        pub fn generated() {
            dead_api();
        }
    };
}

macro_rules! test_generated {
    () => {
        pub fn generated() {}
    };
}

#[cfg(not(test))]
production_generated!();

#[cfg(test)]
test_generated!();

#[cfg(test)]
mod tests {
    #[test]
    fn uses_generated() {
        super::generated();
    }
}

pub fn dead_api() {}
pub fn product_api() {}
