use super::super::test_helpers::{make_scope, make_server_with_lawyer, make_temp_workspace};
use crate::server::types::InspectParams;
use crate::server::PathfinderServer;
use pathfinder_common::config::PathfinderConfig;
use pathfinder_common::sandbox::Sandbox;
use pathfinder_common::types::{DegradedReason, WorkspaceRoot};
use pathfinder_lsp::types::{CallHierarchyCall, CallHierarchyItem};
use pathfinder_lsp::{DefinitionLocation, MockLawyer};
use pathfinder_search::MockScout;
use pathfinder_treesitter::mock::MockSurgeon;
use std::sync::Arc;

// ── read_with_deep_context ────────────────────────────────────────

#[tokio::test]
async fn test_read_with_deep_context_degrades_when_call_hierarchy_unsupported() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().extend([
        Ok(make_scope()),
        Ok(make_scope()),
        Ok(make_scope()),
    ]);

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

    let params = InspectParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.read_with_deep_context_impl(params).await;
    let call_res = result.expect("should succeed");
    let text_content = match &call_res.content[0].raw {
        rmcp::model::RawContent::Text(t) => t.text.clone(),
        _ => panic!("expected text content"),
    };
    let val: crate::server::types::ReadWithDeepContextMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(
        text_content.starts_with("DEGRADED (grep_fallback_dependencies) — results are heuristic (grep-based), verify manually — fallback: use search for authoritative results\n\n0 dependencies loaded\n\nfn login() { }"),
        "text_content: {text_content}"
    );
    assert!(val.degraded);
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::GrepFallbackDependencies)
    );
    assert!(val.dependencies.is_empty());
}

#[tokio::test]
async fn test_read_with_deep_context_lsp_populates_dependencies() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());

    let item = CallHierarchyItem {
        name: "login".into(),
        kind: "function".into(),
        detail: None,
        file: "src/auth.rs".into(),
        line: 9,
        column: 4,
        data: None,
    };
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item.clone()]));

    lawyer.push_outgoing_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "validate_token".into(),
            kind: "function".into(),
            detail: Some("fn validate_token() -> bool".into()),
            file: "src/token.rs".into(),
            line: 15,
            column: 4,
            data: None,
        },
        call_sites: vec![9],
    }]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = InspectParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.read_with_deep_context_impl(params).await;
    let call_res = result.expect("should succeed");
    let text_content = match &call_res.content[0].raw {
        rmcp::model::RawContent::Text(t) => t.text.clone(),
        _ => panic!("expected text content"),
    };
    let val: crate::server::types::ReadWithDeepContextMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(
        text_content.starts_with("1 dependencies loaded\n  fn validate_token() -> bool (src/token.rs:L15)\n\nfn login() { }"),
        "text_content: {text_content}"
    );
    assert!(!val.degraded);
    assert_eq!(val.degraded_reason, None);
    assert_eq!(val.dependencies.len(), 1);
    assert_eq!(
        val.dependencies[0].semantic_path,
        "src/token.rs::validate_token"
    );
    assert_eq!(val.dependencies[0].signature, "fn validate_token() -> bool");
    assert_eq!(val.dependencies[0].file, "src/token.rs");
    assert_eq!(val.dependencies[0].line, 15);
}

// ── read_with_deep_context with outgoing call error ───────────────────

#[tokio::test]
async fn test_read_with_deep_context_outgoing_error_degrades() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());

    let item = CallHierarchyItem {
        name: "login".into(),
        kind: "function".into(),
        detail: None,
        file: "src/auth.rs".into(),
        line: 9,
        column: 4,
        data: None,
    };
    // Prepare succeeds
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));
    // But outgoing call fails
    lawyer.push_outgoing_call_result(Err(pathfinder_lsp::LspError::Protocol(
        "outgoing failed".to_string(),
    )));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = InspectParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.read_with_deep_context_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::ReadWithDeepContextMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Degraded because outgoing call failed
    assert!(val.degraded);
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::LspErrorGrepFallback)
    );
    assert!(val.dependencies.is_empty());
}

// ── read_with_deep_context with empty hierarchy (confirmed zero deps) ──

#[tokio::test]
async fn test_read_with_deep_context_empty_hierarchy_zero_deps() {
    // call_hierarchy_prepare returns Ok([]) AND goto_definition probe returns Ok(Some(...))
    // → LSP is warm, confirmed zero deps. Must NOT be degraded.
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());
    // Empty call hierarchy — ambiguous on its own
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![]));
    // Probe: goto_definition succeeds → LSP is warm → confirmed zero
    lawyer.set_goto_definition_result(Ok(Some(DefinitionLocation {
        file: "src/auth.rs".into(),
        line: 10,
        column: 4,
        preview: "fn login() {}".into(),
    })));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = InspectParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.read_with_deep_context_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::ReadWithDeepContextMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // NOT degraded — LSP warm, genuinely zero deps confirmed
    assert!(
        !val.degraded,
        "must not be degraded when probe confirms LSP is warm"
    );
    assert_eq!(val.degraded_reason, None);
    assert!(val.dependencies.is_empty(), "confirmed zero dependencies");
}

