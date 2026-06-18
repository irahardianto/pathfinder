use super::*;
use serde_json::json;

#[tokio::test]
async fn test_parse_definition_response_null() {
    let result = parse_definition_response(json!(null), Path::new("/")).await;
    assert!(result.expect("should not err").is_none());
}

#[tokio::test]
async fn test_parse_definition_response_location() {
    let response = json!({
        "uri": "file:///home/user/project/src/auth.rs",
        "range": {
            "start": { "line": 41, "character": 4 },
            "end":   { "line": 41, "character": 9 }
        }
    });
    let result = parse_definition_response(response, Path::new("/"))
        .await
        .expect("ok");
    let loc = result.expect("some location");
    assert_eq!(loc.line, 42);
    assert_eq!(loc.column, 5);
    assert!(loc.file.contains("auth.rs"));
}

#[tokio::test]
async fn test_parse_definition_response_array() {
    let response = json!([{
        "uri": "file:///project/src/lib.rs",
        "range": {
            "start": { "line": 9, "character": 0 },
            "end":   { "line": 9, "character": 5 }
        }
    }]);
    let result = parse_definition_response(response, Path::new("/"))
        .await
        .expect("ok");
    let loc = result.expect("some location");
    assert_eq!(loc.line, 10);
    assert!(loc.file.contains("lib.rs"));
}

#[tokio::test]
async fn test_parse_definition_response_location_link() {
    let response = json!({
        "targetUri": "file:///project/src/types.rs",
        "targetRange": {
            "start": { "line": 19, "character": 0 },
            "end":   { "line": 25, "character": 1 }
        },
        "targetSelectionRange": {
            "start": { "line": 19, "character": 4 },
            "end":   { "line": 19, "character": 9 }
        }
    });
    let result = parse_definition_response(response, Path::new("/"))
        .await
        .expect("ok");
    let loc = result.expect("some location");
    assert_eq!(loc.line, 20);
    assert!(loc.file.contains("types.rs"));
}

#[tokio::test]
async fn test_parse_definition_empty_array() {
    let response = json!([]);
    let result = parse_definition_response(response, Path::new("/"))
        .await
        .expect("ok");
    assert!(result.is_none());
}

#[test]
fn test_parse_call_hierarchy_prepare_null() {
    let result = parse_call_hierarchy_prepare_response(&json!(null), Path::new("/workspace"));
    assert!(result.expect("ok").is_empty());
}

#[test]
fn test_parse_call_hierarchy_prepare_success() {
    let temp = std::env::temp_dir().join("pathfinder_ch_test");
    let _ = std::fs::create_dir_all(&temp);
    let file_path = temp.join("src/main.rs");
    std::fs::create_dir_all(temp.join("src")).ok();
    std::fs::write(&file_path, "fn main() {}").ok();

    let file_uri = Url::from_file_path(&file_path).unwrap().to_string();
    let response = json!([{
        "name": "main",
        "kind": 12,
        "detail": "fn()",
        "uri": file_uri,
        "selectionRange": {
            "start": { "line": 0, "character": 2 },
            "end": { "line": 0, "character": 6 }
        }
    }]);

    let result = parse_call_hierarchy_prepare_response(&response, &temp).expect("ok");
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].name, "main");
    assert_eq!(result[0].kind, "function");
    assert_eq!(result[0].line, 1);
    assert_eq!(result[0].column, 3);
    assert_eq!(result[0].detail.as_deref(), Some("fn()"));
    assert!(result[0].data.is_some());

    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn test_parse_call_hierarchy_prepare_kind_mapping() {
    let temp = tempfile::tempdir().expect("temp dir");
    let file_uri = Url::from_file_path(temp.path().join("test.rs"))
        .unwrap()
        .to_string();
    for (kind_int, expected) in [
        (5, "class"),
        (6, "method"),
        (11, "interface"),
        (12, "function"),
        (99, "symbol"),
    ] {
        let response = json!([{
            "name": "item",
            "kind": kind_int,
            "uri": file_uri,
            "selectionRange": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 0, "character": 4 }
            }
        }]);
        let result = parse_call_hierarchy_prepare_response(&response, temp.path()).expect("ok");
        assert_eq!(
            result[0].kind, expected,
            "kind {kind_int} should map to {expected}"
        );
    }
}

#[test]
fn test_parse_call_hierarchy_prepare_not_array() {
    let result =
        parse_call_hierarchy_prepare_response(&json!({"foo": "bar"}), Path::new("/workspace"));
    assert!(result.is_err());
}

