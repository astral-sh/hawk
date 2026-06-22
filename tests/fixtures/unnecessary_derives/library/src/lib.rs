use std::fmt::Debug;
use std::hash::Hash;

#[derive(Debug)]
pub struct UnusedDebug;

#[derive(Debug)]
pub struct CrossCrateDebug;

#[derive(Default)]
pub struct UnusedDefault;

#[derive(Default)]
pub enum UnusedDefaultEnum {
    #[default]
    Default,
}

#[derive(Clone)]
pub struct UnusedClone;

#[derive(Debug, Clone)]
pub struct GroupedUnused;

#[derive(Clone, Debug)]
struct PartiallyUsedDerives;

#[derive(Clone)]
pub struct CrossCrateOuter(dependency::Inner);

#[derive(Clone)]
struct SameCrateInner;

#[derive(Clone)]
pub struct SameCrateOuter(SameCrateInner);

#[derive(Hash)]
pub struct UnusedHash;

#[derive(PartialEq)]
pub struct UnusedPartialEq;

#[derive(Eq, PartialEq)]
pub struct UnusedEq;

#[derive(PartialOrd, PartialEq)]
pub struct UnusedPartialOrd;

#[derive(Ord, Eq, PartialOrd, PartialEq)]
pub struct UnusedOrd;

#[derive(Debug)]
struct UsedDebug;

#[derive(Default)]
struct UsedDefault;

#[derive(Clone)]
struct UsedClone;

#[derive(Clone)]
struct DeadSourceClone;

#[derive(Hash)]
struct UsedHash;

#[derive(Eq, PartialEq)]
struct UsedEq;

#[derive(PartialOrd, PartialEq)]
struct UsedPartialOrd;

#[derive(Ord, Eq, PartialOrd, PartialEq)]
struct UsedOrd;

#[derive(Debug)]
struct InnerDebug;

#[derive(Debug)]
struct OuterDebug(InnerDebug);

#[derive(Debug)]
struct TraitObjectDebug;

#[derive(Clone, Copy)]
struct CopyRequiresClone;

#[derive(Debug)]
struct OpaqueDebug;

#[derive(Debug)]
struct AssociatedDebug;

#[derive(Debug)]
struct GenericBoundDebug;

#[derive(Debug)]
struct FunctionPointerDebug;

#[derive(Eq, PartialEq)]
struct SupertraitEq;

#[derive(Ord, Eq, PartialOrd, PartialEq)]
struct CollectedOrd;

trait RequiresDebug {
    type Item: Debug;
}

struct AssociatedHolder;

struct RequiresDebugBound<T: Debug>(T);

enum UnitVariantConstructor {
    Variant,
}

impl UnitVariantConstructor {
    fn construct() -> Self {
        Self::Variant
    }
}

impl RequiresDebug for AssociatedHolder {
    type Item = AssociatedDebug;
}

trait RequiresEq: Eq {}

impl RequiresEq for SupertraitEq {}

fn require_hash<T: Hash>(value: &T) {
    value.hash(&mut std::collections::hash_map::DefaultHasher::new());
}

fn require_eq<T: Eq>(_: &T) {}

fn require_debug<T: Debug>(_: T) {}

fn opaque_debug() -> impl Debug {
    OpaqueDebug
}

fn dead_source_use() {
    let _ = DeadSourceClone.clone();
}

fn dead_generic_type_use() {
    let _: Option<RequiresDebugBound<GenericBoundDebug>> = None;
}

fn dead_function_pointer_use() {
    let _ = require_debug::<FunctionPointerDebug> as fn(FunctionPointerDebug);
}

fn dead_collect_use() {
    let _ = [CollectedOrd].into_iter().collect::<std::collections::BTreeSet<_>>();
}

pub fn exercise_used_derives() {
    let debug = UsedDebug;
    let _ = format!("{debug:?}");
    let _ = UsedDefault::default();
    let _ = UsedClone.clone();
    let _ = PartiallyUsedDerives.clone();
    require_hash(&UsedHash);
    require_eq(&UsedEq);
    let _ = UsedEq == UsedEq;
    let _ = UsedPartialOrd < UsedPartialOrd;
    let _ = UsedOrd.cmp(&UsedOrd);
    let _ = format!("{:?}", OuterDebug(InnerDebug));
    let _: &dyn Debug = &TraitObjectDebug;
    let _ = CopyRequiresClone;
    let _ = opaque_debug();
    let _ = std::mem::size_of::<AssociatedHolder>();
    let _ = std::mem::size_of::<SupertraitEq>();
    let _ = UnitVariantConstructor::construct();
}

#[cfg(test)]
mod tests {
    #[derive(Debug)]
    pub(super) struct TestOnlyDebug;

    #[test]
    fn uses_debug() {
        let _ = format!("{:?}", TestOnlyDebug);
    }
}