#[tokio::test]
async fn test_read_with_deep_context_empty_hierarchy_warmup_degrades() {
    // call_hierarchy_prepare returns Ok([]) AND goto_definition probe returns Ok(None)
    // → LSP is still warming up. Falls through to grep fallback (PATCH-005).
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().extend([
        Ok(make_scope()),
        Ok(make_scope()),
        Ok(make_scope()),
    ]);

    let lawyer = Arc::new(MockLawyer::default());
    // Empty call hierarchy
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![]));
    // Probe: goto_definition returns Ok(None) → LSP is still warming up
    // MockLawyer::default() already returns Ok(None) for goto_definition, so no extra setup needed.

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = InspectParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.read_with_deep_context_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::ReadWithDeepContextMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // DEGRADED — grep fallback used because LSP warmup returns empty hierarchy
    assert!(
        val.degraded,
        "must be degraded when goto_definition probe also returns None"
    );
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::GrepFallbackDependencies),
        "degraded_reason must indicate grep fallback was used"
    );
    assert!(val.dependencies.is_empty());
}

#[tokio::test]
async fn test_read_with_deep_context_closes_document_on_success() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());
    let item = CallHierarchyItem {
        name: "login".into(),
        kind: "function".into(),
        detail: None,
        file: "src/auth.rs".into(),
        line: 9,
        column: 4,
        data: None,
    };
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer.clone());
    let params = InspectParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };

    let _ = server.read_with_deep_context_impl(params).await;

    tokio::task::yield_now().await;

    assert_eq!(
        lawyer.did_open_call_count(),
        lawyer.did_close_call_count(),
        "DS-1: did_open and did_close must be symmetric in read_with_deep_context"
    );
}

// ── TASK-7: max_dependencies truncation ───────────────────────────────────

/// When outgoing dependencies exceed `max_dependencies`, the result must be
/// truncated and `dependencies_truncated = true`.
#[tokio::test]
async fn test_read_with_deep_context_max_dependencies_truncates_results() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());

    let item = CallHierarchyItem {
        name: "login".into(),
        kind: "function".into(),
        detail: None,
        file: "src/auth.rs".into(),
        line: 9,
        column: 4,
        data: None,
    };
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item.clone()]));

    // Push 5 outgoing callees (each on a distinct file)
    let outgoing_calls: Vec<CallHierarchyCall> = (1..=5)
        .map(|i| CallHierarchyCall {
            item: CallHierarchyItem {
                name: format!("dep_{i}"),
                kind: "function".into(),
                detail: Some(format!("fn dep_{i}()")),
                file: format!("src/dep_{i}.rs"),
                line: i * 5,
                column: 4,
                data: None,
            },
            call_sites: vec![i * 5],
        })
        .collect();
    lawyer.push_outgoing_call_result(Ok(outgoing_calls));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = InspectParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        max_dependencies: 2, // cap below the 5 available
        ..Default::default()
    };
    let result = server
        .read_with_deep_context_impl(params)
        .await
        .expect("should succeed");
    let val: crate::server::types::ReadWithDeepContextMetadata =
        serde_json::from_value(result.structured_content.unwrap()).unwrap();

    assert_eq!(
        val.dependencies.len(),
        2,
        "dependencies must be capped at max_dependencies=2"
    );
    assert!(
        val.dependencies_truncated,
        "dependencies_truncated must be true when budget is exhausted"
    );
}

/// Verify that the `default_max_dependencies()` constant is 50.
#[test]
fn test_read_with_deep_context_default_max_dependencies_is_50() {
    use crate::server::types::default_max_dependencies;
    assert_eq!(
        default_max_dependencies(),
        50,
        "default_max_dependencies must be 50 per the implementation"
    );
}

// ── attempt_grep_fallback with resolved candidates ───────────────

