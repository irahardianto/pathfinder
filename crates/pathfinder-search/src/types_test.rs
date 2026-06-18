use super::*;

#[test]
fn test_search_params_default_values() {
    let d = SearchParams::default();
    assert_eq!(d.workspace_root, PathBuf::from("."));
    assert_eq!(d.max_results, 50);
    assert_eq!(d.path_glob, "**/*");
    assert_eq!(d.context_lines, 2);
    assert_eq!(d.offset, 0);
    assert!(!d.is_regex);
    assert!(d.query.is_empty());
    assert!(d.exclude_glob.is_empty());
}

#[test]
fn test_search_match_serde_roundtrip() {
    let original = SearchMatch {
        file: "src/main.rs".to_owned(),
        line: 42,
        column: 5,
        content: "fn main() {}".to_owned(),
        context_before: vec!["// comment".to_owned()],
        context_after: vec![String::new()],
        enclosing_semantic_path: Some("main".to_owned()),
        is_definition: Some(true),
        version_hash: "abc1234".to_owned(),
        known: Some(false),
    };
    let json = serde_json::to_string(&original).expect("serialize");
    let deserialized: SearchMatch = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(original, deserialized);
}

#[test]
fn test_search_match_skip_serializing_none_fields() {
    let m = SearchMatch {
        file: "src/lib.rs".to_owned(),
        line: 1,
        column: 1,
        content: "pub fn add() {}".to_owned(),
        context_before: vec![],
        context_after: vec![],
        enclosing_semantic_path: None,
        is_definition: None,
        version_hash: "def5678".to_owned(),
        known: None,
    };
    let json = serde_json::to_string(&m).expect("serialize");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse");
    let obj = parsed.as_object().expect("should be object");

    // is_definition and known should be absent due to skip_serializing_if
    assert!(
        !obj.contains_key("is_definition"),
        "is_definition=None should be omitted: {json}"
    );
    assert!(
        !obj.contains_key("known"),
        "known=None should be omitted: {json}"
    );
    // Other fields should still be present
    assert!(obj.contains_key("file"));
    assert!(obj.contains_key("line"));
    assert!(obj.contains_key("version_hash"));
}

#[test]
fn test_search_result_serde_roundtrip() {
    let original = SearchResult {
        matches: vec![SearchMatch {
            file: "src/main.rs".to_owned(),
            line: 10,
            column: 3,
            content: "let x = 1;".to_owned(),
            context_before: vec![],
            context_after: vec![],
            enclosing_semantic_path: None,
            is_definition: None,
            version_hash: "aaa1111".to_owned(),
            known: None,
        }],
        total_matches: 1,
        truncated: false,
        files_searched: 5,
        files_in_scope: 10,
        binary_skipped: 2,
        gitignored_skipped: 3,
        other_skipped: 0,
    };
    let json = serde_json::to_string(&original).expect("serialize");
    let deserialized: SearchResult = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(deserialized.total_matches, original.total_matches);
    assert_eq!(deserialized.truncated, original.truncated);
    assert_eq!(deserialized.files_searched, original.files_searched);
    assert_eq!(deserialized.files_in_scope, original.files_in_scope);
    assert_eq!(deserialized.binary_skipped, original.binary_skipped);
    assert_eq!(deserialized.gitignored_skipped, original.gitignored_skipped);
    assert_eq!(deserialized.other_skipped, original.other_skipped);
    assert_eq!(deserialized.matches.len(), 1);
    assert_eq!(deserialized.matches[0], original.matches[0]);
}
