#![allow(clippy::unwrap_used, clippy::expect_used, clippy::needless_return)]
use crate::server::types::ReplaceFullParams;
use crate::server::PathfinderServer;

use super::helpers::UnsupportedDiagLawyer;
use pathfinder_common::config::PathfinderConfig;
use pathfinder_common::sandbox::Sandbox;
use pathfinder_common::types::{VersionHash, WorkspaceRoot};
use pathfinder_search::MockScout;
use pathfinder_treesitter::mock::MockSurgeon;
use rmcp::handler::server::wrapper::Parameters;
use std::sync::Arc;
use tempfile::tempdir;

fn make_server_with_lawyer(
    ws_dir: &tempfile::TempDir,
    mock_surgeon: MockSurgeon,
    mock_lawyer: pathfinder_lsp::MockLawyer,
) -> PathfinderServer {
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(mock_surgeon),
        Arc::new(mock_lawyer),
    )
}

/// Helper: write a tiny Go source file and build a `MockSurgeon` whose
/// `resolve_full_range` returns a range covering the whole file.
fn setup_full_replace_fixture(
    ws_dir: &tempfile::TempDir,
    filepath: &str,
    src: &str,
) -> (MockSurgeon, VersionHash) {
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, src).unwrap();

    let src_bytes = src.as_bytes();
    let hash = VersionHash::compute(src_bytes);

    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .resolve_full_range_results
        .lock()
        .unwrap()
        .push(Ok((
            pathfinder_treesitter::surgeon::FullRange {
                start_byte: 0,
                end_byte: src_bytes.len(),
                indent_column: 0,
            },
            std::sync::Arc::from(src_bytes),
            hash.clone(),
        )));

    (mock_surgeon, hash)
}

// ── no_lsp: did_open returns NoLspAvailable → validation skipped ────

#[tokio::test]
async fn test_run_lsp_validation_no_lsp() {
    let ws_dir = tempdir().expect("temp dir");
    let filepath = "src/auth.go";
    let src = "func Login() {}";
    let (mock_surgeon, hash) = setup_full_replace_fixture(&ws_dir, filepath, src);

    let mock_lawyer = pathfinder_lsp::MockLawyer::default();
    mock_lawyer.set_did_open_error(pathfinder_lsp::LspError::NoLspAvailable);

    let server = make_server_with_lawyer(&ws_dir, mock_surgeon, mock_lawyer);

    let params = ReplaceFullParams {
        semantic_path: format!("{filepath}::Login"),
        base_version: hash.as_str().to_owned(),
        new_code: "func Login() { return }\n".to_owned(),
        ignore_validation_failures: false,
    };
    let result = server
        .replace_full(Parameters(params))
        .await
        .expect("should succeed — no_lsp gracefully degrades");
    let resp = result.0;

    assert!(resp.success);
    assert_eq!(resp.validation.status, "skipped");
    assert!(resp.validation_skipped);
    assert_eq!(resp.validation_skipped_reason.as_deref(), Some("no_lsp"));
}

// ── unsupported: did_open returns UnsupportedCapability → skipped ───

#[tokio::test]
async fn test_run_lsp_validation_unsupported() {
    let ws_dir = tempdir().expect("temp dir");
    let filepath = "src/auth.go";
    let src = "func Login() {}";
    let (mock_surgeon, hash) = setup_full_replace_fixture(&ws_dir, filepath, src);

    let mock_lawyer = pathfinder_lsp::MockLawyer::default();
    mock_lawyer.set_did_open_error(pathfinder_lsp::LspError::UnsupportedCapability {
        capability: "textDocument/diagnostic".to_owned(),
    });

    let server = make_server_with_lawyer(&ws_dir, mock_surgeon, mock_lawyer);

    let params = ReplaceFullParams {
        semantic_path: format!("{filepath}::Login"),
        base_version: hash.as_str().to_owned(),
        new_code: "func Login() { return }\n".to_owned(),
        ignore_validation_failures: false,
    };
    let result = server
        .replace_full(Parameters(params))
        .await
        .expect("should succeed — unsupported gracefully degrades");
    let resp = result.0;

    assert_eq!(resp.validation.status, "skipped");
    assert!(resp.validation_skipped);
    assert_eq!(
        resp.validation_skipped_reason.as_deref(),
        Some("pull_diagnostics_unsupported")
    );
}

