use super::super::test_helpers::{make_scope, make_server_with_lawyer, make_temp_workspace};
use super::*;
use pathfinder_common::config::PathfinderConfig;
use pathfinder_common::sandbox::Sandbox;
use pathfinder_common::types::{DegradedReason, WorkspaceRoot};
use pathfinder_lsp::types::{CallHierarchyCall, CallHierarchyItem, ReferenceLocation};
use pathfinder_lsp::{LspError, MockLawyer};
use pathfinder_search::MockScout;
use pathfinder_treesitter::mock::MockSurgeon;
use std::sync::Arc;

// ── symbol_overview ─────────────────────────────────────────────

#[tokio::test]
async fn test_symbol_overview_aggregates_callers_callees_references() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().extend([
        Ok(make_scope()),
        Ok(make_scope()),
        Ok(make_scope()),
    ]);

    let lawyer = Arc::new(MockLawyer::default());

    // Configure find_callers_callees
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

    // Configure find_all_references
    lawyer.set_references_result(Ok(vec![
        ReferenceLocation {
            file: "src/main.rs".into(),
            line: 10,
            column: 8,
            snippet: "login();".into(),
        },
        ReferenceLocation {
            file: "src/tests.rs".into(),
            line: 5,
            column: 4,
            snippet: "let _ = login();".into(),
        },
    ]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = crate::server::types::TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_references: 50,
        ..Default::default()
    };

    let result = server.symbol_overview_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::SymbolOverviewResponse =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Verify source
    assert!(val.source.is_some());
    let source = val.source.as_ref().unwrap();
    assert_eq!(source.content, "fn login() { }");
    assert_eq!(source.start_line, 9);
    assert_eq!(source.end_line, 9);

    // Verify impact
    assert!(val.impact.is_some());
    let impact = val.impact.as_ref().unwrap();
    assert!(impact.incoming.is_some());
    assert!(impact.outgoing.is_some());
    assert_eq!(impact.incoming.as_ref().unwrap().len(), 1);
    assert_eq!(impact.outgoing.as_ref().unwrap().len(), 1);

    // Verify references
    assert!(val.references.is_some());
    let refs = val.references.as_ref().unwrap();
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].file, "src/main.rs");
    assert_eq!(refs[1].file, "src/tests.rs");

    // Verify not degraded
    assert!(!val.degraded);
    assert!(val.degraded_reason.is_none());
    assert_eq!(val.lsp_readiness, Some("ready".to_owned()));
}

#[tokio::test]
async fn test_symbol_overview_no_impact_no_references_shows_unavailable() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().extend([
        Ok(make_scope()),
        Ok(make_scope()),
        Ok(make_scope()),
    ]);

    let lawyer = Arc::new(MockLawyer::default());

    // Configure empty impact (no items, no errors)
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

    lawyer.push_incoming_call_result(Ok(vec![])); // No incoming
    lawyer.push_outgoing_call_result(Ok(vec![])); // No outgoing

    // Configure empty references
    lawyer.set_references_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = crate::server::types::TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_references: 50,
        ..Default::default()
    };

    let result = server.symbol_overview_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::SymbolOverviewResponse =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Verify impact shows empty arrays (prepare succeeded but BFS found nothing)
    assert!(val.impact.is_some());
    let impact = val.impact.as_ref().unwrap();
    assert!(impact.incoming.is_some());
    assert!(impact.outgoing.is_some());
    assert_eq!(impact.incoming.as_ref().unwrap().len(), 0);
    assert_eq!(impact.outgoing.as_ref().unwrap().len(), 0);

    // Verify references shows 0 files
    assert!(val.references.is_some());
    let refs = val.references.as_ref().unwrap();
    assert_eq!(refs.len(), 0);
    assert_eq!(val.files_referenced, 0);

    // Degraded because results are empty
    assert!(val.degraded);
    assert_eq!(val.lsp_readiness, Some("warming_up".to_owned()));
}

