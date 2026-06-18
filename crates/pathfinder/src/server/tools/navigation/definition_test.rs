use super::super::test_helpers::{make_scope, make_server_with_lawyer, make_temp_workspace};
use super::*;
use crate::server::types::LocateParams;
use crate::server::PathfinderServer;
use pathfinder_common::config::PathfinderConfig;
use pathfinder_common::sandbox::Sandbox;
use pathfinder_common::types::{DegradedReason, WorkspaceRoot};
use pathfinder_lsp::{DefinitionLocation, MockLawyer};
use pathfinder_search::MockScout;
use pathfinder_treesitter::mock::MockSurgeon;
use std::sync::Arc;

/// Extract `GetDefinitionResponse` from a `CallToolResult.structured_content`.
/// Replaces the old `call_res.0` tuple-unwrap from the `Json<T>` era.
fn unpack_def(res: rmcp::model::CallToolResult) -> crate::server::types::GetDefinitionResponse {
    serde_json::from_value(res.structured_content.expect("structured_content")).unwrap()
}

// ── get_definition ───────────────────────────────────────────────

#[tokio::test]
async fn test_get_definition_routes_to_lawyer_success() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());
    lawyer.set_goto_definition_result(Ok(Some(DefinitionLocation {
        file: "src/auth.rs".into(),
        line: 42,
        column: 5,
        preview: "pub fn login() -> bool {".into(),
    })));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer.clone());
    let params = LocateParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };

    let result = server.get_definition_impl(params).await;
    let call_res = result.expect("should succeed");
    let val = unpack_def(call_res);

    assert_eq!(val.file, "src/auth.rs");
    assert_eq!(val.line, 42);
    assert_eq!(val.preview, "pub fn login() -> bool {");
    assert!(!val.degraded);
    assert_eq!(lawyer.goto_definition_call_count(), 1);
}

#[tokio::test]
async fn test_get_definition_degrades_when_no_lsp() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    // Default MockLawyer returns Ok(None); use NoOpLawyer for NoLspAvailable
    let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);
    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        surgeon,
        lawyer,
    );

    let params = LocateParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.get_definition_impl(params).await;
    // Should return NO_LSP_AVAILABLE error
    let Err(err) = result else {
        panic!("expected error but got Ok");
    };
    let code = err
        .data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(code, "NO_LSP_AVAILABLE");
}

#[tokio::test]
async fn test_get_definition_rejects_empty_semantic_path() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(MockLawyer::default());
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = LocateParams {
        semantic_path: Some(String::default()),
        ..Default::default()
    };
    let result = server.get_definition_impl(params).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_get_definition_rejects_sandbox_denied_path() {
    let surgeon = Arc::new(MockSurgeon::new());
    let lawyer = Arc::new(MockLawyer::default());
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = LocateParams {
        semantic_path: Some(".git/objects/abc::def".to_owned()),
        ..Default::default()
    };
    let result = server.get_definition_impl(params).await;
    let Err(err) = result else {
        panic!("expected error but got Ok");
    };
    let code = err
        .data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(code, "ACCESS_DENIED");
}

// ── get_definition LSP error path ──────────────────────────────────

#[tokio::test]
async fn test_get_definition_lsp_error_no_grep_match_returns_lsp_error() {
    // When a generic LSP error fires AND grep returns nothing, the original error is surfaced.
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());
    // Simulate an LSP protocol error (not NoLspAvailable, not None)
    lawyer.set_goto_definition_result(Err(LspError::Protocol("LSP protocol error".to_string())));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);
    let params = LocateParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };

    let result = server.get_definition_impl(params).await;
    let Err(err) = result else {
        panic!("expected error but got Ok");
    };
    let code = err
        .data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(code, "LSP_ERROR");
}

// ── catch-all Err(e) grep fallback ───────────────────────────────