#[tokio::test]
async fn test_read_with_deep_context_grep_fallback_resolves_candidates() {
    // PATCH-005: When LSP is unavailable, grep fallback extracts call candidates
    // from the symbol body and resolves each via search.
    let surgeon = Arc::new(MockSurgeon::new());
    // First call: read_symbol_scope_enriched (for the main function)
    // Second call: read_symbol_scope (for grep fallback candidate extraction)
    // Use scope content that contains a function call to exercise candidate extraction.
    let mut scope = make_scope();
    scope.content = "fn login() -> bool { validate_token() }".to_string();
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .extend([Ok(scope.clone()), Ok(scope)]);

    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(
        ws_dir.path().join("src/auth.rs"),
        "fn login() -> bool { validate_token() }",
    )
    .unwrap();

    let scout = Arc::new(MockScout::default());
    // First search: resolve "validate_token" candidate
    scout.set_result(Ok(pathfinder_search::SearchResult {
        matches: vec![pathfinder_search::SearchMatch {
            file: "src/token.rs".to_string(),
            line: 5,
            column: 1,
            content: "fn validate_token() -> bool { true }".to_string(),
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
    }));

    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        scout,
        surgeon,
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = InspectParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.read_with_deep_context_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::ReadWithDeepContextMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(val.degraded);
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::GrepFallbackDependencies)
    );
    // The grep fallback should have resolved "validate_token" from the scope body.
    assert!(
        !val.dependencies.is_empty(),
        "expected at least 1 resolved dependency, got {}",
        val.dependencies.len()
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn test_attempt_grep_fallback_semantic_path_is_hierarchical() {
    let surgeon = Arc::new(MockSurgeon::new());
    let mut scope = make_scope();
    scope.content = "fn login() -> bool { validate_token() }".to_string();
    // First read: read_symbol_scope_enriched (for login)
    // Second read: read_symbol_scope (candidate extraction)
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .extend([Ok(scope.clone()), Ok(scope)]);

    // When the grep fallback resolves the "validate_token" candidate to src/token.rs:5,
    // we want to mock treesitter enclosing_symbol_detail returning a qualified path.
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(Some(pathfinder_treesitter::surgeon::ExtractedSymbol {
            name: "validate_token".to_owned(),
            semantic_path: "TokenValidator.validate_token".to_owned(),
            start_line: 4,
            end_line: 8,
            name_column: 4,
            kind: pathfinder_treesitter::surgeon::SymbolKind::Function,
            byte_range: 0..0,
            access_level: pathfinder_treesitter::surgeon::AccessLevel::Public,
            children: vec![],
        })));

    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(
        ws_dir.path().join("src/auth.rs"),
        "fn login() -> bool { validate_token() }",
    )
    .unwrap();

    let scout = Arc::new(MockScout::default());
    scout.set_result(Ok(pathfinder_search::SearchResult {
        matches: vec![pathfinder_search::SearchMatch {
            file: "src/token.rs".to_string(),
            line: 5,
            column: 1,
            content: "fn validate_token() -> bool { true }".to_string(),
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
    }));

    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        scout,
        surgeon,
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = InspectParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.read_with_deep_context_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::ReadWithDeepContextMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(val.degraded);
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::GrepFallbackDependencies)
    );
    assert_eq!(val.dependencies.len(), 1);
    assert_eq!(
        val.dependencies[0].semantic_path, "src/token.rs::TokenValidator.validate_token",
        "grep fallback dependency path should be qualified using surgeon enclosing_symbol_detail"
    );
}

// ── detail: None fallback ────────────────────────────────────────

#[tokio::test]
async fn test_read_with_deep_context_detail_none_falls_back_to_name() {
    // When callee.detail is None, the signature should fall back to callee.name.
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());

    let item = CallHierarchyItem {
        name: "login".into(),
        kind: "function".into(),
        detail: None,
        file: "src/auth.rs".into(),
        line: 9,
        column: 4,
        data: None,
    };
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));

    // Outgoing call with detail=None
    lawyer.push_outgoing_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "validate_token".into(),
            kind: "function".into(),
            detail: None, // No detail — should fall back to name
            file: "src/token.rs".into(),
            line: 15,
            column: 4,
            data: None,
        },
        call_sites: vec![9],
    }]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = InspectParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.read_with_deep_context_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::ReadWithDeepContextMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(!val.degraded);
    assert_eq!(val.dependencies.len(), 1);
    // Signature should be the name since detail is None
    assert_eq!(val.dependencies[0].signature, "validate_token");
    assert_eq!(
        val.dependencies[0].semantic_path,
        "src/token.rs::validate_token"
    );
}

// ── Warmup retry success ────────────────────────────────────────

#[tokio::test]
async fn test_read_with_deep_context_warmup_retry_success() {
    // LSP returns Ok([]) first (warmup), then Ok(items) on retry.
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());

    let item = CallHierarchyItem {
        name: "login".into(),
        kind: "function".into(),
        detail: None,
        file: "src/auth.rs".into(),
        line: 9,
        column: 4,
        data: None,
    };

    // First call: empty (warmup)
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![]));
    // goto_definition probe: Ok(None) — confirms LSP warming up
    // (default MockLawyer returns Ok(None))
    // Retry call: succeeds with items
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));
    // Outgoing deps for retry
    lawyer.push_outgoing_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "validate_token".into(),
            kind: "function".into(),
            detail: Some("fn validate_token() -> bool".into()),
            file: "src/token.rs".into(),
            line: 15,
            column: 4,
            data: None,
        },
        call_sites: vec![9],
    }]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = InspectParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.read_with_deep_context_impl(params).await;
    let call_res = result.expect("should succeed on retry");
    let val: crate::server::types::ReadWithDeepContextMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(!val.degraded, "should NOT be degraded on retry success");
    assert_eq!(val.dependencies.len(), 1);
    assert_eq!(
        val.dependencies[0].semantic_path,
        "src/token.rs::validate_token"
    );
}

// ── project_only filtering in append_outgoing_deps ──────────────

