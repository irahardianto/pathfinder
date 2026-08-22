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

/// Check that no $defs entry has both a `const` and a sibling `description`.
fn check_no_const_with_sibling_description(
    defs: &serde_json::Value,
    tool_name: &str,
    violations: &mut Vec<String>,
) {
    if let serde_json::Value::Object(map) = defs {
        for (def_name, def_val) in map {
            if let serde_json::Value::Object(def_obj) = def_val {
                if def_obj.contains_key("const") && def_obj.contains_key("description") {
                    violations.push(format!(
                        "Tool '{tool_name}' $defs.{def_name} has both 'const' and sibling 'description'"
                    ));
                }
                // Check nested properties or items if any
                for (nested_k, nested_v) in def_obj {
                    if let serde_json::Value::Object(nested_obj) = nested_v {
                        if nested_obj.contains_key("const")
                            && nested_obj.contains_key("description")
                        {
                            violations.push(format!(
                                "Tool '{tool_name}' $defs.{def_name}.{nested_k} has both 'const' and sibling 'description'"
                            ));
                        }
                    }
                }
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
