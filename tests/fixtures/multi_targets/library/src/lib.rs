#[cfg(target_os = "linux")]
pub fn linux_api() {
    linux_helper();
}

#[cfg(target_os = "linux")]
pub fn linux_helper() {}

#[cfg(windows)]
pub fn windows_api() {
    windows_helper();
}

#[cfg(windows)]
pub fn windows_helper() {}

pub fn shared_api() {}

pub fn unused_api() {}