#[tokio::test]
async fn test_read_with_deep_context_filters_non_workspace_deps() {
    // When project_only=true, callees from non-workspace files should be filtered.
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());

    let item = CallHierarchyItem {
        name: "login".into(),
        kind: "function".into(),
        detail: None,
        file: "src/auth.rs".into(),
        line: 9,
        column: 4,
        data: None,
    };
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));

    // Outgoing: one workspace dep + one non-workspace (absolute path = stdlib)
    lawyer.push_outgoing_call_result(Ok(vec![
        CallHierarchyCall {
            item: CallHierarchyItem {
                name: "validate_token".into(),
                kind: "function".into(),
                detail: Some("fn validate_token()".into()),
                file: "src/token.rs".into(),
                line: 15,
                column: 4,
                data: None,
            },
            call_sites: vec![9],
        },
        CallHierarchyCall {
            item: CallHierarchyItem {
                name: "println".into(),
                kind: "function".into(),
                detail: Some("macro println".into()),
                file: "/rust/library/std/src/io/stdio.rs".into(), // absolute = non-workspace
                line: 100,
                column: 1,
                data: None,
            },
            call_sites: vec![9],
        },
    ]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = InspectParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.read_with_deep_context_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::ReadWithDeepContextMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(!val.degraded);
    // Only the workspace dep should be included
    assert_eq!(val.dependencies.len(), 1);
    assert_eq!(
        val.dependencies[0].semantic_path,
        "src/token.rs::validate_token"
    );
}

// ── Dedup in append_outgoing_deps ────────────────────────────────

#[tokio::test]
async fn test_read_with_deep_context_deduplicates_deps() {
    // When LSP returns the same callee multiple times, dedup should prevent duplicates.
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());

    let item = CallHierarchyItem {
        name: "login".into(),
        kind: "function".into(),
        detail: None,
        file: "src/auth.rs".into(),
        line: 9,
        column: 4,
        data: None,
    };
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));

    // Outgoing: same callee returned twice (simulates LSP duplicate)
    lawyer.push_outgoing_call_result(Ok(vec![
        CallHierarchyCall {
            item: CallHierarchyItem {
                name: "validate_token".into(),
                kind: "function".into(),
                detail: Some("fn validate_token()".into()),
                file: "src/token.rs".into(),
                line: 15,
                column: 4,
                data: None,
            },
            call_sites: vec![9],
        },
        CallHierarchyCall {
            item: CallHierarchyItem {
                name: "validate_token".into(),
                kind: "function".into(),
                detail: Some("fn validate_token()".into()),
                file: "src/token.rs".into(),
                line: 15,
                column: 4,
                data: None,
            },
            call_sites: vec![10],
        },
    ]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = InspectParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.read_with_deep_context_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::ReadWithDeepContextMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(!val.degraded);
    // Dedup should prevent the duplicate
    assert_eq!(
        val.dependencies.len(),
        1,
        "duplicate callees should be deduped, got {}",
        val.dependencies.len()
    );
}

// ── java_resolve_pattern tests ─────────────────────────────────────────

fn make_java_re(candidate: &str) -> regex::Regex {
    let pattern = super::super::java_resolve_pattern(candidate);
    regex::Regex::new(&pattern).expect("valid regex")
}

#[test]
fn test_java_resolve_pattern_constructor() {
    let re = make_java_re("MyClass");
    assert!(re.is_match("public MyClass(String name)"));
    assert!(re.is_match("  private MyClass()"));
    assert!(
        re.is_match("public void MyClass()"),
        "method arm correctly matches 'public void MyClass()' as a method definition"
    );
    assert!(!re.is_match("new MyClass()"));
}

#[test]
fn test_java_resolve_pattern_method() {
    let re = make_java_re("process");
    assert!(re.is_match("public void process()"));
    assert!(re.is_match("int[] process()"));
    assert!(re.is_match("public int[][] process()"));
    assert!(re.is_match("public <T> T process()"));
    assert!(re.is_match("public strictfp void process()"));
    assert!(!re.is_match("new process()"));
}

#[test]
fn test_java_resolve_pattern_record() {
    let re = make_java_re("Point");
    assert!(re.is_match("public record Point(int x, int y)"));
    assert!(re.is_match("record Point(String name)"));
}

#[test]
fn test_java_resolve_pattern_class() {
    let re = make_java_re("MyClass");
    assert!(re.is_match("public class MyClass"));
    assert!(re.is_match("sealed class MyClass"));
    assert!(re.is_match("non-sealed class MyClass"));
    assert!(re.is_match("public strictfp class MyClass"));
}

#[test]
fn test_java_resolve_pattern_rejects_false_positives() {
    let re = make_java_re("MyError");
    assert!(!re.is_match("throw new MyError(\"msg\")"));
    assert!(!re.is_match("return new MyError()"));
    let re2 = make_java_re("MyClass");
    assert!(
        !re2.is_match("new MyClass()"),
        "constructor call 'new MyClass()' must not match"
    );
}

#[test]
fn test_java_resolve_pattern_bounded_generics() {
    let re = make_java_re("sort");
    assert!(
        re.is_match("public <T extends Comparable<T>> void sort(List<T> list)"),
        "must match bounded generics in method type params"
    );
    let re2 = make_java_re("MyClass");
    assert!(
        re2.is_match("public <T extends Comparable<T>> MyClass(T item)"),
        "must match bounded generics in constructor type params"
    );
}

#[test]
fn test_java_resolve_pattern_static_record_and_strictfp_class() {
    let re = make_java_re("Inner");
    assert!(
        re.is_match("static record Inner(String name)"),
        "must match 'static record Inner(String name)'"
    );
    let re2 = make_java_re("MathUtils");
    assert!(
        re2.is_match("strictfp class MathUtils"),
        "must match 'strictfp class MathUtils'"
    );
    assert!(
        re2.is_match("public strictfp class MathUtils"),
        "must match 'public strictfp class MathUtils'"
    );
}

// ── extract_file_imports tests ────────────────────────────────────────────

/// Helper: write a file to the workspace and call `extract_file_imports`.
async fn extract_imports_for_content(filename: &str, content: &str) -> Vec<String> {
    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(MockSurgeon::new()),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let file_path = ws_dir.path().join(filename);
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&file_path, content).unwrap();
    server.extract_file_imports(&file_path).await
}

