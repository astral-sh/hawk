pub fn aarch64_api() {}

pub fn x86_64_api() {}

pub fn unused_api() {}

#[path = "shared.rs"]
pub mod repeated_a;

#[path = "shared.rs"]
pub mod repeated_b;

pub fn shared_entry() {
    #[cfg(target_arch = "x86_64")]
    repeated_a::shared_api();
}

pub fn aarch64_entry() {
    #[cfg(target_arch = "x86_64")]
    impossible_target_path();
}

pub fn impossible_target_path() {}

pub fn expected_dead_on_x86() {}