#[tokio::test]
async fn test_get_definition_generic_lsp_error_falls_back_to_grep() {
    // When a generic LSP error fires and grep DOES find a match,
    // the result should be Ok with degraded=true and reason containing "lsp_error_grep_fallback".
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(
        ws_dir.path().join("src/auth.rs"),
        "fn login() -> bool { true }",
    )
    .unwrap();

    // Scout returns a match so the fallback succeeds
    let scout = Arc::new(MockScout::default());
    scout.set_result(Ok(pathfinder_search::SearchResult {
        matches: vec![pathfinder_search::SearchMatch {
            file: "src/auth.rs".to_string(),
            line: 1,
            column: 1,
            content: "fn login() -> bool { true }".to_string(),
            context_before: vec![],
            context_after: vec![],
            enclosing_semantic_path: None,
            is_definition: None,
            version_hash: "sha256:abc".to_string(),
            known: Some(false),
        }],
        total_matches: 1,
        truncated: false,
        files_searched: 0,
        files_in_scope: 0,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    }));

    // Lawyer returns a generic LSP error (not NoLspAvailable)
    let lawyer = Arc::new(MockLawyer::default());
    lawyer.set_goto_definition_result(Err(LspError::Protocol("protocol violation".to_string())));

    let server = PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

    let params = LocateParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.get_definition_impl(params).await;
    let Ok(res) = result else {
        panic!("expected Ok with grep fallback, got Err");
    };
    let val = unpack_def(res);
    assert!(val.degraded, "should be degraded");
    assert_eq!(val.file, "src/auth.rs");
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::LspErrorGrepFallback),
        "degraded_reason should be lsp_error_grep_fallback: {:?}",
        val.degraded_reason
    );
}

#[tokio::test]
async fn test_get_definition_connection_lost_falls_back_to_grep() {
    // Same as above but with a "connection lost" error message — exercises
    // the same code path with a different error variant text.
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(
        ws_dir.path().join("src/auth.rs"),
        "fn login() -> bool { true }",
    )
    .unwrap();

    let scout = Arc::new(MockScout::default());
    scout.set_result(Ok(pathfinder_search::SearchResult {
        matches: vec![pathfinder_search::SearchMatch {
            file: "src/auth.rs".to_string(),
            line: 1,
            column: 1,
            content: "fn login() -> bool { true }".to_string(),
            context_before: vec![],
            context_after: vec![],
            enclosing_semantic_path: None,
            is_definition: None,
            version_hash: "sha256:abc".to_string(),
            known: Some(false),
        }],
        total_matches: 1,
        truncated: false,
        files_searched: 0,
        files_in_scope: 0,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    }));

    let lawyer = Arc::new(MockLawyer::default());
    lawyer.set_goto_definition_result(Err(LspError::ConnectionLost));

    let server = PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

    let params = LocateParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.get_definition_impl(params).await;
    let Ok(res) = result else {
        panic!("expected Ok with grep fallback, got Err");
    };
    let val = unpack_def(res);
    assert!(val.degraded, "should be degraded");
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::LspErrorGrepFallback),
        "degraded_reason: {:?}",
        val.degraded_reason
    );
}

#[tokio::test]
async fn test_get_definition_lsp_none_no_grep_fallback_returns_symbol_not_found() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));
    // Set up extract_symbols to return empty list for did_you_mean
    surgeon
        .extract_symbols_results
        .lock()
        .unwrap()
        .push(Ok(Vec::new()));

    // Default MockLawyer returns Ok(None) for goto_definition.
    // MockScout returns empty results → no grep fallback.
    let lawyer = Arc::new(MockLawyer::default());
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = LocateParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.get_definition_impl(params).await;
    let Err(err) = result else {
        panic!("expected error but got Ok");
    };
    let code = err
        .data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(code, "SYMBOL_NOT_FOUND");
}

#[tokio::test]
async fn test_get_definition_grep_fallback_with_mock_scout() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    // MockLawyer returns Ok(None) — triggers grep fallback
    let _lawyer = Arc::new(MockLawyer::default());

    // Use NoOpLawyer (NoLspAvailable path) + MockScout with results
    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Write a file so search can find it
    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(
        ws_dir.path().join("src/other.rs"),
        "fn login() -> bool { true }",
    )
    .unwrap();

    let scout = Arc::new(MockScout::default());
    scout.set_result(Ok(pathfinder_search::SearchResult {
        matches: vec![pathfinder_search::SearchMatch {
            file: "src/other.rs".to_string(),
            line: 1,
            column: 1,
            content: "fn login() -> bool { true }".to_string(),
            context_before: vec![],
            context_after: vec![],
            enclosing_semantic_path: None,
            is_definition: None,
            version_hash: "sha256:abc".to_string(),
            known: Some(false),
        }],
        total_matches: 1,
        truncated: false,
        files_searched: 0,
        files_in_scope: 0,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    }));

    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        scout,
        surgeon,
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = LocateParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.get_definition_impl(params).await;
    let Ok(res) = result else {
        panic!("expected Ok with grep fallback, got Err");
    };
    // Should return degraded result from grep
    let val = unpack_def(res);
    assert!(val.degraded);
    assert_eq!(val.file, "src/other.rs");
    assert!(val
        .degraded_reason
        .as_ref()
        .unwrap()
        .to_string()
        .contains("grep_fallback"));
}