#[tokio::test]
async fn test_symbol_overview_with_references_only() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().extend([
        Ok(make_scope()),
        Ok(make_scope()),
        Ok(make_scope()),
    ]);

    let lawyer = Arc::new(MockLawyer::default());

    // Configure references
    lawyer.set_references_result(Ok(vec![
        ReferenceLocation {
            file: "src/main.rs".into(),
            line: 10,
            column: 8,
            snippet: "login();".into(),
        },
        ReferenceLocation {
            file: "src/auth.rs".into(),
            line: 5,
            column: 4,
            snippet: "fn login() {".into(),
        },
    ]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = crate::server::types::TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_references: 50,
        ..Default::default()
    };

    let result = server.symbol_overview_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::SymbolOverviewResponse =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Verify references aggregated
    assert!(val.references.is_some());
    let refs = val.references.as_ref().unwrap();
    assert_eq!(refs.len(), 2);
    assert_eq!(val.files_referenced, 2);
}

#[tokio::test]
async fn test_symbol_overview_degraded_when_lsp_unavailable() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().extend([
        Ok(make_scope()),
        Ok(make_scope()),
        Ok(make_scope()),
    ]);

    // Use NoOpLawyer to simulate LSP unavailable
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

    let params = crate::server::types::TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_references: 50,
        ..Default::default()
    };

    let result = server.symbol_overview_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::SymbolOverviewResponse =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Verify degraded
    assert!(val.degraded);
    assert_eq!(val.degraded_reason, Some(DegradedReason::NoLsp));
    assert_eq!(val.lsp_readiness, Some("unavailable".to_owned()));

    // Verify impact unavailable
    assert!(val.impact.is_none());

    // Verify references unavailable
    assert!(val.references.is_none());
}

#[tokio::test]
async fn test_symbol_overview_lsp_error_references_degraded() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().extend([
        Ok(make_scope()),
        Ok(make_scope()),
        Ok(make_scope()),
    ]);

    let lawyer = Arc::new(MockLawyer::default());

    // Configure LSP error for references - this tests line 3061 Err(_) branch
    // Also configure a valid prepare result so impact is not degraded
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
    lawyer.set_references_lsp_error(Err(LspError::Timeout {
        operation: "references".to_string(),
        timeout_ms: 10000,
    }));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = crate::server::types::TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_references: 50,
        ..Default::default()
    };

    let result = server.symbol_overview_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::SymbolOverviewResponse =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Verify degraded on LSP error in find_all_references_impl
    assert!(val.degraded);
    // Only references degraded, not impact
    assert!(!val.impact_degraded);
    assert!(val.references_degraded);
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::LspTimeoutGrepFallback)
    );
    assert_eq!(val.lsp_readiness, Some("warming_up".to_owned()));
    assert_eq!(val.warm_start_in_progress, Some(true));

    // References unavailable due to degradation
    assert!(val.references.is_none());
    assert_eq!(val.files_referenced, 0);
}

#[tokio::test]
async fn test_symbol_overview_bfs_error_logs_warning_continues_with_empty_results() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().extend([
        Ok(make_scope()),
        Ok(make_scope()),
        Ok(make_scope()),
    ]);

    let lawyer = Arc::new(MockLawyer::default());

    // Configure LSP error for impact
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
    lawyer.push_incoming_call_result(Err(LspError::Protocol(
        "LSP call hierarchy error".to_string(),
    )));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = crate::server::types::TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_references: 50,
        ..Default::default()
    };

    let result = server.symbol_overview_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::SymbolOverviewResponse =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Verify degraded (empty BFS call hierarchy results in degradation)
    assert!(val.degraded);
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::LspWarmupGrepFallback)
    );
    assert_eq!(val.lsp_readiness, Some("warming_up".to_owned()));

    // Impact is populated with empty arrays (prepare succeeded)
    assert!(val.impact.is_some());
    let impact = val.impact.as_ref().unwrap();
    assert!(impact.incoming.is_some());
    assert!(impact.outgoing.is_some());
}

