#![allow(
    missing_docs,
    dead_code,
    clippy::pedantic,
    clippy::doc_markdown,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::field_reassign_with_default
)]

//! Integration tests for `test-mock-lsp`.

use std::time::Duration;
use test_mock_lsp::canned_index::CannedIndex;
use test_mock_lsp::config::MockConfig;
use test_mock_lsp::handlers;
use test_mock_lsp::protocol::{write_malformed_response, JsonRpcResponse, MalformMode};
use test_mock_lsp::script::{ScriptAction, ScriptEngine};

#[test]
fn test_script_timeline_end_to_end() {
    let script_json = serde_json::json!([
        {
            "method": "textDocument/prepareCallHierarchy",
            "after_ms": 100,
            "response": [{ "name": "target_fn", "kind": 12 }]
        },
        {
            "method": "textDocument/prepareCallHierarchy",
            "response": []
        }
    ]);

    let mut engine = ScriptEngine::from_value(&script_json).unwrap();
    let t0 = std::time::Instant::now();
    engine.set_start_time(t0);

    // Initial call before 100ms -> []
    let eval_early = engine
        .evaluate("textDocument/prepareCallHierarchy")
        .unwrap();
    assert_eq!(
        eval_early.action,
        ScriptAction::Respond(serde_json::json!([]))
    );

    // Fast forward start time by 150ms -> non-empty
    engine.set_start_time(t0 - Duration::from_millis(150));
    let eval_late = engine
        .evaluate("textDocument/prepareCallHierarchy")
        .unwrap();
    assert_eq!(
        eval_late.action,
        ScriptAction::Respond(serde_json::json!([{ "name": "target_fn", "kind": 12 }]))
    );
}

#[test]
fn test_position_aware_canned_index_all_methods() {
    let raw = serde_json::json!({
        "file:///project/src/main.rs": {
            "(42, 10)": {
                "definition": {
                    "uri": "${WORKSPACE}/src/lib.rs",
                    "range": { "start": { "line": 5, "character": 0 }, "end": { "line": 5, "character": 10 } }
                },
                "references": [
                    { "uri": "${WORKSPACE}/src/main.rs", "range": { "start": { "line": 42, "character": 10 }, "end": { "line": 42, "character": 20 } } },
                    { "uri": "${WORKSPACE}/src/other.rs", "range": { "start": { "line": 1, "character": 4 }, "end": { "line": 1, "character": 14 } } }
                ],
                "prepare_call_hierarchy": [
                    {
                        "name": "calculate",
                        "kind": 12,
                        "uri": "${WORKSPACE}/src/main.rs",
                        "range": { "start": { "line": 42, "character": 0 }, "end": { "line": 45, "character": 1 } },
                        "selectionRange": { "start": { "line": 42, "character": 10 }, "end": { "line": 42, "character": 19 } }
                    }
                ],
                "incoming_calls": [
                    {
                        "from": { "name": "caller_fn", "kind": 12, "uri": "${WORKSPACE}/src/main.rs" },
                        "fromRanges": [{ "start": { "line": 10, "character": 4 }, "end": { "line": 10, "character": 13 } }]
                    }
                ],
                "outgoing_calls": [
                    {
                        "to": { "name": "helper_fn", "kind": 12, "uri": "${WORKSPACE}/src/lib.rs" },
                        "fromRanges": [{ "start": { "line": 43, "character": 4 }, "end": { "line": 43, "character": 13 } }]
                    }
                ],
                "implementation": [
                    {
                        "uri": "${WORKSPACE}/src/impl.rs",
                        "range": { "start": { "line": 100, "character": 0 }, "end": { "line": 110, "character": 1 } }
                    }
                ]
            }
        }
    });

    let index = CannedIndex::from_value(&raw).unwrap();
    let ws = "/my/test/workspace";

    // 1. Definition
    let def = index
        .lookup_definition("file:///project/src/main.rs", 42, 10, Some(ws))
        .unwrap();
    assert_eq!(def["uri"], "/my/test/workspace/src/lib.rs");

    // 2. References (Task 2.4)
    let refs = index
        .lookup_references("src/main.rs", 42, 10, Some(ws))
        .unwrap();
    assert_eq!(refs.as_array().unwrap().len(), 2);

    // 3. Prepare call hierarchy (Task 2.2)
    let prep = index
        .lookup_prepare_call_hierarchy("src/main.rs", 42, 10, Some(ws))
        .unwrap();
    assert_eq!(prep[0]["name"], "calculate");

    // 4. Incoming calls (Task 2.2)
    let incoming = index
        .lookup_incoming_calls("src/main.rs", 42, 10, Some(ws))
        .unwrap();
    assert_eq!(incoming[0]["from"]["name"], "caller_fn");

    // 5. Outgoing calls (Task 2.2)
    let outgoing = index
        .lookup_outgoing_calls("src/main.rs", 42, 10, Some(ws))
        .unwrap();
    assert_eq!(outgoing[0]["to"]["name"], "helper_fn");

    // 6. Implementation (Task 2.4)
    let impls = index
        .lookup_implementation("src/main.rs", 42, 10, Some(ws))
        .unwrap();
    assert_eq!(impls[0]["uri"], "/my/test/workspace/src/impl.rs");
}

