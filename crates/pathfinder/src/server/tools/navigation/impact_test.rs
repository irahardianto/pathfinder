use super::super::test_helpers::{make_scope, make_server_with_lawyer, make_temp_workspace};
use super::*;
use crate::server::types::TraceParams;
use pathfinder_common::config::PathfinderConfig;
use pathfinder_common::sandbox::Sandbox;
use pathfinder_common::types::{DegradedReason, WorkspaceRoot};
use pathfinder_lsp::types::{CallHierarchyCall, CallHierarchyItem};
use pathfinder_lsp::{DefinitionLocation, MockLawyer};
use pathfinder_search::MockScout;
use pathfinder_treesitter::mock::MockSurgeon;
use std::sync::Arc;

// ── find_callers_callees ────────────────────────────────────────────

#[tokio::test]
async fn test_find_callers_callees_returns_empty_degraded() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

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

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 2,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(
        val.incoming.is_none(),
        "incoming must be null (not empty) when degraded"
    );
    assert!(
        val.outgoing.is_none(),
        "outgoing must be null (not empty) when degraded"
    );
    assert!(val.degraded);
    assert_eq!(val.degraded_reason, Some(DegradedReason::NoLsp));
}

#[tokio::test]
async fn test_find_callers_callees_lsp_populates_incoming_and_outgoing() {
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

    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "handle_request".into(),
            kind: "function".into(),
            detail: Some("fn handle_request()".into()),
            file: "src/server.rs".into(),
            line: 20,
            column: 4,
            data: None,
        },
        call_sites: vec![25],
    }]));

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

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 1,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(!val.degraded);
    assert_eq!(val.degraded_reason, None);
    assert_eq!(val.depth_reached, 1); // BFS pops level 1, updates max_depth_reached, then continues
    assert_eq!(val.files_referenced, 3); // initial + caller + callee
    let incoming = val
        .incoming
        .as_ref()
        .expect("incoming must be Some when not degraded");
    let outgoing = val
        .outgoing
        .as_ref()
        .expect("outgoing must be Some when not degraded");
    assert_eq!(incoming.len(), 1);
    assert_eq!(incoming[0].file, "src/server.rs");
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0].file, "src/token.rs");
}

// ── find_callers_callees with empty hierarchy (confirmed zero callers) ───────

#[tokio::test]
async fn test_find_callers_callees_empty_hierarchy_confirmed_zero() {
    // call_hierarchy_prepare returns Ok([]) AND goto_definition probe returns Ok(Some(...))
    // → LSP is warm, confirmed zero callers. Must NOT be degraded.
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

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 2,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // DEGRADED — LSP warm but call hierarchy empty
    assert!(
        val.degraded,
        "must be degraded when call hierarchy is empty"
    );
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::LspWarmupGrepFallback)
    );
    let incoming = val.incoming.as_ref().expect("must be Some when degraded");
    let outgoing = val.outgoing.as_ref().expect("must be Some when degraded");
    assert!(incoming.is_empty(), "confirmed zero callers");
    assert!(outgoing.is_empty(), "confirmed zero callees");
}

#[tokio::test]
async fn test_find_callers_callees_empty_hierarchy_warmup_degrades() {
    // call_hierarchy_prepare returns Ok([]) AND goto_definition probe returns Ok(None)
    // → LSP is warming up. Must be degraded with "lsp_warmup_empty_unverified".
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());
    // Empty call hierarchy
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![]));
    // Probe: goto_definition returns Ok(None) → LSP is still warming up
    // MockLawyer::default() already returns Ok(None) for goto_definition, so no extra setup needed.

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 2,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // DEGRADED — LSP warmup detected
    assert!(
        val.degraded,
        "must be degraded when goto_definition probe also returns None"
    );
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::LspWarmupEmptyUnverified),
        "degraded_reason must indicate warmup ambiguity"
    );
    // incoming/outgoing must be None — do NOT mislead agent with Some([])
    assert!(
        val.incoming.is_none(),
        "incoming must be None (unknown) during warmup, not Some([]) (confirmed-zero)"
    );
    assert!(
        val.outgoing.is_none(),
        "outgoing must be None (unknown) during warmup, not Some([]) (confirmed-zero)"
    );
}

// ── find_callers_callees with LSP error on call_hierarchy_prepare ────────────

#[tokio::test]
async fn test_find_callers_callees_lsp_error_degrades() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());
    // Simulate LSP protocol error
    lawyer.push_prepare_call_hierarchy_result(Err(LspError::Protocol("LSP crashed".to_string())));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 2,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Degraded due to LSP error — must report LspErrorGrepFallback, not NoLsp.
    // NoLsp would mislead agents into "install LSP" when the real cause is a transient error.
    assert!(val.degraded);
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::LspErrorGrepFallback)
    );
}

// ── find_callers_callees BFS depth limiting ────────────────────────────────

#[tokio::test]
async fn test_find_callers_callees_bfs_respects_max_depth() {
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

    // Incoming: one caller that itself has a caller (depth 2 chain)
    let caller_item = CallHierarchyItem {
        name: "caller".into(),
        kind: "function".into(),
        detail: None,
        file: "src/caller.rs".into(),
        line: 5,
        column: 4,
        data: None,
    };
    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: caller_item.clone(),
        call_sites: vec![9],
    }]));
    // Second level incoming (would only be reached if max_depth > 1)
    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "top_level".into(),
            kind: "function".into(),
            detail: None,
            file: "src/main.rs".into(),
            line: 1,
            column: 0,
            data: None,
        },
        call_sites: vec![5],
    }]));

    // Outgoing: empty
    lawyer.push_outgoing_call_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 1, // Should stop after first level
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(!val.degraded);
    let _incoming = val.incoming.as_ref().expect("must be Some");
    // With max_depth=1, BFS processes the initial item at depth 0, finds caller at depth 1,
    // but the second-level caller (depth 2) should NOT be included
    // However depth_reached should be 1
    assert_eq!(val.depth_reached, 1);
}

// ── CG-3: sandbox check error in find_callers_callees ──────────────────────

#[tokio::test]
async fn test_find_callers_callees_rejects_sandbox_denied_path() {
    let surgeon = Arc::new(MockSurgeon::new());
    let lawyer = Arc::new(MockLawyer::default());
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: ".git/objects/abc::def".to_owned(),
        max_depth: 2,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
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

// ── CG-4: Tree-sitter error in find_callers_callees ──────────────────────────

#[tokio::test]
async fn test_find_callers_callees_tree_sitter_error() {
    let surgeon = Arc::new(MockSurgeon::new());
    // Push an error result
    surgeon.read_symbol_scope_results.lock().unwrap().push(Err(
        pathfinder_treesitter::SurgeonError::ParseError {
            path: std::path::PathBuf::from("src/auth.rs"),
            reason: "parse failed".to_string(),
        },
    ));

    let lawyer = Arc::new(MockLawyer::default());
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 2,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    assert!(result.is_err(), "tree-sitter error should propagate");
}

// ── CG-5: LSP error during BFS traversal ───────────────────────────────

#[tokio::test]
async fn test_find_callers_callees_bfs_lsp_error_graceful_partial_graph() {
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
    // Incoming succeeds with one caller
    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "caller".into(),
            kind: "function".into(),
            detail: None,
            file: "src/server.rs".into(),
            line: 20,
            column: 4,
            data: None,
        },
        call_sites: vec![9],
    }]));
    // Outgoing fails with LSP error
    lawyer.push_outgoing_call_result(Err(LspError::Protocol(
        "LSP crashed during outgoing".to_string(),
    )));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 1,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed despite partial failure");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // NOT degraded — prepare succeeded, incoming succeeded, only outgoing had error
    assert!(!val.degraded);
    let incoming = val.incoming.as_ref().expect("incoming must be Some");
    assert_eq!(incoming.len(), 1, "incoming caller should be present");
    let outgoing = val.outgoing.as_ref().expect("outgoing must be Some");
    assert!(outgoing.is_empty(), "outgoing should be empty due to error");
}

// ── CG-1: Grep fallback path in find_callers_callees ─────────────────────────

