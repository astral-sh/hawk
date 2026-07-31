pub fn product_api() {
    standalone_first_api();
    standalone_second_api();
}

/// ```
/// library::doc_api();
/// ```
pub fn doc_api() {}

/// ```standalone_crate
/// library::standalone_first_api();
/// ```
pub fn standalone_first_api() {}

/// ```standalone_crate
/// library::standalone_second_api();
/// ```
pub fn standalone_second_api() {}

pub fn unused() {}
