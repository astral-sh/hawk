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

#[cfg(not(test))]
pub fn cfg_dependent() {}

#[cfg(test)]
pub fn cfg_dependent() {}