// ── pre_diag_timeout: first pull_diagnostics errors → skipped ───────

#[tokio::test]
async fn test_run_lsp_validation_pre_diag_timeout() {
    let ws_dir = tempdir().expect("temp dir");
    let filepath = "src/auth.go";
    let src = "func Login() {}";
    let (mock_surgeon, hash) = setup_full_replace_fixture(&ws_dir, filepath, src);

    let mock_lawyer = pathfinder_lsp::MockLawyer::default();
    // did_open succeeds (default); first pull_diagnostics returns a protocol
    // error — any error that is not UnsupportedCapability maps to
    // "diagnostic_timeout" in run_lsp_validation.
    mock_lawyer.push_pull_diagnostics_result(Err("LSP timed out".to_owned()));

    let server = make_server_with_lawyer(&ws_dir, mock_surgeon, mock_lawyer);

    let params = ReplaceFullParams {
        semantic_path: format!("{filepath}::Login"),
        base_version: hash.as_str().to_owned(),
        new_code: "func Login() { return }\n".to_owned(),
        ignore_validation_failures: false,
    };
    let result = server
        .replace_full(Parameters(params))
        .await
        .expect("should succeed — pre-diag timeout gracefully degrades");
    let resp = result.0;

    assert!(resp.success);
    assert_eq!(resp.validation.status, "skipped");
    assert!(resp.validation_skipped);
    assert_eq!(
        resp.validation_skipped_reason.as_deref(),
        Some("lsp_protocol_error")
    );
}

// ── pre_diag_unsupported: first pull_diagnostics → UnsupportedCapability
//    → skipped with "pull_diagnostics_unsupported" reason ────────────────

#[tokio::test]
async fn test_run_lsp_validation_pull_diagnostics_unsupported() {
    let ws_dir = tempdir().expect("temp dir");
    let filepath = "src/auth.go";
    let src = "func Login() {}";
    // `mock_surgeon` is used in the first call but we need a fresh surgeon
    // for the second server construction; discard the first fixture result.
    let (_mock_surgeon, hash) = setup_full_replace_fixture(&ws_dir, filepath, src);

    // UnsupportedDiagLawyer always returns UnsupportedCapability from
    // pull_diagnostics, exercising the "pull_diagnostics_unsupported" branch.
    let (mock_surgeon_2, _) = setup_full_replace_fixture(&ws_dir, filepath, src);
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(mock_surgeon_2),
        Arc::new(UnsupportedDiagLawyer),
    );

    let params = ReplaceFullParams {
        semantic_path: format!("{filepath}::Login"),
        base_version: hash.as_str().to_owned(),
        new_code: "func Login() { return }\n".to_owned(),
        ignore_validation_failures: false,
    };
    let result = server
        .replace_full(Parameters(params))
        .await
        .expect("should succeed — pull_diagnostics_unsupported degrades");
    let resp = result.0;

    assert_eq!(resp.validation.status, "skipped");
    assert_eq!(
        resp.validation_skipped_reason.as_deref(),
        Some("pull_diagnostics_unsupported")
    );
}

// ── post_diag_timeout: second pull_diagnostics errors → skipped ──────