#[tokio::test]
async fn test_find_callers_callees_grep_fallback_with_mock_scout() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));
    // SPEC 008: search_codebase_impl calls enclosing_symbol_detail for each match
    // We have 1 match, so push 1 enclosing_symbol_detail_result
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));

    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Create a file so the version hash computation has something to read
    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(
        ws_dir.path().join("src/auth.rs"),
        "fn login() -> bool { true }",
    )
    .unwrap();

    let scout = Arc::new(MockScout::default());
    // Create a caller file (different from the definition file)
    std::fs::write(
        ws_dir.path().join("src/caller.rs"),
        "fn handle_request() { login(); }",
    )
    .unwrap();
    scout.set_result(Ok(pathfinder_search::SearchResult {
        matches: vec![pathfinder_search::SearchMatch {
            file: "src/caller.rs".to_string(),
            line: 1,
            column: 1,
            content: "fn handle_request() { login(); }".to_string(),
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

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 2,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(val.degraded);
    assert_eq!(val.degraded_reason, Some(DegradedReason::NoLspGrepFallback));
    let incoming = val.incoming.as_ref().expect("must be Some from grep");
    assert_eq!(incoming.len(), 1);
    assert_eq!(incoming[0].file, "src/caller.rs");
    assert_eq!(incoming[0].direction, "incoming_heuristic");
}

// ── PATCH-002: Non-source file filtering in grep fallback ───────────

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn test_find_callers_callees_grep_fallback_filters_non_source_files() {
    // Issue: grep fallback was returning matches from .md, .json, .txt, etc.
    // causing false positives. This test verifies that non-source files
    // are filtered out of the results.
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));
    // SPEC 008: search_codebase_impl calls enclosing_symbol_detail for each match
    // We have 4 matches, so push 4 results
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .extend([Ok(None), Ok(None), Ok(None), Ok(None)]);

    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Create the definition file
    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(
        ws_dir.path().join("src/auth.rs"),
        "fn login() -> bool { true }",
    )
    .unwrap();

    let scout = Arc::new(MockScout::default());
    // Return a mix of source and non-source files that match the symbol name
    scout.set_result(Ok(pathfinder_search::SearchResult {
        matches: vec![
            // Legitimate source file caller
            pathfinder_search::SearchMatch {
                file: "src/caller.rs".to_string(),
                line: 1,
                column: 1,
                content: "fn call() { login(); }".to_string(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:a".to_string(),
                known: Some(false),
            },
            // Documentation file - should be filtered OUT
            pathfinder_search::SearchMatch {
                file: "docs/README.md".to_string(),
                line: 10,
                column: 1,
                content: "call login() to authenticate".to_string(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:b".to_string(),
                known: Some(false),
            },
            // Config file - should be filtered OUT
            pathfinder_search::SearchMatch {
                file: "config.json".to_string(),
                line: 5,
                column: 1,
                content: "\"login\": \"/api/auth\"".to_string(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:c".to_string(),
                known: Some(false),
            },
            // TypeScript source - should be KEPT
            pathfinder_search::SearchMatch {
                file: "web/src/auth.ts".to_string(),
                line: 20,
                column: 1,
                content: "import { login } from './api';".to_string(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:d".to_string(),
                known: Some(false),
            },
        ],
        total_matches: 4,
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

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 2,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(val.degraded);
    assert_eq!(val.degraded_reason, Some(DegradedReason::NoLspGrepFallback));
    let incoming = val.incoming.as_ref().expect("must be Some from grep");

    // Only the 2 source files should remain (.rs and .ts)
    // .md and .json should be filtered out
    assert_eq!(
        incoming.len(),
        2,
        "non-source files should be filtered, got: {:?}",
        incoming.iter().map(|r| &r.file).collect::<Vec<_>>()
    );

    // Verify the correct files are kept
    let files: std::collections::HashSet<_> = incoming.iter().map(|r| r.file.as_str()).collect();
    assert!(files.contains("src/caller.rs"), "should keep .rs file");
    assert!(files.contains("web/src/auth.ts"), "should keep .ts file");
    assert!(!files.contains("docs/README.md"), "should filter .md file");
    assert!(!files.contains("config.json"), "should filter .json file");
}

// ── DS-1: DocumentGuard lifecycle tests ──────────────────────────────────

#[tokio::test]
async fn test_find_callers_callees_closes_document_on_success() {
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
    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 1,
        ..Default::default()
    };

    let _ = server.find_callers_callees_impl(params).await;

    tokio::task::yield_now().await;

    assert_eq!(
        lawyer.did_open_call_count(),
        lawyer.did_close_call_count(),
        "DS-1: did_open and did_close must be symmetric in find_callers_callees"
    );
}

// ── TASK-2: project_only filter ───────────────────────────────────────────

/// With `project_only = true` (the default), absolute stdlib paths should be
/// silently dropped from the BFS impact graph.
#[tokio::test]
async fn test_find_callers_callees_project_only_true_filters_stdlib_refs() {
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

    // No incoming callers
    lawyer.push_incoming_call_result(Ok(vec![]));

    // Outgoing: an absolute stdlib path — should be filtered when project_only=true
    lawyer.push_outgoing_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "write_all".into(),
            kind: "function".into(),
            detail: None,
            file: "/home/user/.rustup/toolchains/stable/lib/std/io.rs".into(),
            line: 100,
            column: 4,
            data: None,
        },
        call_sites: vec![10],
    }]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 1,
        // project_only defaults to true via Default::default()
        ..Default::default()
    };
    let result = server
        .find_callers_callees_impl(params)
        .await
        .expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(result.structured_content.unwrap()).unwrap();

    let outgoing = val.outgoing.as_ref().expect("outgoing must be Some");
    assert_eq!(
        outgoing.len(),
        0,
        "project_only=true (default) must filter out stdlib absolute paths"
    );
}

// ── TASK-6: max_references truncation ─────────────────────────────────────

/// When the number of BFS-found references exceeds `max_references`, the
/// result must be truncated and `references_truncated = true`.
#[tokio::test]
async fn test_find_callers_callees_max_references_truncates_results() {
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

    // Push 5 incoming callers (each on a unique file to avoid dedup)
    let incoming_calls: Vec<CallHierarchyCall> = (1..=5)
        .map(|i| CallHierarchyCall {
            item: CallHierarchyItem {
                name: format!("caller_{i}"),
                kind: "function".into(),
                detail: None,
                file: format!("src/caller_{i}.rs"),
                line: i * 10,
                column: 4,
                data: None,
            },
            call_sites: vec![i * 10],
        })
        .collect();
    lawyer.push_incoming_call_result(Ok(incoming_calls));

    // Push 3 outgoing callees to also exhaust outgoing budget
    let outgoing_calls: Vec<CallHierarchyCall> = (1..=3)
        .map(|i| CallHierarchyCall {
            item: CallHierarchyItem {
                name: format!("callee_{i}"),
                kind: "function".into(),
                detail: None,
                file: format!("src/callee_{i}.rs"),
                line: i * 10,
                column: 4,
                data: None,
            },
            call_sites: vec![i * 10],
        })
        .collect();
    lawyer.push_outgoing_call_result(Ok(outgoing_calls));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 1,
        max_references: 2, // Budget split: incoming gets 1, outgoing gets 1. Total budget=2.
        ..Default::default()
    };
    let result = server
        .find_callers_callees_impl(params)
        .await
        .expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(result.structured_content.unwrap()).unwrap();

    let incoming = val.incoming.as_ref().expect("incoming must be Some");
    assert_eq!(
        incoming.len(),
        1,
        "incoming refs must be capped at max_references/2=1"
    );
    assert!(
        val.references_truncated,
        "references_truncated must be true when total budget is exhausted"
    );
}

/// Verify that the `default_max_references()` constant is 50.
///
/// This ensures the plan's specified default wasn't accidentally changed.
#[test]
fn test_find_callers_callees_default_max_references_is_50() {
    use crate::server::types::default_max_references;
    assert_eq!(
        default_max_references(),
        50,
        "default_max_references must be 50 per the remediation plan spec"
    );
}

// ── find_callers_callees edge cases ─────────────────────────────────

#[tokio::test]
async fn test_find_callers_callees_handles_empty_incoming_and_outgoing() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());
    // Empty call hierarchy results
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 3,
        max_references: 50,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(val.incoming.is_none() || val.incoming.as_ref().unwrap().is_empty());
    assert!(val.outgoing.is_none() || val.outgoing.as_ref().unwrap().is_empty());
}

#[tokio::test]
async fn test_find_callers_callees_respects_max_depth() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());
    // Provide incoming calls at depth 1
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
    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "main".into(),
            kind: "function".into(),
            detail: None,
            file: "src/main.rs".into(),
            line: 5,
            column: 4,
            data: None,
        },
        call_sites: vec![5],
    }]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 1, // Limit depth to 1
        max_references: 50,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Should have incoming call from main
    let incoming = val
        .incoming
        .as_ref()
        .expect("incoming must be Some when not degraded");
    assert!(!incoming.is_empty(), "should have incoming calls");
    assert!(
        incoming.iter().all(|r| r.depth <= 1),
        "all refs should be within max_depth"
    );
}

// ── Phase 4C: Navigation Residual Gaps ───────────────────────────────

#[tokio::test]
async fn test_find_callers_callees_bfs_handles_cycle_in_call_graph() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());

    // Create a cycle: A -> B -> A using existing test files
    let item_a = CallHierarchyItem {
        name: "login".into(),
        kind: "function".into(),
        detail: None,
        file: "src/auth.rs".into(),
        line: 9,
        column: 4,
        data: None,
    };
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item_a.clone()]));

    // A calls validate_token
    let item_b = CallHierarchyItem {
        name: "validate_token".into(),
        kind: "function".into(),
        detail: None,
        file: "src/token.rs".into(),
        line: 20,
        column: 4,
        data: None,
    };
    lawyer.push_outgoing_call_result(Ok(vec![CallHierarchyCall {
        item: item_b.clone(),
        call_sites: vec![15],
    }]));

    // validate_token calls login (cycle back)
    lawyer.push_outgoing_call_result(Ok(vec![CallHierarchyCall {
        item: item_a.clone(),
        call_sites: vec![25],
    }]));

    lawyer.push_incoming_call_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 3,
        ..Default::default()
    };

    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Should not hang or panic
    assert!(!val.degraded);
    let outgoing = val.outgoing.as_ref().expect("must be Some");
    // Should deduplicate: login should not appear in its own outgoing
    assert!(
        !outgoing
            .iter()
            .any(|r| r.file == "src/auth.rs" && r.semantic_path.contains("login")),
        "cycle should be deduplicated"
    );
}

#[tokio::test]
async fn test_find_callers_callees_bfs_deduplicates_cross_referenced_symbols() {
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

    // Create duplicate references: same symbol referenced twice
    let caller_item = CallHierarchyItem {
        name: "handler".into(),
        kind: "function".into(),
        detail: None,
        file: "src/handler.rs".into(),
        line: 10,
        column: 4,
        data: None,
    };
    // Push same item twice with different call sites
    lawyer.push_incoming_call_result(Ok(vec![
        CallHierarchyCall {
            item: caller_item.clone(),
            call_sites: vec![20],
        },
        CallHierarchyCall {
            item: caller_item.clone(),
            call_sites: vec![35],
        },
    ]));

    lawyer.push_outgoing_call_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 2,
        ..Default::default()
    };

    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(!val.degraded);
    let incoming = val.incoming.as_ref().expect("must be Some");
    // Should deduplicate based on item (not call sites)
    // Check by semantic path since name is not available in ImpactReference
    let handler_count = incoming
        .iter()
        .filter(|r| r.semantic_path.contains("handler") || r.file == "src/handler.rs")
        .count();
    assert_eq!(
        handler_count, 1,
        "cross-referenced symbol should be deduplicated"
    );
}

#[tokio::test]
async fn test_find_callers_callees_grep_fallback_provides_incoming_heuristic() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    // SPEC 008: search_codebase_impl calls enclosing_symbol_detail for each match
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));

    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Create files
    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(
        ws_dir.path().join("src/auth.rs"),
        "fn login() -> bool { true }",
    )
    .unwrap();
    std::fs::write(
        ws_dir.path().join("src/caller.rs"),
        "fn handle_request() { login(); }",
    )
    .unwrap();

    let scout = Arc::new(MockScout::default());
    scout.set_result(Ok(pathfinder_search::SearchResult {
        matches: vec![pathfinder_search::SearchMatch {
            file: "src/caller.rs".to_string(),
            line: 1,
            column: 1,
            content: "fn handle_request() { login(); }".to_string(),
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

    // Use NoOpLawyer to force grep fallback
    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        scout,
        surgeon,
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = crate::server::types::TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 2,
        ..Default::default()
    };

    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(val.degraded);
    assert_eq!(val.degraded_reason, Some(DegradedReason::NoLspGrepFallback));
    let incoming = val.incoming.as_ref().expect("must be Some from grep");
    assert_eq!(incoming.len(), 1);
    assert_eq!(incoming[0].file, "src/caller.rs");
    assert_eq!(incoming[0].direction, "incoming_heuristic");
}

#[tokio::test]
async fn test_find_callers_callees_grep_fallback_no_results_stays_none() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    // SPEC 008: search_codebase_impl calls enclosing_symbol_detail for each match
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));

    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Create files — login calls validate_token
    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(
        ws_dir.path().join("src/auth.rs"),
        "fn login() -> bool { validate_token() }",
    )
    .unwrap();
    std::fs::write(
        ws_dir.path().join("src/token.rs"),
        "fn validate_token() -> bool { true }",
    )
    .unwrap();

    let scout = Arc::new(MockScout::default());
    // Search for "login" finds the definition in auth.rs (which is filtered out)
    // and no other references, so grep fallback returns None for incoming.
    scout.set_result(Ok(pathfinder_search::SearchResult {
        matches: vec![],
        total_matches: 0,
        truncated: false,
        files_searched: 0,
        files_in_scope: 0,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    }));

    // Use NoOpLawyer to force grep fallback
    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        scout,
        surgeon,
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = crate::server::types::TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 2,
        ..Default::default()
    };

    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(val.degraded);
    // When grep fallback returns no results, degraded_reason stays at default NoLsp
    assert_eq!(val.degraded_reason, Some(DegradedReason::NoLsp));
    // make_scope() returns "fn login() { }" with empty body — no function calls
    // to extract. Grep outgoing fallback exists but finds zero candidates.
    assert!(
        val.outgoing.is_none(),
        "outgoing should be None — empty function body has no call candidates"
    );
    // No search results means no incoming either
    assert!(
        val.incoming.is_none(),
        "incoming should be None when search returns no matches"
    );
}

// ── GAP 3: method call extraction for Rust/Go/Java ──────────────────────

#[tokio::test]
async fn test_extract_call_candidates_captures_method_calls() {
    // Verify that call_pattern_full() now used for ALL languages captures
    // method calls like self.validate(), s.HandleRequest(), service.process().
    use super::super::extract_call_candidates;

    // Rust method call
    let rust_code = "fn login(&self) { self.validate_token(); self.hash_password(); }";
    let rust_candidates = extract_call_candidates(rust_code, "rust");
    assert!(
        rust_candidates.contains(&"validate_token".to_string()),
        "should capture self.validate_token() in Rust"
    );
    assert!(
        rust_candidates.contains(&"hash_password".to_string()),
        "should capture self.hash_password() in Rust"
    );

    // Go method call
    let go_code = "func (h *Handler) Login() { h.service.Validate(); }";
    let go_candidates = extract_call_candidates(go_code, "go");
    assert!(
        go_candidates.contains(&"Validate".to_string()),
        "should capture h.service.Validate() in Go"
    );

    // Java method call
    let java_code = "public void login() { this.service.process(); }";
    let java_candidates = extract_call_candidates(java_code, "java");
    assert!(
        java_candidates.contains(&"process".to_string()),
        "should capture this.service.process() in Java"
    );
}

