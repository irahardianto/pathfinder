use super::*;
use crate::client::fake_transport::FakeTransport;
use crate::client::tests::make_running_client;
use std::sync::Arc;

fn make_running_client_with_caps(language_id: &str) -> (LspClient, Arc<FakeTransport>) {
    let (client, fake) = make_running_client(language_id);

    if let Some(entry) = client.processes.get(language_id) {
        if let crate::client::ProcessEntry::Running(state) = entry.value() {
            let mut caps = state.live_capabilities.write();
            caps.call_hierarchy_provider = true;
            caps.definition_provider = true;
            caps.references_provider = true;
            caps.implementation_provider = true;
        }
    }

    (client, fake)
}

#[tokio::test]
async fn test_lawyer_goto_definition_with_location_response() {
    let (client, fake) = make_running_client("rust");

    let workspace = Path::new("/workspace");
    std::fs::create_dir_all(workspace.join("src")).ok();

    fake.set_response(
        "textDocument/definition",
        serde_json::json!({
            "result": {
                "uri": "file:///workspace/src/auth.rs",
                "range": {
                    "start": { "line": 41, "character": 4 },
                    "end": { "line": 41, "character": 9 }
                }
            }
        }),
    );

    let result = client
        .goto_definition(workspace, Path::new("src/main.rs"), 10, 5)
        .await;

    assert!(result.is_ok(), "goto_definition should succeed: {result:?}");
    let loc = result.unwrap();
    assert!(loc.is_some(), "should return a location");
    let loc = loc.unwrap();
    assert_eq!(loc.line, 42);
    assert_eq!(loc.column, 5);
}

#[tokio::test]
async fn test_lawyer_goto_definition_with_null_response() {
    let (client, fake) = make_running_client("rust");

    let workspace = Path::new("/workspace");

    fake.set_response(
        "textDocument/definition",
        serde_json::json!({ "result": null }),
    );

    let result = client
        .goto_definition(workspace, Path::new("src/main.rs"), 10, 5)
        .await;

    assert!(result.is_ok(), "goto_definition should succeed: {result:?}");
    assert!(
        result.unwrap().is_none(),
        "null response should return None"
    );
}

#[tokio::test]
async fn test_lawyer_goto_definition_with_array_response() {
    let (client, fake) = make_running_client("rust");

    let workspace = Path::new("/workspace");

    fake.set_response(
        "textDocument/definition",
        serde_json::json!({
            "result": [{
                "uri": "file:///workspace/src/lib.rs",
                "range": {
                    "start": { "line": 9, "character": 0 },
                    "end": { "line": 9, "character": 5 }
                }
            }]
        }),
    );

    let result = client
        .goto_definition(workspace, Path::new("src/main.rs"), 10, 5)
        .await;

    assert!(result.is_ok(), "goto_definition should succeed: {result:?}");
    let loc = result.unwrap();
    assert!(loc.is_some(), "array response should return first location");
    let loc = loc.unwrap();
    assert_eq!(loc.line, 10);
}

#[tokio::test]
async fn test_lawyer_call_hierarchy_prepare_with_items() {
    let (client, fake) = make_running_client_with_caps("rust");

    let workspace = Path::new("/workspace");
    std::fs::create_dir_all(workspace.join("src")).ok();
    let file_path = workspace.join("src/main.rs");
    std::fs::write(&file_path, "fn main() {}").ok();

    let file_uri = Url::from_file_path(&file_path).unwrap().to_string();

    fake.set_response(
        "textDocument/prepareCallHierarchy",
        serde_json::json!({
            "result": [{
                "name": "main",
                "kind": 12,
                "detail": "fn()",
                "uri": file_uri,
                "selectionRange": {
                    "start": { "line": 0, "character": 2 },
                    "end": { "line": 0, "character": 6 }
                }
            }]
        }),
    );

    let result = client
        .call_hierarchy_prepare(workspace, Path::new("src/main.rs"), 1, 3)
        .await;

    assert!(
        result.is_ok(),
        "call_hierarchy_prepare should succeed: {result:?}"
    );
    let items = result.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].name, "main");
    assert_eq!(items[0].kind, "function");

    let _ = std::fs::remove_file(&file_path);
}