#[tokio::test]
async fn test_extract_file_imports_java() {
    let imports = extract_imports_for_content(
        "Foo.java",
        "package com.example;\nimport java.util.List;\nimport java.io.File;\npublic class Foo {}",
    )
    .await;
    assert_eq!(imports.len(), 3);
    assert!(imports[0].contains("package com.example"));
    assert!(imports[1].contains("import java.util.List"));
}

#[tokio::test]
async fn test_extract_file_imports_kotlin() {
    let imports = extract_imports_for_content(
        "Foo.kt",
        "package com.example\nimport kotlin.collections.List\nclass Foo",
    )
    .await;
    assert_eq!(imports.len(), 2);
}

#[tokio::test]
async fn test_extract_file_imports_csharp() {
    let imports = extract_imports_for_content(
        "Foo.cs",
        "using System;\nusing System.Collections.Generic;\nnamespace MyApp {}\nclass Foo {}",
    )
    .await;
    assert_eq!(imports.len(), 3);
    assert!(imports[0].contains("using System"));
    assert!(imports[2].contains("namespace MyApp"));
}

#[tokio::test]
async fn test_extract_file_imports_python() {
    let imports = extract_imports_for_content(
        "foo.py",
        "import os\nfrom pathlib import Path\ndef foo(): pass",
    )
    .await;
    assert_eq!(imports.len(), 2);
    assert!(imports[0].contains("import os"));
    assert!(imports[1].contains("from pathlib"));
}

#[tokio::test]
async fn test_extract_file_imports_typescript() {
    let imports = extract_imports_for_content(
        "foo.ts",
        "import { Foo } from './foo';\nconst x = require('bar');\nexport class Baz {}",
    )
    .await;
    assert_eq!(imports.len(), 2);
    assert!(imports[0].contains("import { Foo }"));
    assert!(imports[1].contains("require("));
}

#[tokio::test]
async fn test_extract_file_imports_tsx() {
    let imports = extract_imports_for_content(
        "foo.tsx",
        "import React from 'react';\nimport{Component} from 'react';\nconst App = () => {};",
    )
    .await;
    assert_eq!(imports.len(), 2);
}

#[tokio::test]
async fn test_extract_file_imports_javascript() {
    let imports = extract_imports_for_content(
        "foo.js",
        "import lodash from 'lodash';\nconst fs = require('fs');\nmodule.exports = {};",
    )
    .await;
    assert_eq!(imports.len(), 2);
}

#[tokio::test]
async fn test_extract_file_imports_jsx() {
    let imports = extract_imports_for_content(
        "foo.jsx",
        "import React from 'react';\nconst Component = () => <div/>;",
    )
    .await;
    assert_eq!(imports.len(), 1);
}

#[tokio::test]
async fn test_extract_file_imports_mjs() {
    let imports = extract_imports_for_content(
        "foo.mjs",
        "import { readFile } from 'fs/promises';\nexport const x = 1;",
    )
    .await;
    assert_eq!(imports.len(), 1);
}

#[tokio::test]
async fn test_extract_file_imports_cjs() {
    let imports = extract_imports_for_content(
        "foo.cjs",
        "const path = require('path');\nmodule.exports = {};",
    )
    .await;
    assert_eq!(imports.len(), 1);
    assert!(imports[0].contains("require("));
}

#[tokio::test]
async fn test_extract_file_imports_rust() {
    let imports = extract_imports_for_content(
        "foo.rs",
        "use std::io::Read;\nextern crate serde;\nfn main() {}",
    )
    .await;
    assert_eq!(imports.len(), 2);
    assert!(imports[0].contains("use std::io::Read"));
    assert!(imports[1].contains("extern crate serde"));
}

#[tokio::test]
async fn test_extract_file_imports_go_single_line() {
    let imports = extract_imports_for_content("foo.go", "import \"fmt\"\nfunc main() {}").await;
    assert_eq!(imports.len(), 1);
    assert!(imports[0].contains("import \"fmt\""));
}