#[tokio::test]
async fn test_run_lsp_validation_post_diag_timeout() {
    let ws_dir = tempdir().expect("temp dir");
    let filepath = "src/auth.go";
    let src = "func Login() {}";
    let (mock_surgeon, hash) = setup_full_replace_fixture(&ws_dir, filepath, src);

    let mock_lawyer = pathfinder_lsp::MockLawyer::default();
    // Pre-edit pull_diagnostics succeeds with empty diags.
    mock_lawyer.push_pull_diagnostics_result(Ok(vec![]));
    // Post-edit pull_diagnostics errors (e.g. timeout).
    mock_lawyer.push_pull_diagnostics_result(Err("timeout after 10s".to_owned()));

    let server = make_server_with_lawyer(&ws_dir, mock_surgeon, mock_lawyer);

    let params = ReplaceFullParams {
        semantic_path: format!("{filepath}::Login"),
        base_version: hash.as_str().to_owned(),
        new_code: "func Login() { return }\n".to_owned(),
        ignore_validation_failures: false,
    };
    let result = server
        .replace_full(Parameters(params))
        .await
        .expect("should succeed — post-diag timeout gracefully degrades");
    let resp = result.0;

    assert!(resp.success);
    assert_eq!(resp.validation.status, "skipped");
    assert!(resp.validation_skipped);
    assert_eq!(
        resp.validation_skipped_reason.as_deref(),
        Some("lsp_protocol_error")
    );
}

// ── blocking: new errors introduced + ignore_validation_failures=false ─

#[tokio::test]
async fn test_run_lsp_validation_blocking() {
    use pathfinder_lsp::types::{LspDiagnostic, LspDiagnosticSeverity};

    let ws_dir = tempdir().expect("temp dir");
    let filepath = "src/auth.go";
    let src = "func Login() {}";
    let (mock_surgeon, hash) = setup_full_replace_fixture(&ws_dir, filepath, src);

    let mock_lawyer = pathfinder_lsp::MockLawyer::default();
    // Pre-edit: no errors.
    mock_lawyer.push_pull_diagnostics_result(Ok(vec![]));
    // Post-edit: one new error introduced.
    mock_lawyer.push_pull_diagnostics_result(Ok(vec![LspDiagnostic {
        severity: LspDiagnosticSeverity::Error,
        code: Some("E001".into()),
        message: "undefined: Foo".into(),
        file: filepath.to_owned(),
        start_line: 1,
        end_line: 1,
    }]));

    let server = make_server_with_lawyer(&ws_dir, mock_surgeon, mock_lawyer);

    // ignore_validation_failures = false → should block
    let params = ReplaceFullParams {
        semantic_path: format!("{filepath}::Login"),
        base_version: hash.as_str().to_owned(),
        new_code: "func Login() { Foo() }\n".to_owned(),
        ignore_validation_failures: false,
    };
    let result = server.replace_full(Parameters(params)).await;

    let Err(err) = result else {
        panic!("expected VALIDATION_FAILED error when new errors are introduced");
    };
    let code = err
        .data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(code, "VALIDATION_FAILED", "got: {err:?}");
    // Confirm the introduced error is surfaced (nested under details.introduced_errors
    // because pathfinder_to_error_data serializes ErrorResponse which has a `details` field)
    let introduced = err
        .data
        .as_ref()
        .and_then(|d| d.get("details"))
        .and_then(|d| d.get("introduced_errors"))
        .and_then(|v| v.as_array())
        .map_or(0, Vec::len);
    assert_eq!(
        introduced, 1,
        "one new error should appear in introduced_errors"
    );
}

// ── workspace blocking: new errors in other files block the edit ────────

