fn main() {
    #[cfg(target_arch = "aarch64")]
    library::aarch64_api();

    #[cfg(target_arch = "x86_64")]
    library::x86_64_api();

    library::shared_entry();

    #[cfg(target_arch = "aarch64")]
    {
        library::aarch64_entry();
        library::expected_dead_on_x86();
    }
}