#[tokio::test]
async fn test_extract_file_imports_go_multiline_block() {
    let content = "package main\n\nimport (\n\t\"fmt\"\n\t\"os\"\n)\n\nfunc main() {}";
    let imports = extract_imports_for_content("foo.go", content).await;
    // Should capture: `import (`, `"fmt"`, `"os"`, `)`
    assert_eq!(imports.len(), 4);
    assert!(imports[0].contains("import ("));
    assert!(imports[3].trim() == ")");
}

#[tokio::test]
async fn test_extract_file_imports_swift() {
    let imports =
        extract_imports_for_content("Foo.swift", "import Foundation\nimport UIKit\nclass Foo {}")
            .await;
    assert_eq!(imports.len(), 2);
    assert!(imports[0].contains("import Foundation"));
}

#[tokio::test]
async fn test_extract_file_imports_ruby() {
    let imports = extract_imports_for_content(
        "foo.rb",
        "require 'json'\nrequire_relative 'helper'\nclass Foo; end",
    )
    .await;
    assert_eq!(imports.len(), 2);
    assert!(imports[0].contains("require 'json'"));
    assert!(imports[1].contains("require_relative 'helper'"));
}

#[tokio::test]
async fn test_extract_file_imports_unknown_ext() {
    // Unknown extension falls through to generic `import ` check
    let imports =
        extract_imports_for_content("foo.xyz", "import something\nrandom line\nimport another")
            .await;
    assert_eq!(imports.len(), 2);
}

#[tokio::test]
async fn test_extract_file_imports_max_cap() {
    // MAX_IMPORTS = 200; generate 210 import lines, expect only 200
    let lines: Vec<String> = (0..210).map(|i| format!("import pkg_{i};")).collect();
    let content = lines.join("\n");
    let imports = extract_imports_for_content("Foo.java", &content).await;
    assert_eq!(imports.len(), 200);
}

#[tokio::test]
async fn test_extract_file_imports_file_not_found() {
    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(MockSurgeon::new()),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );
    let nonexistent = ws_dir.path().join("nonexistent.java");
    let imports = server.extract_file_imports(&nonexistent).await;
    assert!(imports.is_empty());
}

#[tokio::test]
async fn test_extract_file_imports_scala() {
    let imports = extract_imports_for_content(
        "Foo.scala",
        "package com.example\nimport scala.collection.mutable\nobject Foo",
    )
    .await;
    assert_eq!(imports.len(), 2);
}

// ── include_imports=true path ────────────────────────────────────────

#[tokio::test]
async fn test_read_with_deep_context_include_imports_true() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().extend([
        Ok(make_scope()),
        Ok(make_scope()),
        Ok(make_scope()),
    ]);

    let ws_dir = make_temp_workspace();
    // Write a Rust file with `use` imports so extract_file_imports finds something
    std::fs::write(
        ws_dir.path().join("src/auth.rs"),
        "use std::io::Read;\nuse std::collections::HashMap;\n\nfn login() { }",
    )
    .unwrap();

    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        surgeon,
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = InspectParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        include_imports: true,
        ..Default::default()
    };
    let result = server.read_with_deep_context_impl(params).await;
    let call_res = result.expect("should succeed");
    let text_content = match &call_res.content[0].raw {
        rmcp::model::RawContent::Text(t) => t.text.clone(),
        _ => panic!("expected text content"),
    };
    let val: crate::server::types::ReadWithDeepContextMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // imports should be populated
    assert!(!val.imports.is_empty(), "imports must not be empty");
    // Text channel should contain "Imports:" block
    assert!(
        text_content.contains("Imports:"),
        "text must contain Imports block, got: {text_content}"
    );
    assert!(
        text_content.contains("use std::io::Read"),
        "text must contain imported line"
    );
}

// ── lsp_readiness and resolution_strategy metadata branches ──────────────

#[tokio::test]
async fn test_read_with_deep_context_lsp_error_grep_fallback_metadata() {
    // When outgoing call fails (LspErrorGrepFallback), lsp_readiness should be
    // "unavailable" and resolution_strategy should be "treesitter_fallback".
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());
    let item = CallHierarchyItem {
        name: "login".into(),
        kind: "function".into(),
        detail: None,
        file: "src/auth.rs".into(),
        line: 9,
        column: 4,
        data: None,
    };
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));
    lawyer.push_outgoing_call_result(Err(pathfinder_lsp::LspError::Protocol(
        "call_hierarchy_outgoing failed".to_string(),
    )));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = InspectParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.read_with_deep_context_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::ReadWithDeepContextMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(val.degraded);
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::LspErrorGrepFallback)
    );
    // LspErrorGrepFallback is not in the warming_up list, so lsp_readiness = "unavailable"
    assert_eq!(val.lsp_readiness, Some("unavailable".to_owned()));
    // resolution_strategy for degraded + not NoLsp + not GrepFallbackDependencies
    assert_eq!(
        val.resolution_strategy,
        Some("treesitter_fallback".to_owned())
    );
    // warm_start_in_progress is None when lsp_readiness is "unavailable"
    assert_eq!(val.warm_start_in_progress, None);
}

