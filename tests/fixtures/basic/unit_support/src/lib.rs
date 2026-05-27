pub fn product_entry() {}

#[allow(dead_code)]
mod hidden_production {
    pub fn not_exported() {}
}

#[cfg(test)]
pub fn test_entry() {
    test_only_helper();
}

#[cfg(test)]
pub fn test_only_helper() {}

#[cfg(test)]
mod tests {
    #[test]
    fn exercises_test_surface() {
        super::test_entry();
    }
}
