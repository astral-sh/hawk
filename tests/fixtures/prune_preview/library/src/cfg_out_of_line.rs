#[cfg(not(test))]
fn cfg_dependent_child() {}

#[cfg(test)]
fn cfg_dependent_child() {}

#[cfg(windows)]
fn target_stripped_child() {}
