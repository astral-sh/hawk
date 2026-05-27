mod exported {
    pub struct Kept;
    pub struct Narrow;
}

pub use exported::{Kept, Narrow};

pub fn use_narrow_internally() {
    let _ = exported::Narrow;
}