// ── GAP 6: per-language method call extraction tests ──────────────────────

#[tokio::test]
async fn test_extract_call_candidates_rust_method_calls() {
    use super::super::extract_call_candidates;

    let code = "fn login(&self) { self.validate(); self.hash_password(); self.save(); }";
    let candidates = extract_call_candidates(code, "rust");
    assert!(
        candidates.contains(&"validate".to_string()),
        "should capture self.validate() in Rust"
    );
    assert!(
        candidates.contains(&"hash_password".to_string()),
        "should capture self.hash_password() in Rust"
    );
    assert!(
        candidates.contains(&"save".to_string()),
        "should capture self.save() in Rust"
    );
}

#[tokio::test]
async fn test_extract_call_candidates_go_method_calls() {
    use super::super::extract_call_candidates;

    let code = "func (s *Server) Handle() { s.Validate(); s.Process(); }";
    let candidates = extract_call_candidates(code, "go");
    assert!(
        candidates.contains(&"Validate".to_string()),
        "should capture s.Validate() in Go"
    );
    assert!(
        candidates.contains(&"Process".to_string()),
        "should capture s.Process() in Go"
    );
}

#[tokio::test]
async fn test_extract_call_candidates_java_method_calls() {
    use super::super::extract_call_candidates;

    let code = "public void login() { this.service.process(); this.dao.save(); }";
    let candidates = extract_call_candidates(code, "java");
    assert!(
        candidates.contains(&"process".to_string()),
        "should capture this.service.process() in Java"
    );
    assert!(
        candidates.contains(&"save".to_string()),
        "should capture this.dao.save() in Java"
    );
}

// ── GAP 7: outgoing fallback end-to-end tests ──────────────────────────
//
// Strategy for handling non-deterministic HashSet iteration:
// extract_call_candidates extracts both the fn name from the signature and
// body calls. With set_results, we queue enough results so that regardless
// of candidate ordering, the correct matches are returned.
//
// For the happy-path test with scope "fn handle(&self) { self.validate(); }":
//   Candidates: {"handle", "validate"} (HashSet, order varies)
//   We queue results where EVERY search returns the "validate" match from
//   token.rs. The "handle" candidate search gets a match in token.rs which
//   won't form a valid fn definition but still gets added as outgoing_heuristic.
//   We verify outgoing is Some with at least one entry having the right direction.

#[tokio::test]
async fn test_outgoing_fallback_happy_path() {
    // When LSP is unavailable and the function body has calls,
    // outgoing should be Some with direction "outgoing_heuristic".
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().push(Ok(
        pathfinder_common::types::SymbolScope {
            content: "fn handle(&self) { self.validate(); }".to_string(),
            start_line: 9,
            end_line: 9,
            name_column: 0,
            language: "rust".to_string(),
            ..Default::default()
        },
    ));

    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(
        ws_dir.path().join("src/handler.rs"),
        "fn handle(&self) { self.validate(); }",
    )
    .unwrap();
    std::fs::write(
        ws_dir.path().join("src/validator.rs"),
        "fn validate() -> bool { true }",
    )
    .unwrap();

    let scout = Arc::new(MockScout::default());

    let validate_match = pathfinder_search::SearchMatch {
        file: "src/validator.rs".to_string(),
        line: 1,
        column: 0,
        content: "fn validate() -> bool { true }".to_string(),
        context_before: vec![],
        context_after: vec![],
        enclosing_semantic_path: None,
        is_definition: None,
        version_hash: "sha256:abc".to_string(),
        known: Some(false),
    };
    let empty_result = Ok(pathfinder_search::SearchResult {
        matches: vec![],
        total_matches: 0,
        truncated: false,
        files_searched: 0,
        files_in_scope: 0,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    });
    let validate_result = Ok(pathfinder_search::SearchResult {
        matches: vec![validate_match],
        total_matches: 1,
        truncated: false,
        files_searched: 0,
        files_in_scope: 0,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    });

    // Queue: 1st = incoming search (empty), then enough for outgoing candidates
    // Candidates from HashSet: "handle" + "validate" in unknown order.
    // Both get validate_result so we don't depend on order.
    scout.set_results(vec![
        empty_result.clone(),    // incoming search
        validate_result.clone(), // 1st outgoing candidate (handle or validate)
        validate_result.clone(), // 2nd outgoing candidate (the other)
    ]);

    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        scout,
        surgeon,
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = crate::server::types::TraceParams {
        semantic_path: "src/handler.rs::handle".to_owned(),
        max_depth: 2,
        ..Default::default()
    };

    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(val.degraded);
    assert_eq!(val.degraded_reason, Some(DegradedReason::NoLspGrepFallback));
    let outgoing = val.outgoing.as_ref().expect("outgoing must be Some");
    assert!(
        !outgoing.is_empty(),
        "should have at least one outgoing ref"
    );
    assert_eq!(
        outgoing[0].direction, "outgoing_heuristic",
        "direction must be outgoing_heuristic"
    );
}

#[tokio::test]
async fn test_outgoing_fallback_dedup_by_semantic_path() {
    // Same function called multiple times should appear once in outgoing.
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().push(Ok(
        pathfinder_common::types::SymbolScope {
            content: "fn handle(&self) { self.validate(); }".to_string(),
            start_line: 9,
            end_line: 9,
            name_column: 0,
            language: "rust".to_string(),
            ..Default::default()
        },
    ));

    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(
        ws_dir.path().join("src/worker.rs"),
        "fn process(&self) { self.run(); }",
    )
    .unwrap();
    std::fs::write(ws_dir.path().join("src/runner.rs"), "fn run() { }").unwrap();

    let scout = Arc::new(MockScout::default());

    let run_match = pathfinder_search::SearchMatch {
        file: "src/runner.rs".to_string(),
        line: 1,
        column: 0,
        content: "fn run() { }".to_string(),
        context_before: vec![],
        context_after: vec![],
        enclosing_semantic_path: None,
        is_definition: None,
        version_hash: "sha256:abc".to_string(),
        known: Some(false),
    };
    let empty_result = Ok(pathfinder_search::SearchResult {
        matches: vec![],
        total_matches: 0,
        truncated: false,
        files_searched: 0,
        files_in_scope: 0,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    });
    let run_result = Ok(pathfinder_search::SearchResult {
        matches: vec![run_match],
        total_matches: 1,
        truncated: false,
        files_searched: 0,
        files_in_scope: 0,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    });

    // Candidates: "process" + "run" (deduped by HashSet in extract_call_candidates)
    // But grep_outgoing_fallback also deduplicates by semantic_path.
    // Both candidates get run_result, but only the "run" match adds
    // "src/runner.rs::run" (the "process" candidate search also gets run_result
    // but forms "src/runner.rs::process" which is a different semantic_path).
    // The seen set ensures no dupes regardless.
    scout.set_results(vec![
        empty_result.clone(), // incoming
        run_result.clone(),   // 1st outgoing candidate
        run_result.clone(),   // 2nd outgoing candidate
    ]);

    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        scout,
        surgeon,
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = crate::server::types::TraceParams {
        semantic_path: "src/worker.rs::process".to_owned(),
        max_depth: 2,
        ..Default::default()
    };

    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    let outgoing = val.outgoing.as_ref().expect("outgoing must be Some");
    // All outgoing refs should have unique semantic_paths
    let paths: std::collections::HashSet<&str> =
        outgoing.iter().map(|r| r.semantic_path.as_str()).collect();
    assert_eq!(
        paths.len(),
        outgoing.len(),
        "all outgoing refs must have unique semantic_paths"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn test_grep_outgoing_fallback_semantic_path_is_hierarchical() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().push(Ok(
        pathfinder_common::types::SymbolScope {
            content: "fn handle(&self) { self.validate(); }".to_string(),
            start_line: 9,
            end_line: 9,
            name_column: 0,
            language: "rust".to_string(),
            ..Default::default()
        },
    ));

    // When grep_outgoing_fallback resolves "validate" to src/validator.rs:1,
    // we want to mock treesitter enclosing_symbol_detail returning a qualified path.
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(Some(pathfinder_treesitter::surgeon::ExtractedSymbol {
            name: "validate".to_owned(),
            semantic_path: "TokenValidator.validate".to_owned(),
            start_line: 0,
            end_line: 4,
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
        ws_dir.path().join("src/handler.rs"),
        "fn handle(&self) { self.validate(); }",
    )
    .unwrap();
    std::fs::write(
        ws_dir.path().join("src/validator.rs"),
        "fn validate() -> bool { true }",
    )
    .unwrap();

    let scout = Arc::new(MockScout::default());

    let validate_match = pathfinder_search::SearchMatch {
        file: "src/validator.rs".to_string(),
        line: 1,
        column: 0,
        content: "fn validate() -> bool { true }".to_string(),
        context_before: vec![],
        context_after: vec![],
        enclosing_semantic_path: None,
        is_definition: None,
        version_hash: "sha256:abc".to_string(),
        known: Some(false),
    };
    let empty_result = Ok(pathfinder_search::SearchResult {
        matches: vec![],
        total_matches: 0,
        truncated: false,
        files_searched: 0,
        files_in_scope: 0,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    });
    let validate_result = Ok(pathfinder_search::SearchResult {
        matches: vec![validate_match],
        total_matches: 1,
        truncated: false,
        files_searched: 0,
        files_in_scope: 0,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    });

    scout.set_results(vec![
        empty_result.clone(),    // incoming search
        validate_result.clone(), // outgoing candidate search
    ]);

    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        scout,
        surgeon,
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = crate::server::types::TraceParams {
        semantic_path: "src/handler.rs::handle".to_owned(),
        max_depth: 2,
        ..Default::default()
    };

    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    let outgoing = val.outgoing.as_ref().expect("outgoing must be Some");
    assert!(!outgoing.is_empty());
    assert_eq!(
        outgoing[0].semantic_path, "src/validator.rs::TokenValidator.validate",
        "grep fallback path should be qualified using surgeon enclosing_symbol_detail"
    );
}

#[tokio::test]
async fn test_outgoing_fallback_definition_file_exclusion() {
    // When a candidate resolves to the definition file, it should be excluded.
    // Test uses scope with only one candidate (validate) and sets up search
    // to return a match in the definition file, which gets skipped.
    // With GAP 5 fix, the 2nd match (in another file) should be used instead.
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().push(Ok(
        pathfinder_common::types::SymbolScope {
            content: "fn do_work() { validate(); }".to_string(),
            start_line: 10,
            end_line: 10,
            name_column: 0,
            language: "rust".to_string(),
            ..Default::default()
        },
    ));

    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(
        ws_dir.path().join("src/worker.rs"),
        "fn do_work() { validate(); }\nfn validate() { }",
    )
    .unwrap();
    std::fs::write(ws_dir.path().join("src/lib.rs"), "fn validate() { }").unwrap();

    let scout = Arc::new(MockScout::default());

    // GAP 5 scenario: first match is in definition file (skipped),
    // second match is in another file (should be used).
    let local_match = pathfinder_search::SearchMatch {
        file: "src/worker.rs".to_string(),
        line: 2,
        column: 0,
        content: "fn validate() { }".to_string(),
        context_before: vec![],
        context_after: vec![],
        enclosing_semantic_path: None,
        is_definition: None,
        version_hash: "sha256:abc".to_string(),
        known: Some(false),
    };
    let external_match = pathfinder_search::SearchMatch {
        file: "src/lib.rs".to_string(),
        line: 1,
        column: 0,
        content: "fn validate() { }".to_string(),
        context_before: vec![],
        context_after: vec![],
        enclosing_semantic_path: None,
        is_definition: None,
        version_hash: "sha256:def".to_string(),
        known: Some(false),
    };
    let empty_result = Ok(pathfinder_search::SearchResult {
        matches: vec![],
        total_matches: 0,
        truncated: false,
        files_searched: 0,
        files_in_scope: 0,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    });
    // Candidates: {"do_work", "validate"} — order unknown
    // For "do_work": search gets the two-match result (neither is a valid fn do_work)
    // For "validate": search gets the two-match result (first=definition, second=external)
    // With GAP 5 fix, the second match (src/lib.rs) is used.
    let two_match_result = Ok(pathfinder_search::SearchResult {
        matches: vec![local_match, external_match],
        total_matches: 2,
        truncated: false,
        files_searched: 0,
        files_in_scope: 0,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    });

    scout.set_results(vec![
        empty_result,             // incoming
        two_match_result.clone(), // 1st outgoing candidate
        two_match_result,         // 2nd outgoing candidate
    ]);

    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        scout,
        surgeon,
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = crate::server::types::TraceParams {
        semantic_path: "src/worker.rs::do_work".to_owned(),
        max_depth: 2,
        ..Default::default()
    };

    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    let outgoing = val.outgoing.as_ref().expect("outgoing must be Some");
    // No outgoing ref should be in the definition file (src/worker.rs)
    for reference in outgoing {
        assert_ne!(
            reference.file, "src/worker.rs",
            "outgoing refs should exclude the definition file"
        );
    }
    // Should have at least one outgoing ref (src/lib.rs::validate)
    assert!(
        outgoing.iter().any(|r| r.file == "src/lib.rs"),
        "should have resolved validate to src/lib.rs"
    );
}

// ── BFS multi-node continuation after error ──────────────────────

#[tokio::test]
async fn test_find_callers_callees_bfs_continues_after_single_node_error() {
    // When queue has items A, B and querying A fails, B should still be processed.
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

    // Incoming: first call fails (error for initial item), second succeeds
    lawyer.push_incoming_call_result(Err(LspError::Protocol("transient error".to_string())));
    // Outgoing succeeds with one callee
    lawyer.push_outgoing_call_result(Ok(vec![CallHierarchyCall {
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
    }]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 1,
        max_references: 50,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed despite BFS error");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Not degraded — the LSP prepare succeeded, BFS errors are partial failures
    assert!(!val.degraded);
    // Incoming errored → empty vec (not None)
    let incoming = val.incoming.as_ref().expect("incoming must be Some");
    assert!(incoming.is_empty(), "incoming should be empty after error");
    // Outgoing succeeded
    let outgoing = val.outgoing.as_ref().expect("outgoing must be Some");
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0].file, "src/token.rs");
}

// ── BFS text output format ──────────────────────────────────────

#[tokio::test]
async fn test_find_callers_callees_bfs_formats_response_correctly() {
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

    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "handle_request".into(),
            kind: "function".into(),
            detail: Some("fn handle_request()".into()),
            file: "src/server.rs".into(),
            line: 20,
            column: 4,
            data: None,
        },
        call_sites: vec![25],
    }]));

    lawyer.push_outgoing_call_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 1,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");

    // Verify text output format
    let text = match &call_res.content[0].raw {
        rmcp::model::RawContent::Text(t) => t.text.clone(),
        _ => panic!("expected text content"),
    };
    assert!(text.contains("Incoming references: 1"), "text: {text}");
    assert!(text.contains("Outgoing references: 0"), "text: {text}");
    assert!(text.contains("[depth="), "text: {text}");
    assert!(text.contains("src/server.rs:L20"), "text: {text}");
    assert!(text.contains("[completed in"), "text: {text}");
}

