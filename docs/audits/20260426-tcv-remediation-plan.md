# Coverage Gap Remediation Plan

## Overview

415 TCV-001 occurrences across 4 files. This plan categorizes them, assesses
which gaps are real risks vs noise, and provides step-by-step instructions for
each remediation.

---

## Bug Fix (Already Applied)

### BUG-001: `merge_rust_impl_blocks` swapped `rsplit_once` bindings

File: `crates/pathfinder-treesitter/src/symbols.rs` line 425

`rsplit_once('.')` returns `(prefix, suffix)` but the old code destructured as
`(method_name, parent_path)` — reversed. For `"MyStruct#2.foo"` this produced
`"foo.MyStruct#2"` instead of `"MyStruct.foo"`.

Fix: swap to `(parent_path, method_name)`.

Status: FIXED. All workspace tests pass.

---

## Coverage Gap Analysis

### Categories of Uncovered Lines

The 415 occurrences fall into these patterns:

1. **Sandbox check error branches** — `if let Err(e) = self.sandbox.check(...)` blocks
2. **Tree-sitter read error branches** — `Err(e)` in surgeon calls
3. **LSP error branches in BFS** — `Err(e)` in `call_hierarchy_incoming/outgoing`
4. **LSP error in analyze_impact prepare** — `Err(e)` in `call_hierarchy_prepare`
5. **Version hash computation** — `tokio::fs::read` + `VersionHash::compute` blocks
6. **Grep fallback path in analyze_impact** — the NoLspAvailable grep heuristic
7. **Degraded detection in search_codebase** — `SupportedLanguage::detect` returning None
8. **Helper functions** — `RipgrepScout::new()`, `render_recursive`, `filter_symbols`
9. **Tracing/logging statements** — `tracing::info!` / `tracing::warn!` in happy paths

---

## Assessment: Which Gaps Matter

### HIGH PRIORITY (real risk, must remediate)

| ID | Pattern | Files | Why it matters |
|----|---------|-------|----------------|
| CG-1 | Grep fallback in `analyze_impact` | navigation.rs L700-740 | Core degraded path. If LSP is down, this IS the path agents use. Untested = silent breakage. |
| CG-2 | Version hash computation | navigation.rs L755-775 | OCC edits depend on correct hashes. Wrong hash = rejected edits. Files must exist on disk for tests. |
| CG-3 | Sandbox check error branches | navigation.rs (3 sites), source_file.rs (1 site), search.rs (0 sites) | Security boundary. Must verify ACCESS_DENIED is returned correctly. |
| CG-4 | Tree-sitter error in `analyze_impact` | navigation.rs L608-618 | Error message must be meaningful for agents. |

### MEDIUM PRIORITY (worth testing, lower risk)

| ID | Pattern | Files | Why it matters |
|----|---------|-------|----------------|
| CG-5 | LSP error during BFS traversal | navigation.rs L546 | Partial graph returned. Must verify graceful degradation. |
| CG-6 | Degraded detection in `search_codebase` | search.rs L95-101 | Agents rely on `degraded` flag. Must verify flag is set correctly. |
| CG-7 | `RipgrepScout::new()` | ripgrep.rs | Trivial constructor, but 100% coverage is cheap. |

### LOW PRIORITY (tracing noise, safe to defer)

| ID | Pattern | Files | Why it's low |
|----|---------|-------|--------------|
| CG-8 | `tracing::info!` / `tracing::warn!` calls | All 4 files | Logging side effects. Not business logic. Covered by integration tests that don't collect coverage. |
| CG-9 | `render_recursive` | source_file.rs | Only called from `render_symbol_tree`, which IS tested. Coverage gap is likely due to conditional branches in recursion. |
| CG-10 | `filter_symbols` line-range filtering | source_file.rs | Pure function with existing unit tests. Gap is likely edge cases in the `start_line > 1` branch. |

---

## Remediation Instructions

### Task 1: Add test for `analyze_impact` grep fallback path

File: `crates/pathfinder/src/server/tools/navigation.rs`

The grep fallback path activates when:
1. `NoOpLawyer` is used (returns `NoLspAvailable`)
2. `MockScout` returns matches for the symbol name

Add this test to the `mod tests` block:

```
#[tokio::test]
async fn test_analyze_impact_grep_fallback_with_mock_scout() {
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(make_scope()));

    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Create a file so the version hash computation has something to read
    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(
        ws_dir.path().join("src/auth.rs"),
        "fn login() -> bool { true }",
    ).unwrap();

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
            version_hash: "sha256:abc".to_string(),
            known: Some(false),
        }],
        total_matches: 1,
        truncated: false,
    }));

    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        scout,
        surgeon,
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = AnalyzeImpactParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 2,
    };
    let result = server.analyze_impact_impl(params).await;
    let call_res = result.expect("should succeed");
    let val: crate::server::types::AnalyzeImpactMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    assert!(val.degraded);
    assert_eq!(
        val.degraded_reason.as_deref(),
        Some("no_lsp_grep_fallback")
    );
    let incoming = val.incoming.as_ref().expect("must be Some from grep");
    assert_eq!(incoming.len(), 1);
    assert_eq!(incoming[0].file, "src/auth.rs");
    assert_eq!(incoming[0].direction, "incoming_heuristic");
    // Version hashes should include the target file and the match file
    assert!(
        val.version_hashes.contains_key("src/auth.rs"),
        "version_hashes must include the referenced file"
    );
}
```

Verification: `cargo test -p pathfinder test_analyze_impact_grep_fallback_with_mock_scout`

---

### Task 2: Add test for sandbox check error in `analyze_impact`

File: `crates/pathfinder/src/server/tools/navigation.rs`

Add this test:

```
#[tokio::test]
async fn test_analyze_impact_rejects_sandbox_denied_path() {
    let surgeon = Arc::new(MockSurgeon::new());
    let lawyer = Arc::new(MockLawyer::default());
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = AnalyzeImpactParams {
        semantic_path: ".git/objects/abc::def".to_owned(),
        max_depth: 2,
    };
    let result = server.analyze_impact_impl(params).await;
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
```

Verification: `cargo test -p pathfinder test_analyze_impact_rejects_sandbox_denied_path`

---

### Task 3: Add test for sandbox check error in `read_source_file`

File: `crates/pathfinder/src/server/tools/source_file.rs`

This requires adding a `mod tests` block with server setup, or adding an
integration test. Since `read_source_file_impl` is `pub(crate)`, use an
integration test.

File: `crates/pathfinder/tests/test_source_file_sandbox.rs`

```
#![allow(clippy::unwrap_used)]
use pathfinder_common::config::PathfinderConfig;
use pathfinder_common::sandbox::Sandbox;
use pathfinder_common::types::WorkspaceRoot;
use pathfinder_search::MockScout;
use pathfinder_treesitter::mock::MockSurgeon;
use std::sync::Arc;
use tempfile::tempdir;

#[tokio::test]
async fn test_read_source_file_rejects_sandbox_denied_path() {
    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let server = pathfinder::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(MockSurgeon::default()),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = pathfinder::server::types::ReadSourceFileParams {
        filepath: ".git/HEAD".to_owned(),
        start_line: 1,
        end_line: None,
        detail_level: "full".to_owned(),
    };
    let result = server.read_source_file_impl(params).await;
    assert!(result.is_err(), "sandbox should deny .git paths");
    let err = result.unwrap_err();
    let code = err.data.as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(code, "ACCESS_DENIED");
}
```

Note: This test requires that `ReadSourceFileParams` and `read_source_file_impl`
are accessible from the integration test. Verify visibility. If `read_source_file_impl`
is `pub(crate)`, the test must live in `crates/pathfinder/src/server/tools/source_file.rs`
within `mod tests`.

Verification: `cargo test -p pathfinder test_read_source_file_rejects_sandbox_denied_path`

---

### Task 4: Add test for Tree-sitter error in `analyze_impact`

File: `crates/pathfinder/src/server/tools/navigation.rs`

```
#[tokio::test]
async fn test_analyze_impact_tree_sitter_error() {
    let surgeon = Arc::new(MockSurgeon::new());
    // Push an error result
    surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Err(pathfinder_treesitter::SurgeonError::ParseError(
            "parse failed".to_string(),
        )));

    let lawyer = Arc::new(MockLawyer::default());
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = AnalyzeImpactParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 2,
    };
    let result = server.analyze_impact_impl(params).await;
    assert!(result.is_err(), "tree-sitter error should propagate");
}
```

Note: Verify that `SurgeonError::ParseError` is the correct variant. Check the
actual error type used by `MockSurgeon` in the codebase.

Verification: `cargo test -p pathfinder test_analyze_impact_tree_sitter_error`

---

### Task 5: Add test for LSP error during BFS traversal

File: `crates/pathfinder/src/server/tools/navigation.rs`

```
#[tokio::test]
async fn test_analyze_impact_bfs_lsp_error_graceful_partial_graph() {
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
    lawyer.push_outgoing_call_result(Err("LSP crashed during outgoing".to_string()));

    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = AnalyzeImpactParams {
        semantic_path: "src/auth.rs::login".to_owned(),
        max_depth: 1,
    };
    let result = server.analyze_impact_impl(params).await;
    let call_res = result.expect("should succeed despite partial failure");
    let val: crate::server::types::AnalyzeImpactMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

    // Should NOT be degraded — prepare succeeded, incoming succeeded, only outgoing had error
    assert!(!val.degraded);
    let incoming = val.incoming.as_ref().expect("incoming must be Some");
    assert_eq!(incoming.len(), 1, "incoming caller should be present");
    let outgoing = val.outgoing.as_ref().expect("outgoing must be Some");
    assert!(outgoing.is_empty(), "outgoing should be empty due to error");
}
```

