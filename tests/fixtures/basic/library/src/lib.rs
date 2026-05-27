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
    pub const LIVE_VALUE: u8 = 1;

    pub const DEAD_VALUE: u8 = 2;

    pub fn live_inside_crate() {}

    pub fn dead_method() {}
}

pub fn use_namespace() {
    let _ = InternalNamespace::LIVE_VALUE;
    InternalNamespace::live_inside_crate();
}

pub struct InternalFields {
    pub constructed: u8,
    pub projected: u8,
}

pub struct InternalTupleFields(pub u8);

pub struct DeadFields {
    pub unused: u8,
}

pub fn exercise_fields() {
    let fields = InternalFields {
        constructed: 1,
        projected: 2,
    };
    let InternalFields { constructed, .. } = fields;
    let _ = (constructed, fields.projected);
    let _ = InternalTupleFields(3);
}

pub struct ProductFields {
    pub used_across_crates: u8,
}

pub struct ProductConstants;

impl ProductConstants {
    pub const USED_ACROSS_CRATES: u8 = 1;
}

pub struct ConstructedTuple(u8);

pub enum ConstructedEnum {
    Active,
    Dead,
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

pub enum ProductEnum {
    UsedAcrossCrates,
    UsedInternally,
    Unused,
}

pub fn product_enum() -> ProductEnum {
    let _ = ProductEnum::UsedInternally;
    ProductEnum::UsedAcrossCrates
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

pub struct OffsetFields {
    pub used_by_offset_of: u8,
}

pub struct FieldPayload;

pub struct ExposedPayloadField {
    pub payload: FieldPayload,
}

pub fn exposed_payload_field() -> ExposedPayloadField {
    ExposedPayloadField {
        payload: FieldPayload,
    }
}

#[derive(Clone, Copy)]
pub struct UnionFieldPayload;

pub union ExposedPayloadUnion {
    pub payload: UnionFieldPayload,
}

pub fn exposed_payload_union() -> ExposedPayloadUnion {
    ExposedPayloadUnion {
        payload: UnionFieldPayload,
    }
}

mod dead_export_target {
    pub fn dead_export_path() {}
}

pub use dead_export_target::dead_export_path;

mod typechecked_export_target {
    pub struct TypecheckedExportPath;
}

pub use typechecked_export_target::TypecheckedExportPath;

pub mod internal_outer {
    pub mod internal_nested {
        pub(crate) fn invoke() {}
    }
}

pub fn exercise_internal_public_modules() {
    internal_outer::internal_nested::invoke();
}

pub mod consumed_outer {
    pub mod consumed_nested {
        pub fn invoke() {}
    }
}

pub mod dead_outer {
    pub mod dead_nested {}
}

pub mod alias_namespace {
    pub use crate::export_target::through_reexport as nested_through_reexport;
}

#[derive(macro_dep::ArchiveMirror)]
pub struct MirroredFields {
    pub required_through_archive: u8,
}

pub fn exercise_mirrored_source() {
    let _ = MirroredFields {
        required_through_archive: 1,
    };
}

pub fn archived_mirrored_fields() -> ArchivedMirroredFields {
    ArchivedMirroredFields {
        required_through_archive: 1,
    }
}

pub fn integration_test_support() {
    test_only_helper();
}

pub fn test_only_helper() {}

// Keep an unrelated machine-applicable rustc suggestion in the fix fixture.
use std::fmt::Debug;
