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
    use tokio::io::AsyncReadExt;
    let Ok(file) = tokio::fs::File::open(abs_path).await else {
        return String::default();
    };
    let mut reader = tokio::io::BufReader::new(file);
    let mut line = Vec::new();

    for idx in 0..=line_index {
        line.clear();
        let mut bytes_read = 0;
        loop {
            let mut byte = [0u8; 1];
            if reader.read_exact(&mut byte).await.is_ok() {
                let b = byte[0];
                if b == b'\n' {
                    break;
                }
                if idx == line_index && bytes_read < 512 {
                    line.push(b);
                }
                bytes_read += 1;
                if bytes_read >= 10240 {
                    // Skip rest of excessively long line to prevent memory growth
                    loop {
                        let mut skip_byte = [0u8; 1];
                        if reader.read_exact(&mut skip_byte).await.is_err() || skip_byte[0] == b'\n'
                        {
                            break;
                        }
                    }
                    break;
                }
            } else {
                if idx == line_index && !line.is_empty() {
                    break;
                }
                return String::default();
            }
        }
    }
    String::from_utf8_lossy(&line).trim().to_owned()
}

fn parse_uri_and_range(
    location: &serde_json::Value,
) -> Option<(String, u64, u64, Option<std::path::PathBuf>)> {
    let (uri_str, start_line, start_char) = if location.get("targetUri").is_some() {
        let range = location
            .get("targetSelectionRange")
            .or_else(|| location.get("targetRange"));
        (
            location["targetUri"].as_str().unwrap_or(""),
            range.and_then(|r| r["start"]["line"].as_u64()).unwrap_or(0),
            range
                .and_then(|r| r["start"]["character"].as_u64())
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
/// - `LspError::ServerError` — LSP returned a JSON-RPC error object
pub async fn parse_definition_response(
    response: serde_json::Value,
    workspace_root: &Path,
) -> Result<Option<DefinitionLocation>, LspError> {
    if response.is_null() {
        return Ok(None);
    }

    if let Some(err_obj) = response.get("error") {
        let code = err_obj
            .get("code")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        let message = err_obj
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("LSP error")
            .to_owned();
        let data = err_obj.get("data").cloned();
        return Err(LspError::ServerError {
            code,
            message,
            data,
        });
    }

    let location = if response.is_array() {
        response
            .as_array()
            .and_then(|arr| {
                let len = arr.len();
                if len > 1 {
                    tracing::debug!(
                        count = len,
                        "LSP: definition response has multiple locations, returning first (use parse_definition_response_multi for all)"
                    );
                }
                arr.first().cloned()
            })
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
        String::default()
    };

    Ok(Some(DefinitionLocation {
        file,
        line: u32::try_from(start_line.saturating_add(1)).unwrap_or(1),
        column: u32::try_from(start_char.saturating_add(1)).unwrap_or(1),
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
        String::default()
    };

    Some(DefinitionLocation {
        file,
        line: u32::try_from(start_line.saturating_add(1)).unwrap_or(1),
        column: u32::try_from(start_char.saturating_add(1)).unwrap_or(1),
        preview,
    })
}

/// Parse multiple definition locations from a response (Location, Location[], or `LocationLink`[]).
///
/// Preserves all definition locations in order (closing LSP-A7).
pub async fn parse_definition_response_multi(
    response: &serde_json::Value,
    workspace_root: &Path,
) -> Vec<DefinitionLocation> {
    if response.is_null() {
        return Vec::new();
    }

    let items_opt = response.as_array().map(Vec::as_slice).or_else(|| {
        response
            .get("result")
            .and_then(serde_json::Value::as_array)
            .map(Vec::as_slice)
    });

    if let Some(items) = items_opt {
        let mut result = Vec::with_capacity(items.len());
        for item in items {
            if let Some(loc) = parse_single_definition_location(item, workspace_root).await {
                result.push(loc);
            }
        }
        result
    } else {
        let target = response.get("result").unwrap_or(response);
        parse_single_definition_location(target, workspace_root)
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
                .unwrap_or(0)
                .saturating_add(1),
        )
        .unwrap_or(1);
        let column = u32::try_from(
            item["selectionRange"]["start"]["character"]
                .as_u64()
                .unwrap_or(0)
                .saturating_add(1),
        )
        .unwrap_or(1);

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

        let ranges = call.get(ranges_key).and_then(|r| r.as_array());
        let mut call_sites = Vec::with_capacity(ranges.map_or(0, Vec::len));
        if let Some(ranges) = ranges {
            for range in ranges {
                if let Some(line) = range
                    .get("start")
                    .and_then(|s| s.get("line"))
                    .and_then(serde_json::Value::as_u64)
                {
                    call_sites.push(u32::try_from(line.saturating_add(1)).unwrap_or(1));
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
/// - `LspError::Protocol` — response is not an array, contains invalid URIs, or missing/malformed ranges
/// - `LspError::ServerError` — LSP returned a JSON-RPC error object (LSP-A4)
pub async fn parse_references_response(
    response: &serde_json::Value,
    workspace_root: &Path,
) -> Result<Vec<ReferenceLocation>, LspError> {
    if response.is_null() {
        return Ok(Vec::new());
    }

    if let Some(err_obj) = response.get("error") {
        let code = err_obj
            .get("code")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        let message = err_obj
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("LSP error")
            .to_owned();
        let data = err_obj.get("data").cloned();
        return Err(LspError::ServerError {
            code,
            message,
            data,
        });
    }

    let references = if let Some(arr) = response.as_array() {
        arr
    } else if let Some(arr) = response.get("result").and_then(|r| r.as_array()) {
        arr
    } else if response
        .get("result")
        .is_some_and(serde_json::Value::is_null)
    {
        return Ok(Vec::new());
    } else {
        return Err(LspError::Protocol(
            "expected array in references response".to_owned(),
        ));
    };

    let mut result = Vec::with_capacity(references.len());
    let mut skipped_count = 0usize;

    for ref_item in references {
        let Some(uri_str) = ref_item.get("uri").and_then(|u| u.as_str()) else {
            skipped_count += 1;
            continue;
        };

        let uri =
            Url::parse(uri_str).map_err(|e| LspError::Protocol(format!("invalid URI: {e}")))?;

        let Ok(file_path) = uri.to_file_path() else {
            tracing::debug!(uri = %uri_str, "Skipping non-file URI in references");
            skipped_count += 1;
            continue;
        };

        let Ok(relative_path) = file_path.strip_prefix(workspace_root) else {
            tracing::debug!(path = %file_path.display(), "Skipping reference outside workspace root");
            skipped_count += 1;
            continue;
        };

        let range = ref_item
            .get("range")
            .ok_or_else(|| LspError::Protocol("missing range in reference".to_owned()))?;

        let start = range
            .get("start")
            .ok_or_else(|| LspError::Protocol("missing start in reference range".to_owned()))?;

        let start_line = start
            .get("line")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                LspError::Protocol("missing start.line in reference range".to_owned())
            })?;

        let start_char = start
            .get("character")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                LspError::Protocol("missing start.character in reference range".to_owned())
            })?;

        let line = u32::try_from(start_line.saturating_add(1)).unwrap_or(1);
        let column = u32::try_from(start_char.saturating_add(1)).unwrap_or(1);

        let snippet = read_preview_line(
            &workspace_root.join(relative_path),
            usize::try_from(line).unwrap_or(0).saturating_sub(1),
        )
        .await;

        result.push(ReferenceLocation {
            file: relative_path.to_string_lossy().into_owned(),
            line,
            column,
            snippet,
        });
    }

    if skipped_count > 0 {
        tracing::debug!(
            skipped_count,
            "parse_references_response: skipped non-workspace or non-file reference entries"
        );
    }

    Ok(result)
}

#[cfg(test)]
#[path = "response_parsers_test.rs"]
mod tests;