#[tokio::test]
async fn test_symbol_overview_partial_degradation_treesitter_fails_refs_ok() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().extend([
        Ok(make_scope()),
        Ok(make_scope()),
        Ok(make_scope()),
    ]);

    let lawyer = Arc::new(MockLawyer::default());

    // Configure references to succeed
    lawyer.set_references_result(Ok(vec![ReferenceLocation {
        file: "src/main.rs".into(),
        line: 10,
        column: 8,
        snippet: "login();".into(),
    }]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = crate::server::types::TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_references: 50,
        ..Default::default()
    };

    let result = server.symbol_overview_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::SymbolOverviewResponse =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Verify degraded (impact failed due to LSP not providing items)
    assert!(val.degraded);
    // Degraded reason is LspWarmupEmptyUnverified (prepare returned empty, goto_definition returned None)
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::LspWarmupEmptyUnverified)
    );

    // Impact unavailable due to degradation
    assert!(val.impact.is_none());

    // References available (partial degradation)
    assert!(val.references.is_some());
    let refs = val.references.as_ref().unwrap();
    assert_eq!(refs.len(), 1);
}

#[tokio::test]
async fn test_symbol_overview_rejects_empty_semantic_path() {
    let surgeon = Arc::new(MockSurgeon::new());

    let lawyer = Arc::new(MockLawyer::default());
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = crate::server::types::TraceParams {
        semantic_path: String::new(),
        max_references: 50,
        ..Default::default()
    };

    let result = server.symbol_overview_impl(params).await;
    assert!(result.is_err(), "should reject empty semantic path");
}

#[tokio::test]
async fn test_symbol_overview_file_not_found_returns_error() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let lawyer = Arc::new(MockLawyer::default());
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

    let params = crate::server::types::TraceParams {
        semantic_path: "nonexistent/path.rs::function".to_owned(),
        max_references: 50,
        ..Default::default()
    };

    let result = server.symbol_overview_impl(params).await;
    assert!(result.is_err(), "should return error for nonexistent file");
}

#[tokio::test]
async fn test_symbol_overview_respects_max_callers_callees_limit() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().extend([
        Ok(make_scope()),
        Ok(make_scope()),
        Ok(make_scope()),
    ]);

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

    // Configure 5 incoming calls
    let incoming: Vec<_> = (0..5)
        .map(|i| CallHierarchyCall {
            item: CallHierarchyItem {
                name: format!("caller{i}"),
                kind: "function".into(),
                detail: Some(format!("fn caller{i}()")),
                file: format!("src/caller{i}.rs"),
                line: u32::try_from(i + 1).unwrap(),
                column: 4,
                data: None,
            },
            call_sites: vec![u32::try_from(i + 10).unwrap()],
        })
        .collect();
    lawyer.push_incoming_call_result(Ok(incoming));

    lawyer.push_outgoing_call_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = crate::server::types::TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_references: 6,
        ..Default::default()
    };

    let result = server.symbol_overview_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::SymbolOverviewResponse =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Verify max_callers_callees limit respected
    assert!(val.impact.is_some());
    let impact = val.impact.as_ref().unwrap();
    assert!(impact.incoming.is_some());
    let incoming = impact.incoming.as_ref().unwrap();
    assert_eq!(
        incoming.len(),
        3,
        "should return exactly max_callers_callees/2=3, got {}",
        incoming.len()
    );
}

// ── impact_result returning Err(_) ──────────────────────────────

#[tokio::test]
async fn test_symbol_overview_impact_err_sets_degraded() {
    // When find_callers_callees_impl returns Ok with degraded=true (LSP error),
    // the overview should propagate the degraded state.
    // Note: find_callers_callees_impl returns Ok(degraded) on LSP errors, not Err.
    // The Err(_) branch in symbol_overview_impl is for unexpected failures.
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().extend([
        Ok(make_scope()),
        Ok(make_scope()),
        Ok(make_scope()),
    ]);

    let lawyer = Arc::new(MockLawyer::default());

    // Make call_hierarchy_prepare fail → find_callers_callees_impl returns Ok(degraded)
    // with grep fallback (which also fails since no scout results configured)
    lawyer.push_prepare_call_hierarchy_result(Err(LspError::Protocol("LSP crashed".to_string())));

    // References succeed
    lawyer.set_references_result(Ok(vec![ReferenceLocation {
        file: "src/main.rs".into(),
        line: 10,
        column: 8,
        snippet: "login();".into(),
    }]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = crate::server::types::TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_references: 50,
        ..Default::default()
    };

    let result = server.symbol_overview_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::SymbolOverviewResponse =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Verify degraded due to impact error
    assert!(val.degraded);
    // Only impact degraded, references are fine
    assert!(val.impact_degraded);
    assert!(!val.references_degraded);
    // find_callers_callees_impl returns degraded_reason=LspErrorGrepFallback when LSP error occurs
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::LspErrorGrepFallback)
    );
    assert_eq!(val.lsp_readiness, Some("unavailable".to_owned()));

    // Impact is None because find_callers_callees returns degraded metadata with None incoming/outgoing
    assert!(val.impact.is_none());

    // References still available (partial degradation)
    assert!(val.references.is_some());
    assert_eq!(val.references.as_ref().unwrap().len(), 1);
}

