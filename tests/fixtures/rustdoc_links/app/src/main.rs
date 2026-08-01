use macro_package::passthrough;

#[cfg(doc)]
/// Links to [`library::CROSS_CRATE_CFG_DOC_LINKED_CONSTANT`].
pub fn cross_crate_cfg_doc_documented() {}

/// Links to [`library::PRIVATE_BINARY_DOC_LINKED_CONSTANT`].
fn private_binary_documented() {}

passthrough! {
    fn generated_by_proc_macro() {}
}

#[cfg(not(doc))]
fn main() {
    generated_by_proc_macro();
    private_binary_documented();
    let _ = (
        library::cross_crate_cfg_doc_linked_constant(),
        library::private_binary_doc_linked_constant(),
        library::proc_macro_cfg_doc_linked_constant(),
    );
    library::use_documented_reexports();
}
