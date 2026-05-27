fn main() {
    library::entry();
    let _ = library::product_value();
    let _ = library::product_context();
    let _ = library::RefinedBuildContext::resolve(&library::refined_build_dispatch());
    library::exercise_private_context();
    library::exercise_reexported_value();
    library::use_namespace();
    library::exercise_constructors();
    library::through_reexport();
    library::exercise_internal_trait();
    library::exercise_fields();
    let fields = library::ProductFields {
        used_across_crates: 1,
    };
    let _ = fields.used_across_crates;
    let _ = std::mem::offset_of!(library::OffsetFields, used_by_offset_of);
    let _ = library::exposed_payload_field().payload;
    let _ = unsafe { library::exposed_payload_union().payload };
    let _ = library::ProductConstants::USED_ACROSS_CRATES;
    match library::product_enum() {
        library::ProductEnum::UsedAcrossCrates => {}
        _ => {}
    }
    library::exercise_internal_public_modules();
    library::consumed_outer::consumed_nested::invoke();
    library::exercise_mirrored_source();
    let _ = library::archived_mirrored_fields().required_through_archive;
    unit_support::product_entry();
}

#[allow(dead_code)]
fn typechecked_cross_crate_references() {
    let _ = library::TypeCheckedAcrossCrates;
    let _ = library::PublicRenderer::render(&library::PublicRendererValue);
    let _ = library::TypecheckedExportPath;
}
