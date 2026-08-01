extern crate proc_macro;

use proc_macro::TokenStream;

#[cfg_attr(
    doc,
    doc = "Links to [`library::PROC_MACRO_CFG_DOC_LINKED_CONSTANT`]."
)]
#[proc_macro]
pub fn passthrough(input: TokenStream) -> TokenStream {
    input
}