// ── DS-1: DocumentGuard lifecycle tests ──────────────────────────────────

#[tokio::test]
async fn test_get_definition_closes_document_on_success() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());
    lawyer.set_goto_definition_result(Ok(Some(DefinitionLocation {
        file: "src/auth.rs".into(),
        line: 42,
        column: 5,
        preview: "pub fn login() -> bool {".into(),
    })));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer.clone());
    let params = LocateParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };

    let _ = server.get_definition_impl(params).await;

    // Yield so the spawned `did_close` task (from MockDocumentLease Drop) runs.
    tokio::task::yield_now().await;

    assert_eq!(
        lawyer.did_open_call_count(),
        lawyer.did_close_call_count(),
        "DS-1: did_open and did_close must be symmetric on success"
    );
}

#[tokio::test]
async fn test_get_definition_closes_document_on_lsp_error() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());
    // Simulate an LSP protocol error after the document is opened
    lawyer.set_goto_definition_result(Err(LspError::Protocol("LSP crashed".to_string())));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer.clone());
    let params = LocateParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };

    let _ = server.get_definition_impl(params).await;

    tokio::task::yield_now().await;

    assert_eq!(
        lawyer.did_open_call_count(),
        lawyer.did_close_call_count(),
        "DS-1: did_close must be called even when LSP returns an error"
    );
}

// ── TASK-3: did_you_mean suggestions ─────────────────────────────────────

/// When `get_definition` fails (LSP None, grep empty), and `extract_symbols`
/// returns close-but-not-exact symbol names, the error payload should contain
/// `did_you_mean` suggestions computed by Levenshtein distance.
#[tokio::test]
async fn test_get_definition_returns_did_you_mean_suggestions_on_symbol_not_found() {
    use pathfinder_treesitter::surgeon::{ExtractedSymbol, SymbolKind};

    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    // Provide close symbol names so did_you_mean can produce suggestions.
    // The caller is looking for "login" — we provide "logIn" and "logon" as candidates.
    let symbols = vec![
        ExtractedSymbol {
            name: "logIn".to_owned(),
            semantic_path: "logIn".to_owned(),
            kind: SymbolKind::Function,
            byte_range: 0..5,
            start_line: 0,
            end_line: 0,
            name_column: 0,
            access_level: pathfinder_treesitter::surgeon::AccessLevel::Public,
            children: vec![],
        },
        ExtractedSymbol {
            name: "logon".to_owned(),
            semantic_path: "logon".to_owned(),
            kind: SymbolKind::Function,
            byte_range: 10..15,
            start_line: 1,
            end_line: 1,
            name_column: 0,
            access_level: pathfinder_treesitter::surgeon::AccessLevel::Public,
            children: vec![],
        },
    ];
    surgeon
        .extract_symbols_results
        .lock()
        .unwrap()
        .push(Ok(symbols));

    // MockLawyer returns Ok(None) — triggers warmup retry → grep fallback → did_you_mean path.
    // MockScout returns empty results → grep fallback finds nothing → SymbolNotFound.
    let lawyer = Arc::new(MockLawyer::default());
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = LocateParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.get_definition_impl(params).await;
    let Err(err) = result else {
        panic!("expected SYMBOL_NOT_FOUND error, got Ok");
    };

    // Verify error code
    let code = err
        .data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(
        code, "SYMBOL_NOT_FOUND",
        "error code must be SYMBOL_NOT_FOUND"
    );

    // Verify did_you_mean field is non-empty and contains expected candidates.
    // The suggestions are nested in data.details.did_you_mean (via `to_details()`).
    let suggestions = err
        .data
        .as_ref()
        .and_then(|d| d.get("details"))
        .and_then(|d| d.get("did_you_mean"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        !suggestions.is_empty(),
        "did_you_mean must contain suggestions when similar symbols exist"
    );
    let has_login_variant = suggestions
        .iter()
        .any(|s| s.as_str().is_some_and(|s| s.contains("log")));
    assert!(
        has_login_variant,
        "suggestions should include close matches like 'logIn' or 'logon', got: {suggestions:?}"
    );
}

// ── get_definition grep fallback ────────────────────────────────────

#[tokio::test]
async fn test_get_definition_grep_fallback_when_lsp_returns_none() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    // MockLawyer with no result set returns Ok(None) by default
    let lawyer = Arc::new(MockLawyer::default());

    // Configure MockScout to return a search result for the grep fallback
    let scout = Arc::new(MockScout::default());
    scout.set_result(Ok(pathfinder_search::SearchResult {
        matches: vec![pathfinder_search::SearchMatch {
            file: "src/auth.rs".to_owned(),
            line: 10,
            column: 4,
            content: "pub fn login() -> bool {".to_owned(),
            context_before: vec![],
            context_after: vec![],
            enclosing_semantic_path: Some("src/auth.rs::login".to_owned()),
            is_definition: Some(true),
            version_hash: "hash".to_owned(),
            known: None,
        }],
        total_matches: 1,
        truncated: false,
        files_searched: 1,
        files_in_scope: 1,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    }));

    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    let server = PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

    let params = LocateParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.get_definition_impl(params).await;
    let call_res = result.expect("should succeed via grep fallback");
    let val = unpack_def(call_res);

    assert_eq!(val.file, "src/auth.rs");
    assert_eq!(val.line, 10);
    assert!(val.degraded, "should be degraded when using grep fallback");
    assert!(val.degraded_reason.is_some(), "degraded_reason must be set");
}