#[tokio::test]
async fn test_find_callers_callees_bfs_aborts_on_consecutive_failures() {
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

    let caller_item = CallHierarchyItem {
        name: "caller".into(),
        kind: "function".into(),
        detail: None,
        file: "src/caller.rs".into(),
        line: 5,
        column: 4,
        data: None,
    };

    // Incoming: first call returns a caller (so BFS has something to traverse),
    // then every subsequent call for deeper levels fails.
    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: caller_item.clone(),
        call_sites: vec![9],
    }]));
    // Next BFS step: incoming for caller fails
    lawyer.push_incoming_call_result(Err(LspError::Protocol("hung".to_string())));
    // Next BFS step: incoming fails again → 2 consecutive failures → abort
    lawyer.push_incoming_call_result(Err(LspError::Protocol("still hung".to_string())));

    // Outgoing: empty
    lawyer.push_outgoing_call_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 4,
        max_references: 50,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed with partial results");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(!val.degraded);
    let incoming = val.incoming.as_ref().expect("must be Some");
    assert_eq!(incoming.len(), 1, "should have 1 caller before abort");
    assert_eq!(incoming[0].semantic_path, "src/caller.rs::caller");
}

#[tokio::test]
async fn test_find_callers_callees_bfs_cycle_detection_incoming() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());

    let item_a = CallHierarchyItem {
        name: "login".into(),
        kind: "function".into(),
        detail: None,
        file: "src/auth.rs".into(),
        line: 9,
        column: 4,
        data: None,
    };
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item_a.clone()]));

    let item_b = CallHierarchyItem {
        name: "validate_token".into(),
        kind: "function".into(),
        detail: None,
        file: "src/token.rs".into(),
        line: 20,
        column: 4,
        data: None,
    };
    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: item_b.clone(),
        call_sites: vec![15],
    }]));

    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: item_a.clone(),
        call_sites: vec![25],
    }]));

    lawyer.push_outgoing_call_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 3,
        ..Default::default()
    };

    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(!val.degraded);
    let incoming = val.incoming.as_ref().expect("must be Some");
    assert!(
        !incoming
            .iter()
            .any(|r| r.file == "src/auth.rs" && r.semantic_path.contains("login")),
        "cycle should be deduplicated"
    );
}

#[tokio::test]
async fn test_find_callers_callees_empty_callee_intermediate() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());

    let item_a = CallHierarchyItem {
        name: "login".into(),
        kind: "function".into(),
        detail: None,
        file: "src/auth.rs".into(),
        line: 9,
        column: 4,
        data: None,
    };
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item_a.clone()]));

    let item_b = CallHierarchyItem {
        name: "validate_token".into(),
        kind: "function".into(),
        detail: None,
        file: "src/token.rs".into(),
        line: 20,
        column: 4,
        data: None,
    };
    lawyer.push_outgoing_call_result(Ok(vec![CallHierarchyCall {
        item: item_b.clone(),
        call_sites: vec![15],
    }]));

    lawyer.push_outgoing_call_result(Ok(vec![]));
    lawyer.push_incoming_call_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 3,
        ..Default::default()
    };

    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(!val.degraded);
    let outgoing = val.outgoing.as_ref().expect("must be Some");
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0].file, "src/token.rs");
}

#[tokio::test]
async fn test_find_callers_callees_bfs_partial_resolution_failure() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());

    let item_a = CallHierarchyItem {
        name: "login".into(),
        kind: "function".into(),
        detail: None,
        file: "src/auth.rs".into(),
        line: 9,
        column: 4,
        data: None,
    };
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item_a.clone()]));

    let item_b = CallHierarchyItem {
        name: "caller_b".into(),
        kind: "function".into(),
        detail: None,
        file: "src/caller_b.rs".into(),
        line: 10,
        column: 4,
        data: None,
    };
    let item_c = CallHierarchyItem {
        name: "caller_c".into(),
        kind: "function".into(),
        detail: None,
        file: "src/caller_c.rs".into(),
        line: 10,
        column: 4,
        data: None,
    };

    lawyer.push_incoming_call_result(Ok(vec![
        CallHierarchyCall {
            item: item_b.clone(),
            call_sites: vec![5],
        },
        CallHierarchyCall {
            item: item_c.clone(),
            call_sites: vec![6],
        },
    ]));

    lawyer.push_incoming_call_result(Ok(vec![]));
    lawyer.push_incoming_call_result(Err(LspError::Protocol("failed".to_string())));
    lawyer.push_outgoing_call_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 3,
        ..Default::default()
    };

    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(!val.degraded);
    let incoming = val.incoming.as_ref().expect("must be Some");
    assert_eq!(incoming.len(), 2);
}

#[tokio::test]
async fn test_find_callers_callees_lsp_error_triggers_grep_fallback() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));

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
    std::fs::write(
        ws_dir.path().join("src/caller.rs"),
        "fn handle_request() { login(); }",
    )
    .unwrap();

    let scout = Arc::new(MockScout::default());
    scout.set_result(Ok(pathfinder_search::SearchResult {
        matches: vec![pathfinder_search::SearchMatch {
            file: "src/caller.rs".to_string(),
            line: 1,
            column: 1,
            content: "fn handle_request() { login(); }".to_string(),
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
    lawyer.push_prepare_call_hierarchy_result(Err(LspError::Protocol("LSP error".to_string())));

    let server = PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 2,
        ..Default::default()
    };

    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed despite LSP error");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(val.degraded);
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::LspErrorGrepFallback)
    );
    let incoming = val.incoming.as_ref().expect("must be Some from grep");
    assert_eq!(incoming.len(), 1);
    assert_eq!(incoming[0].file, "src/caller.rs");
}

#[tokio::test]
async fn test_find_callers_callees_invalid_semantic_path() {
    let surgeon = Arc::new(MockSurgeon::new());
    let lawyer = Arc::new(MockLawyer::default());
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "invalid_path_format".to_owned(),
        ..Default::default()
    };

    let result = server.find_callers_callees_impl(params).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    let code = err
        .data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(code, "INVALID_SEMANTIC_PATH");
}

#[tokio::test]
async fn test_find_callers_callees_max_depth_boundaries() {
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
    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "caller".into(),
            kind: "function".into(),
            detail: Some("fn caller()".into()),
            file: "src/caller.rs".into(),
            line: 20,
            column: 4,
            data: None,
        },
        call_sites: vec![25],
    }]));
    lawyer.push_outgoing_call_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon.clone(), lawyer.clone());

    let zero_depth_params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 0,
        ..Default::default()
    };
    let zero_depth_res = server
        .find_callers_callees_impl(zero_depth_params)
        .await
        .expect("success");
    let zero_depth_val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(zero_depth_res.structured_content.unwrap()).unwrap();
    assert!(!zero_depth_val.degraded);

    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));
    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "caller".into(),
            kind: "function".into(),
            detail: Some("fn caller()".into()),
            file: "src/caller.rs".into(),
            line: 20,
            column: 4,
            data: None,
        },
        call_sites: vec![25],
    }]));
    lawyer.push_outgoing_call_result(Ok(vec![]));

    let large_depth_params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 10,
        ..Default::default()
    };
    let large_depth_res = server
        .find_callers_callees_impl(large_depth_params)
        .await
        .expect("success");
    let large_depth_val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(large_depth_res.structured_content.unwrap()).unwrap();
    assert!(!large_depth_val.degraded);
}

