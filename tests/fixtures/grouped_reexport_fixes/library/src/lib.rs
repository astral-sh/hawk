mod exported {
    pub struct Kept;
    pub struct Narrow;
}

pub use exported::{Kept, Narrow};

pub fn use_narrow_internally() {
    let _ = exported::Narrow;
}

mod split_consumers {
    pub struct ProductionOnly;
    pub struct TestOnly;
}

pub use split_consumers::{ProductionOnly, TestOnly};

pub fn use_production_internally() {
    let _ = ProductionOnly;
}

#[cfg(test)]
mod tests {
    #[test]
    fn uses_test_only() {
        let _ = super::TestOnly;
    }
}