// ── Both impact AND references degraded ─────────────────────────

#[tokio::test]
async fn test_symbol_overview_both_degraded() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().extend([
        Ok(make_scope()),
        Ok(make_scope()),
        Ok(make_scope()),
    ]);

    let lawyer = Arc::new(MockLawyer::default());

    // Make call_hierarchy_prepare fail → impact degraded
    lawyer.push_prepare_call_hierarchy_result(Err(LspError::Protocol("LSP crashed".to_string())));

    // Make references fail → references degraded
    lawyer.set_references_lsp_error(Err(LspError::ConnectionLost));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = crate::server::types::TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_references: 50,
        ..Default::default()
    };

    let result = server.symbol_overview_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::SymbolOverviewResponse =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Both degraded
    assert!(val.degraded);
    assert!(val.impact_degraded);
    assert!(val.references_degraded);
    // Impact error takes priority in degraded_reason (LspErrorGrepFallback)
    assert_eq!(
        val.degraded_reason,
        Some(DegradedReason::LspErrorGrepFallback)
    );
    assert_eq!(val.lsp_readiness, Some("unavailable".to_owned()));

    // Both unavailable
    assert!(val.impact.is_none());
    assert!(val.references.is_none());
    assert_eq!(val.files_referenced, 0);
}

// ── max_references is respected ──────────────────────────────────

#[tokio::test]
async fn test_symbol_overview_respects_max_references() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().extend([
        Ok(make_scope()),
        Ok(make_scope()),
        Ok(make_scope()),
    ]);

    let lawyer = Arc::new(MockLawyer::default());

    // Configure 5 references
    let refs: Vec<_> = (0..5)
        .map(|i| ReferenceLocation {
            file: format!("src/file{i}.rs"),
            line: u32::try_from(i + 1).unwrap(),
            column: 1,
            snippet: format!("// ref {i}"),
        })
        .collect();
    lawyer.set_references_result(Ok(refs));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = crate::server::types::TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_references: 3,
        ..Default::default()
    };

    let result = server.symbol_overview_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::SymbolOverviewResponse =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Verify max_references is respected
    assert!(val.references.is_some());
    let refs = val.references.as_ref().unwrap();
    assert_eq!(
        refs.len(),
        3,
        "should respect max_references=3, got {}",
        refs.len()
    );
}

// ── BATCH-04 Remaining Coverage Tests for overview.rs ─────────────────────

#[tokio::test]
async fn test_symbol_overview_find_callers_callees_err() {
    let surgeon = Arc::new(MockSurgeon::new());
    // Push 3 Ok scopes so read_symbol_scope always succeeds, eliminating concurrency queue races
    surgeon.read_symbol_scope_results.lock().unwrap().extend([
        Ok(make_scope()),
        Ok(make_scope()),
        Ok(make_scope()),
    ]);

    let lawyer = Arc::new(MockLawyer::default());
    // Configure call_hierarchy_prepare to fail to trigger degraded mode for callers/callees
    lawyer.push_prepare_call_hierarchy_result(Err(LspError::NoLspAvailable));

    // Configure references to succeed
    lawyer.set_references_result(Ok(vec![ReferenceLocation {
        file: "src/main.rs".into(),
        line: 10,
        column: 8,
        snippet: "login();".into(),
    }]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = crate::server::types::TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_references: 50,
        ..Default::default()
    };

    let result = server.symbol_overview_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::SymbolOverviewResponse =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(val.degraded);
    assert!(val.impact.is_none());
    assert!(val.references.is_some());
}

#[tokio::test]
async fn test_symbol_overview_find_all_references_err() {
    let surgeon = Arc::new(MockSurgeon::new());
    // Push 3 Ok scopes so read_symbol_scope always succeeds, eliminating concurrency queue races
    surgeon.read_symbol_scope_results.lock().unwrap().extend([
        Ok(make_scope()),
        Ok(make_scope()),
        Ok(make_scope()),
    ]);

    let lawyer = Arc::new(MockLawyer::default());
    // Configure references to fail to trigger degraded mode for references
    lawyer.set_references_lsp_error(Err(LspError::NoLspAvailable));

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
    lawyer.push_incoming_call_result(Ok(vec![]));
    lawyer.push_outgoing_call_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = crate::server::types::TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_references: 50,
        ..Default::default()
    };

    let result = server.symbol_overview_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::SymbolOverviewResponse =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(val.degraded);
    assert!(val.references.is_none());
    assert!(val.impact.is_some());
}

