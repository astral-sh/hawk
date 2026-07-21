pub fn live() {}

/// Documentation attached to a removable declaration.
#[deprecated(note = "exercise a source-spanned attribute")]
pub fn documented_dead() {}

pub struct DeadParent {
    pub field: u8,
}

pub mod dead_outer {
    pub mod dead_inner {}
}

pub enum LiveEnum {
    Used,
    Unused,
}

pub fn blocked_by_private_caller() {
    transitively_blocked();
}

pub fn transitively_blocked() {}

fn private_caller() {
    blocked_by_private_caller();
}

pub mod blocked_module {
    pub(crate) fn nested_callee() {}
}

fn caller_outside_blocked_module() {
    blocked_module::nested_callee();
}

pub fn blocked_at_end_boundary() {}fn caller_at_end_boundary() { blocked_at_end_boundary(); }

#[cfg(not(test))]
pub fn cfg_dependent() {}

#[cfg(test)]
pub fn cfg_dependent() {}

pub mod blocked_outer_with_dead_child {
    pub(crate) fn called() {}
    pub mod removable_inner {}
}

fn caller_outside_outer() {
    blocked_outer_with_dead_child::called();
}

pub mod config_protected {
    pub fn retained_child() {}
}

pub mod inner_cfg_module {
    #![cfg(not(test))]
    pub fn cfg_dependent_child() {}
}

pub mod inner_cfg_attr_module {
    #![cfg_attr(test, allow(unused_variables))]
    pub fn cfg_attr_dependent_child() {}
}

pub mod dead_out_of_line;

pub mod cfg_out_of_line;

pub mod registered_callbacks {
    pub extern "C" fn callback() {}

    #[used]
    static CALLBACK: extern "C" fn() = callback;
}

pub fn contains_used_static() {
    #[used]
    static KEEP: [u8; 1] = [1];
}

pub fn contains_allowed_function() {
    #[allow(dead_code)]
    fn keep() {}
}

pub fn contains_expected_function() {
    #[expect(dead_code)]
    fn keep() {}
}
