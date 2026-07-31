use std::arch::global_asm;

global_asm!("/* {} */", sym global_asm_callback);

pub fn entry() {}

pub fn global_asm_callback() {
    global_asm_helper();
}

pub fn global_asm_helper() {}

#[unsafe(no_mangle)]
pub extern "C" fn exported_callback() {}

#[unsafe(export_name = "renamed_symbol")]
pub extern "C" fn renamed_callback() {}

#[used]
pub static RETAINED_REGISTRATION: fn() = retained_callback;

pub fn retained_callback() {
    retained_helper();
}

pub fn retained_helper() {}

pub static UNRETAINED_REGISTRATION: fn() = unretained_callback;

pub fn unretained_callback() {
    unretained_helper();
}

pub fn unretained_helper() {}

macro_rules! register_callback {
    ($callback:path) => {
        const _: () = {
            #[used]
            static REGISTRATION: fn() = $callback;
        };
    };
}

register_callback!(macro_registered_callback);

pub fn macro_registered_callback() {}