#[tokio::test]
async fn test_lawyer_call_hierarchy_incoming_with_calls() {
    let (client, fake) = make_running_client_with_caps("rust");

    let workspace = Path::new("/workspace");
    std::fs::create_dir_all(workspace.join("src")).ok();
    let caller_file = workspace.join("src/caller.rs");
    std::fs::write(&caller_file, "fn caller() {}").ok();

    let caller_uri = Url::from_file_path(&caller_file).unwrap().to_string();

    fake.set_response(
        "callHierarchy/incomingCalls",
        serde_json::json!({
            "result": [{
                "from": {
                    "name": "caller",
                    "kind": 12,
                    "uri": caller_uri,
                    "selectionRange": {
                        "start": { "line": 0, "character": 2 },
                        "end": { "line": 0, "character": 8 }
                    }
                },
                "fromRanges": [
                    { "start": { "line": 5 }, "end": { "line": 5 } }
                ]
            }]
        }),
    );

    let item = CallHierarchyItem {
        name: "main".to_owned(),
        kind: "function".to_owned(),
        detail: None,
        file: "src/main.rs".to_owned(),
        line: 1,
        column: 1,
        data: Some(serde_json::json!({"uri": "file:///test", "range": {"start": {"line": 0}}})),
    };

    let result = client.call_hierarchy_incoming(workspace, &item).await;

    assert!(
        result.is_ok(),
        "call_hierarchy_incoming should succeed: {result:?}"
    );
    let calls = result.unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].item.name, "caller");
    assert_eq!(calls[0].call_sites, vec![6]);

    let _ = std::fs::remove_file(&caller_file);
}

#[tokio::test]
async fn test_lawyer_call_hierarchy_outgoing_with_calls() {
    let (client, fake) = make_running_client_with_caps("rust");

    let workspace = Path::new("/workspace");
    std::fs::create_dir_all(workspace.join("src")).ok();
    let callee_file = workspace.join("src/callee.rs");
    std::fs::write(&callee_file, "fn callee() {}").ok();

    let callee_uri = Url::from_file_path(&callee_file).unwrap().to_string();

    fake.set_response(
        "callHierarchy/outgoingCalls",
        serde_json::json!({
            "result": [{
                "to": {
                    "name": "callee",
                    "kind": 12,
                    "uri": callee_uri,
                    "selectionRange": {
                        "start": { "line": 0, "character": 2 },
                        "end": { "line": 0, "character": 8 }
                    }
                },
                "fromRanges": [
                    { "start": { "line": 10 }, "end": { "line": 10 } }
                ]
            }]
        }),
    );

    let item = CallHierarchyItem {
        name: "main".to_owned(),
        kind: "function".to_owned(),
        detail: None,
        file: "src/main.rs".to_owned(),
        line: 1,
        column: 1,
        data: Some(serde_json::json!({"uri": "file:///test", "range": {"start": {"line": 0}}})),
    };

    let result = client.call_hierarchy_outgoing(workspace, &item).await;

    assert!(
        result.is_ok(),
        "call_hierarchy_outgoing should succeed: {result:?}"
    );
    let calls = result.unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].item.name, "callee");
    assert_eq!(calls[0].call_sites, vec![11]);

    let _ = std::fs::remove_file(&callee_file);
}

