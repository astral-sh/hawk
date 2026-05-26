pub fn entry() {
    internal_helper();
}

pub fn internal_helper() {}

pub struct ProductValue;

pub fn product_value() -> ProductValue {
    ProductValue
}

pub trait ProductContext {
    type Options;
}

pub struct Context;

pub struct ContextOptions;

pub type ContextOptionsAlias = ContextOptions;

impl ProductContext for Context {
    type Options = ContextOptionsAlias;
}

pub fn product_context() -> Context {
    Context
}

pub trait RefinedBuildContext {
    fn resolve(&self) -> impl std::fmt::Debug;
}

pub struct RefinedBuildDispatch;

#[derive(Debug)]
pub struct RefinedBuildError;

#[allow(refining_impl_trait)]
impl RefinedBuildContext for RefinedBuildDispatch {
    fn resolve(&self) -> Result<(), RefinedBuildError> {
        Err(RefinedBuildError)
    }
}

pub fn refined_build_dispatch() -> RefinedBuildDispatch {
    RefinedBuildDispatch
}

trait PrivateContext {
    type Options;
}

struct PrivateContextValue;

pub struct PrivateContextOptions;

impl PrivateContext for PrivateContextValue {
    type Options = PrivateContextOptions;
}

pub fn exercise_private_context() {
    let _ = PrivateContextOptions;
}

mod exported {
    pub struct ReexportedValue;
}

pub use exported::ReexportedValue;

pub fn exercise_reexported_value() {
    let _ = exported::ReexportedValue;
}

pub struct TypeCheckedAcrossCrates;

pub trait PublicRenderer {
    fn render(&self) -> PublicRenderResult {
        PublicRenderResult
    }
}

pub struct PublicRenderResult;

pub struct PublicRendererValue;

impl PublicRenderer for PublicRendererValue {}

pub trait InternalRenderer {
    fn render(&self) -> InternalRenderResult {
        InternalRenderResult
    }
}

pub struct InternalRenderResult;

struct InternalRendererValue;

impl InternalRenderer for InternalRendererValue {}

pub fn exercise_internal_trait() {
    let _ = InternalRendererValue.render();
}

pub struct InternalNamespace;

impl InternalNamespace {
    pub fn live_inside_crate() {}

    pub fn dead_method() {}
}

pub fn use_namespace() {
    InternalNamespace::live_inside_crate();
}

pub struct ConstructedTuple(u8);

pub enum ConstructedEnum {
    Active,
}

pub union DeadUnion {
    pub value: u8,
}

pub fn exercise_constructors() {
    let tuple = ConstructedTuple(1);
    let ConstructedTuple(value) = tuple;
    let _ = value;
    let _ = ConstructedEnum::Active;
}

mod export_target {
    pub fn through_reexport() {}
}

pub use export_target::through_reexport;

pub fn dead_entry() {
    dead_helper();
}

pub fn dead_helper() {}

#[allow(dead_code)]
pub fn dead_code_allowed_entry() {
    dead_code_allowed_helper();
}

pub fn dead_code_allowed_helper() {}
