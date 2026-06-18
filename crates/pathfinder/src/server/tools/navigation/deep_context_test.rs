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
        semantic_path: "src/auth.rs::login".to_owned(),
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
        semantic_path: "src/auth.rs::login".to_owned(),
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
        semantic_path: "src/auth.rs::login".to_owned(),
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
        semantic_path: "src/auth.rs::login".to_owned(),
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
        semantic_path: "src/auth.rs::login".to_owned(),
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
        semantic_path: "src/auth.rs::login".to_owned(),
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
        semantic_path: "src/auth.rs::login".to_owned(),
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
        semantic_path: "src/auth.rs::login".to_owned(),
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
        semantic_path: "src/auth.rs::login".to_owned(),
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
        semantic_path: "src/auth.rs::login".to_owned(),
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
        semantic_path: "src/auth.rs::login".to_owned(),
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
        semantic_path: "src/auth.rs::login".to_owned(),
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