#[tokio::test]
async fn test_run_lsp_validation_workspace_blocking() {
    use pathfinder_lsp::types::{LspDiagnostic, LspDiagnosticSeverity};

    let ws_dir = tempdir().expect("temp dir");
    let filepath = "src/auth.go";
    let src = "func Login() {}";
    let (mock_surgeon, hash) = setup_full_replace_fixture(&ws_dir, filepath, src);

    let mock_lawyer = pathfinder_lsp::MockLawyer::default();
    // Pre-edit diagnostics (file + workspace)
    mock_lawyer.push_pull_diagnostics_result(Ok(vec![]));
    mock_lawyer.push_pull_workspace_diagnostics_result(Ok(vec![]));

    // Post-edit diagnostics (no errors in single file, but 1 error in workspace)
    mock_lawyer.push_pull_diagnostics_result(Ok(vec![]));
    mock_lawyer.push_pull_workspace_diagnostics_result(Ok(vec![LspDiagnostic {
        severity: LspDiagnosticSeverity::Error,
        code: Some("E002".into()),
        message: "cannot call Login with 1 argument".into(),
        file: "src/main.go".to_owned(), // Different file!
        start_line: 5,
        end_line: 5,
    }]));

    let server = make_server_with_lawyer(&ws_dir, mock_surgeon, mock_lawyer);

    // ignore_validation_failures = false → should block
    let params = ReplaceFullParams {
        semantic_path: format!("{filepath}::Login"),
        base_version: hash.as_str().to_owned(),
        new_code: "func Login(a string) { }\n".to_owned(), // changed signature
        ignore_validation_failures: false,
    };
    let result = server.replace_full(Parameters(params)).await;

    let Err(err) = result else {
        panic!("expected VALIDATION_FAILED error when workspace errors are introduced");
    };
    let code = err
        .data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(code, "VALIDATION_FAILED", "got: {err:?}");

    // Confirm the workspace error is reported
    let introduced = err
        .data
        .as_ref()
        .and_then(|d| d.get("details"))
        .and_then(|d| d.get("introduced_errors"))
        .and_then(|v| v.as_array())
        .expect("should have introduced_errors");
    assert_eq!(
        introduced.len(),
        1,
        "one workspace error should appear in introduced_errors"
    );
    let first_err_file = introduced[0].get("file").and_then(|v| v.as_str()).unwrap();
    assert_eq!(
        first_err_file, "src/main.go",
        "error should be in src/main.go"
    );
}

// ── blocking_ignored: new errors + ignore_validation_failures=true → passes

#[tokio::test]
async fn test_run_lsp_validation_blocking_ignored() {
    use pathfinder_lsp::types::{LspDiagnostic, LspDiagnosticSeverity};

    let ws_dir = tempdir().expect("temp dir");
    let filepath = "src/auth.go";
    let src = "func Login() {}";
    let (mock_surgeon, hash) = setup_full_replace_fixture(&ws_dir, filepath, src);

    let mock_lawyer = pathfinder_lsp::MockLawyer::default();
    mock_lawyer.push_pull_diagnostics_result(Ok(vec![]));
    mock_lawyer.push_pull_diagnostics_result(Ok(vec![LspDiagnostic {
        severity: LspDiagnosticSeverity::Error,
        code: Some("E001".into()),
        message: "undefined: Foo".into(),
        file: filepath.to_owned(),
        start_line: 1,
        end_line: 1,
    }]));

    let server = make_server_with_lawyer(&ws_dir, mock_surgeon, mock_lawyer);

    // ignore_validation_failures = true → should NOT block, file is written
    let params = ReplaceFullParams {
        semantic_path: format!("{filepath}::Login"),
        base_version: hash.as_str().to_owned(),
        new_code: "func Login() { Foo() }\n".to_owned(),
        ignore_validation_failures: true,
    };
    let _result = server
        .replace_full(Parameters(params))
        .await
        .expect("should succeed when ignore_validation_failures=true");
    let filepath = "src/auth.go";
    let src = "func Login() {}";
    let (mock_surgeon, hash) = setup_full_replace_fixture(&ws_dir, filepath, src);

    let mock_lawyer = pathfinder_lsp::MockLawyer::default();
    // One pre-existing warning (non-error) in both pre and post.
    let existing_warning = LspDiagnostic {
        severity: LspDiagnosticSeverity::Warning,
        code: Some("W001".into()),
        message: "unused import".into(),
        file: filepath.to_owned(),
        start_line: 1,
        end_line: 1,
    };
    mock_lawyer.push_pull_diagnostics_result(Ok(vec![existing_warning.clone()]));
    mock_lawyer.push_pull_diagnostics_result(Ok(vec![existing_warning]));

    let server = make_server_with_lawyer(&ws_dir, mock_surgeon, mock_lawyer);

    let params = ReplaceFullParams {
        semantic_path: format!("{filepath}::Login"),
        base_version: hash.as_str().to_owned(),
        new_code: "func Login() { return }\n".to_owned(),
        ignore_validation_failures: false,
    };
    let result = server
        .replace_full(Parameters(params))
        .await
        .expect("should succeed — no new errors");
    let resp = result.0;

    assert!(resp.success);
    assert_eq!(resp.validation.status, "passed");
    assert!(!resp.validation_skipped);
    assert!(resp.validation.introduced_errors.is_empty());
    assert!(resp.validation.resolved_errors.is_empty());
}