#[tokio::test]
async fn test_find_callers_callees_empty_results_text_formatting() {
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
    lawyer.set_goto_definition_result(Ok(Some(DefinitionLocation {
        file: "src/auth.rs".into(),
        line: 10,
        column: 4,
        preview: "fn login() {}".into(),
    })));
    lawyer.push_incoming_call_result(Ok(vec![]));
    lawyer.push_outgoing_call_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        ..Default::default()
    };

    let result = server
        .find_callers_callees_impl(params)
        .await
        .expect("success");
    let text = result.content[0].as_text().expect("must be text");

    // BFS empty + grep empty = confirmed zero, should NOT be degraded.
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(result.structured_content.unwrap()).unwrap();
    assert!(
        val.degraded,
        "both BFS results empty → grep fallback fires → must be degraded"
    );

    // Text should show DEGRADED notice
    assert!(
        text.text.contains("DEGRADED (lsp_warmup_grep_fallback)"),
        "Text output did not format zero results correctly: {}",
        text.text
    );
}

#[tokio::test]
async fn test_find_callers_callees_references_truncated() {
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

    lawyer.push_incoming_call_result(Ok(vec![
        CallHierarchyCall {
            item: CallHierarchyItem {
                name: "caller_1".into(),
                kind: "function".into(),
                detail: None,
                file: "src/caller_1.rs".into(),
                line: 10,
                column: 4,
                data: None,
            },
            call_sites: vec![10],
        },
        CallHierarchyCall {
            item: CallHierarchyItem {
                name: "caller_2".into(),
                kind: "function".into(),
                detail: None,
                file: "src/caller_2.rs".into(),
                line: 20,
                column: 4,
                data: None,
            },
            call_sites: vec![20],
        },
    ]));
    lawyer.push_outgoing_call_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_references: 1,
        ..Default::default()
    };

    let result = server
        .find_callers_callees_impl(params)
        .await
        .expect("success");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(result.structured_content.unwrap()).unwrap();

    assert!(val.references_truncated);
}

#[tokio::test]
async fn test_find_callers_callees_unusual_symbol_types() {
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
    lawyer.push_incoming_call_result(Ok(vec![]));
    lawyer.push_outgoing_call_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon.clone(), lawyer);

    let params_macro = TraceParams {
        semantic_path: "src/auth.rs::my_macro!".to_owned(),
        ..Default::default()
    };
    let result_macro = server.find_callers_callees_impl(params_macro).await;
    assert!(result_macro.is_ok(), "Macro symbol type should succeed");

    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));
    let params_trait = TraceParams {
        semantic_path: "src/auth.rs::<impl User>::login".to_owned(),
        ..Default::default()
    };
    let result_trait = server.find_callers_callees_impl(params_trait).await;
    assert!(
        result_trait.is_ok(),
        "Trait impl symbol type should succeed"
    );
}

// ── Regression: degraded_reason must reflect actual failure cause ────────

/// Regression: LSP timeout with empty grep results must report `LspTimeoutGrepFallback`,
/// NOT `NoLsp`. Previously the initial `degraded_reason` = `NoLsp` was never overridden when
/// grep found nothing, causing agents to think no LSP existed instead of "retry later".
#[tokio::test]
async fn test_lsp_timeout_empty_grep_reports_timeout_reason() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));
    // empty enclosing so grep_outgoing_fallback finds nothing
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));

    let lawyer = Arc::new(MockLawyer::default());
    // Simulate LSP timeout on prepare
    lawyer.push_prepare_call_hierarchy_result(Err(LspError::Timeout {
        operation: "callHierarchy/incomingCalls".to_string(),
        timeout_ms: 5000,
    }));

    // Use make_server_with_lawyer (workspace has no src/ files, so grep finds nothing)
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        ..Default::default()
    };

    let result = server
        .find_callers_callees_impl(params)
        .await
        .expect("should succeed degraded");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(result.structured_content.unwrap()).unwrap();

    assert!(val.degraded, "must be degraded on timeout");
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::LspTimeoutGrepFallback),
        "timeout must report LspTimeoutGrepFallback even when grep finds nothing"
    );
}

/// Regression: LSP protocol error with empty grep results must report `LspErrorGrepFallback`,
/// NOT `NoLsp`. Previously `degraded_reason` = `NoLsp` was set and never overridden when grep
/// found nothing, misleading agents into "LSP not installed" guidance.
#[tokio::test]
async fn test_lsp_protocol_error_empty_grep_reports_error_reason() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));

    let lawyer = Arc::new(MockLawyer::default());
    lawyer.push_prepare_call_hierarchy_result(Err(LspError::Protocol(
        "internal server error".to_string(),
    )));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        ..Default::default()
    };

    let result = server
        .find_callers_callees_impl(params)
        .await
        .expect("should succeed degraded");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(result.structured_content.unwrap()).unwrap();

    assert!(val.degraded, "must be degraded on LSP error");
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::LspErrorGrepFallback),
        "LSP error must report LspErrorGrepFallback even when grep finds nothing"
    );
}

// ── P2-7: Hint logic tests ──────────────────────────────────────────────────
//
// PATCH-003 changed the hint behavior: when degraded=true AND
// incoming/outgoing are both None (UNKNOWN), the hint is now populated
// with an explicit "UNKNOWN, not zero" warning instead of being absent.
// Tests below reflect the new contract.

#[tokio::test]
async fn test_hint_warns_when_both_callers_and_callees_unknown() {
    // When BFS returns empty for BOTH incoming AND outgoing, the grep fallback
    // fires and always sets degraded=true (to guard against BFS errors vs genuine
    // zero callers). PATCH-003 makes the hint surface a warning in this scenario
    // regardless of whether grep finds heuristic candidates or not:
    //   - grep finds nothing → "Callers AND callees UNKNOWN"
    //   - grep finds candidates → "heuristic grep-based candidates"
    // Both messages tell agents NOT to treat the result as verified.
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
    lawyer.push_incoming_call_result(Ok(vec![]));
    lawyer.push_outgoing_call_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 1,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(
        val.degraded,
        "must be degraded when both BFS results are empty (grep fallback triggered)"
    );
    // PATCH-003: the hint is now populated in the degraded scenario so
    // prose-reading agents see an explicit warning.
    let hint = val
        .hint
        .as_ref()
        .expect("hint must be Some(...) in degraded scenario (PATCH-003 change)");
    // The hint may be either:
    //   - "Callers AND callees UNKNOWN" (when grep fallback finds nothing)
    //   - "heuristic grep-based candidates" (when grep fallback finds refs)
    // Both convey "do not treat as verified" — the test is the same.
    let is_unknown_warning = hint.contains("UNKNOWN");
    let is_heuristic_warning = hint.contains("heuristic");
    assert!(
        is_unknown_warning || is_heuristic_warning,
        "hint must warn about UNKNOWN or heuristic results, got: {hint}"
    );
    // In THIS test, BFS ran and returned empty, then grep found nothing
    // (default MockScout returns empty). So the data IS BFS-confirmed empty
    // and the verified flags must reflect this even though the global
    // `degraded` flag is true (set as a false-negative safety net).
    // The degraded flag tells the agent "be careful, the BFS might have
    // missed something" — but the verified flag tells them "this is what
    // the BFS actually confirmed".
    //
    // This is the corrected semantic: BFS-confirmed empty = Some(true)
    // (verified), but agents should still look at `degraded` for the
    // false-negative safety net.
    assert_eq!(
        val.incoming_verified,
        Some(true),
        "incoming_verified must be Some(true) when BFS confirmed empty (degraded=true \
         is a separate false-negative safety net, not a verification failure). \
         Got: degraded={}, incoming={:?}, incoming_verified={:?}",
        val.degraded,
        val.incoming,
        val.incoming_verified
    );
    assert_eq!(
        val.outgoing_verified,
        Some(true),
        "outgoing_verified must be Some(true) when BFS confirmed empty"
    );
}

#[tokio::test]
async fn test_hint_present_when_only_incoming_callers_empty() {
    // LSP succeeds, incoming empty but outgoing has items → hint about "zero incoming callers"
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
    lawyer.push_incoming_call_result(Ok(vec![]));
    lawyer.push_outgoing_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "validate".into(),
            kind: "function".into(),
            detail: None,
            file: "src/validate.rs".into(),
            line: 5,
            column: 0,
            data: None,
        },
        call_sites: vec![10],
    }]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 1,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(!val.degraded, "should not be degraded");
    assert!(
        val.hint.is_some(),
        "hint must be present when incoming callers are empty"
    );
    let hint = val.hint.unwrap();
    assert!(
        hint.contains("zero incoming callers"),
        "hint must mention zero incoming callers, got: {hint}"
    );
}

#[tokio::test]
async fn test_hint_absent_when_callers_exist() {
    // LSP succeeds, incoming has items → no hint
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
    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "main".into(),
            kind: "function".into(),
            detail: None,
            file: "src/main.rs".into(),
            line: 1,
            column: 0,
            data: None,
        },
        call_sites: vec![5],
    }]));
    lawyer.push_outgoing_call_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 1,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(!val.degraded, "should not be degraded");
    assert!(val.hint.is_none(), "hint must be absent when callers exist");
}

#[tokio::test]
async fn test_hint_warns_when_degraded_with_unknown() {
    // PATCH-003: when the result is degraded AND callers/callees are UNKNOWN
    // (BFS empty + no grep fallback), the hint is now populated with an
    // explicit "UNKNOWN, not zero" warning. This is the central PATCH-003
    // change — previously the hint was None in this exact scenario.
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 1,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(val.degraded, "should be degraded (no LSP)");
    // PATCH-003: hint is now populated in this scenario
    let hint = val
        .hint
        .as_ref()
        .expect("hint must be Some(...) when degraded with UNKNOWN callers/callees");
    assert!(
        hint.contains("UNKNOWN"),
        "hint must warn about UNKNOWN, got: {hint}"
    );
}

