fn main() {
    #[cfg(target_os = "linux")]
    library::linux_api();
    #[cfg(windows)]
    library::windows_api();
    library::shared_api();
}