// ── empty_diagnostics_both_snapshots: warmup signal ─────────────────────────

#[test]
fn test_build_validation_outcome_empty_snapshots_signals_warmup() {
    // When both pre and post diagnostic snapshots are empty, the validation
    // outcome must be skipped with reason "empty_diagnostics_both_snapshots".
    // This prevents agents from trusting a vacuously-clean pass during LSP warmup.
    use crate::server::tools::edit::text_edit::build_validation_outcome;
    use std::path::Path;

    let outcome = build_validation_outcome(
        &[], // pre_diags: empty (LSP warmup or genuinely clean)
        &[], // post_diags: empty
        false,
        Path::new("src/lib.rs"),
    );

    assert!(
        outcome.skipped,
        "validation_skipped must be true when both snapshots are empty"
    );
    assert_eq!(
        outcome.skipped_reason.as_deref(),
        Some("empty_diagnostics_both_snapshots"),
        "skipped_reason must identify the warmup signal"
    );
    assert_eq!(
        outcome.validation.status, "uncertain",
        "status must be 'uncertain' when both snapshots are empty (LSP may be warming up)"
    );
    assert!(
        !outcome.should_block,
        "should_block must be false — empty snapshots are never a blocker"
    );
}

#[test]
fn test_build_validation_outcome_non_empty_pre_does_not_skip() {
    // If pre_diags has errors but post is empty (errors resolved),
    // we must NOT trigger the warmup-skip path — the diff is meaningful.
    use crate::server::tools::edit::text_edit::build_validation_outcome;
    use pathfinder_lsp::types::{LspDiagnostic, LspDiagnosticSeverity};
    use std::path::Path;

    let pre = vec![LspDiagnostic {
        severity: LspDiagnosticSeverity::Error,
        code: None,
        message: "pre-existing error".to_owned(),
        file: "src/lib.rs".to_owned(),
        start_line: 1,
        end_line: 1,
    }];

    let outcome = build_validation_outcome(&pre, &[], false, Path::new("src/lib.rs"));

    // pre non-empty → NOT the warmup-skip path
    assert!(
        !outcome.skipped,
        "must not skip when pre_diags is non-empty (diff is meaningful)"
    );
    assert_eq!(outcome.validation.status, "passed");
    assert!(!outcome.should_block);
}

// ── Push diagnostics tests (PATCH-002) ──────────────────────────────
//
// These tests verify the push diagnostics path in run_lsp_validation.
// The push path is triggered when capability_status reports diagnostics_strategy
// as "push" for the file's language. This is the path used by gopls and
// typescript-language-server (they don't support pull diagnostics).
//
// Mock setup: set_capability_status with diagnostics_strategy: Some("push")
// queues results via push_pull_diagnostics_result (shared queue with pull).

// ── push_validation_no_errors: pre and post both empty → passes ────

#[tokio::test]
async fn test_push_validation_no_errors() {
    let ws_dir = tempdir().expect("temp dir");
    let filepath = "src/auth.go";
    let src = "func Login() {}";
    let (mock_surgeon, hash) = setup_full_replace_fixture(&ws_dir, filepath, src);

    let mock_lawyer = pathfinder_lsp::MockLawyer::default();

    // Configure capability_status to report push diagnostics for Go
    mock_lawyer.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "gopls connected (push diagnostics)".to_string(),
            indexing_complete: Some(true),
            uptime_seconds: Some(30),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
        },
    )]));

    // Pre-edit: no errors (collect_diagnostics call 1)
    mock_lawyer.push_pull_diagnostics_result(Ok(vec![]));
    // Post-edit: no errors (collect_diagnostics call 2)
    mock_lawyer.push_pull_diagnostics_result(Ok(vec![]));

    let server = make_server_with_lawyer(&ws_dir, mock_surgeon, mock_lawyer);

    let params = ReplaceFullParams {
        semantic_path: format!("{filepath}::Login"),
        base_version: hash.as_str().to_owned(),
        new_code: "func Login() { return }\n".to_owned(),
        ignore_validation_failures: false,
    };
    let result = server
        .replace_full(Parameters(params))
        .await
        .expect("should succeed — push validation with no errors");
    let resp = result.0;

    assert!(
        resp.success,
        "edit should succeed when push validation finds no new errors"
    );
    assert_eq!(
        resp.validation.status, "uncertain",
        "push validation with empty pre and post should be 'uncertain' (warmup signal)"
    );
    assert!(
        resp.validation_skipped,
        "empty push snapshots should trigger skip"
    );
    assert_eq!(
        resp.validation_skipped_reason.as_deref(),
        Some("empty_diagnostics_both_snapshots"),
        "skip reason should indicate empty snapshots"
    );
}

