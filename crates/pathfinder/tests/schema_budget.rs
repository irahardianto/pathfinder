//! Schema budget and structure tests for Pathfinder MCP tools.
//!
//! Enforces:
//! - Total schema token budget (<= 1,674 tokens post-WS-1; <= 1,200 tokens post-WS-5)
//! - Zero `anyOf` and zero `oneOf` keys in all tool parameter schemas (RSP-B6)
//! - No `$defs` entry has both a `const` and a sibling `description`
//! - Tool descriptions are capped at <= 6 lines

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::pedantic,
    missing_docs
)]

mod common;

use common::{
    create_test_server, create_transport_pair, initialize_request, initialized_notification,
    read_jsonrpc, tools_list_request, write_jsonrpc,
};
use rmcp::ServiceExt;

/// Characters per token constant used for schema budgeting.
const CHARS_PER_TOKEN: f64 = 3.6;

/// Committed budget constant post-WS-5 (3 surviving tools: inspect, locate, trace).
const WS5_SURVIVING_BUDGET_TOKENS: usize = 1200;

/// Surviving tools in Pathfinder v6.
const SURVIVING_TOOLS: &[&str] = &["inspect", "locate", "trace"];

async fn get_tools_list_response() -> serde_json::Value {
    let bundle = create_test_server();
    let (server_read, server_write, mut client_reader, mut client_writer) = create_transport_pair();

    let server_handle = tokio::spawn(async move {
        bundle
            .server
            .serve((server_read, server_write))
            .await
            .expect("server start")
    });

    write_jsonrpc(&mut client_writer, &initialize_request(1)).await;
    let _ = read_jsonrpc(&mut client_reader).await;
    write_jsonrpc(&mut client_writer, &initialized_notification()).await;

    write_jsonrpc(&mut client_writer, &tools_list_request(2)).await;
    let resp = read_jsonrpc(&mut client_reader).await;

    drop(client_writer);
    let service = server_handle.await.expect("join");
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), service.waiting()).await;

    resp
}

/// Recursively find all occurrences of forbidden keys ("anyOf", "oneOf") in JSON.
fn find_forbidden_keys(value: &serde_json::Value, path: &str, violations: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                let current_path = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                if k == "anyOf" || k == "oneOf" {
                    violations.push(format!("Forbidden key '{k}' found at: {current_path}"));
                }
                find_forbidden_keys(v, &current_path, violations);
            }
        }
        serde_json::Value::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                let current_path = format!("{path}[{i}]");
                find_forbidden_keys(v, &current_path, violations);
            }
        }
        _ => {}
    }
}

/// Check that no `$defs` entry has both a `const` and a sibling `description`,
/// anywhere in the schema tree (not just the first two levels of nesting).
fn check_no_const_with_sibling_description(
    defs: &serde_json::Value,
    tool_name: &str,
    violations: &mut Vec<String>,
) {
    fn walk(value: &serde_json::Value, path: &str, tool_name: &str, violations: &mut Vec<String>) {
        if let serde_json::Value::Object(map) = value {
            if map.contains_key("const") && map.contains_key("description") {
                violations.push(format!(
                    "Tool '{tool_name}' {path} has both 'const' and sibling 'description'"
                ));
            }
            for (k, v) in map {
                let current_path = format!("{path}.{k}");
                walk(v, &current_path, tool_name, violations);
            }
        } else if let serde_json::Value::Array(arr) = value {
            for (i, v) in arr.iter().enumerate() {
                let current_path = format!("{path}[{i}]");
                walk(v, &current_path, tool_name, violations);
            }
        }
    }

    if let serde_json::Value::Object(map) = defs {
        for (def_name, def_val) in map {
            walk(def_val, &format!("$defs.{def_name}"), tool_name, violations);
        }
    }
}