#[tokio::test]
async fn test_get_definition_grep_fallback_when_no_lsp() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    // NoOpLawyer returns NoLspAvailable for all methods
    let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);

    // Configure MockScout to return a search result for the grep fallback
    let scout = Arc::new(MockScout::default());
    scout.set_result(Ok(pathfinder_search::SearchResult {
        matches: vec![pathfinder_search::SearchMatch {
            file: "src/auth.rs".to_owned(),
            line: 10,
            column: 4,
            content: "pub fn login() -> bool {".to_owned(),
            context_before: vec![],
            context_after: vec![],
            enclosing_semantic_path: Some("src/auth.rs::login".to_owned()),
            is_definition: Some(true),
            version_hash: "hash".to_owned(),
            known: None,
        }],
        total_matches: 1,
        truncated: false,
        files_searched: 1,
        files_in_scope: 1,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    }));

    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    let server = PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

    let params = LocateParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.get_definition_impl(params).await;
    let call_res = result.expect("should succeed via grep fallback");
    let val = unpack_def(call_res);

    assert_eq!(val.file, "src/auth.rs");
    assert_eq!(val.line, 10);
    assert!(val.degraded, "should be degraded when using grep fallback");
}

// ── LspError::Timeout branch ────────────────────────────────────

#[tokio::test]
async fn test_get_definition_lsp_timeout_falls_back_to_grep() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(
        ws_dir.path().join("src/auth.rs"),
        "pub fn login() -> bool { true }",
    )
    .unwrap();

    let scout = Arc::new(MockScout::default());
    scout.set_result(Ok(pathfinder_search::SearchResult {
        matches: vec![pathfinder_search::SearchMatch {
            file: "src/auth.rs".to_string(),
            line: 1,
            column: 1,
            content: "pub fn login() -> bool { true }".to_string(),
            context_before: vec![],
            context_after: vec![],
            enclosing_semantic_path: None,
            is_definition: None,
            version_hash: "sha256:abc".to_string(),
            known: Some(false),
        }],
        total_matches: 1,
        truncated: false,
        files_searched: 0,
        files_in_scope: 0,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    }));

    let lawyer = Arc::new(MockLawyer::default());
    lawyer.set_goto_definition_result(Err(LspError::Timeout {
        operation: "goto_definition".to_string(),
        timeout_ms: 10000,
    }));

    let server = PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

    let params = LocateParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.get_definition_impl(params).await;
    let Ok(res) = result else {
        panic!("expected Ok with grep fallback after timeout, got Err");
    };
    let val = unpack_def(res);
    assert!(val.degraded, "should be degraded");
    assert_eq!(val.file, "src/auth.rs");
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::LspTimeoutGrepFallback),
        "degraded_reason should be LspTimeoutGrepFallback: {:?}",
        val.degraded_reason
    );
}

// ── Multi-file grep fallback chain (strategies 2-4) ─────────────

