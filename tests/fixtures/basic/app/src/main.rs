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
}

#[allow(dead_code)]
fn typechecked_cross_crate_references() {
    let _ = library::TypeCheckedAcrossCrates;
    let _ = library::PublicRenderer::render(&library::PublicRendererValue);
}