/// Minimal Rust string-literal extractor. Returns the (unescaped-quote) content
/// of every `"..."` literal in `source`. Deliberately does not handle raw
/// strings (`r"..."`) — agent-facing guidance text in this codebase is never
/// written as a raw string, and treating `r"` as a normal quote start would
/// just yield a harmless empty/truncated literal, not a false negative on a
/// real guidance string.
fn extract_string_literals(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut literals = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != b'"' {
                if bytes[j] == b'\\' && j + 1 < bytes.len() {
                    j += 2;
                } else {
                    j += 1;
                }
            }
            let end = j.min(source.len());
            literals.push(source[start..end].to_string());
            i = j + 1;
        } else {
            i += 1;
        }
    }
    literals
}

/// Recursively collect all `.rs` file paths under `dir`, skipping `target/`
/// and any file whose name ends in `_test.rs` (matching the acceptance
/// grep's `--glob '!*_test.rs'`).
fn collect_rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().and_then(|n| n.to_str()) == Some("target") {
                continue;
            }
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            let is_test_file = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with("_test.rs"));
            if !is_test_file {
                out.push(path);
            }
        }
    }
}

#[tokio::test]
async fn test_tools_list_schema_budget() {
    let resp = get_tools_list_response().await;
    let tools = resp["result"]["tools"]
        .as_array()
        .expect("tools should be an array");

    assert_eq!(
        tools.len(),
        3,
        "expected exactly 3 tools registered (inspect, locate, trace)"
    );

    let mut total_chars = 0;

    for tool in tools {
        let name = tool["name"].as_str().unwrap_or("");
        let description = tool["description"].as_str().unwrap_or("");
        let schema_str = serde_json::to_string(&tool["inputSchema"]).expect("serialize schema");

        let tool_chars = name.len() + description.len() + schema_str.len();
        total_chars += tool_chars;

        assert!(
            SURVIVING_TOOLS.contains(&name),
            "Unexpected tool '{name}' — only inspect, locate, trace allowed"
        );

        let lines: Vec<&str> = description.lines().collect();
        println!(
            "Tool: {:<10} | desc lines: {:<2} | desc chars: {:<4} | schema chars: {:<4} | total chars: {:<4} | tokens: {:<4}",
            name,
            lines.len(),
            description.len(),
            schema_str.len(),
            tool_chars,
            (tool_chars as f64 / CHARS_PER_TOKEN).round() as usize
        );
        assert!(
            lines.len() <= 6,
            "Tool '{name}' description has {} lines, exceeding max 6 lines",
            lines.len()
        );
    }

    let total_tokens = (total_chars as f64 / CHARS_PER_TOKEN).round() as usize;

    println!(
        "\nTotal Chars: {} | Total Tokens: {} (Budget: <= {} tokens)",
        total_chars, total_tokens, WS5_SURVIVING_BUDGET_TOKENS
    );

    assert!(
        total_tokens <= WS5_SURVIVING_BUDGET_TOKENS,
        "Total tokens {total_tokens} exceeds WS-5 budget of {WS5_SURVIVING_BUDGET_TOKENS} tokens"
    );
}

#[tokio::test]
async fn test_zero_anyof_and_zero_oneof_keys() {
    let resp = get_tools_list_response().await;
    let tools = resp["result"]["tools"]
        .as_array()
        .expect("tools should be an array");

    let mut violations = Vec::new();
    for tool in tools {
        let name = tool["name"].as_str().unwrap_or("unknown");
        let input_schema = &tool["inputSchema"];
        find_forbidden_keys(input_schema, name, &mut violations);
    }

    assert!(
        violations.is_empty(),
        "Found anyOf/oneOf keys in tool schemas (RSP-B6 violation):\n{:#?}",
        violations
    );
}

#[tokio::test]
async fn test_no_defs_has_const_with_sibling_description() {
    let resp = get_tools_list_response().await;
    let tools = resp["result"]["tools"]
        .as_array()
        .expect("tools should be an array");

    let mut violations = Vec::new();
    for tool in tools {
        let name = tool["name"].as_str().unwrap_or("unknown");
        if let Some(defs) = tool["inputSchema"].get("$defs") {
            check_no_const_with_sibling_description(defs, name, &mut violations);
        }
    }

    assert!(
        violations.is_empty(),
        "Found $defs entries with both 'const' and sibling 'description':\n{:#?}",
        violations
    );
}

