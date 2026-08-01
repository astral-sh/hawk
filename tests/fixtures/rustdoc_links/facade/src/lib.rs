#[doc(inline)]
pub use library::ForeignInlinedDocumented;

/// Links to [`library::CrossCrateDocumentedReexport`].
pub fn cross_crate_reexport_documented() {}

pub mod docs {
    pub mod library {
        pub const SHADOWED_EXTERNAL_LINK: usize = 0;
    }

    /// Links to local [`library::SHADOWED_EXTERNAL_LINK`], not the dependency.
    pub fn shadowed_documentation() {}
}
