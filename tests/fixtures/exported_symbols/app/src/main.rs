unsafe extern "C" {
    fn exported_callback();
    fn renamed_symbol();
}

fn main() {
    library::entry();
    unsafe {
        exported_callback();
        renamed_symbol();
    }
}
