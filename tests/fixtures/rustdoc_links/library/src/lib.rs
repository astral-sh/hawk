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

pub const INLINE_TRAIT_IMPL_LINKED_CONSTANT: usize = 11;

pub const INLINE_DIRECT_TRAIT_IMPL_LINKED_CONSTANT: usize = 12;

pub const NESTED_REEXPORT_LINKED_CONSTANT: usize = 13;

pub const USED_REFERENCE_DEFINITION: usize = 14;

pub const UNUSED_REFERENCE_DEFINITION: usize = 15;

pub const HIDDEN_VARIANT_FIELD_LINKED_CONSTANT: usize = 16;

pub const CFG_DOC_LINKED_CONSTANT: usize = 17;

pub const CFG_ATTR_DOC_LINKED_CONSTANT: usize = 18;

pub const HIDDEN_NESTED_REEXPORT_LINKED_CONSTANT: usize = 19;

pub const PRIVATE_INLINE_TRAIT_IMPL_LINKED_CONSTANT: usize = 20;

pub const PRIVATE_REFERENCE_INLINE_TRAIT_IMPL_LINKED_CONSTANT: usize = 21;

pub const PRIVATE_DYNAMIC_INLINE_TRAIT_IMPL_LINKED_CONSTANT: usize = 22;

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

/// Links through a [reference definition][used].
///
/// [used]: crate::USED_REFERENCE_DEFINITION
pub fn reference_documented() {}

/// This reference definition is never used.
///
/// [unused]: crate::UNUSED_REFERENCE_DEFINITION
pub fn unused_reference_documented() {}

#[cfg(doc)]
/// Links to [`crate::CFG_DOC_LINKED_CONSTANT`].
pub fn cfg_doc_documented() {}

#[cfg_attr(
    doc,
    doc = "Links to [`crate::CFG_ATTR_DOC_LINKED_CONSTANT`]."
)]
pub fn cfg_attr_doc_documented() {}

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
pub mod hidden_inline_trait_source {
    pub trait InlinedTrait {}

    /// Links to [`crate::INLINE_TRAIT_IMPL_LINKED_CONSTANT`].
    impl<T> InlinedTrait for T {}

    pub trait InlinedDirectTrait {}

    /// Links to [`crate::INLINE_DIRECT_TRAIT_IMPL_LINKED_CONSTANT`].
    impl InlinedDirectTrait for crate::DocumentedType {}

    pub trait InlinedPrivateTrait {}

    pub trait InlinedPrivateReferenceTrait {}

    pub trait InlinedPrivateDynamicTrait {}

    struct PrivateType;

    trait PrivateDynamicTrait {}

    /// Links to [`crate::PRIVATE_INLINE_TRAIT_IMPL_LINKED_CONSTANT`].
    impl InlinedPrivateTrait for PrivateType {}

    /// Links to [`crate::PRIVATE_REFERENCE_INLINE_TRAIT_IMPL_LINKED_CONSTANT`].
    impl InlinedPrivateReferenceTrait for &PrivateType {}

    /// Links to [`crate::PRIVATE_DYNAMIC_INLINE_TRAIT_IMPL_LINKED_CONSTANT`].
    impl InlinedPrivateDynamicTrait for dyn PrivateDynamicTrait {}
}

#[doc(inline)]
pub use hidden_inline_trait_source::{
    InlinedDirectTrait, InlinedPrivateDynamicTrait, InlinedPrivateReferenceTrait,
    InlinedPrivateTrait, InlinedTrait,
};

#[doc(hidden)]
pub mod hidden_nested_item_source {
    /// Links to [`crate::NESTED_REEXPORT_LINKED_CONSTANT`].
    pub struct NestedReexported;
}

#[doc(hidden)]
pub mod hidden_nested_reexport_item_source {
    /// Links to [`crate::HIDDEN_NESTED_REEXPORT_LINKED_CONSTANT`].
    pub struct HiddenNestedReexported;
}

#[doc(hidden)]
pub mod hidden_nested_module_source {
    pub mod facade {
        #[doc(inline)]
        pub use crate::hidden_nested_item_source::NestedReexported;

        #[doc(hidden)]
        #[doc(inline)]
        pub use crate::hidden_nested_reexport_item_source::HiddenNestedReexported;
    }
}

#[doc(inline)]
pub use hidden_nested_module_source::facade as nested_facade;

#[doc(hidden)]
pub mod hidden_variant_source {
    pub enum InlinedEnum {
        #[doc(hidden)]
        Hidden {
            /// Links to [`crate::HIDDEN_VARIANT_FIELD_LINKED_CONSTANT`].
            field: usize,
        },
        Visible,
    }
}

#[doc(inline)]
pub use hidden_variant_source::InlinedEnum;

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
