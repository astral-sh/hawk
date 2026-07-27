#[cfg(feature = "extra")]
pub fn extra_api() {
    extra_helper();
}

#[cfg(feature = "extra")]
pub fn extra_helper() {}

pub fn fallback_api() {}

pub fn unused_api() {}
