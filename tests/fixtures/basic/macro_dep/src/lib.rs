use proc_macro::TokenStream;

#[proc_macro]
pub fn passthrough(input: TokenStream) -> TokenStream {
    input
}

#[proc_macro_derive(ArchiveMirror)]
pub fn archive_mirror(input: TokenStream) -> TokenStream {
    let visibility = if input.to_string().contains("pub required_through_archive") {
        "pub "
    } else {
        ""
    };
    format!("pub struct ArchivedMirroredFields {{ {visibility}required_through_archive: u8 }}")
        .parse()
        .expect("valid generated declaration")
}