// ── push_validation_clean_pass: pre and post both non-empty, no new errors → passes ──

#[tokio::test]
async fn test_push_validation_clean_pass() {
    use pathfinder_lsp::types::{LspDiagnostic, LspDiagnosticSeverity};

    let ws_dir = tempdir().expect("temp dir");
    let filepath = "src/auth.go";
    let src = "func Login() {}";
    let (mock_surgeon, hash) = setup_full_replace_fixture(&ws_dir, filepath, src);

    let mock_lawyer = pathfinder_lsp::MockLawyer::default();

    mock_lawyer.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "gopls connected (push diagnostics)".to_string(),
            indexing_complete: Some(true),
            uptime_seconds: Some(30),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
        },
    )]));

    // Same pre-existing warning in both snapshots → no NEW errors
    let existing_warning = LspDiagnostic {
        severity: LspDiagnosticSeverity::Warning,
        code: Some("W001".into()),
        message: "unused variable".into(),
        file: filepath.to_owned(),
        start_line: 1,
        end_line: 1,
    };
    mock_lawyer.push_pull_diagnostics_result(Ok(vec![existing_warning.clone()]));
    mock_lawyer.push_pull_diagnostics_result(Ok(vec![existing_warning]));

    let server = make_server_with_lawyer(&ws_dir, mock_surgeon, mock_lawyer);

    let params = ReplaceFullParams {
        semantic_path: format!("{filepath}::Login"),
        base_version: hash.as_str().to_owned(),
        new_code: "func Login() { return }\n".to_owned(),
        ignore_validation_failures: false,
    };
    let result = server
        .replace_full(Parameters(params))
        .await
        .expect("should succeed — push validation with no new errors");
    let resp = result.0;

    assert!(resp.success);
    assert_eq!(resp.validation.status, "passed");
    assert!(!resp.validation_skipped);
    assert!(resp.validation.introduced_errors.is_empty());
    assert!(resp.validation.resolved_errors.is_empty());
}

// ── push_validation_introduced_error: post has new error → blocks edit ──

#[tokio::test]
async fn test_push_validation_introduced_error() {
    use pathfinder_lsp::types::{LspDiagnostic, LspDiagnosticSeverity};

    let ws_dir = tempdir().expect("temp dir");
    let filepath = "src/auth.go";
    let src = "func Login() {}";
    let (mock_surgeon, hash) = setup_full_replace_fixture(&ws_dir, filepath, src);

    let mock_lawyer = pathfinder_lsp::MockLawyer::default();

    mock_lawyer.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "gopls connected (push diagnostics)".to_string(),
            indexing_complete: Some(true),
            uptime_seconds: Some(30),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
        },
    )]));

    // Pre-edit: no errors
    mock_lawyer.push_pull_diagnostics_result(Ok(vec![]));
    // Post-edit: one new error introduced
    mock_lawyer.push_pull_diagnostics_result(Ok(vec![LspDiagnostic {
        severity: LspDiagnosticSeverity::Error,
        code: Some("E001".into()),
        message: "undefined: Foo".into(),
        file: filepath.to_owned(),
        start_line: 1,
        end_line: 1,
    }]));

    let server = make_server_with_lawyer(&ws_dir, mock_surgeon, mock_lawyer);

    let params = ReplaceFullParams {
        semantic_path: format!("{filepath}::Login"),
        base_version: hash.as_str().to_owned(),
        new_code: "func Login() { Foo() }\n".to_owned(),
        ignore_validation_failures: false,
    };
    let result = server.replace_full(Parameters(params)).await;

    let Err(err) = result else {
        panic!("expected VALIDATION_FAILED error when push diagnostics finds new errors");
    };
    let code = err
        .data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(code, "VALIDATION_FAILED", "got: {err:?}");

    let introduced = err
        .data
        .as_ref()
        .and_then(|d| d.get("details"))
        .and_then(|d| d.get("introduced_errors"))
        .and_then(|v| v.as_array())
        .map_or(0, Vec::len);
    assert_eq!(
        introduced, 1,
        "one new error should appear in introduced_errors from push path"
    );
}

