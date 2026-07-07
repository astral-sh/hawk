use external_trait::ExternalDispatch;
use std::fmt;

struct Formatted;

impl fmt::Display for Formatted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        display_helper();
        formatter.write_str("formatted")
    }
}

pub fn display_helper() {}

trait StaticDispatch {
    fn run(&self);
}

struct StaticUsed;

impl StaticDispatch for StaticUsed {
    fn run(&self) {
        static_used_helper();
    }
}

struct StaticUnused;

impl StaticDispatch for StaticUnused {
    fn run(&self) {
        static_unused_helper();
    }
}

pub fn static_used_helper() {}

pub fn static_unused_helper() {}

trait GenericDispatch {
    fn run(&self);
}

struct GenericUsed;

impl GenericDispatch for GenericUsed {
    fn run(&self) {
        generic_used_helper();
    }
}

struct GenericOther;

impl GenericDispatch for GenericOther {
    fn run(&self) {
        generic_other_helper();
    }
}

pub fn generic_used_helper() {}

pub fn generic_other_helper() {}

fn call_generic<T: GenericDispatch>(value: &T) {
    value.run();
}

trait DynamicDispatch {
    fn run(&self);
}

struct DynamicUsed;

impl DynamicDispatch for DynamicUsed {
    fn run(&self) {
        dynamic_used_helper();
    }
}

struct DynamicOther;

impl DynamicDispatch for DynamicOther {
    fn run(&self) {
        dynamic_other_helper();
    }
}

pub fn dynamic_used_helper() {}

pub fn dynamic_other_helper() {}

fn call_dynamic(value: &dyn DynamicDispatch) {
    value.run();
}

trait DefaultDispatch {
    fn run(&self) {
        default_helper();
    }
}

struct DefaultUsed;

impl DefaultDispatch for DefaultUsed {}

pub fn default_helper() {}

trait EntirelyUnusedDispatch {
    fn run(&self);
}

struct EntirelyUnused;

impl EntirelyUnusedDispatch for EntirelyUnused {
    fn run(&self) {
        unused_trait_helper();
    }
}

pub fn unused_trait_helper() {}

pub trait ExportedDispatch {
    fn run(&self);
}

struct ExportedUnused;

impl ExportedDispatch for ExportedUnused {
    fn run(&self) {
        exported_trait_helper();
    }
}

pub fn exported_trait_helper() {}

struct ExternalUsed;

impl ExternalDispatch for ExternalUsed {
    fn run(&self) {
        external_helper();
    }
}

pub fn external_helper() {}

trait ConstantDispatch {
    const VALUE: u8;
}

struct ConstantUsed;

impl ConstantDispatch for ConstantUsed {
    const VALUE: u8 = constant_used_helper();
}

struct ConstantUnused;

impl ConstantDispatch for ConstantUnused {
    const VALUE: u8 = constant_unused_helper();
}

pub const fn constant_used_helper() -> u8 {
    1
}

pub const fn constant_unused_helper() -> u8 {
    2
}

struct Dropped;

impl Drop for Dropped {
    fn drop(&mut self) {
        drop_helper();
    }
}

pub fn drop_helper() {}

pub fn entry() {
    let _ = format!("{Formatted}");
    StaticDispatch::run(&StaticUsed);
    call_generic(&GenericUsed);
    call_dynamic(&DynamicUsed);
    DefaultUsed.run();
    external_caller::call_external(&ExternalUsed);
    let _ = <ConstantUsed as ConstantDispatch>::VALUE;
    let _dropped = Dropped;
}