#[tokio::test]
async fn test_lawyer_references_with_locations() {
    let (client, fake) = make_running_client("rust");

    let workspace = Path::new("/workspace");
    std::fs::create_dir_all(workspace.join("src")).ok();
    let file_path = workspace.join("src/main.rs");
    std::fs::write(&file_path, "fn main() { main(); }").ok();

    let file_uri = Url::from_file_path(&file_path).unwrap().to_string();

    fake.set_response(
        "textDocument/references",
        serde_json::json!({
            "result": [
                {
                    "uri": file_uri,
                    "range": {
                        "start": { "line": 0, "character": 3 },
                        "end": { "line": 0, "character": 7 }
                    }
                },
                {
                    "uri": file_uri,
                    "range": {
                        "start": { "line": 0, "character": 13 },
                        "end": { "line": 0, "character": 17 }
                    }
                }
            ]
        }),
    );

    let result = client
        .references(workspace, Path::new("src/main.rs"), 1, 4)
        .await;

    assert!(result.is_ok(), "references should succeed: {result:?}");
    let refs = result.unwrap();
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].line, 1);
    assert_eq!(refs[1].line, 1);

    let _ = std::fs::remove_file(&file_path);
}

#[tokio::test]
async fn test_lawyer_goto_implementation_with_locations() {
    let (client, fake) = make_running_client("rust");

    let workspace = Path::new("/workspace");

    fake.set_response(
        "textDocument/implementation",
        serde_json::json!({
            "result": [{
                "uri": "file:///workspace/src/impl.rs",
                "range": {
                    "start": { "line": 5, "character": 0 },
                    "end": { "line": 5, "character": 10 }
                }
            }]
        }),
    );

    let result = client
        .goto_implementation(workspace, Path::new("src/main.rs"), 10, 5)
        .await;

    assert!(
        result.is_ok(),
        "goto_implementation should succeed: {result:?}"
    );
    let locs = result.unwrap();
    assert_eq!(locs.len(), 1);
    assert_eq!(locs[0].line, 6);
}

#[tokio::test]
async fn test_lawyer_goto_definition_no_lsp() {
    let (client, _fake) = make_running_client("rust");

    let result = client
        .goto_definition(Path::new("/workspace"), Path::new("src/main.xyz"), 1, 1)
        .await;
    assert!(matches!(result, Err(LspError::NoLspAvailable)));
}

#[tokio::test]
async fn test_lawyer_call_hierarchy_prepare_no_lsp() {
    let (client, _fake) = make_running_client("rust");

    let result = client
        .call_hierarchy_prepare(Path::new("/workspace"), Path::new("src/main.xyz"), 1, 1)
        .await;
    assert!(matches!(result, Err(LspError::NoLspAvailable)));
}

#[tokio::test]
async fn test_lawyer_goto_definition_request_error_returns_error() {
    let (client, fake) = make_running_client_with_caps("rust");

    fake.kill();

    let result = client
        .goto_definition(Path::new("/workspace"), Path::new("src/main.rs"), 10, 5)
        .await;

    assert!(
        result.is_err(),
        "goto_definition should fail when transport is dead: {result:?}"
    );
    assert!(
        matches!(result, Err(LspError::ConnectionLost)),
        "should return ConnectionLost, got: {result:?}"
    );
}

#[tokio::test]
async fn test_lawyer_call_hierarchy_prepare_request_error_returns_error() {
    let (client, fake) = make_running_client_with_caps("rust");

    fake.kill();

    let result = client
        .call_hierarchy_prepare(Path::new("/workspace"), Path::new("src/main.rs"), 1, 3)
        .await;

    assert!(
        result.is_err(),
        "call_hierarchy_prepare should fail when transport is dead: {result:?}"
    );
    assert!(
        matches!(result, Err(LspError::ConnectionLost)),
        "should return ConnectionLost, got: {result:?}"
    );
}

#[tokio::test]
async fn test_lawyer_references_request_error_returns_error() {
    let (client, fake) = make_running_client_with_caps("rust");

    fake.kill();

    let result = client
        .references(Path::new("/workspace"), Path::new("src/main.rs"), 1, 4)
        .await;

    assert!(
        result.is_err(),
        "references should fail when transport is dead: {result:?}"
    );
    assert!(
        matches!(result, Err(LspError::ConnectionLost)),
        "should return ConnectionLost, got: {result:?}"
    );
}