#[tokio::test]
async fn test_read_with_deep_context_non_degraded_resolution_strategy() {
    // When not degraded AND engines contains "lsp", resolution_strategy = "lsp_call_hierarchy"
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());
    let item = CallHierarchyItem {
        name: "login".into(),
        kind: "function".into(),
        detail: None,
        file: "src/auth.rs".into(),
        line: 9,
        column: 4,
        data: None,
    };
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));
    lawyer.push_outgoing_call_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = InspectParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.read_with_deep_context_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::ReadWithDeepContextMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(!val.degraded);
    assert_eq!(val.lsp_readiness, Some("ready".to_owned()));
    assert_eq!(
        val.resolution_strategy,
        Some("lsp_call_hierarchy".to_owned())
    );
    assert_eq!(val.warm_start_in_progress, Some(false));
}

#[tokio::test]
async fn test_read_with_deep_context_no_lsp_resolution_treesitter_direct() {
    // When degraded + NoLsp, resolution_strategy = "treesitter_direct"
    let surgeon = Arc::new(MockSurgeon::new());
    // read_symbol_scope_enriched + attempt_grep_fallback read_symbol_scope
    surgeon.read_symbol_scope_results.lock().unwrap().extend([
        Ok(make_scope()),
        Ok(make_scope()),
        Ok(make_scope()),
    ]);

    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // NoOpLawyer → NoLspAvailable error → triggers grep fallback
    // But grep fallback changes degraded_reason to GrepFallbackDependencies.
    // To get NoLsp, we need a case where no fallback runs.
    // This actually doesn't happen in the current code because NoLspAvailable
    // always triggers grep fallback. Let's verify the grep_fallback path.
    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        surgeon,
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = InspectParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.read_with_deep_context_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::ReadWithDeepContextMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(val.degraded);
    // NoLspAvailable triggers grep fallback, which sets GrepFallbackDependencies
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::GrepFallbackDependencies)
    );
    assert_eq!(val.resolution_strategy, Some("grep_fallback".to_owned()));
}

// ── grep fallback: max_dependencies truncation ──────────────────────

#[tokio::test]
async fn test_grep_fallback_max_dependencies_truncation() {
    // When grep fallback resolves more candidates than max_dependencies,
    // dependencies_truncated = true and only max_dependencies are kept.
    let surgeon = Arc::new(MockSurgeon::new());
    // Content with multiple function calls so extract_call_candidates finds them.
    // Need at least 4 distinct identifiers that get past keyword filtering.
    let body = "fn login() { alpha(); beta(); gamma(); delta(); epsilon() }";
    let mut scope = make_scope();
    scope.content = body.to_string();
    // First scope: read_symbol_scope_enriched; second: read_symbol_scope in grep fallback
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .extend([Ok(scope.clone()), Ok(scope)]);

    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(ws_dir.path().join("src/auth.rs"), body).unwrap();

    let scout = Arc::new(MockScout::default());
    // Queue 5 search results — one per candidate call extracted from the scope content.
    let search_results: Vec<Result<pathfinder_search::SearchResult, String>> = (0..5)
        .map(|i| {
            Ok(pathfinder_search::SearchResult {
                matches: vec![pathfinder_search::SearchMatch {
                    file: format!("src/dep_{i}.rs"),
                    line: 1,
                    column: 1,
                    content: format!("fn dep_{i}() {{}}"),
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
            })
        })
        .collect();
    scout.set_results(search_results);

    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        scout,
        surgeon,
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = InspectParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        max_dependencies: 2, // cap below available candidates
        ..Default::default()
    };
    let result = server.read_with_deep_context_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::ReadWithDeepContextMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(val.degraded);
    assert!(
        val.dependencies.len() <= 2,
        "dependencies must be capped at max_dependencies=2, got {}",
        val.dependencies.len()
    );
    assert!(
        val.dependencies_truncated,
        "dependencies_truncated must be true when budget exhausted"
    );
}

// ── Err(other LSP error) triggers grep fallback ──────────────────────

#[tokio::test]
async fn test_read_with_deep_context_lsp_protocol_error_triggers_grep_fallback() {
    // Err(LspError::Protocol(...)) on call_hierarchy_prepare should trigger
    // the generic Err(e) branch which calls attempt_grep_fallback.
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().extend([
        Ok(make_scope()),
        Ok(make_scope()),
        Ok(make_scope()),
    ]);

    let lawyer = Arc::new(MockLawyer::default());
    lawyer.push_prepare_call_hierarchy_result(Err(pathfinder_lsp::LspError::Protocol(
        "server crashed".to_string(),
    )));

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

    let params = InspectParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.read_with_deep_context_impl(params).await;
    let call_res = result.expect("should succeed with grep fallback");
    let val: crate::server::types::ReadWithDeepContextMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(val.degraded);
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::GrepFallbackDependencies)
    );
}

