#[cfg(doc)]
compile_error!("a binary skipped by Cargo documentation must not receive cfg(doc)");

fn main() {}