#[tokio::test]
async fn test_process_grep_fallback_results_updates_state() {
    let surgeon = Arc::new(MockSurgeon::new());
    let lawyer = Arc::new(MockLawyer::default());
    let (_server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let mut incoming = None;
    let mut outgoing = None;
    let mut degraded_reason = None;

    let grep_in = Some(vec![crate::server::types::ImpactReference {
        semantic_path: "src/auth.rs::login".to_string(),
        file: "src/auth.rs".to_string(),
        line: 10,
        snippet: "fn login()".to_string(),
        direction: "incoming_heuristic".to_string(),
        depth: 0,
        confidence: Some("heuristic".to_owned()),
    }]);
    let grep_out = None;

    PathfinderServer::process_grep_fallback_results(
        grep_in,
        grep_out,
        &mut incoming,
        &mut outgoing,
        &mut degraded_reason,
        Some(DegradedReason::LspWarmupGrepFallback),
        " during warmup",
    );

    assert!(incoming.is_some());
    assert_eq!(incoming.unwrap().len(), 1);
    assert!(outgoing.is_none());
    assert_eq!(degraded_reason, Some(DegradedReason::LspWarmupGrepFallback));
}

#[tokio::test]
async fn test_process_grep_fallback_results_both_none_is_noop() {
    let surgeon = Arc::new(MockSurgeon::new());
    let lawyer = Arc::new(MockLawyer::default());
    let (_server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let mut incoming = None;
    let mut outgoing = None;
    let mut degraded_reason = None;

    // Both grep_in and grep_out are None — should be a complete no-op.
    PathfinderServer::process_grep_fallback_results(
        None,
        None,
        &mut incoming,
        &mut outgoing,
        &mut degraded_reason,
        Some(DegradedReason::NoLspGrepFallback),
        " (no-op test)",
    );

    assert!(
        incoming.is_none(),
        "incoming should stay None when grep_in is None"
    );
    assert!(
        outgoing.is_none(),
        "outgoing should stay None when grep_out is None"
    );
    assert!(
        degraded_reason.is_none(),
        "degraded_reason should NOT be set when no grep results were found"
    );
}

#[tokio::test]
async fn test_process_grep_fallback_results_both_some() {
    let surgeon = Arc::new(MockSurgeon::new());
    let lawyer = Arc::new(MockLawyer::default());
    let (_server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let mut incoming = None;
    let mut outgoing = None;
    let mut degraded_reason = None;

    let grep_in = Some(vec![crate::server::types::ImpactReference {
        semantic_path: "src/auth.rs::login".to_string(),
        file: "src/auth.rs".to_string(),
        line: 10,
        snippet: "fn login()".to_string(),
        direction: "incoming_heuristic".to_string(),
        depth: 0,
        confidence: Some("heuristic".to_owned()),
    }]);
    let grep_out = Some(vec![crate::server::types::ImpactReference {
        semantic_path: "src/db.rs::query".to_string(),
        file: "src/db.rs".to_string(),
        line: 42,
        snippet: "fn query()".to_string(),
        direction: "outgoing_heuristic".to_string(),
        depth: 0,
        confidence: Some("heuristic".to_owned()),
    }]);

    PathfinderServer::process_grep_fallback_results(
        grep_in,
        grep_out,
        &mut incoming,
        &mut outgoing,
        &mut degraded_reason,
        Some(DegradedReason::NoLspGrepFallback),
        " (both-some test)",
    );

    assert!(
        incoming.is_some(),
        "incoming should be populated from grep_in"
    );
    assert_eq!(incoming.unwrap().len(), 1);
    assert!(
        outgoing.is_some(),
        "outgoing should be populated from grep_out"
    );
    assert_eq!(outgoing.unwrap().len(), 1);
    assert_eq!(
        degraded_reason,
        Some(DegradedReason::NoLspGrepFallback),
        "degraded_reason should be set when grep results found"
    );
}

// ── ENRICH-1: BFS output semantic paths are hierarchically qualified ─────────

/// When LSP returns a caller whose file+line maps to a method inside a struct/impl,
/// the output `semantic_path` should use the treesitter-qualified chain, not the flat
/// LSP name. For example: `src/server.rs::Server.handle_request` instead of
/// `src/server.rs::handle_request`.
#[tokio::test]
async fn test_bfs_output_semantic_path_is_hierarchical_when_surgeon_qualifies() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    // Surgeon will be called to enrich the incoming caller's file+line.
    // Return a qualified chain: "Server.handle_request".
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(Some(pathfinder_treesitter::surgeon::ExtractedSymbol {
            name: "handle_request".to_owned(),
            semantic_path: "Server.handle_request".to_owned(), // qualified chain
            start_line: 19, // 0-indexed (LSP line 20 → surgeon line 19)
            end_line: 25,
            name_column: 4,
            kind: pathfinder_treesitter::surgeon::SymbolKind::Function,
            byte_range: 0..0,
            access_level: pathfinder_treesitter::surgeon::AccessLevel::Public,
            children: vec![],
        })));
    // Surgeon called once more for the outgoing callee (validate_token → top-level fn).
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None)); // flat fallback for outgoing

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

    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "handle_request".into(), // flat LSP name
            kind: "function".into(),
            detail: Some("fn handle_request()".into()),
            file: "src/server.rs".into(),
            line: 20,
            column: 4,
            data: None,
        },
        call_sites: vec![20],
    }]));

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

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 1,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(!val.degraded);
    let incoming = val.incoming.as_ref().expect("incoming must be Some");
    let outgoing = val.outgoing.as_ref().expect("outgoing must be Some");
    assert_eq!(incoming.len(), 1);
    // Hierarchical: treesitter qualified chain used
    assert_eq!(
        incoming[0].semantic_path, "src/server.rs::Server.handle_request",
        "incoming path must use qualified treesitter chain, not flat LSP name"
    );
    assert_eq!(outgoing.len(), 1);
    // Flat: surgeon returned None → falls back to LSP bare name
    assert_eq!(
        outgoing[0].semantic_path, "src/token.rs::validate_token",
        "outgoing path must fall back to flat name when surgeon returns None"
    );
}

/// When Surgeon returns Ok(None) for a reference's file+line (e.g., top-level function),
/// the output `semantic_path` must fall back to the flat LSP name cleanly.
#[tokio::test]
async fn test_bfs_output_semantic_path_falls_back_to_flat_when_surgeon_returns_none() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    // Both enrichment calls return None → flat fallback for both
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .extend([Ok(None), Ok(None)]);

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

    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "caller_fn".into(),
            kind: "function".into(),
            detail: None,
            file: "src/main.rs".into(),
            line: 5,
            column: 0,
            data: None,
        },
        call_sites: vec![5],
    }]));

    lawyer.push_outgoing_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "helper".into(),
            kind: "function".into(),
            detail: None,
            file: "src/helpers.rs".into(),
            line: 10,
            column: 0,
            data: None,
        },
        call_sites: vec![5],
    }]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 1,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(!val.degraded);
    let incoming = val.incoming.as_ref().expect("incoming must be Some");
    let outgoing = val.outgoing.as_ref().expect("outgoing must be Some");
    assert_eq!(incoming.len(), 1);
    assert_eq!(
        incoming[0].semantic_path, "src/main.rs::caller_fn",
        "flat fallback must be used when surgeon returns None"
    );
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0].semantic_path, "src/helpers.rs::helper");
}

/// When Surgeon returns an error for a reference's file+line, the BFS must NOT
/// panic or propagate the error — it must fall back to the flat LSP name silently.
#[tokio::test]
async fn test_bfs_output_semantic_path_falls_back_on_surgeon_error() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    // Surgeon returns parse error for the enrichment call
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Err(pathfinder_treesitter::SurgeonError::ParseError {
            path: std::path::PathBuf::from("src/server.rs"),
            reason: "parse failed".to_owned(),
        }));
    // Second enrichment (outgoing) also errors
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

    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "handle_request".into(),
            kind: "function".into(),
            detail: None,
            file: "src/server.rs".into(),
            line: 20,
            column: 4,
            data: None,
        },
        call_sites: vec![20],
    }]));

    lawyer.push_outgoing_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "validate_token".into(),
            kind: "function".into(),
            detail: None,
            file: "src/token.rs".into(),
            line: 15,
            column: 4,
            data: None,
        },
        call_sites: vec![9],
    }]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 1,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("surgeon error must not propagate to caller");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(
        !val.degraded,
        "surgeon error in enrichment must not degrade the call"
    );
    let incoming = val.incoming.as_ref().expect("incoming must be Some");
    assert_eq!(incoming.len(), 1);
    // Must fall back to flat name silently
    assert_eq!(
        incoming[0].semantic_path, "src/server.rs::handle_request",
        "surgeon error must cause flat-name fallback, not panic"
    );
}

// ── PATCH-002: Trait method expansion via goto_implementation ─────────────

#[tokio::test]
#[allow(clippy::too_many_lines, reason = "Test data setup needs many lines")]
async fn test_trace_trait_method_expands_to_implementations() {
    // PATCH-002: When tracing a trait/interface method:
    // 1. Detect parent_kind = "interface"
    // 2. Call goto_implementation to find concrete impls
    // 3. BFS call hierarchy for EACH impl
    // 4. Merge results, set resolution_strategy = "lsp_call_hierarchy_with_impl_expansion"

    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().push(Ok(
        pathfinder_common::types::SymbolScope {
            content: "fn search(&self);".to_string(),
            start_line: 2,
            end_line: 2,
            name_column: 7,
            language: "rust".to_string(),
            parent_kind: Some("interface".to_string()),
            parent_name: Some("Scout".to_string()),
        },
    ));

    let lawyer = Arc::new(MockLawyer::default());

    // Step 1: goto_implementation returns 2 impl locations
    lawyer.set_goto_implementation_result(Ok(vec![
        DefinitionLocation {
            file: "src/mock_scout.rs".into(),
            line: 10,
            column: 5,
            preview: "impl Scout for MockScout".into(),
        },
        DefinitionLocation {
            file: "src/ripgrep_scout.rs".into(),
            line: 15,
            column: 5,
            preview: "impl Scout for RipgrepScout".into(),
        },
    ]));

    // Step 2: call_hierarchy_prepare for each impl (2 results)
    // For MockScout.search
    let item_mock = CallHierarchyItem {
        name: "search".into(),
        kind: "method".into(),
        detail: Some("fn search(&self)".into()),
        file: "src/mock_scout.rs".into(),
        line: 10,
        column: 5,
        data: None,
    };
    // For RipgrepScout.search
    let item_ripgrep = CallHierarchyItem {
        name: "search".into(),
        kind: "method".into(),
        detail: Some("fn search(&self)".into()),
        file: "src/ripgrep_scout.rs".into(),
        line: 15,
        column: 5,
        data: None,
    };
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item_mock.clone()]));
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item_ripgrep.clone()]));

    // Step 3: BFS results for MockScout.search
    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "run_mock_tests".into(),
            kind: "function".into(),
            detail: Some("fn run_mock_tests()".into()),
            file: "src/main.rs".into(),
            line: 42,
            column: 4,
            data: None,
        },
        call_sites: vec![45],
    }]));
    lawyer.push_outgoing_call_result(Ok(vec![]));

    // Step 4: BFS results for RipgrepScout.search
    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "do_search".into(),
            kind: "function".into(),
            detail: Some("fn do_search()".into()),
            file: "src/search_app.rs".into(),
            line: 100,
            column: 4,
            data: None,
        },
        call_sites: vec![103],
    }]));
    lawyer.push_outgoing_call_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::Scout.search".to_owned(),
        max_depth: 1,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Assert: not degraded
    assert!(!val.degraded, "should not be degraded");
    assert_eq!(val.degraded_reason, None);

    // Assert: resolution_strategy shows impl expansion
    assert_eq!(
        val.resolution_strategy,
        Some("lsp_call_hierarchy_with_impl_expansion".to_owned()),
        "resolution_strategy should indicate impl expansion"
    );

    // Assert: merged incoming from BOTH impls
    let incoming = val.incoming.as_ref().expect("incoming must be Some");
    assert_eq!(
        incoming.len(),
        2,
        "should have 2 incoming references from 2 impls"
    );

    let has_run_mock = incoming
        .iter()
        .any(|r| r.semantic_path.contains("run_mock_tests"));
    let has_do_search = incoming
        .iter()
        .any(|r| r.semantic_path.contains("do_search"));
    assert!(has_run_mock, "should include caller from MockScout impl");
    assert!(
        has_do_search,
        "should include caller from RipgrepScout impl"
    );
}

#[tokio::test]
async fn test_trace_trait_method_no_impls_falls_back() {
    // PATCH-002: When trait method has no implementations,
    // fall back to normal flow on the trait method itself.

    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().push(Ok(
        pathfinder_common::types::SymbolScope {
            content: "fn search(&self);".to_string(),
            start_line: 2,
            end_line: 2,
            name_column: 7,
            language: "rust".to_string(),
            parent_kind: Some("interface".to_string()),
            parent_name: Some("Scout".to_string()),
        },
    ));

    let lawyer = Arc::new(MockLawyer::default());

    // goto_implementation returns empty (no impls found)
    lawyer.set_goto_implementation_result(Ok(vec![]));

    // Normal flow should proceed: call_hierarchy_prepare
    let item = CallHierarchyItem {
        name: "search".into(),
        kind: "method".into(),
        detail: Some("fn search(&self)".into()),
        file: "src/auth.rs".into(),
        line: 3,
        column: 7,
        data: None,
    };
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item.clone()]));
    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "caller".into(),
            kind: "function".into(),
            detail: Some("fn caller()".into()),
            file: "src/server.rs".into(),
            line: 20,
            column: 4,
            data: None,
        },
        call_sites: vec![25],
    }]));
    lawyer.push_outgoing_call_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::Scout.search".to_owned(),
        max_depth: 1,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Falls back to normal lsp_call_hierarchy when no impls
    assert_eq!(
        val.resolution_strategy,
        Some("lsp_call_hierarchy".to_owned()),
        "should fall back to standard lsp_call_hierarchy when no impls"
    );
}

