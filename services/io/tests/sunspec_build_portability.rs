#[test]
fn sunspec_catalog_uses_manifest_relative_include_paths() {
    let build_script = include_str!("../build.rs");

    assert!(
        build_script.contains("concat!(env!(\\\"CARGO_MANIFEST_DIR\\\")"),
        "embedded model paths must resolve from the current checkout at compile time"
    );
    assert!(
        !build_script.contains("to_string_lossy"),
        "generated Rust must not retain an absolute build-host path"
    );
}