// ── push_validation_pre_fails: pre-edit collect_diagnostics errors → skipped ──

#[tokio::test]
async fn test_push_validation_pre_fails() {
    let ws_dir = tempdir().expect("temp dir");
    let filepath = "src/auth.go";
    let src = "func Login() {}";
    let (mock_surgeon, hash) = setup_full_replace_fixture(&ws_dir, filepath, src);

    let mock_lawyer = pathfinder_lsp::MockLawyer::default();

    mock_lawyer.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "gopls connected (push diagnostics)".to_string(),
            indexing_complete: Some(true),
            uptime_seconds: Some(30),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
        },
    )]));

    // Pre-edit collect_diagnostics fails (e.g., LSP timeout)
    mock_lawyer.push_pull_diagnostics_result(Err("push collection timed out".to_owned()));

    let server = make_server_with_lawyer(&ws_dir, mock_surgeon, mock_lawyer);

    let params = ReplaceFullParams {
        semantic_path: format!("{filepath}::Login"),
        base_version: hash.as_str().to_owned(),
        new_code: "func Login() { return }\n".to_owned(),
        ignore_validation_failures: false,
    };
    let result = server
        .replace_full(Parameters(params))
        .await
        .expect("should succeed — push pre-diag failure gracefully degrades");
    let resp = result.0;

    assert!(resp.success, "edit should succeed despite pre-diag failure");
    assert_eq!(resp.validation.status, "skipped");
    assert!(resp.validation_skipped);
    assert_eq!(
        resp.validation_skipped_reason.as_deref(),
        Some("lsp_protocol_error"),
        "push pre-diag failure should map to lsp_protocol_error"
    );
}

// ── push_validation_post_fails: post-edit collect_diagnostics errors → skipped ──

#[tokio::test]
async fn test_push_validation_post_fails() {
    let ws_dir = tempdir().expect("temp dir");
    let filepath = "src/auth.go";
    let src = "func Login() {}";
    let (mock_surgeon, hash) = setup_full_replace_fixture(&ws_dir, filepath, src);

    let mock_lawyer = pathfinder_lsp::MockLawyer::default();

    mock_lawyer.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "gopls connected (push diagnostics)".to_string(),
            indexing_complete: Some(true),
            uptime_seconds: Some(30),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
        },
    )]));

    // Pre-edit: succeeds with empty diags
    mock_lawyer.push_pull_diagnostics_result(Ok(vec![]));
    // Post-edit: fails (e.g., LSP crashed mid-collection)
    mock_lawyer
        .push_pull_diagnostics_result(Err("connection lost during push collection".to_owned()));

    let server = make_server_with_lawyer(&ws_dir, mock_surgeon, mock_lawyer);

    let params = ReplaceFullParams {
        semantic_path: format!("{filepath}::Login"),
        base_version: hash.as_str().to_owned(),
        new_code: "func Login() { return }\n".to_owned(),
        ignore_validation_failures: false,
    };
    let result = server
        .replace_full(Parameters(params))
        .await
        .expect("should succeed — push post-diag failure gracefully degrades");
    let resp = result.0;

    assert!(
        resp.success,
        "edit should succeed despite post-diag failure"
    );
    assert_eq!(resp.validation.status, "skipped");
    assert!(resp.validation_skipped);
    assert_eq!(
        resp.validation_skipped_reason.as_deref(),
        Some("lsp_protocol_error"),
        "push post-diag failure should map to lsp_protocol_error"
    );
}

