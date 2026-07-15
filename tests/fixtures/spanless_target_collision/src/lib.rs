macro_rules! generated {
    () => {
        #[cfg(not(test))]
        pub fn generated() {
            dead_api();
        }

        #[cfg(test)]
        pub fn generated() {}
    };
}

macro_rules! nested {
    () => {
        macro_rules! production_generated {
            () => {
                pub fn nested_generated() {
                    dead_api();
                }
            };
        }

        macro_rules! test_generated {
            () => {
                pub fn nested_generated() {}
            };
        }

        #[cfg(not(test))]
        production_generated!();

        #[cfg(test)]
        test_generated!();
    };
}

generated!();
nested!();

#[cfg(test)]
mod tests {
    #[test]
    fn uses_generated() {
        super::generated();
        super::nested_generated();
    }
}

pub fn dead_api() {}
pub fn product_api() {}
