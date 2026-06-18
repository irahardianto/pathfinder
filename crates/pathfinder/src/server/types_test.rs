use super::*;

#[test]
fn test_default_value_helpers() {
    assert_eq!(default_path_glob(), "**/*");
    assert_eq!(default_max_results(), 50);
    assert_eq!(default_context_lines(), 2);
    assert_eq!(default_repo_map_path(), ".");
    assert_eq!(default_max_tokens(), 16_000);
    assert_eq!(default_max_tokens_per_file(), 2_000);
    assert_eq!(default_max_depth(), 3);
    assert_eq!(default_max_references(), 50);
    assert_eq!(default_max_dependencies(), 50);
    assert_eq!(default_start_line(), 1);
    assert_eq!(default_max_lines(), 500);
    assert_eq!(default_detail_level(), "compact");
    assert!(default_group_by_file());
    assert_eq!(default_filter_mode(), FilterMode::CodeOnly);
}

#[test]
fn test_filepath_alias_deserialization() {
    let json_data = serde_json::json!({
        "path": "src/lib.rs",
        "start_line": 10,
    });
    let read_params: ReadParams = serde_json::from_value(json_data).unwrap();
    assert_eq!(read_params.filepath, Some("src/lib.rs".to_string()));
}