#[tokio::test]
async fn test_get_definition_multi_strategy_fallback() {
    // Tests that when Strategy 1 (file-scoped) returns empty,
    // the chain falls through to Strategy 3 (global) via set_results().
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(
        ws_dir.path().join("src/other.rs"),
        "pub fn login() -> bool { true }",
    )
    .unwrap();

    let scout = Arc::new(MockScout::default());
    // Strategy 1 (grep_definition_in_file): empty
    // Strategy 3 (grep_definition_global): finds match
    scout.set_results(vec![
        // Strategy 1 returns empty (file-scoped search)
        Ok(pathfinder_search::SearchResult {
            matches: vec![],
            total_matches: 0,
            truncated: false,
            files_searched: 1,
            files_in_scope: 1,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }),
        // Strategy 3 returns match (global search)
        Ok(pathfinder_search::SearchResult {
            matches: vec![pathfinder_search::SearchMatch {
                file: "src/other.rs".to_string(),
                line: 1,
                column: 1,
                content: "pub fn login() -> bool { true }".to_string(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:abc".to_string(),
                known: Some(false),
            }],
            total_matches: 1,
            truncated: false,
            files_searched: 1,
            files_in_scope: 1,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }),
    ]);

    // NoOpLawyer to force grep fallback path
    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        scout,
        surgeon,
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = LocateParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.get_definition_impl(params).await;
    let Ok(res) = result else {
        panic!("expected Ok with multi-strategy grep fallback, got Err");
    };
    let val = unpack_def(res);
    assert!(val.degraded, "should be degraded");
    assert_eq!(val.file, "src/other.rs");
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::NoLspGrepFallback),
        "degraded_reason: {:?}",
        val.degraded_reason
    );
}

// ── Warmup retry success path ───────────────────────────────────

#[tokio::test]
async fn test_get_definition_warmup_retry_success() {
    // LSP returns Ok(None) first (warmup), then Ok(Some(def)) on retry.
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());
    // First call (via queue): Ok(None) — simulates warmup
    lawyer.push_goto_definition_result(Ok(None));
    // Second call (via set, consumed after queue is empty): Ok(Some(def)) for retry
    lawyer.set_goto_definition_result(Ok(Some(DefinitionLocation {
        file: "src/auth.rs".into(),
        line: 42,
        column: 5,
        preview: "pub fn login() -> bool {".into(),
    })));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);
    let params = LocateParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.get_definition_impl(params).await;
    let call_res = result.expect("should succeed on retry");
    let val = unpack_def(call_res);

    assert_eq!(val.file, "src/auth.rs");
    assert_eq!(val.line, 42);
    assert!(!val.degraded, "should NOT be degraded on retry success");
    assert_eq!(
        val.resolution_strategy,
        Some("lsp_retry".to_owned()),
        "should indicate retry strategy"
    );
    assert_eq!(
        val.lsp_readiness,
        Some("warming_up".to_owned()),
        "should indicate warming_up"
    );
    assert_eq!(
        val.warm_start_in_progress,
        Some(true),
        "should indicate warm_start_in_progress"
    );
}

// ── grep fallback with 2-segment symbol path ───────────────────────────

