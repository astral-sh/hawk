#[cfg(doc)]
compile_error!("a documentation-disabled target must not receive cfg(doc)");

pub const UNRENDERED_DOC_LINKED_CONSTANT: usize = 29;

pub fn unrendered_doc_linked_constant() -> usize {
    UNRENDERED_DOC_LINKED_CONSTANT
}

/// Links to [`UNRENDERED_DOC_LINKED_CONSTANT`], but this target is not documented.
pub fn unrendered_documentation() {}
