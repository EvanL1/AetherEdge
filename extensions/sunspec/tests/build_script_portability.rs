#[test]
fn sunspec_catalog_uses_manifest_relative_include_paths() {
    let generated = include_str!(concat!(env!("OUT_DIR"), "/sunspec_models.rs"));
    let model_entries = generated
        .lines()
        .filter(|line| line.contains("include_str!"))
        .collect::<Vec<_>>();

    assert!(!model_entries.is_empty(), "generated catalog is empty");
    assert!(
        model_entries.iter().all(|line| line
            .contains(r#"include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/models/json/model_"#)),
        "embedded model paths must resolve from the current checkout: {model_entries:#?}"
    );
    assert!(
        !generated.contains(env!("CARGO_MANIFEST_DIR")),
        "generated Rust must not retain an absolute build-host path"
    );
}