#[test]
fn test_parse_call_hierarchy_prepare_response_invalid_uri_fallback() {
    let response = json!([{
        "name": "main",
        "kind": 12,
        "detail": "fn()",
        "uri": "invalid-uri",
        "selectionRange": {
            "start": { "line": 0, "character": 2 },
            "end": { "line": 0, "character": 6 }
        }
    }]);

    let result =
        parse_call_hierarchy_prepare_response(&response, Path::new("/workspace")).expect("ok");
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].file, "invalid-uri");
}

#[test]
fn test_parse_call_hierarchy_calls_null() {
    let result = parse_call_hierarchy_calls_response(
        &json!(null),
        Path::new("/workspace"),
        "from",
        "fromRanges",
    );
    assert!(result.expect("ok").is_empty());
}

#[test]
fn test_parse_call_hierarchy_calls_incoming() {
    let temp = std::env::temp_dir().join("pathfinder_chi_test");
    let _ = std::fs::create_dir_all(&temp);
    let file_path = temp.join("src/caller.rs");
    std::fs::create_dir_all(temp.join("src")).ok();
    std::fs::write(&file_path, "fn caller() {}").ok();

    let file_uri = Url::from_file_path(&file_path).unwrap().to_string();
    let response = json!([{
        "from": {
            "name": "caller",
            "kind": 12,
            "uri": file_uri,
            "selectionRange": {
                "start": { "line": 0, "character": 2 },
                "end": { "line": 0, "character": 8 }
            }
        },
        "fromRanges": [
            { "start": { "line": 5 }, "end": { "line": 5 } }
        ]
    }]);

    let result =
        parse_call_hierarchy_calls_response(&response, &temp, "from", "fromRanges").expect("ok");

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].item.name, "caller");
    assert_eq!(result[0].call_sites, vec![6]);

    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn test_parse_call_hierarchy_calls_outgoing() {
    let temp = std::env::temp_dir().join("pathfinder_cho_test");
    let _ = std::fs::create_dir_all(&temp);
    let file_path = temp.join("src/callee.rs");
    std::fs::create_dir_all(temp.join("src")).ok();
    std::fs::write(&file_path, "fn callee() {}").ok();

    let file_uri = Url::from_file_path(&file_path).unwrap().to_string();
    let response = json!([{
        "to": {
            "name": "callee",
            "kind": 12,
            "uri": file_uri,
            "selectionRange": {
                "start": { "line": 0, "character": 2 },
                "end": { "line": 0, "character": 8 }
            }
        },
        "fromRanges": [
            { "start": { "line": 10 }, "end": { "line": 10 } }
        ]
    }]);

    let result =
        parse_call_hierarchy_calls_response(&response, &temp, "to", "fromRanges").expect("ok");

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].item.name, "callee");
    assert_eq!(result[0].call_sites, vec![11]);

    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn test_parse_call_hierarchy_calls_not_array() {
    let result = parse_call_hierarchy_calls_response(
        &json!("not array"),
        Path::new("/workspace"),
        "from",
        "fromRanges",
    );
    assert!(result.is_err());
}

#[test]
fn test_parse_call_hierarchy_calls_missing_item_key_skipped() {
    let response = json!([{
        "wrong_key": {
            "name": "x",
            "kind": 12,
            "uri": "file:///test.rs",
            "selectionRange": {"start": {"line": 0, "character": 0 }, "end": {"line": 0, "character": 1}}
        }
    }]);
    let result = parse_call_hierarchy_calls_response(
        &response,
        Path::new("/workspace"),
        "from",
        "fromRanges",
    )
    .expect("ok");
    assert!(
        result.is_empty(),
        "entry without 'from' key should be skipped"
    );
}

#[tokio::test]
async fn test_parse_references_response_with_locations() {
    let temp = tempfile::tempdir().expect("temp dir");
    let workspace_root = temp.path();
    let src_dir = workspace_root.join("src");
    std::fs::create_dir_all(&src_dir).expect("create src dir");
    let file_path = src_dir.join("lib.rs");
    std::fs::write(&file_path, "pub fn helper() {}").expect("write test file");

    let file_uri = Url::from_file_path(&file_path).unwrap().to_string();

    let response = json!([{
        "uri": file_uri,
        "range": {
            "start": { "line": 0, "character": 8 },
            "end": { "line": 0, "character": 14 }
        }
    }]);

    let result = parse_references_response(&response, workspace_root)
        .await
        .expect("should parse successfully");

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].file, "src/lib.rs");
    assert_eq!(result[0].line, 1);
    assert_eq!(result[0].column, 9);
    assert!(result[0].snippet.contains("helper"));
}