// ── ENRICH-2: inspect dependency semantic paths are hierarchically qualified ──

/// When LSP returns an outgoing callee whose file+line maps to a method inside a
/// struct/class, the dependency `semantic_path` should use the treesitter-qualified
/// chain: `src/token.rs::TokenValidator.validate_token` instead of
/// `src/token.rs::validate_token`.
#[tokio::test]
async fn test_inspect_dependency_semantic_path_is_hierarchical_when_surgeon_qualifies() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    // Surgeon returns a qualified chain for the callee's file+line
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(Some(pathfinder_treesitter::surgeon::ExtractedSymbol {
            name: "validate_token".to_owned(),
            semantic_path: "TokenValidator.validate_token".to_owned(),
            start_line: 14, // 0-indexed (LSP line 15 → 14)
            end_line: 20,
            name_column: 4,
            kind: pathfinder_treesitter::surgeon::SymbolKind::Function,
            byte_range: 0..0,
            access_level: pathfinder_treesitter::surgeon::AccessLevel::Public,
            children: vec![],
        })));

    let lawyer = Arc::new(MockLawyer::default());
    let item = CallHierarchyItem {
        name: "login".into(),
        kind: "function".into(),
        detail: None,
        file: "src/auth.rs".into(),
        line: 9,
        column: 4,
        data: None,
    };
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item.clone()]));

    lawyer.push_outgoing_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "validate_token".into(), // flat LSP name
            kind: "function".into(),
            detail: Some("fn validate_token() -> bool".into()),
            file: "src/token.rs".into(),
            line: 15,
            column: 4,
            data: None,
        },
        call_sites: vec![9],
    }]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = InspectParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.read_with_deep_context_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::ReadWithDeepContextMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(!val.degraded);
    assert_eq!(val.dependencies.len(), 1);
    assert_eq!(
        val.dependencies[0].semantic_path, "src/token.rs::TokenValidator.validate_token",
        "dependency path must use qualified treesitter chain, not flat LSP name"
    );
    assert_eq!(val.dependencies[0].signature, "fn validate_token() -> bool");
    assert_eq!(val.dependencies[0].file, "src/token.rs");
    assert_eq!(val.dependencies[0].line, 15);
}

/// When Surgeon returns Ok(None) for a callee's file+line (top-level function),
/// the dependency `semantic_path` must fall back to the flat LSP name cleanly.
#[tokio::test]
async fn test_inspect_dependency_semantic_path_falls_back_to_flat_when_surgeon_returns_none() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    // Surgeon returns None → flat fallback
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));

    let lawyer = Arc::new(MockLawyer::default());
    let item = CallHierarchyItem {
        name: "login".into(),
        kind: "function".into(),
        detail: None,
        file: "src/auth.rs".into(),
        line: 9,
        column: 4,
        data: None,
    };
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item.clone()]));

    lawyer.push_outgoing_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "validate_token".into(),
            kind: "function".into(),
            detail: Some("fn validate_token() -> bool".into()),
            file: "src/token.rs".into(),
            line: 15,
            column: 4,
            data: None,
        },
        call_sites: vec![9],
    }]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = InspectParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.read_with_deep_context_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::ReadWithDeepContextMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(!val.degraded);
    assert_eq!(val.dependencies.len(), 1);
    assert_eq!(
        val.dependencies[0].semantic_path, "src/token.rs::validate_token",
        "flat fallback must be used when surgeon returns None"
    );
}

/// When Surgeon returns an error for a callee's file+line, inspect must NOT
/// panic or propagate — it must fall back to the flat LSP name silently.
#[tokio::test]
async fn test_inspect_dependency_semantic_path_falls_back_on_surgeon_error() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    // Surgeon returns an error
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Err(pathfinder_treesitter::SurgeonError::ParseError {
            path: std::path::PathBuf::from("src/token.rs"),
            reason: "parse failed".to_owned(),
        }));

    let lawyer = Arc::new(MockLawyer::default());
    let item = CallHierarchyItem {
        name: "login".into(),
        kind: "function".into(),
        detail: None,
        file: "src/auth.rs".into(),
        line: 9,
        column: 4,
        data: None,
    };
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item.clone()]));

    lawyer.push_outgoing_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "validate_token".into(),
            kind: "function".into(),
            detail: Some("fn validate_token() -> bool".into()),
            file: "src/token.rs".into(),
            line: 15,
            column: 4,
            data: None,
        },
        call_sites: vec![9],
    }]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = InspectParams {
        semantic_path: Some("src/auth.rs::login".to_owned()),
        ..Default::default()
    };
    let result = server.read_with_deep_context_impl(params).await;
    let call_res = result.expect("surgeon error must not propagate to caller");
    let val: crate::server::types::ReadWithDeepContextMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(
        !val.degraded,
        "surgeon error in enrichment must not degrade inspect"
    );
    assert_eq!(val.dependencies.len(), 1);
    assert_eq!(
        val.dependencies[0].semantic_path, "src/token.rs::validate_token",
        "surgeon error must cause flat-name fallback, not panic"
    );
}