Verification: `cargo test -p pathfinder test_analyze_impact_bfs_lsp_error_graceful_partial_graph`

---

### Task 6: Add test for degraded flag in `search_codebase`

File: `crates/pathfinder/src/server/tools/search.rs`

Add to the existing `mod tests` block. This test verifies that when a match
is in a file with an unsupported language, `degraded: true` is returned.

```
#[tokio::test]
async fn test_search_codebase_degraded_on_unsupported_language() {
    let ws_dir = tempfile::tempdir().unwrap();
    let ws = pathfinder_common::types::WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = pathfinder_common::config::PathfinderConfig::default();
    let sandbox = pathfinder_common::sandbox::Sandbox::new(ws.path(), &config.sandbox);

    // Create a file with an unsupported extension
    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(ws_dir.path().join("src/data.xyz"), "findme content").unwrap();

    // Use real RipgrepScout so it actually searches the filesystem
    let scout = Arc::new(pathfinder_search::RipgrepScout::new());
    let surgeon = Arc::new(pathfinder_treesitter::mock::MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);

    let server = crate::server::PathfinderServer::with_all_engines(
        ws, config, sandbox, scout, surgeon, lawyer,
    );

    let params = SearchCodebaseParams {
        query: "findme".to_owned(),
        is_regex: false,
        path_glob: "**/*.xyz".to_owned(),
        exclude_glob: String::default(),
        max_results: 10,
        context_lines: 0,
        known_files: vec![],
        group_by_file: false,
        filter_mode: pathfinder_common::types::FilterMode::default(),
    };
    let result = server.search_codebase_impl(params).await;
    let response = result.expect("search should succeed");
    assert!(response.degraded, "should be degraded for unsupported language");
    assert_eq!(
        response.degraded_reason.as_deref(),
        Some("unsupported_language")
    );
}
```

Note: Requires verifying that `SearchCodebaseParams` has these exact fields and
defaults. Adjust field names if they differ.

Verification: `cargo test -p pathfinder test_search_codebase_degraded_on_unsupported_language`

---

### Task 7: Add trivial test for `RipgrepScout::new()`

File: `crates/pathfinder-search/src/ripgrep.rs`

```
#[test]
fn test_ripgrep_scout_new() {
    let _scout = RipgrepScout::new();
}
```

Add to the existing `mod tests` block.

Verification: `cargo test -p pathfinder-search test_ripgrep_scout_new`

---

## Execution Order

1. BUG-001 (already fixed) — verify with `cargo test --workspace`
2. Task 2 (sandbox check in analyze_impact) — quick, no external deps
3. Task 4 (tree-sitter error in analyze_impact) — quick, uses existing mocks
4. Task 5 (BFS LSP error) — uses existing mock infrastructure
5. Task 1 (grep fallback) — needs MockScout setup, most complex new test
6. Task 6 (search_codebase degraded) — needs real filesystem + RipgrepScout
7. Task 3 (read_source_file sandbox) — may need visibility adjustments
8. Task 7 (RipgrepScout::new) — trivial, do last

After all tasks: `cargo test --workspace` then `cargo llvm-cov` to verify
coverage improvement.

---

## Patterns NOT Worth Testing

These are tracing/log statements. They execute in every test that hits the
surrounding code but don't contribute to coverage counts because coverage
tools track line-level, not statement-level:

- All `tracing::info!()` calls
- All `tracing::warn!()` calls
- `render_recursive` — tested via `test_render_symbol_tree_nested`
- `filter_symbols` when called with `start_line > 1` — tested via
  `test_filter_symbols`

Adding tests solely to cover log statements is wasteful. If you want coverage
numbers to improve for these, the correct approach is to ensure existing tests
exercise the code paths that CONTAIN the log statements, not to test the logs
themselves.

---

## Estimated Coverage Impact

| Task | New lines covered | Risk reduced |
|------|-------------------|-------------|
| Task 1 | ~25 lines | High — core degraded path |
| Task 2 | ~8 lines | High — security boundary |
| Task 3 | ~8 lines | High — security boundary |
| Task 4 | ~8 lines | Medium — error propagation |
| Task 5 | ~10 lines | Medium — partial failure handling |
| Task 6 | ~10 lines | Medium — feature flag correctness |
| Task 7 | ~3 lines | Low — trivial |
| **Total** | **~72 lines** | |

Remaining ~343 uncovered lines are predominantly tracing statements that are
already exercised by existing tests at the line level but not counted by the
coverage tool due to how Rust's coverage instrumentation works with macro
expansions.
