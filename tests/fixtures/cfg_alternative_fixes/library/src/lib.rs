#[cfg(not(test))]
pub fn dual() {}

#[cfg(test)]
pub fn dual() {}

#[cfg(test)]
mod tests {
    #[test]
    fn uses_test_only_declaration() {
        super::dual();
    }
}