#[tokio::test]
async fn test_symbol_overview_line_count_and_source_inclusion() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon.read_symbol_scope_results.lock().unwrap().extend([
        Ok(pathfinder_common::types::SymbolScope {
            content: "fn login() {\n    println!(\"hello\");\n}".to_owned(),
            start_line: 10,
            end_line: 12,
            name_column: 0,
            language: "rust".to_owned(),
        }),
        Ok(make_scope()),
        Ok(make_scope()),
    ]);

    let lawyer = Arc::new(MockLawyer::default());
    let item = CallHierarchyItem {
        name: "login".into(),
        kind: "function".into(),
        detail: None,
        file: "src/auth.rs".into(),
        line: 10,
        column: 4,
        data: None,
    };
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![item.clone()]));
    lawyer.push_incoming_call_result(Ok(vec![]));
    lawyer.push_outgoing_call_result(Ok(vec![]));
    lawyer.set_references_result(Ok(vec![]));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);
    let params = crate::server::types::TraceParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_references: 50,
        ..Default::default()
    };

    let result = server.symbol_overview_impl(params).await;
    let call_res = result.expect("should succeed");

    let call_res_json = serde_json::to_value(&call_res).unwrap();
    let text = call_res_json["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();

    // Assert 3 lines instead of 2 (12 - 10 + 1 = 3)
    assert!(text.contains("SYMBOL: src/auth.rs::login (3 lines)"));
    // Assert source code is embedded in text
    assert!(text.contains("```\nfn login() {\n    println!(\"hello\");\n}\n```"));
}

#[test]
fn test_resolve_degraded_reason_scenarios() {
    // Scenario 1: No degradation
    let (degraded, reason, readiness, warm) =
        PathfinderServer::resolve_degraded_reason(false, None, false, None);
    assert!(!degraded);
    assert!(reason.is_none());
    assert_eq!(readiness, Some("ready".to_string()));
    assert_eq!(warm, Some(false));

    // Scenario 2: One degraded, warming up
    let (degraded, reason, readiness, warm) = PathfinderServer::resolve_degraded_reason(
        true,
        Some(DegradedReason::LspWarmupGrepFallback),
        false,
        None,
    );
    assert!(degraded);
    assert_eq!(reason, Some(DegradedReason::LspWarmupGrepFallback));
    assert_eq!(readiness, Some("warming_up".to_string()));
    assert_eq!(warm, Some(true));

    // Scenario 3: Both degraded, one warming up, one not
    let (degraded, reason, readiness, warm) = PathfinderServer::resolve_degraded_reason(
        true,
        Some(DegradedReason::NoLsp),
        true,
        Some(DegradedReason::LspTimeoutGrepFallback),
    );
    assert!(degraded);
    assert_eq!(reason, Some(DegradedReason::LspTimeoutGrepFallback));
    assert_eq!(readiness, Some("warming_up".to_string()));
    assert_eq!(warm, Some(true));
}

#[test]
fn test_render_overview_text_format() {
    let text = PathfinderServer::render_overview_text(
        "src/auth.rs::login",
        10,
        12,
        "fn login() {}",
        None,
        None,
        0,
        false,
        None,
        123,
    );

    assert!(text.contains("SYMBOL: src/auth.rs::login (3 lines)"));
    assert!(text.contains("CALLERS: unavailable"));
    assert!(text.contains("REFERENCES: unavailable"));
    assert!(text.contains("[completed in 123ms]"));
}
