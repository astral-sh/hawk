use std::rc::Rc;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct FrozenSet(Vec<u8>);

impl FrozenSet {
    fn iter(&self) -> std::slice::Iter<'_, u8> {
        self.0.iter()
    }
}

#[derive(Debug)]
pub struct SemanticIndex {
    imported_modules: Arc<FrozenSet>,
}

impl SemanticIndex {
    fn imported_module_count(&self) -> usize {
        self.imported_modules.iter().count()
    }
}

struct LocalIndex {
    imported_modules: Rc<FrozenSet>,
}

impl LocalIndex {
    fn imported_module_count(&self) -> usize {
        self.imported_modules.iter().count()
    }
}

struct SharedBeforeStorage {
    imported_modules: Arc<FrozenSet>,
}

impl SharedBeforeStorage {
    fn imported_module_count(&self) -> usize {
        self.imported_modules.iter().count()
    }
}

struct WrapperEscapes {
    imported_modules: Arc<FrozenSet>,
}

fn arc_module_count(modules: &Arc<FrozenSet>) -> usize {
    modules.iter().count()
}

impl WrapperEscapes {
    fn imported_module_count(&self) -> usize {
        arc_module_count(&self.imported_modules)
    }
}

#[derive(Clone)]
struct CloneOwner {
    imported_modules: Arc<FrozenSet>,
}

impl CloneOwner {
    fn imported_module_count(&self) -> usize {
        self.imported_modules.iter().count()
    }
}

#[repr(C)]
struct LayoutSensitive {
    imported_modules: Arc<FrozenSet>,
}

impl LayoutSensitive {
    fn imported_module_count(&self) -> usize {
        self.imported_modules.iter().count()
    }
}

pub struct PublicField {
    pub imported_modules: Arc<FrozenSet>,
}

impl PublicField {
    fn imported_module_count(&self) -> usize {
        self.imported_modules.iter().count()
    }
}

struct Destructured {
    imported_modules: Arc<FrozenSet>,
}

impl Destructured {
    fn imported_module_count(&self) -> usize {
        self.imported_modules.iter().count()
    }
}

struct MacroConstructed {
    imported_modules: Arc<FrozenSet>,
}

impl MacroConstructed {
    fn imported_module_count(&self) -> usize {
        self.imported_modules.iter().count()
    }
}

macro_rules! macro_constructed {
    () => {
        MacroConstructed {
            imported_modules: Arc::new(FrozenSet(vec![10])),
        }
    };
}

struct SharedInTests {
    imported_modules: Arc<FrozenSet>,
}

impl SharedInTests {
    fn imported_module_count(&self) -> usize {
        self.imported_modules.iter().count()
    }
}

pub fn exercise() -> usize {
    let semantic = SemanticIndex {
        imported_modules: Arc::new(FrozenSet(vec![1, 2])),
    };
    let local = LocalIndex {
        imported_modules: Rc::new(FrozenSet(vec![3, 4])),
    };

    let shared = Arc::new(FrozenSet(vec![5]));
    let shared_before_storage = SharedBeforeStorage {
        imported_modules: Arc::clone(&shared),
    };
    let wrapper_escapes = WrapperEscapes {
        imported_modules: Arc::new(FrozenSet(vec![6])),
    };
    let clone_owner = CloneOwner {
        imported_modules: Arc::new(FrozenSet(vec![7])),
    };
    let layout_sensitive = LayoutSensitive {
        imported_modules: Arc::new(FrozenSet(vec![8])),
    };
    let public_field = PublicField {
        imported_modules: Arc::new(FrozenSet(vec![9])),
    };
    let destructured = Destructured {
        imported_modules: Arc::new(FrozenSet(vec![10])),
    };
    let destructured_count = destructured.imported_module_count();
    let Destructured { imported_modules } = destructured;
    let _destructured_modules = Arc::clone(&imported_modules);
    let macro_constructed = macro_constructed!();
    let shared_in_tests = SharedInTests {
        imported_modules: Arc::new(FrozenSet(vec![11])),
    };

    semantic.imported_module_count()
        + local.imported_module_count()
        + shared_before_storage.imported_module_count()
        + wrapper_escapes.imported_module_count()
        + clone_owner.imported_module_count()
        + layout_sensitive.imported_module_count()
        + public_field.imported_module_count()
        + destructured_count
        + macro_constructed.imported_module_count()
        + shared_in_tests.imported_module_count()
}

#[cfg(test)]
mod tests {
    use super::{Arc, FrozenSet, SharedInTests};

    #[test]
    fn shares_test_field() {
        let index = SharedInTests {
            imported_modules: Arc::new(FrozenSet(vec![1])),
        };
        let _modules = Arc::clone(&index.imported_modules);
    }
}
