extern crate proc_macro;

use proc_macro::TokenStream;

#[cfg_attr(
    doc,
    doc = "Links to [`library::PROC_MACRO_CFG_DOC_LINKED_CONSTANT`]."
)]
#[proc_macro]
pub fn passthrough(input: TokenStream) -> TokenStream {
    if cfg!(doc) {
        "compile_error!(\"a cfg(doc) proc macro must not execute downstream\");"
            .parse()
            .expect("static expansion")
    } else {
        input
    }
}
