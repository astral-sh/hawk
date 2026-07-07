#[cfg(feature = "fast-path")]
fn main() {}

#[cfg(not(feature = "fast-path"))]
fn main() {
    library::fallback_api();
}