#[tokio::test]
async fn test_get_definition_grep_fallback_with_two_segment_symbol() {
    // Tests that the grep fallback finds a definition when using a 2-segment
    // symbol path (e.g., MyStruct.my_method). Strategy 1 (file-scoped) finds
    // the match on the first pattern; subsequent patterns and strategies
    // consume empty results from the default MockScout.
    let surgeon = Arc::new(MockSurgeon::new());

    let mut scope = make_scope();
    scope.content = "pub fn my_method(&self) { ... }".to_string();
    surgeon.read_symbol_scope_results.lock().unwrap().clear();
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(scope));

    let scout = Arc::new(MockScout::default());
    // set_result: first search returns the match, all subsequent return empty.
    // Strategy 1 (grep_definition_in_file) finds the match on pattern 1.
    scout.set_result(Ok(pathfinder_search::SearchResult {
        matches: vec![pathfinder_search::SearchMatch {
            file: "src/mystruct.rs".to_string(),
            line: 10,
            column: 4,
            content: "pub fn my_method(&self) {}".to_string(),
            context_before: vec![],
            context_after: vec![],
            enclosing_semantic_path: None,
            is_definition: None,
            version_hash: "sha256:def".to_string(),
            known: Some(false),
        }],
        total_matches: 1,
        truncated: false,
        files_searched: 1,
        files_in_scope: 1,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    }));

    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    std::fs::write(
        ws_dir.path().join("src/mystruct.rs"),
        "impl MyStruct { pub fn my_method(&self) {} }",
    )
    .unwrap();

    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        scout,
        surgeon,
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = LocateParams {
        semantic_path: Some("src/mystruct.rs::MyStruct.my_method".to_owned()),
        ..Default::default()
    };
    let result = server.get_definition_impl(params).await;
    match &result {
        Ok(res) => {
            let val = unpack_def(res.clone());
            assert!(val.degraded, "should be degraded");
            assert_eq!(val.file, "src/mystruct.rs");
            assert_eq!(val.line, 10);
            assert!(
                val.degraded_reason.is_some(),
                "degraded_reason should be set"
            );
            // With 2-segment symbol and file-scoped match, reason is GrepFallbackFileScoped
            assert_eq!(
                val.degraded_reason,
                Some(DegradedReason::NoLspGrepFallback),
                "degraded_reason: {:?}",
                val.degraded_reason
            );
        }
        Err(err) => {
            let code = err
                .data
                .as_ref()
                .and_then(|d| d.get("error"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            panic!("expected Ok with grep fallback, got Err({code}): {err:?}");
        }
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn test_get_definition_grep_impl_method_strategy() {
    // Tests Strategy 2: grep_impl_method. When a 2-segment symbol like
    // Sandbox.check is looked up and Strategy 1 (file-scoped) returns empty,
    // the fallback searches for the impl block, then for the method within it.
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(
        ws_dir.path().join("src/sandbox.rs"),
        "impl Sandbox {\n    pub fn check(&self) -> bool { true }\n}",
    )
    .unwrap();

    let scout = Arc::new(MockScout::default());
    // Queue results for the sequential scout.search calls.
    // definition_patterns("rs", "check") produces 4 patterns, each consuming one result.
    // Then grep_impl_method needs 2 more (impl block + method search).
    scout.set_results(vec![
        // Strategy 1 pattern 1: fn\s+check\b — empty (no fn in sandbox.rs matches)
        Ok(pathfinder_search::SearchResult {
            matches: vec![],
            total_matches: 0,
            truncated: false,
            files_searched: 1,
            files_in_scope: 1,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }),
        // Strategy 1 pattern 2: struct|enum|trait|type|mod\s+check\b — empty
        Ok(pathfinder_search::SearchResult {
            matches: vec![],
            total_matches: 0,
            truncated: false,
            files_searched: 1,
            files_in_scope: 1,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }),
        // Strategy 1 pattern 3: const|static\s+check\b — empty
        Ok(pathfinder_search::SearchResult {
            matches: vec![],
            total_matches: 0,
            truncated: false,
            files_searched: 1,
            files_in_scope: 1,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }),
        // Strategy 1 pattern 4: \bcheck\b — empty
        Ok(pathfinder_search::SearchResult {
            matches: vec![],
            total_matches: 0,
            truncated: false,
            files_searched: 1,
            files_in_scope: 1,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }),
        // Strategy 2 step 1: impl block search finds src/sandbox.rs
        Ok(pathfinder_search::SearchResult {
            matches: vec![pathfinder_search::SearchMatch {
                file: "src/sandbox.rs".to_string(),
                line: 1,
                column: 1,
                content: "impl Sandbox {".to_string(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:abc".to_string(),
                known: Some(false),
            }],
            total_matches: 1,
            truncated: false,
            files_searched: 1,
            files_in_scope: 1,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }),
        // Strategy 2 step 2: method search finds fn check in src/sandbox.rs
        Ok(pathfinder_search::SearchResult {
            matches: vec![pathfinder_search::SearchMatch {
                file: "src/sandbox.rs".to_string(),
                line: 2,
                column: 4,
                content: "pub fn check(&self) -> bool { true }".to_string(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:def".to_string(),
                known: Some(false),
            }],
            total_matches: 1,
            truncated: false,
            files_searched: 1,
            files_in_scope: 1,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }),
    ]);

    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        scout,
        surgeon,
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = LocateParams {
        semantic_path: Some("src/sandbox.rs::Sandbox.check".to_owned()),
        ..Default::default()
    };
    let result = server.get_definition_impl(params).await;
    let Ok(res) = result else {
        panic!("expected Ok with grep_impl_method fallback, got Err");
    };
    let val = unpack_def(res);
    assert!(val.degraded, "should be degraded");
    assert_eq!(val.file, "src/sandbox.rs");
    assert_eq!(val.line, 2);
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::GrepFallbackImplScoped),
        "degraded_reason should be GrepFallbackImplScoped, got {:?}",
        val.degraded_reason
    );
    assert_eq!(
        val.resolution_strategy,
        Some("grep_impl".to_owned()),
        "resolution_strategy should be grep_impl"
    );
}
