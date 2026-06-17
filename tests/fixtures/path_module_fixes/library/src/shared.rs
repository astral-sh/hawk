pub struct Shared {
    pub(crate) value: u8,
}

pub(crate) fn exercise() {
    let value = Shared { value: 1 };
    let _ = value.value;
}