#[tokio::test]
async fn test_parse_references_response_null_returns_empty() {
    let result = parse_references_response(&json!(null), Path::new("/workspace"))
        .await
        .expect("ok");
    assert!(
        result.is_empty(),
        "null response should return empty vector"
    );
}

#[tokio::test]
async fn test_parse_references_response_invalid_uri_returns_error() {
    let response = json!([{
        "uri": "not-a-valid-uri",
        "range": {
            "start": { "line": 5, "character": 0 },
            "end": { "line": 5, "character": 10 }
        }
    }]);

    let result = parse_references_response(&response, Path::new("/workspace")).await;
    assert!(
        result.is_err(),
        "invalid URI should return error, not empty vector"
    );
    if let Err(LspError::Protocol(msg)) = result {
        assert!(
            msg.contains("invalid URI"),
            "error should mention invalid URI"
        );
    } else {
        panic!("expected Protocol error for invalid URI");
    }
}

#[tokio::test]
async fn test_parse_references_response_large_line() {
    let temp = tempfile::tempdir().expect("temp dir");
    let workspace_root = temp.path();
    let src_dir = workspace_root.join("src");
    std::fs::create_dir_all(&src_dir).expect("create src dir");
    let file_path = src_dir.join("lib.rs");
    std::fs::write(&file_path, "pub fn helper() {}").expect("write test file");

    let file_uri = Url::from_file_path(&file_path).unwrap().to_string();

    let response = json!([{
        "uri": file_uri,
        "range": {
            "start": { "line": u64::MAX, "character": 8 },
            "end": { "line": u64::MAX, "character": 14 }
        }
    }]);

    let result = parse_references_response(&response, workspace_root)
        .await
        .expect("should parse successfully");

    assert_eq!(result.len(), 1);
    // Since try_from(u64::MAX.saturating_add(1)) overflows u32, line will fall back to 1.
    assert_eq!(result[0].line, 1);
}

#[test]
fn test_resolve_relative_path_with_file_uri() {
    let temp = tempfile::tempdir().expect("temp dir");
    let file_path = temp.path().join("src/lib.rs");
    std::fs::create_dir_all(temp.path().join("src")).ok();
    std::fs::write(&file_path, "").ok();

    let uri = Url::from_file_path(&file_path).unwrap().to_string();
    let result = resolve_relative_path(&uri, temp.path(), &uri);
    assert_eq!(result, "src/lib.rs");
}

#[test]
fn test_resolve_relative_path_invalid_uri() {
    let result = resolve_relative_path("not-a-uri", Path::new("/workspace"), "not-a-uri");
    assert_eq!(result, "not-a-uri");
}

#[test]
fn test_resolve_relative_path_outside_workspace() {
    let temp = tempfile::tempdir().expect("temp dir");
    let outside = tempfile::tempdir().expect("temp dir 2");
    let file_path = outside.path().join("lib.rs");
    std::fs::write(&file_path, "").ok();

    let uri = Url::from_file_path(&file_path).unwrap().to_string();
    let result = resolve_relative_path(&uri, temp.path(), &uri);
    assert!(
        result.contains("lib.rs"),
        "should fall back to full path when outside workspace"
    );
}

#[tokio::test]
async fn test_parse_references_response_skips_non_file_uri() {
    let temp = tempfile::tempdir().expect("temp dir");
    let workspace_root = temp.path();
    let src_dir = workspace_root.join("src");
    std::fs::create_dir_all(&src_dir).expect("create src dir");
    let file_path = src_dir.join("lib.rs");
    std::fs::write(&file_path, "pub fn helper() {}").expect("write test file");

    let file_uri = Url::from_file_path(&file_path).unwrap().to_string();

    let response = json!([
        {
            "uri": "jdt://contents/foo/bar/Baz.class",
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 0, "character": 5 }
            }
        },
        {
            "uri": file_uri,
            "range": {
                "start": { "line": 0, "character": 8 },
                "end": { "line": 0, "character": 14 }
            }
        }
    ]);

    let result = parse_references_response(&response, workspace_root)
        .await
        .expect("should parse successfully");

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].file, "src/lib.rs");
}

#[tokio::test]
async fn test_read_preview_line_bounds() {
    let temp = tempfile::tempdir().expect("temp dir");
    let file_path = temp.path().join("long_line.txt");
    // Write 20KB line of 'a's
    let long_line = "a".repeat(20000);
    std::fs::write(&file_path, &long_line).expect("write");

    let snippet = read_preview_line(&file_path, 0).await;
    // The snippet should be bounded (<= 512 bytes)
    assert!(snippet.len() <= 512);
    assert!(snippet.chars().all(|c| c == 'a'));
}