/// Contract 2 Rule 7: no field of type `Option<Evidence<_>>` — the type
/// introduced specifically to carry the null=UNKNOWN /
/// verified-empty=CONFIRMED-ZERO / heuristic tri-state — may carry
/// `skip_serializing_if` (RSP-A9). `skip_serializing_if` on such a field
/// would erase the UNKNOWN-vs-ABSENT distinction: the whole point of
/// `Evidence<T>` is that `None` is a value the model must read, not a key it
/// must notice is missing.
///
/// Scoped to `Option<Evidence<_>>` specifically, not every `Option<Vec<_>>`
/// in the file: plain `Option<Vec<_>>` fields also cover request parameters
/// (e.g. `InspectParams::semantic_paths`) and intentionally-simplified
/// batch/opt-in fields (e.g. `heuristic_candidates`, which is documented as
/// "never populated when Ok" and carries no confirmed-zero semantic) that
/// Contract 2 Rule 7 was not written to constrain.
///
/// This is a source-level check: the property depends on a serde attribute,
/// which does not change the `tools/list` JSON-Schema shape and therefore
/// cannot be detected from the schema response alone.
#[test]
fn test_no_skip_serializing_if_on_tri_state_evidence_fields() {
    let types_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/server/types.rs");
    let source = std::fs::read_to_string(&types_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", types_path.display()));

    let lines: Vec<&str> = source.lines().collect();
    let mut violations = Vec::new();

    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let is_tri_state_field =
            trimmed.starts_with("pub ") && trimmed.contains(": Option<Evidence<");
        if !is_tri_state_field {
            continue;
        }

        // Walk backwards over contiguous attribute (`#[...]`) and doc-comment
        // (`///`) lines directly above the field declaration.
        let mut j = idx;
        while j > 0 {
            let prev = lines[j - 1].trim_start();
            if prev.starts_with("#[") || prev.starts_with("///") || prev.starts_with("//!") {
                if prev.contains("skip_serializing_if") {
                    violations.push(format!(
                        "{}:{}: field `{}` is Option<Evidence<_>> and carries \
                         skip_serializing_if — this erases the UNKNOWN-vs-ABSENT distinction \
                         (Contract 2 Rule 7 / RSP-A9)",
                        types_path.display(),
                        j,
                        trimmed.trim_end_matches(',')
                    ));
                }
                j -= 1;
            } else {
                break;
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Contract 2 Rule 7 violations found:\n{}",
        violations.join("\n")
    );
}

/// Contract 4: guidance strings emitted to an agent may only name tools that
/// exist (`locate`, `trace`, `inspect`). Scans every string literal in
/// `crates/pathfinder/src` and `crates/pathfinder-common/src` (excluding test
/// files) for a call-shaped reference to a deleted tool (`search(`,
/// `explore(`, `read(`, `health(`, `lsp_health(`) — the same acceptance grep
/// `00-plan.md`/`03-contracts.md` specify, run as an automated regression
/// test instead of a manual `rg` invocation.
#[test]
fn test_no_guidance_strings_name_deleted_tools() {
    let forbidden = ["search(", "explore(", "read(", "health(", "lsp_health("];

    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut roots = vec![manifest_dir.join("src")];
    let common_src = manifest_dir.join("../pathfinder-common/src");
    if common_src.exists() {
        roots.push(common_src);
    }

    let mut files = Vec::new();
    for root in &roots {
        collect_rs_files(root, &mut files);
    }
    assert!(
        !files.is_empty(),
        "expected to find at least one .rs file to scan under {roots:?}"
    );

    let mut violations = Vec::new();
    for file in &files {
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        for literal in extract_string_literals(&source) {
            for pattern in forbidden {
                if literal.contains(pattern) {
                    violations.push(format!(
                        "{}: guidance string names deleted tool pattern '{pattern}': {literal:?}",
                        file.display()
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Contract 4 violation — guidance strings naming deleted tools:\n{}",
        violations.join("\n")
    );
}