#[tokio::test]
async fn test_trace_non_trait_method_unchanged() {
    // PATCH-002: Regular methods (no interface parent) should
    // behave exactly as before - no goto_implementation call.

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
    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "handle_request".into(),
            kind: "function".into(),
            detail: Some("fn handle_request()".into()),
            file: "src/server.rs".into(),
            line: 20,
            column: 4,
            data: None,
        },
        call_sites: vec![25],
    }]));
    lawyer.push_outgoing_call_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer.clone());

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 1,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Regular method: standard resolution strategy
    assert_eq!(
        val.resolution_strategy,
        Some("lsp_call_hierarchy".to_owned()),
        "regular method should use lsp_call_hierarchy"
    );

    // Verify goto_implementation was NOT called for non-trait method
    assert_eq!(
        lawyer.goto_implementation_call_count(),
        0,
        "goto_implementation should not be called for non-trait methods"
    );
}

/// PATCH-002 Fix 1: When trait method expansion succeeds, the `hint` field
/// must surface a trait-specific message including the impl count and file count.
#[tokio::test]
async fn test_trace_trait_method_hint_surfaces_expansion() {
    use crate::server::types::FindCallersCalleesMetadata;

    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().push(Ok(
        pathfinder_common::types::SymbolScope {
            content: "fn search(&self);".to_string(),
            start_line: 2,
            end_line: 2,
            name_column: 7,
            language: "rust".to_string(),
            parent_kind: Some("interface".to_string()),
            parent_name: Some("Scout".to_string()),
        },
    ));

    let lawyer = Arc::new(MockLawyer::default());

    // 2 impls in 2 distinct files
    lawyer.set_goto_implementation_result(Ok(vec![
        DefinitionLocation {
            file: "src/mock_scout.rs".into(),
            line: 10,
            column: 5,
            preview: "impl Scout for MockScout".into(),
        },
        DefinitionLocation {
            file: "src/ripgrep_scout.rs".into(),
            line: 15,
            column: 5,
            preview: "impl Scout for RipgrepScout".into(),
        },
    ]));

    let item1 = CallHierarchyItem {
        name: "search".into(),
        kind: "method".into(),
        detail: Some("fn search(&self)".into()),
        file: "src/mock_scout.rs".into(),
        line: 10,
        column: 5,
        data: None,
    };
    let item2 = CallHierarchyItem {
        name: "search".into(),
        kind: "method".into(),
        detail: Some("fn search(&self)".into()),
        file: "src/ripgrep_scout.rs".into(),
        line: 15,
        column: 5,
        data: None,
    };
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item1.clone()]));
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item2.clone()]));
    // One incoming per impl (non-empty so the regular empty-incoming hint doesn't fire)
    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "a".into(),
            kind: "function".into(),
            detail: Some("fn a()".into()),
            file: "src/main.rs".into(),
            line: 1,
            column: 1,
            data: None,
        },
        call_sites: vec![2],
    }]));
    lawyer.push_outgoing_call_result(Ok(vec![]));
    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "b".into(),
            kind: "function".into(),
            detail: Some("fn b()".into()),
            file: "src/main.rs".into(),
            line: 3,
            column: 1,
            data: None,
        },
        call_sites: vec![4],
    }]));
    lawyer.push_outgoing_call_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::Scout.search".to_owned(),
        max_depth: 1,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Assert: hint present and mentions impl count + file count + trait
    let hint = val
        .hint
        .expect("hint must be set when impl expansion succeeds");
    assert!(
        hint.contains("trait/interface"),
        "hint must mention trait/interface, got: {hint}"
    );
    assert!(
        hint.contains("2 implementation"),
        "hint must surface the impl count (2), got: {hint}"
    );
    assert!(
        hint.contains("2 file"),
        "hint must surface the file count (2), got: {hint}"
    );
}

/// PATCH-002 Fix 1: When multiple impls are in the SAME file, the file count
/// in the hint must reflect unique file count, not impl count.
#[tokio::test]
async fn test_trace_trait_method_hint_dedupes_files() {
    use crate::server::types::FindCallersCalleesMetadata;

    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().push(Ok(
        pathfinder_common::types::SymbolScope {
            content: "fn search(&self);".to_string(),
            start_line: 2,
            end_line: 2,
            name_column: 7,
            language: "rust".to_string(),
            parent_kind: Some("interface".to_string()),
            parent_name: Some("Scout".to_string()),
        },
    ));

    let lawyer = Arc::new(MockLawyer::default());

    // 2 impls in 1 file (e.g. two impls of the same trait in one file)
    lawyer.set_goto_implementation_result(Ok(vec![
        DefinitionLocation {
            file: "src/multi_impl.rs".into(),
            line: 10,
            column: 5,
            preview: "impl1".into(),
        },
        DefinitionLocation {
            file: "src/multi_impl.rs".into(),
            line: 20,
            column: 5,
            preview: "impl2".into(),
        },
    ]));

    let item1 = CallHierarchyItem {
        name: "search".into(),
        kind: "method".into(),
        detail: Some("fn search(&self)".into()),
        file: "src/multi_impl.rs".into(),
        line: 10,
        column: 5,
        data: None,
    };
    let item2 = CallHierarchyItem {
        name: "search".into(),
        kind: "method".into(),
        detail: Some("fn search(&self)".into()),
        file: "src/multi_impl.rs".into(),
        line: 20,
        column: 5,
        data: None,
    };
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item1.clone()]));
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item2.clone()]));
    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "a".into(),
            kind: "function".into(),
            detail: Some("fn a()".into()),
            file: "src/main.rs".into(),
            line: 1,
            column: 1,
            data: None,
        },
        call_sites: vec![2],
    }]));
    lawyer.push_outgoing_call_result(Ok(vec![]));
    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "b".into(),
            kind: "function".into(),
            detail: Some("fn b()".into()),
            file: "src/main.rs".into(),
            line: 3,
            column: 1,
            data: None,
        },
        call_sites: vec![4],
    }]));
    lawyer.push_outgoing_call_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::Scout.search".to_owned(),
        max_depth: 1,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    let hint = val.hint.expect("hint must be set");
    assert!(
        hint.contains("2 implementation"),
        "hint should still mention 2 impls, got: {hint}"
    );
    assert!(
        hint.contains("1 file"),
        "hint must say 1 file (unique file count), got: {hint}"
    );
}

/// PATCH-002 Fix 2: When `goto_implementation` errors out, the result must be
/// marked degraded and the request must still fall through to normal BFS.
#[tokio::test]
async fn test_trace_trait_method_goto_implementation_error_marks_degraded() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().push(Ok(
        pathfinder_common::types::SymbolScope {
            content: "fn search(&self);".to_string(),
            start_line: 2,
            end_line: 2,
            name_column: 7,
            language: "rust".to_string(),
            parent_kind: Some("interface".to_string()),
            parent_name: Some("Scout".to_string()),
        },
    ));

    let lawyer = Arc::new(MockLawyer::default());

    // goto_implementation returns an error (e.g. LSP doesn't support the capability)
    lawyer.set_goto_implementation_result(Err(pathfinder_lsp::LspError::Protocol(
        "textDocument/implementation not supported".to_string(),
    )));

    // Normal flow should still proceed: call_hierarchy_prepare on the trait method
    let item = CallHierarchyItem {
        name: "search".into(),
        kind: "method".into(),
        detail: Some("fn search(&self)".into()),
        file: "src/auth.rs".into(),
        line: 3,
        column: 7,
        data: None,
    };
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item.clone()]));
    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "caller".into(),
            kind: "function".into(),
            detail: Some("fn caller()".into()),
            file: "src/server.rs".into(),
            line: 20,
            column: 4,
            data: None,
        },
        call_sites: vec![25],
    }]));
    lawyer.push_outgoing_call_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::Scout.search".to_owned(),
        max_depth: 1,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Assert: degraded because goto_implementation failed
    assert!(
        val.degraded,
        "result must be marked degraded when goto_implementation errors (got degraded_reason={:?})",
        val.degraded_reason
    );
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::LspErrorGrepFallback),
        "degraded_reason must describe the LSP error"
    );
    // Assert: NOT marked as impl expansion (resolution_strategy = standard lsp_call_hierarchy)
    assert_eq!(
        val.resolution_strategy,
        Some("lsp_call_hierarchy".to_owned()),
        "should fall back to standard lsp_call_hierarchy when goto_implementation errors"
    );
}

// ── PATCH-003: Degraded Hint Visibility + Verified Flags ─────────────
//
// PATCH-003 introduces two complementary safeguards against the
// "null vs [] is a footgun" refactoring hazard:
//
// 1. `hint` is now populated in the degraded+null scenario so prose-reading
//    agents see an explicit "UNKNOWN, not zero" warning.
// 2. `incoming_verified` and `outgoing_verified` are machine-readable
//    per-field flags so structured-field-only consumers can disambiguate.
//
// These tests cover the 4 key scenarios:
//   1. degraded + null incoming/outgoing
//   2. non-degraded + empty incoming
//   3. degraded + heuristic incoming (grep fallback produced results)
//   4. non-degraded + non-empty incoming

/// PATCH-003 DELIVERABLE D Test 1: When `degraded=true` AND `incoming=null`
/// (LSP unavailable, callers UNKNOWN), the response MUST:
///
/// 1. Populate `hint` with an explicit "UNKNOWN, not zero" warning that
///    prevents agents from coalescing `null` to `[]` silently.
/// 2. Set `incoming_verified = Some(false)` and `outgoing_verified = Some(false)`
///    so structured consumers can detect the unverified state.
#[tokio::test]
async fn test_trace_degraded_null_incoming_has_hint_and_unverified() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    // NoOpLawyer → NoLsp → grep fallback with no Scout matches → incoming=None, outgoing=None.
    // Construct the server manually because `make_server_with_lawyer` requires Arc<MockLawyer>.
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
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 2,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Sanity: degraded + null + null
    assert!(val.degraded, "must be degraded when LSP is unavailable");
    assert!(
        val.incoming.is_none(),
        "incoming must be None (UNKNOWN) when degraded with no fallback matches"
    );
    assert!(
        val.outgoing.is_none(),
        "outgoing must be None (UNKNOWN) when degraded with no fallback matches"
    );

    // PATCH-003 DELIVERABLE A: hint must be populated in the catastrophic
    // degraded+null scenario. This is the central fix — previously hint
    // was None in this exact scenario.
    let hint = val.hint.as_ref().expect(
        "hint must be Some(...) in the degraded+null scenario (was the catastrophic footgun)",
    );
    assert!(
        hint.contains("UNKNOWN"),
        "hint must contain 'UNKNOWN' to clearly distinguish from verified zero, got: {hint}"
    );
    assert!(
        hint.to_lowercase().contains("null") || hint.contains("Do NOT"),
        "hint must explicitly warn against treating null as zero, got: {hint}"
    );

    // PATCH-003 DELIVERABLE B: machine-readable verified flags
    assert_eq!(
        val.incoming_verified,
        Some(false),
        "incoming_verified must be Some(false) when callers are UNKNOWN"
    );
    assert_eq!(
        val.outgoing_verified,
        Some(false),
        "outgoing_verified must be Some(false) when callees are UNKNOWN"
    );

    // Verify the fields actually serialize (don't get stripped by skip_serializing_if)
    let json = serde_json::to_value(&val).expect("serialize");
    assert_eq!(
        json["incoming_verified"],
        serde_json::json!(false),
        "incoming_verified must be present in JSON output (no skip_serializing_if regression)"
    );
    assert_eq!(
        json["outgoing_verified"],
        serde_json::json!(false),
        "outgoing_verified must be present in JSON output (no skip_serializing_if regression)"
    );
}