#[test]
fn test_all_six_malform_modes_produce_expected_framing_lsp_a4() {
    let resp = JsonRpcResponse::success(
        Some(serde_json::json!(101)),
        serde_json::json!({ "status": "ok" }),
    );

    // 1. Bad content length
    let mut buf1 = Vec::new();
    write_malformed_response(&mut buf1, &resp, None, MalformMode::BadContentLength).unwrap();
    let s1 = String::from_utf8(buf1).unwrap();
    assert!(s1.starts_with("Content-Length: "));

    // 2. Invalid JSON
    let mut buf2 = Vec::new();
    write_malformed_response(&mut buf2, &resp, None, MalformMode::InvalidJson).unwrap();
    let s2 = String::from_utf8(buf2).unwrap();
    assert!(s2.contains("corrupted_unclosed_json"));

    // 3. No result no error (protocol.rs:230)
    let mut buf3 = Vec::new();
    write_malformed_response(&mut buf3, &resp, None, MalformMode::NoResultNoError).unwrap();
    let s3 = String::from_utf8(buf3).unwrap();
    assert!(!s3.contains("\"result\""));
    assert!(!s3.contains("\"error\""));
    assert!(s3.contains("\"id\":101"));

    // 4. String ID (protocol.rs:197-205)
    let mut buf4 = Vec::new();
    write_malformed_response(&mut buf4, &resp, None, MalformMode::StringId).unwrap();
    let s4 = String::from_utf8(buf4).unwrap();
    assert!(s4.contains("\"id\":\"101\""));

    // 5. Echo method in response (protocol.rs:179-194)
    let mut buf5 = Vec::new();
    write_malformed_response(
        &mut buf5,
        &resp,
        Some("textDocument/definition"),
        MalformMode::EchoMethodInResponse,
    )
    .unwrap();
    let s5 = String::from_utf8(buf5).unwrap();
    assert!(s5.contains("\"method\":\"textDocument/definition\""));
    assert!(s5.contains("\"result\""));

    // 6. None
    let mut buf6 = Vec::new();
    write_malformed_response(&mut buf6, &resp, None, MalformMode::None).unwrap();
    let s6 = String::from_utf8(buf6).unwrap();
    assert!(s6.contains("\"jsonrpc\":\"2.0\""));
    assert!(s6.contains("\"result\":{\"status\":\"ok\"}"));
}

#[test]
fn test_position_encoding_negotiation_and_non_ascii() {
    let config_utf8 = MockConfig {
        position_encoding: Some("utf-8".to_string()),
        ..Default::default()
    };
    let init_res_utf8 = handlers::handle_initialize(None, &config_utf8);
    assert_eq!(init_res_utf8["capabilities"]["positionEncoding"], "utf-8");

    let config_utf16 = MockConfig {
        position_encoding: Some("utf-16".to_string()),
        ..Default::default()
    };
    let init_res_utf16 = handlers::handle_initialize(None, &config_utf16);
    assert_eq!(init_res_utf16["capabilities"]["positionEncoding"], "utf-16");
}
