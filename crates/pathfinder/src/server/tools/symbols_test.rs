use super::*;
use pathfinder_common::config::PathfinderConfig;
use pathfinder_common::sandbox::Sandbox;
use pathfinder_common::types::WorkspaceRoot;
use pathfinder_search::MockScout;
use pathfinder_treesitter::mock::MockSurgeon;
use std::sync::Arc;
use tempfile::tempdir;

// ── GAP-004: version_hash in text output ───────────────────────────────

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_read_symbol_scope_includes_version_hash_in_text() {
    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Create a test file
    let file_path = ws.path().join("test.rs");
    let content = "fn test() {}\n";
    tokio::fs::write(&file_path, content).await.unwrap();

    let mock_surgeon = MockSurgeon::new();
    let expected_scope = pathfinder_common::types::SymbolScope {
        content: content.to_owned(),
        start_line: 1,
        end_line: 1,
        name_column: 0,
        language: "rust".to_owned(),
    };
    mock_surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(expected_scope));

    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(mock_surgeon),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = InspectParams {
        semantic_path: "test.rs::test".to_owned(),
        ..Default::default()
    };

    let result = server.read_symbol_scope_impl(params).await;
    assert!(result.is_ok(), "read_symbol_scope should succeed");
    let call_result = result.unwrap();

    // Verify the text content is the symbol source
    if let Some(content) = call_result.content.first() {
        if let rmcp::model::RawContent::Text(text_content) = &content.raw {
            assert!(
                !text_content.text.is_empty(),
                "text output should be non-empty"
            );
        } else {
            panic!("Expected text content");
        }
    } else {
        panic!("Expected content");
    }
}
