//! Response parsers for LSP JSON-RPC responses.
//!
//! Pure functions that parse LSP protocol responses into domain types.
//! No side effects, no field access on `LspClient`.
//!
//! File preview extraction uses async I/O (`tokio::fs`) to avoid blocking
//! the tokio runtime on filesystem reads during LSP response processing.

use crate::types::{CallHierarchyCall, CallHierarchyItem, ReferenceLocation};
use crate::{DefinitionLocation, LspError};
use std::path::Path;
use url::Url;

fn resolve_relative_path(uri_str: &str, workspace_root: &Path, fallback: &str) -> String {
    Url::parse(uri_str)
        .ok()
        .and_then(|u: Url| u.to_file_path().ok())
        .and_then(|p| {
            p.strip_prefix(workspace_root)
                .ok()
                .map(|rp| rp.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| fallback.to_owned())
}

async fn read_preview_line(abs_path: &Path, line_index: usize) -> String {
    match tokio::fs::read_to_string(abs_path).await {
        Ok(content) => content
            .lines()
            .nth(line_index)
            .map(|l| l.trim().to_owned())
            .unwrap_or_default(),
        Err(_) => String::new(),
    }
}

fn parse_uri_and_range(
    location: &serde_json::Value,
) -> Option<(String, u64, u64, Option<std::path::PathBuf>)> {
    let (uri_str, start_line, start_char) = if location.get("targetUri").is_some() {
        (
            location["targetUri"].as_str().unwrap_or(""),
            location["targetSelectionRange"]["start"]["line"]
                .as_u64()
                .unwrap_or(0),
            location["targetSelectionRange"]["start"]["character"]
                .as_u64()
                .unwrap_or(0),
        )
    } else {
        (
            location["uri"].as_str().unwrap_or(""),
            location["range"]["start"]["line"].as_u64().unwrap_or(0),
            location["range"]["start"]["character"]
                .as_u64()
                .unwrap_or(0),
        )
    };

    if uri_str.is_empty() {
        return None;
    }

    let abs_path = Url::parse(uri_str)
        .ok()
        .and_then(|u: Url| u.to_file_path().ok());

    Some((uri_str.to_owned(), start_line, start_char, abs_path))
}

/// Parse a single LSP definition response into a `DefinitionLocation`.
///
/// # Errors
/// - `LspError::Protocol` — response has no URI
pub async fn parse_definition_response(
    response: serde_json::Value,
    workspace_root: &Path,
) -> Result<Option<DefinitionLocation>, LspError> {
    if response.is_null() {
        return Ok(None);
    }

    let location = if response.is_array() {
        response
            .as_array()
            .and_then(|arr| arr.first())
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    } else {
        response
    };

    if location.is_null() {
        return Ok(None);
    }

    let Some((uri_str, start_line, start_char, abs_path)) = parse_uri_and_range(&location) else {
        return Err(LspError::Protocol(
            "definition response missing URI".to_owned(),
        ));
    };

    let file = resolve_relative_path(&uri_str, workspace_root, &uri_str);

    let preview = if let Some(ref p) = abs_path {
        read_preview_line(p, usize::try_from(start_line).unwrap_or(0)).await
    } else {
        String::new()
    };

    Ok(Some(DefinitionLocation {
        file,
        line: u32::try_from(start_line + 1).unwrap_or(1),
        column: u32::try_from(start_char + 1).unwrap_or(1),
        preview,
    }))
}

/// Parse a single definition location value.
///
/// Returns `None` if the location is null or has no URI.
pub async fn parse_single_definition_location(
    location: &serde_json::Value,
    workspace_root: &Path,
) -> Option<DefinitionLocation> {
    if location.is_null() {
        return None;
    }

    let (uri_str, start_line, start_char, abs_path) = parse_uri_and_range(location)?;

    let file = resolve_relative_path(&uri_str, workspace_root, &uri_str);

    let preview = if let Some(ref p) = abs_path {
        read_preview_line(p, usize::try_from(start_line).unwrap_or(0)).await
    } else {
        String::new()
    };

    Some(DefinitionLocation {
        file,
        line: u32::try_from(start_line + 1).unwrap_or(1),
        column: u32::try_from(start_char + 1).unwrap_or(1),
        preview,
    })
}

/// Parse multiple definition locations from a response.
pub async fn parse_definition_response_multi(
    response: &serde_json::Value,
    workspace_root: &Path,
) -> Vec<DefinitionLocation> {
    if response.is_null() {
        return Vec::new();
    }

    if let Some(items) = response.as_array() {
        let mut result = Vec::with_capacity(items.len());
        for item in items {
            if let Some(loc) = parse_single_definition_location(item, workspace_root).await {
                result.push(loc);
            }
        }
        result
    } else {
        parse_single_definition_location(response, workspace_root)
            .await
            .map(|loc| vec![loc])
            .unwrap_or_default()
    }
}

/// Parse a call hierarchy prepare response into `CallHierarchyItem`s.
///
/// # Errors
/// - `LspError::Protocol` — response is not an array
pub fn parse_call_hierarchy_prepare_response(
    response: &serde_json::Value,
    workspace_root: &Path,
) -> Result<Vec<CallHierarchyItem>, LspError> {
    if response.is_null() {
        return Ok(Vec::new());
    }

    let items = response
        .as_array()
        .ok_or_else(|| LspError::Protocol("expected array".to_owned()))?;

    let mut result = Vec::with_capacity(items.len());
    for item in items {
        let uri_str = item["uri"].as_str().unwrap_or("");
        let file = resolve_relative_path(uri_str, workspace_root, uri_str);

        let line = u32::try_from(
            item["selectionRange"]["start"]["line"]
                .as_u64()
                .unwrap_or(0),
        )
        .unwrap_or(0)
            + 1;
        let column = u32::try_from(
            item["selectionRange"]["start"]["character"]
                .as_u64()
                .unwrap_or(0),
        )
        .unwrap_or(0)
            + 1;

        let kind_int = item["kind"].as_u64().unwrap_or(0);
        let kind = match kind_int {
            5 => "class",
            6 => "method",
            11 => "interface",
            12 => "function",
            _ => "symbol",
        }
        .to_owned();

        result.push(CallHierarchyItem {
            name: item["name"].as_str().unwrap_or("").to_owned(),
            kind,
            detail: item
                .get("detail")
                .and_then(|d| d.as_str())
                .map(ToOwned::to_owned),
            file,
            line,
            column,
            data: Some(item.clone()),
        });
    }

    Ok(result)
}

/// Parse a call hierarchy calls response (incoming or outgoing).
///
/// # Errors
/// - `LspError::Protocol` — response is not an array
pub fn parse_call_hierarchy_calls_response(
    response: &serde_json::Value,
    workspace_root: &Path,
    item_key: &str,
    ranges_key: &str,
) -> Result<Vec<CallHierarchyCall>, LspError> {
    if response.is_null() {
        return Ok(Vec::new());
    }

    let calls = response
        .as_array()
        .ok_or_else(|| LspError::Protocol("expected array".to_owned()))?;

    let mut result = Vec::with_capacity(calls.len());
    for call in calls {
        let Some(item_val) = call.get(item_key) else {
            continue;
        };

        let mut parsed_items = parse_call_hierarchy_prepare_response(
            &serde_json::Value::Array(vec![item_val.clone()]),
            workspace_root,
        )?;
        if parsed_items.is_empty() {
            continue;
        }
        let item = parsed_items.remove(0);

        let mut call_sites = Vec::new();
        if let Some(ranges) = call.get(ranges_key).and_then(|r| r.as_array()) {
            for range in ranges {
                if let Some(line) = range
                    .get("start")
                    .and_then(|s| s.get("line"))
                    .and_then(serde_json::Value::as_u64)
                {
                    call_sites.push(u32::try_from(line).unwrap_or(0) + 1);
                }
            }
        }

        result.push(CallHierarchyCall { item, call_sites });
    }

    Ok(result)
}

/// Parse a textDocument/references response into a list of `ReferenceLocation`.
///
/// # Errors
/// - `LspError::Protocol` — response is not an array, contains invalid URIs, or missing ranges
pub async fn parse_references_response(
    response: &serde_json::Value,
    workspace_root: &Path,
) -> Result<Vec<ReferenceLocation>, LspError> {
    if response.is_null() {
        return Ok(Vec::new());
    }

    let references = response
        .as_array()
        .ok_or_else(|| LspError::Protocol("expected array".to_owned()))?;

    let mut result = Vec::with_capacity(references.len());
    for ref_item in references {
        let Some(uri_str) = ref_item.get("uri").and_then(|u| u.as_str()) else {
            continue;
        };

        let uri =
            Url::parse(uri_str).map_err(|e| LspError::Protocol(format!("invalid URI: {e}")))?;
        let file_path = uri
            .to_file_path()
            .map_err(|()| LspError::Protocol("URI is not a file path".to_owned()))?;
        let relative_path = match file_path.strip_prefix(workspace_root) {
            Ok(p) => p.to_path_buf(),
            Err(_) => file_path,
        };

        let range = ref_item
            .get("range")
            .ok_or_else(|| LspError::Protocol("missing range".to_owned()))?;

        #[allow(clippy::cast_possible_truncation)]
        let line = range
            .get("start")
            .and_then(|s| s.get("line"))
            .and_then(serde_json::Value::as_u64)
            .map_or(1, |l| (l as u32) + 1);

        #[allow(clippy::cast_possible_truncation)]
        let column = range
            .get("start")
            .and_then(|s| s.get("character"))
            .and_then(serde_json::Value::as_u64)
            .map_or(1, |c| (c as u32) + 1);

        let snippet = read_preview_line(
            &workspace_root.join(&relative_path),
            (line as usize).saturating_sub(1),
        )
        .await;

        result.push(ReferenceLocation {
            file: relative_path.to_string_lossy().into_owned(),
            line,
            column,
            snippet,
        });
    }

    Ok(result)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
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

        let result = parse_call_hierarchy_calls_response(&response, &temp, "from", "fromRanges")
            .expect("ok");

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
}
