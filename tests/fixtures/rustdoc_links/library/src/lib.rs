pub struct DocumentedType;

impl DocumentedType {
    pub fn linked_method() {}

    pub fn unlinked_method() {}
}

pub const LINKED_CONSTANT: usize = 1;

pub const UNLINKED_CONSTANT: usize = 2;

pub const PRIVATE_LINKED_CONSTANT: usize = 3;

pub const HIDDEN_LINKED_CONSTANT: usize = 4;

pub struct AliasTarget;

impl AliasTarget {
    pub fn alias_linked_method() {}

    pub fn alias_unlinked_method() {}
}

pub type Alias = AliasTarget;

pub type NamespaceCollision = usize;

#[allow(non_upper_case_globals)]
pub const NamespaceCollision: usize = 5;

/// Links to [`LINKED_CONSTANT`] and [`DocumentedType::linked_method`].
pub fn documented() {}

/// Links to [`Alias::alias_linked_method`].
pub fn alias_documented() {}

/// Links to [`type@NamespaceCollision`], but not the value with the same name.
pub fn namespace_documented() {}

#[doc(hidden)]
/// Links to [`HIDDEN_LINKED_CONSTANT`].
pub fn hidden_documented() {}

/// Links to [`PRIVATE_LINKED_CONSTANT`].
fn private_documented() {}