// ── lsp_error_to_skip_reason: pure function, all variants tested ──────
//
// These tests verify that every LspError variant maps to the correct
// skip reason string. This ensures complete coverage of the match
// statement in lsp_error_to_skip_reason.

#[test]
fn test_lsp_error_to_skip_reason_no_lsp() {
    use crate::server::PathfinderServer;
    use pathfinder_lsp::LspError;

    let err = LspError::NoLspAvailable;
    let reason = PathfinderServer::lsp_error_to_skip_reason(&err);
    assert_eq!(reason, "no_lsp");
}

#[test]
fn test_lsp_error_to_skip_reason_timeout() {
    use crate::server::PathfinderServer;
    use pathfinder_lsp::LspError;

    let err = LspError::Timeout {
        operation: "textDocument/definition".to_owned(),
        timeout_ms: 10_000,
    };
    let reason = PathfinderServer::lsp_error_to_skip_reason(&err);
    assert_eq!(reason, "lsp_timeout");
}

#[test]
fn test_lsp_error_to_skip_reason_protocol() {
    use crate::server::PathfinderServer;
    use pathfinder_lsp::LspError;

    let err = LspError::Protocol("malformed JSON response".to_owned());
    let reason = PathfinderServer::lsp_error_to_skip_reason(&err);
    assert_eq!(reason, "lsp_protocol_error");
}

#[test]
fn test_lsp_error_to_skip_reason_connection_lost() {
    use crate::server::PathfinderServer;
    use pathfinder_lsp::LspError;

    let err = LspError::ConnectionLost;
    let reason = PathfinderServer::lsp_error_to_skip_reason(&err);
    assert_eq!(reason, "lsp_crash");
}

#[test]
fn test_lsp_error_to_skip_reason_unsupported_capability() {
    use crate::server::PathfinderServer;
    use pathfinder_lsp::LspError;

    let err = LspError::UnsupportedCapability {
        capability: "diagnosticProvider".to_owned(),
    };
    let reason = PathfinderServer::lsp_error_to_skip_reason(&err);
    assert_eq!(reason, "pull_diagnostics_unsupported");
}

#[test]
fn test_lsp_error_to_skip_reason_io_not_found() {
    use crate::server::PathfinderServer;
    use pathfinder_lsp::LspError;
    use std::io::{Error, ErrorKind};

    // Io(NotFound) maps to "lsp_not_on_path" - this is the case where
    // the LSP binary is not installed or not in PATH.
    let io_err = Error::new(ErrorKind::NotFound, "No such file or directory");
    let err = LspError::Io(io_err);
    let reason = PathfinderServer::lsp_error_to_skip_reason(&err);
    assert_eq!(reason, "lsp_not_on_path");
}

#[test]
fn test_lsp_error_to_skip_reason_io_other_kinds() {
    use crate::server::PathfinderServer;
    use pathfinder_lsp::LspError;
    use std::io::{Error, ErrorKind};

    // All non-NotFound Io errors map to "lsp_start_failed".
    // Test several common error kinds.
    for (kind, name) in [
        (ErrorKind::PermissionDenied, "PermissionDenied"),
        (ErrorKind::ConnectionRefused, "ConnectionRefused"),
        (ErrorKind::BrokenPipe, "BrokenPipe"),
        (ErrorKind::TimedOut, "TimedOut"),
        (ErrorKind::Other, "Other"),
    ] {
        let io_err = Error::new(kind, format!("{name} error"));
        let err = LspError::Io(io_err);
        let reason = PathfinderServer::lsp_error_to_skip_reason(&err);
        assert_eq!(
            reason, "lsp_start_failed",
            "ErrorKind::{name} should map to 'lsp_start_failed'"
        );
    }
}
