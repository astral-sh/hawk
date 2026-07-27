#[cfg(feature = "fast-path")]
fn main() {
    library::extra_api();
}

#[cfg(not(feature = "fast-path"))]
fn main() {
    library::fallback_api();
}
