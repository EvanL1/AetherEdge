#[path = "../build_support.rs"]
mod build_support;

#[test]
fn windows_paths_are_escaped_as_valid_rust_string_literals() {
    let path = r"D:\a\AetherEdge\libs\aether-model\model_1.json";

    assert_eq!(
        build_support::rust_string_literal(path),
        r#""D:\\a\\AetherEdge\\libs\\aether-model\\model_1.json""#
    );
}
