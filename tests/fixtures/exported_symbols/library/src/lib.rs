pub fn entry() {}

#[unsafe(no_mangle)]
pub extern "C" fn exported_callback() {}

#[unsafe(export_name = "renamed_symbol")]
pub extern "C" fn renamed_callback() {}
