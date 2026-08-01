pub struct DocumentedType {
    pub linked_field: usize,

    pub unlinked_field: usize,
}

impl DocumentedType {
    pub fn linked_method() {}

    pub fn unlinked_method() {}
}

pub const LINKED_CONSTANT: usize = 1;

pub const UNLINKED_CONSTANT: usize = 2;

pub const PRIVATE_LINKED_CONSTANT: usize = 3;

pub const HIDDEN_LINKED_CONSTANT: usize = 4;

pub const INLINE_REEXPORT_LINKED_CONSTANT: usize = 5;

pub const GLOB_INLINE_REEXPORT_LINKED_CONSTANT: usize = 6;

pub const NO_INLINE_REEXPORT_LINKED_CONSTANT: usize = 7;

pub const TRAIT_IMPL_LINKED_CONSTANT: usize = 8;

pub const PRIVATE_TRAIT_IMPL_LINKED_CONSTANT: usize = 9;

pub const HIDDEN_TRAIT_IMPL_LINKED_CONSTANT: usize = 10;

pub struct AliasTarget;

impl AliasTarget {
    pub fn alias_linked_method() {}

    pub fn alias_unlinked_method() {}
}

pub type Alias = AliasTarget;

pub type NamespaceCollision = usize;

#[allow(non_upper_case_globals)]
pub const NamespaceCollision: usize = 8;

pub trait RenderedTrait {}

trait PrivateTrait {}

#[doc(hidden)]
pub trait HiddenTrait {}

pub enum DocumentedEnum {
    Linked { field: usize },
    Unlinked { field: usize },
}

/// Links to [`LINKED_CONSTANT`], [`DocumentedType::linked_method`], and
/// [`field@DocumentedType::linked_field`].
pub fn documented() {}

/// Links to [`Alias::alias_linked_method`].
pub fn alias_documented() {}

/// Links to [`type@NamespaceCollision`], but not the value with the same name.
pub fn namespace_documented() {}

/// Links to [`field@DocumentedEnum::Linked::field`].
pub fn enum_field_documented() {}

#[doc(hidden)]
/// Links to [`HIDDEN_LINKED_CONSTANT`].
pub fn hidden_documented() {}

/// Links to [`PRIVATE_LINKED_CONSTANT`].
fn private_documented() {}

#[doc(hidden)]
pub mod hidden_source {
    /// Links to [`crate::INLINE_REEXPORT_LINKED_CONSTANT`].
    pub struct InlinedDocumentedType;

    /// Links to [`crate::TRAIT_IMPL_LINKED_CONSTANT`].
    impl crate::RenderedTrait for InlinedDocumentedType {}

    /// Links to [`crate::PRIVATE_TRAIT_IMPL_LINKED_CONSTANT`].
    impl crate::PrivateTrait for InlinedDocumentedType {}

    /// Links to [`crate::HIDDEN_TRAIT_IMPL_LINKED_CONSTANT`].
    impl crate::HiddenTrait for InlinedDocumentedType {}
}

#[doc(inline)]
pub use hidden_source::InlinedDocumentedType;

#[doc(hidden)]
pub mod hidden_glob_source {
    /// Links to [`crate::GLOB_INLINE_REEXPORT_LINKED_CONSTANT`].
    pub struct GlobInlinedDocumentedType;
}

#[doc(inline)]
pub use hidden_glob_source::*;

#[doc(hidden)]
pub mod no_inline_source {
    /// Links to [`crate::NO_INLINE_REEXPORT_LINKED_CONSTANT`].
    pub struct NotInlinedDocumentedType;
}

#[doc(no_inline)]
pub use no_inline_source::NotInlinedDocumentedType;
