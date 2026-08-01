pub struct DocumentedType;

impl DocumentedType {
    pub fn linked_method() {}

    pub fn unlinked_method() {}
}

pub const LINKED_CONSTANT: usize = 1;

pub const UNLINKED_CONSTANT: usize = 2;

pub const PRIVATE_LINKED_CONSTANT: usize = 3;

/// Links to [`LINKED_CONSTANT`] and [`DocumentedType::linked_method`].
pub fn documented() {}

/// Links to [`PRIVATE_LINKED_CONSTANT`].
fn private_documented() {}