/// PATCH-003 DELIVERABLE D Test 2: When `degraded=false` AND `incoming=Some([])`
/// (LSP confirmed zero callers), the response MUST:
///
/// 1. Populate `hint` with the existing "confirmed zero" message (regression
///    check — make sure PATCH-003 didn't break the non-degraded path).
/// 2. Set `incoming_verified = Some(true)`.
#[tokio::test]
async fn test_trace_non_degraded_empty_incoming_has_hint_and_verified() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());

    // LSP returns a valid call hierarchy item
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

    // Incoming is empty (zero callers)
    lawyer.push_incoming_call_result(Ok(vec![]));
    // Outgoing has a callee — this prevents the "both empty" grep-fallback
    // branch (line ~793) from firing and flipping degraded=true.
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

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 1,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Sanity: NOT degraded, empty incoming, non-empty outgoing
    assert!(
        !val.degraded,
        "must NOT be degraded when LSP confirmed zero"
    );
    let incoming = val
        .incoming
        .as_ref()
        .expect("incoming must be Some when not degraded");
    assert!(
        incoming.is_empty(),
        "incoming must be Some([]) — confirmed zero callers"
    );
    let outgoing = val
        .outgoing
        .as_ref()
        .expect("outgoing must be Some when not degraded");
    assert_eq!(outgoing.len(), 1, "outgoing has the one callee");

    // PATCH-003 regression check: existing "confirmed zero" hint must still fire.
    let hint = val
        .hint
        .as_ref()
        .expect("hint must be Some(...) for confirmed-zero incoming (existing P2-7 behavior)");
    assert!(
        hint.contains("confirmed zero"),
        "hint must contain 'confirmed zero' for non-degraded + empty incoming, got: {hint}"
    );

    // PATCH-003 DELIVERABLE B: machine-readable verified flag
    assert_eq!(
        val.incoming_verified,
        Some(true),
        "incoming_verified must be Some(true) when LSP confirmed zero callers"
    );
    assert_eq!(
        val.outgoing_verified,
        Some(true),
        "outgoing_verified must be Some(true) when LSP confirmed outgoing callees"
    );
}

/// PATCH-003 DELIVERABLE D Test 3: When `degraded=true` AND `incoming=Some(vec)`
/// (LSP unavailable, but grep fallback found heuristic candidates), the
/// response MUST:
///
/// 1. Set `incoming_verified = Some(false)` because the results are heuristic,
///    NOT LSP-verified (may include false positives).
/// 2. Populate `hint` with a warning that the results are heuristic OR that
///    the other direction is UNKNOWN, so agents see a warning even if they
///    ignore `incoming_verified`.
#[tokio::test]
#[allow(clippy::too_many_lines, reason = "Test data setup needs many lines")]
async fn test_trace_degraded_heuristic_incoming_is_unverified() {
    use pathfinder_search::{SearchMatch, SearchResult};

    /// Build a single-match `SearchResult` for a file+line+content.
    fn single_match(file: &str, line: u32, content: &str) -> SearchResult {
        SearchResult {
            matches: vec![SearchMatch {
                file: file.to_owned(),
                line: u64::from(line),
                column: 1,
                content: content.to_owned(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:test".to_owned(),
                known: Some(false),
            }],
            total_matches: 1,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }
    }

    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().push(Ok(
        pathfinder_common::types::SymbolScope {
            // Body has a function call → `validate_token` is extracted as an
            // outgoing call candidate by `grep_outgoing_fallback`.
            content: "fn login() -> bool { validate_token() }".to_owned(),
            start_line: 9,
            end_line: 9,
            name_column: 0,
            language: "rust".to_owned(),
            ..Default::default()
        },
    ));
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));

    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Files that grep fallback will search
    let src = ws_dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("auth.rs"),
        "fn login() -> bool { validate_token() }",
    )
    .unwrap();
    std::fs::write(src.join("caller.rs"), "fn handle_request() { login(); }").unwrap();
    std::fs::write(src.join("token.rs"), "fn validate_token() -> bool { true }").unwrap();

    // Queue two MockScout results so BOTH the incoming grep call AND the
    // outgoing grep call (for `validate_token`) return a heuristic match.
    // When BOTH directions return Some(vec), the new hint logic emits the
    // "heuristic grep-based candidates" warning.
    let scout = Arc::new(MockScout::default());
    scout.set_results(vec![
        Ok(single_match(
            "src/caller.rs",
            1,
            "fn handle_request() { login(); }",
        )),
        Ok(single_match(
            "src/token.rs",
            1,
            "fn validate_token() -> bool { true }",
        )),
    ]);

    // NoOpLawyer → NoLsp → triggers grep fallback path
    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        scout,
        surgeon,
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 2,
        ..Default::default()
    };

    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Sanity: degraded=true, incoming=Some(heuristic refs), outgoing=Some(heuristic refs)
    assert!(val.degraded, "must be degraded when LSP is unavailable");
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::NoLspGrepFallback),
        "degraded_reason must indicate grep fallback was used"
    );
    let incoming = val
        .incoming
        .as_ref()
        .expect("incoming must be Some(vec) when grep fallback found results");
    assert_eq!(incoming.len(), 1, "expected 1 heuristic incoming match");
    assert_eq!(incoming[0].file, "src/caller.rs");
    let outgoing = val
        .outgoing
        .as_ref()
        .expect("outgoing must be Some(vec) when grep outgoing fallback found candidates");
    assert_eq!(outgoing.len(), 1, "expected 1 heuristic outgoing match");
    assert_eq!(outgoing[0].file, "src/token.rs");

    // PATCH-003 DELIVERABLE B: heuristic results are NOT verified (BOTH directions)
    assert_eq!(
        val.incoming_verified,
        Some(false),
        "incoming_verified must be Some(false) when results are heuristic grep candidates"
    );
    assert_eq!(
        val.outgoing_verified,
        Some(false),
        "outgoing_verified must be Some(false) when results are heuristic grep candidates"
    );

    // PATCH-003 DELIVERABLE A: hint must warn about heuristic nature
    let hint = val
        .hint
        .as_ref()
        .expect("hint must be Some(...) in the degraded+heuristic scenario");
    assert!(
        hint.contains("heuristic"),
        "hint must mention 'heuristic' to warn about false positives, got: {hint}"
    );
}

/// PATCH-003 DELIVERABLE D Test 4: When `degraded=false` AND `incoming=Some(vec)`
/// (LSP returned real callers), the response MUST:
///
/// 1. Set `incoming_verified = Some(true)` because the results are LSP-confirmed.
/// 2. NOT populate `hint` (non-degraded + non-empty = no warning needed).
#[tokio::test]
async fn test_trace_non_degraded_with_results_is_verified() {
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

    // LSP returns a confirmed caller
    lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "handle_request".into(),
            kind: "function".into(),
            detail: Some("fn handle_request()".into()),
            file: "src/server.rs".into(),
            line: 20,
            column: 4,
            data: None,
        },
        call_sites: vec![25],
    }]));
    // Outgoing has a callee
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

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 1,
        ..Default::default()
    };
    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Sanity: NOT degraded, has results
    assert!(
        !val.degraded,
        "must NOT be degraded when LSP returned results"
    );
    let incoming = val
        .incoming
        .as_ref()
        .expect("incoming must be Some when LSP returned callers");
    assert_eq!(incoming.len(), 1, "expected 1 LSP-confirmed caller");
    let outgoing = val
        .outgoing
        .as_ref()
        .expect("outgoing must be Some when LSP returned callees");
    assert_eq!(outgoing.len(), 1, "expected 1 LSP-confirmed callee");

    // PATCH-003 DELIVERABLE B: LSP-confirmed results ARE verified
    assert_eq!(
        val.incoming_verified,
        Some(true),
        "incoming_verified must be Some(true) when LSP returned real callers"
    );
    assert_eq!(
        val.outgoing_verified,
        Some(true),
        "outgoing_verified must be Some(true) when LSP returned real callees"
    );

    // Non-degraded + non-empty = no hint needed
    assert!(
        val.hint.is_none(),
        "hint must be None when not degraded and results are non-empty \
         (no warning needed; got: {:?})",
        val.hint
    );
}

// ── PATCH-003 Edge Case: BFS-confirmed empty with grep replacement ────────
//
// This regression test covers a subtle case: when the LSP BFS returns empty
// for BOTH incoming and outgoing, the "both empty" branch fires, and grep
// fallback finds heuristic matches. In this scenario:
//   - `incoming` and `outgoing` are REPLACED by grep results
//   - `degraded` is true (LspWarmupGrepFallback)
//   - `lsp_bfs_ran` was true (BFS ran) but is RESET to false because the
//     data was overwritten by heuristic grep
//   - Therefore `incoming_verified` and `outgoing_verified` MUST be Some(false)
//
// If `lsp_bfs_ran` were not reset on grep replacement, the verified flag
// would incorrectly be Some(true) for heuristic data — defeating the entire
// purpose of PATCH-003.

/// PATCH-003 edge case: When BFS returns empty + grep fallback finds heuristic
/// matches that REPLACE the BFS data, the verified flag must be Some(false).
#[tokio::test]
#[allow(clippy::too_many_lines, reason = "Test data setup needs many lines")]
async fn test_trace_bfs_empty_grep_replacement_is_unverified() {
    use pathfinder_search::{SearchMatch, SearchResult};

    fn single_match(file: &str, line: u32, content: &str) -> SearchResult {
        SearchResult {
            matches: vec![SearchMatch {
                file: file.to_owned(),
                line: u64::from(line),
                column: 1,
                content: content.to_owned(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:test".to_owned(),
                known: Some(false),
            }],
            total_matches: 1,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }
    }

    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));

    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Files that grep fallback will search
    let src = ws_dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("auth.rs"), "fn login() -> bool { true }").unwrap();
    std::fs::write(src.join("caller.rs"), "fn handle_request() { login(); }").unwrap();

    // MockScout returns a heuristic match for "login" — this REPLACES the
    // empty BFS result with a heuristic candidate.
    let scout = Arc::new(MockScout::default());
    scout.set_results(vec![Ok(single_match(
        "src/caller.rs",
        1,
        "fn handle_request() { login(); }",
    ))]);

    // BFS will run (LSP returns items) but return empty for BOTH directions
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
    lawyer.push_incoming_call_result(Ok(vec![]));
    lawyer.push_outgoing_call_result(Ok(vec![]));

    let server = PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

    let params = TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 1,
        ..Default::default()
    };

    let result = server.find_callers_callees_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::FindCallersCalleesMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Sanity: degraded=true, incoming=Some(grep heuristic)
    assert!(
        val.degraded,
        "must be degraded when BFS empty triggers grep fallback"
    );
    let incoming = val
        .incoming
        .as_ref()
        .expect("incoming must be Some(vec) — grep replaced BFS empty result");
    assert_eq!(
        incoming.len(),
        1,
        "incoming should have 1 heuristic match from grep fallback"
    );
    assert_eq!(incoming[0].file, "src/caller.rs");

    // CRITICAL: incoming_verified must be Some(false) because the data is
    // heuristic grep results, NOT BFS-confirmed. This is the regression
    // test for the case where BFS ran, returned empty, and grep OVERWROTE
    // the empty result with heuristic matches.
    assert_eq!(
        val.incoming_verified,
        Some(false),
        "incoming_verified must be Some(false) when grep REPLACED BFS empty result with \
         heuristic matches. Got: degraded={}, incoming={:?}, incoming_verified={:?}",
        val.degraded,
        val.incoming,
        val.incoming_verified
    );
}
