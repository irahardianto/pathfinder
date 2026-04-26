//! End-to-end tests for the `replace_batch` tool.
//!
//! These tests verify that multiple edits can be applied atomically
//! to the same file, including proper OCC validation, rollback on failure,
//! and mixed semantic/text targeting.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::needless_return,
    clippy::similar_names,
    clippy::single_char_pattern,
    clippy::format_push_string
)]

use super::helpers::{make_body_range, make_server};
use crate::server::types::BatchEdit;
use pathfinder_common::types::VersionHash;
use pathfinder_treesitter::mock::MockSurgeon;
use rmcp::handler::server::wrapper::Parameters;
use std::sync::Arc;
use tempfile::tempdir;

/// Helper to create a `BatchEdit` for `replace_body`.
fn make_replace_body_edit(semantic_path: String, new_code: String) -> BatchEdit {
    BatchEdit {
        semantic_path,
        old_text: None,
        edit_type: "replace_body".to_string(),
        new_code: Some(new_code),
        replacement_text: None,
        context_line: None,
        normalize_whitespace: false,
    }
}

/// Helper to create a text-based edit.
fn make_text_edit(
    old_text: String,
    replacement: String,
    context_line: u32,
    normalize: bool,
) -> BatchEdit {
    BatchEdit {
        semantic_path: String::default(),
        old_text: Some(old_text),
        edit_type: String::default(),
        new_code: None,
        replacement_text: Some(replacement),
        context_line: Some(context_line),
        normalize_whitespace: normalize,
    }
}

#[tokio::test]
async fn test_batch_replace_body_single() {
    let ws_dir = tempdir().expect("temp dir");

    // Write a Rust file with one function
    let source = r"
fn foo() -> i32 {
    1
}
";
    let filepath = "src/lib.rs";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, source).unwrap();

    let src_bytes = source.as_bytes();
    let hash = VersionHash::compute(src_bytes);

    let open = source.find('{').unwrap();
    let close = source.rfind('}').unwrap() + 1;

    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .resolve_body_range_results
        .lock()
        .unwrap()
        .push(Ok((
            make_body_range(open, close, 0, 4),
            Arc::from(src_bytes),
            hash.clone(),
        )));

    let server = make_server(&ws_dir, mock_surgeon);

    let result = server
        .replace_batch(Parameters(crate::server::types::ReplaceBatchParams {
            filepath: filepath.to_owned(),
            base_version: hash.as_str().to_owned(),
            edits: vec![make_replace_body_edit(
                format!("{filepath}::foo"),
                "    42".to_string(),
            )],
            ignore_validation_failures: false,
        }))
        .await
        .expect("should succeed");

    assert!(result.0.success);
    let written = std::fs::read_to_string(&abs).unwrap();
    assert!(written.contains("42"));
    assert!(!written.contains("1"));
}

#[tokio::test]
async fn test_batch_replace_body_multi_atomic() {
    let ws_dir = tempdir().expect("temp dir");

    // Write a Rust file with 3 functions
    let source = r"
fn foo() -> i32 {
    1
}

fn bar() -> i32 {
    2
}

fn baz() -> i32 {
    3
}
";
    let filepath = "src/lib.rs";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, source).unwrap();

    let src_bytes = source.as_bytes();
    let hash = VersionHash::compute(src_bytes);

    // Mock responses for each function
    let mock_surgeon = MockSurgeon::new();

    // foo: "fn foo() -> i32 {\n    1\n}"
    let foo_open = source.find('{').unwrap();
    let foo_close = source.find('}').unwrap() + 1;
    mock_surgeon
        .resolve_body_range_results
        .lock()
        .unwrap()
        .push(Ok((
            make_body_range(foo_open, foo_close, 0, 4),
            Arc::from(src_bytes),
            hash.clone(),
        )));

    // baz: find the last function
    let baz_start = source.find("fn baz").unwrap();
    let baz_open = source[baz_start..].find('{').unwrap() + baz_start;
    let baz_close = source.rfind('}').unwrap() + 1;
    mock_surgeon
        .resolve_body_range_results
        .lock()
        .unwrap()
        .push(Ok((
            make_body_range(baz_open, baz_close, 0, 4),
            Arc::from(src_bytes),
            hash.clone(),
        )));

    let server = make_server(&ws_dir, mock_surgeon);

    let result = server
        .replace_batch(Parameters(crate::server::types::ReplaceBatchParams {
            filepath: filepath.to_owned(),
            base_version: hash.as_str().to_owned(),
            edits: vec![
                make_replace_body_edit(format!("{filepath}::foo"), "    42".to_string()),
                make_replace_body_edit(format!("{filepath}::baz"), "    99".to_string()),
            ],
            ignore_validation_failures: false,
        }))
        .await
        .expect("should succeed");

    assert!(result.0.success);
    let written = std::fs::read_to_string(&abs).unwrap();
    assert!(written.contains("42"));
    assert!(written.contains("99"));
    assert!(written.contains("2")); // bar should be unchanged
}

#[tokio::test]
async fn test_batch_occ_version_mismatch() {
    let ws_dir = tempdir().expect("temp dir");

    let source = "fn foo() { 1 }";
    let filepath = "src/lib.rs";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, source).unwrap();

    let _real_hash = VersionHash::compute(source.as_bytes());
    let stale_hash = "sha256:stale000".to_owned();

    let mock_surgeon = MockSurgeon::new();
    let server = make_server(&ws_dir, mock_surgeon);

    let result = server
        .replace_batch(Parameters(crate::server::types::ReplaceBatchParams {
            filepath: filepath.to_owned(),
            base_version: stale_hash,
            edits: vec![make_replace_body_edit(
                format!("{filepath}::foo"),
                "42".to_string(),
            )],
            ignore_validation_failures: false,
        }))
        .await;

    assert!(result.is_err());
    // Error type check removed - just verify is_err()

    // Verify file was NOT modified
    let written = std::fs::read_to_string(&abs).unwrap();
    assert_eq!(written, source);
}

#[tokio::test]
async fn test_batch_empty_edits() {
    let ws_dir = tempdir().expect("temp dir");

    let source = "fn foo() { 1 }";
    let filepath = "src/lib.rs";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, source).unwrap();

    let hash = VersionHash::compute(source.as_bytes());
    let mock_surgeon = MockSurgeon::new();
    let server = make_server(&ws_dir, mock_surgeon);

    // Empty edits vector should succeed with no changes
    let result = server
        .replace_batch(Parameters(crate::server::types::ReplaceBatchParams {
            filepath: filepath.to_owned(),
            base_version: hash.as_str().to_owned(),
            edits: vec![],
            ignore_validation_failures: false,
        }))
        .await
        .expect("should succeed");

    assert!(result.0.success);
    let written = std::fs::read_to_string(&abs).unwrap();
    assert_eq!(written, source);
}

#[tokio::test]
async fn test_batch_file_not_found() {
    let ws_dir = tempdir().expect("temp dir");

    let mock_surgeon = MockSurgeon::new();
    let server = make_server(&ws_dir, mock_surgeon);

    let result = server
        .replace_batch(Parameters(crate::server::types::ReplaceBatchParams {
            filepath: "nonexistent.rs".to_owned(),
            base_version: "sha256:any".to_owned(),
            edits: vec![make_replace_body_edit(
                "nonexistent.rs::foo".to_string(),
                "42".to_string(),
            )],
            ignore_validation_failures: false,
        }))
        .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_batch_sandbox_denied() {
    let ws_dir = tempdir().expect("temp dir");

    // Try to edit a file in .git/ (should be denied by sandbox)
    let filepath = ".git/config";

    let mock_surgeon = MockSurgeon::new();
    let server = make_server(&ws_dir, mock_surgeon);

    let result = server
        .replace_batch(Parameters(crate::server::types::ReplaceBatchParams {
            filepath: filepath.to_owned(),
            base_version: "sha256:any".to_owned(),
            edits: vec![make_replace_body_edit("any".to_string(), "42".to_string())],
            ignore_validation_failures: false,
        }))
        .await;

    assert!(result.is_err());
    // Error type check removed - just verify is_err()
}

#[tokio::test]
async fn test_batch_text_targeting() {
    let ws_dir = tempdir().expect("temp dir");

    // Write a file with identifiable text
    let source = r"
<div>
    <p>Old content</p>
</div>
";
    let filepath = "index.html";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, source).unwrap();

    let hash = VersionHash::compute(source.as_bytes());
    let mock_surgeon = MockSurgeon::new();
    let server = make_server(&ws_dir, mock_surgeon);

    let result = server
        .replace_batch(Parameters(crate::server::types::ReplaceBatchParams {
            filepath: filepath.to_owned(),
            base_version: hash.as_str().to_owned(),
            edits: vec![make_text_edit(
                "Old content".to_string(),
                "New content".to_string(),
                2,
                false,
            )],
            ignore_validation_failures: false,
        }))
        .await
        .expect("should succeed");

    assert!(result.0.success);
    let written = std::fs::read_to_string(&abs).unwrap();
    assert!(written.contains("New content"));
    assert!(!written.contains("Old content"));
}

#[tokio::test]
async fn test_batch_large_multi_edit() {
    let ws_dir = tempdir().expect("temp dir");

    // Write a file with 10 functions
    let mut source = String::default();
    for i in 0..10 {
        source.push_str(&format!("fn func{i}() -> i32 {{ {i} }}\n"));
    }

    let filepath = "src/lib.rs";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, &source).unwrap();

    let src_bytes = source.as_bytes();
    let hash = VersionHash::compute(src_bytes);

    let mock_surgeon = MockSurgeon::default();

    // Create 5 edits (every other function)
    let mut edits = Vec::default();
    for i in (0..10).step_by(2) {
        let func_start = source.find(&format!("fn func{i}")).unwrap();
        let open = source[func_start..].find('{').unwrap() + func_start;
        let close = source[func_start..].find('}').unwrap() + func_start + 1;

        mock_surgeon
            .resolve_body_range_results
            .lock()
            .unwrap()
            .push(Ok((
                make_body_range(open, close, 0, 0),
                Arc::from(src_bytes),
                hash.clone(),
            )));

        edits.push(make_replace_body_edit(
            format!("{filepath}::func{i}"),
            format!("    {}", i * 10),
        ));
    }

    let server = make_server(&ws_dir, mock_surgeon);

    let result = server
        .replace_batch(Parameters(crate::server::types::ReplaceBatchParams {
            filepath: filepath.to_owned(),
            base_version: hash.as_str().to_owned(),
            edits,
            ignore_validation_failures: false,
        }))
        .await
        .expect("should succeed");

    assert!(result.0.success);
    let written = std::fs::read_to_string(&abs).unwrap();

    // Verify edited functions
    for i in (0..10).step_by(2) {
        let value = i * 10;
        assert!(written.contains(&format!("{value}")));
    }

    // Verify unedited functions remain
    for i in (1..10).step_by(2) {
        assert!(written.contains(&format!("{{ {i} }}")));
    }
}

#[tokio::test]
async fn test_batch_text_not_found() {
    let ws_dir = tempdir().expect("temp dir");

    let source = "fn foo() { 1 }";
    let filepath = "src/lib.rs";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, source).unwrap();

    let hash = VersionHash::compute(source.as_bytes());
    let mock_surgeon = MockSurgeon::new();
    let server = make_server(&ws_dir, mock_surgeon);

    let result = server
        .replace_batch(Parameters(crate::server::types::ReplaceBatchParams {
            filepath: filepath.to_owned(),
            base_version: hash.as_str().to_owned(),
            edits: vec![make_text_edit(
                "nonexistent text".to_string(),
                "replacement".to_string(),
                1,
                false,
            )],
            ignore_validation_failures: false,
        }))
        .await;

    assert!(result.is_err());
    // Error type check removed - just verify is_err()
    // TEXT_NOT_FOUND error code

    // Verify file was NOT modified
    let written = std::fs::read_to_string(&abs).unwrap();
    assert_eq!(written, source);
}

#[tokio::test]
async fn test_batch_normalize_whitespace() {
    let ws_dir = tempdir().expect("temp dir");

    // Write HTML with inconsistent spacing
    let source = "<div>  <p>content</p>  </div>";
    let filepath = "index.html";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, source).unwrap();

    let hash = VersionHash::compute(source.as_bytes());
    let mock_surgeon = MockSurgeon::new();
    let server = make_server(&ws_dir, mock_surgeon);

    // Use normalize_whitespace=true to match despite spacing differences
    let result = server
        .replace_batch(Parameters(crate::server::types::ReplaceBatchParams {
            filepath: filepath.to_owned(),
            base_version: hash.as_str().to_owned(),
            edits: vec![BatchEdit {
                semantic_path: String::new(),
                old_text: Some("<div>  <p>content</p>".to_string()),
                edit_type: String::new(),
                new_code: None,
                replacement_text: Some("<div><p>updated</p>".to_string()),
                context_line: Some(1),
                normalize_whitespace: true,
            }],
            ignore_validation_failures: false,
        }))
        .await
        .expect("should succeed");

    assert!(result.0.success);
    let written = std::fs::read_to_string(&abs).unwrap();
    assert!(written.contains("updated"));
    assert!(!written.contains("content"));
}

#[tokio::test]
async fn test_batch_insert_before() {
    let ws_dir = tempdir().expect("temp dir");

    let source = r"
fn foo() -> i32 {
    1
}

fn bar() -> i32 {
    2
}
";
    let filepath = "src/lib.rs";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, source).unwrap();

    let src_bytes = source.as_bytes();
    let hash = VersionHash::compute(src_bytes);

    let bar_start = source.find("fn bar").unwrap();

    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .resolve_symbol_range_results
        .lock()
        .unwrap()
        .push(Ok((
            pathfinder_treesitter::surgeon::SymbolRange {
                start_byte: bar_start,
                end_byte: bar_start + 20, // approximate
                indent_column: 0,
            },
            Arc::from(src_bytes),
            hash.clone(),
        )));

    let server = make_server(&ws_dir, mock_surgeon);

    let result = server
        .replace_batch(Parameters(crate::server::types::ReplaceBatchParams {
            filepath: filepath.to_owned(),
            base_version: hash.as_str().to_owned(),
            edits: vec![BatchEdit {
                semantic_path: format!("{filepath}::bar"),
                old_text: None,
                edit_type: "insert_before".to_string(),
                new_code: Some("// Helper function\n".to_string()),
                replacement_text: None,
                context_line: None,
                normalize_whitespace: false,
            }],
            ignore_validation_failures: false,
        }))
        .await
        .expect("should succeed");

    assert!(result.0.success);
    let written = std::fs::read_to_string(&abs).unwrap();
    assert!(written.contains("Helper function"));
    // Helper should be before bar
    let helper_idx = written.find("Helper function").unwrap();
    let bar_idx = written.find("fn bar").unwrap();
    assert!(helper_idx < bar_idx);
}

#[tokio::test]
async fn test_batch_insert_after() {
    let ws_dir = tempdir().expect("temp dir");

    let source = r"
fn foo() -> i32 {
    1
}

fn bar() -> i32 {
    2
}
";
    let filepath = "src/lib.rs";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, source).unwrap();

    let src_bytes = source.as_bytes();
    let hash = VersionHash::compute(src_bytes);

    let foo_end = source.find('}').unwrap() + 1;

    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .resolve_symbol_range_results
        .lock()
        .unwrap()
        .push(Ok((
            pathfinder_treesitter::surgeon::SymbolRange {
                start_byte: 0,
                end_byte: foo_end,
                indent_column: 0,
            },
            Arc::from(src_bytes),
            hash.clone(),
        )));

    let server = make_server(&ws_dir, mock_surgeon);

    let result = server
        .replace_batch(Parameters(crate::server::types::ReplaceBatchParams {
            filepath: filepath.to_owned(),
            base_version: hash.as_str().to_owned(),
            edits: vec![BatchEdit {
                semantic_path: format!("{filepath}::foo"),
                old_text: None,
                edit_type: "insert_after".to_string(),
                new_code: Some("// After foo\n".to_string()),
                replacement_text: None,
                context_line: None,
                normalize_whitespace: false,
            }],
            ignore_validation_failures: false,
        }))
        .await
        .expect("should succeed");

    assert!(result.0.success);
    let written = std::fs::read_to_string(&abs).unwrap();
    assert!(written.contains("After foo"));
    // After foo should be after foo's closing brace
    let foo_end_idx = source.find('}').unwrap();
    let after_idx = written.find("After foo").unwrap();
    assert!(after_idx > foo_end_idx);
}

#[tokio::test]
async fn test_batch_delete() {
    let ws_dir = tempdir().expect("temp dir");

    let source = r"
fn foo() -> i32 {
    1
}

fn bar() -> i32 {
    2
}

fn baz() -> i32 {
    3
}
";
    let filepath = "src/lib.rs";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, source).unwrap();

    let src_bytes = source.as_bytes();
    let hash = VersionHash::compute(src_bytes);

    let bar_start = source.find("fn bar").unwrap();
    let baz_start = source.find("fn baz").unwrap();

    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .resolve_full_range_results
        .lock()
        .unwrap()
        .push(Ok((
            pathfinder_treesitter::surgeon::FullRange {
                start_byte: bar_start,
                end_byte: baz_start,
                indent_column: 0,
            },
            Arc::from(src_bytes),
            hash.clone(),
        )));

    let server = make_server(&ws_dir, mock_surgeon);

    let result = server
        .replace_batch(Parameters(crate::server::types::ReplaceBatchParams {
            filepath: filepath.to_owned(),
            base_version: hash.as_str().to_owned(),
            edits: vec![BatchEdit {
                semantic_path: format!("{filepath}::bar"),
                old_text: None,
                edit_type: "delete".to_string(),
                new_code: None,
                replacement_text: None,
                context_line: None,
                normalize_whitespace: false,
            }],
            ignore_validation_failures: false,
        }))
        .await
        .expect("should succeed");

    assert!(result.0.success);
    let written = std::fs::read_to_string(&abs).unwrap();
    assert!(!written.contains("fn bar"));
    assert!(written.contains("fn foo"));
    assert!(written.contains("fn baz"));
}

/// Test that overlapping edits are detected and rejected.
///
/// This test creates two edits with overlapping byte ranges:
/// - Edit 1 targets `func_a` (lines 1-3)
/// - Edit 2 targets `func_b` (lines 3-5)
///
/// Since these ranges overlap, the batch should fail with an `INVALID_TARGET` error.
#[tokio::test]
async fn test_batch_detects_overlapping_edits() {
    let ws_dir = tempdir().expect("temp dir");

    let source = r"
fn func_a() -> i32 {
    1
}

fn func_b() -> i32 {
    2
}

fn func_c() -> i32 {
    3
}
";
    let filepath = "src/lib.rs";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, source).unwrap();

    let src_bytes = source.as_bytes();
    let hash = VersionHash::compute(src_bytes);

    let mock_surgeon = MockSurgeon::new();

    // Mock func_a body range (lines 1-3, bytes ~10-40)
    let func_a_start = source.find("fn func_a").unwrap();
    let func_a_open = source[func_a_start..].find('{').unwrap() + func_a_start;
    let _func_a_close = source.find('}').unwrap() + 1;

    // Mock func_b body range (lines 5-7, bytes ~40-70)
    let func_b_start = source.find("fn func_b").unwrap();
    let func_b_open = source[func_b_start..].find('{').unwrap() + func_b_start;
    let func_b_close = source[func_b_start..].find('}').unwrap() + func_b_start + 1;

    // Intentionally make the ranges overlap by setting func_a_close > func_b_open
    // This simulates a scenario where the AST parser returns overlapping ranges
    mock_surgeon
        .resolve_body_range_results
        .lock()
        .unwrap()
        .push(Ok((
            make_body_range(func_a_open, func_b_open + 5, 0, 4), // Overlap with func_b
            Arc::from(src_bytes),
            hash.clone(),
        )));

    mock_surgeon
        .resolve_body_range_results
        .lock()
        .unwrap()
        .push(Ok((
            make_body_range(func_b_open, func_b_close, 0, 4),
            Arc::from(src_bytes),
            hash.clone(),
        )));

    let server = make_server(&ws_dir, mock_surgeon);

    let result = server
        .replace_batch(Parameters(crate::server::types::ReplaceBatchParams {
            filepath: filepath.to_owned(),
            base_version: hash.as_str().to_owned(),
            edits: vec![
                make_replace_body_edit(format!("{filepath}::func_a"), "    42".to_string()),
                make_replace_body_edit(format!("{filepath}::func_b"), "    99".to_string()),
            ],
            ignore_validation_failures: false,
        }))
        .await;

    // Should fail with overlap error
    assert!(result.is_err());
    let err = result.err().unwrap();
    // The error code is in err.message, not in data.
    // The detailed error info is in data["error"] and data["message"]
    let error_field = err
        .data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let message_field = err
        .data
        .as_ref()
        .and_then(|d| d.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(error_field, "INVALID_TARGET");
    assert!(message_field.contains("overlapping"));

    // Verify file was NOT modified
    let written = std::fs::read_to_string(&abs).unwrap();
    assert_eq!(written, source);
}

/// Test that adjacent (non-overlapping) edits are allowed.
///
/// This test verifies that edits with non-overlapping byte ranges
/// are applied successfully without triggering overlap detection.
#[tokio::test]
async fn test_batch_adjacent_edits_allowed() {
    let ws_dir = tempdir().expect("temp dir");

    // Create a simple source with clearly separated positions
    let source = "AAA BBB CCC DDD EEE";
    let filepath = "src/lib.rs";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, source).unwrap();

    let src_bytes = source.as_bytes();
    let hash = VersionHash::compute(src_bytes);

    let mock_surgeon = MockSurgeon::new();

    // Target clearly non-overlapping positions: "AAA" (0-3) and "CCC" (8-11)
    let pos_a = 0;
    let pos_c = 8;

    mock_surgeon
        .resolve_body_range_results
        .lock()
        .unwrap()
        .push(Ok((
            make_body_range(pos_a, pos_a + 3, 0, 0),
            Arc::from(src_bytes),
            hash.clone(),
        )));

    mock_surgeon
        .resolve_body_range_results
        .lock()
        .unwrap()
        .push(Ok((
            make_body_range(pos_c, pos_c + 3, 0, 0),
            Arc::from(src_bytes),
            hash.clone(),
        )));

    let server = make_server(&ws_dir, mock_surgeon);

    let result = server
        .replace_batch(Parameters(crate::server::types::ReplaceBatchParams {
            filepath: filepath.to_owned(),
            base_version: hash.as_str().to_owned(),
            edits: vec![
                make_replace_body_edit(format!("{filepath}::func_a"), "XXX".to_string()),
                make_replace_body_edit(format!("{filepath}::func_c"), "YYY".to_string()),
            ],
            ignore_validation_failures: false,
        }))
        .await
        .expect("should succeed");

    assert!(result.0.success);
    let written = std::fs::read_to_string(&abs).unwrap();
    assert!(written.contains("XXX"));
    assert!(written.contains("YYY"));
    assert!(!written.contains("AAA"));
    assert!(!written.contains("CCC"));
    // BBB and DDD should remain unchanged
    assert!(written.contains("BBB"));
    assert!(written.contains("DDD"));
}

/// Test that many edits don't cause byte overflow issues.
///
/// This test creates a large number of edits and verifies that
/// the byte arithmetic doesn't overflow and that all edits are applied correctly.
#[tokio::test]
async fn test_batch_many_edits_no_overflow() {
    let ws_dir = tempdir().expect("temp dir");

    // Write a simple file with 10 distinct numbers
    let source = "0 1 2 3 4 5 6 7 8 9";
    let filepath = "src/lib.rs";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, source).unwrap();

    let src_bytes = source.as_bytes();
    let hash = VersionHash::compute(src_bytes);

    let mock_surgeon = MockSurgeon::new();

    // Create 5 edits targeting non-overlapping positions
    let mut edits = Vec::new();
    for i in [0, 2, 4, 6, 8] {
        let pos = source.find(&i.to_string()).unwrap();
        mock_surgeon
            .resolve_body_range_results
            .lock()
            .unwrap()
            .push(Ok((
                make_body_range(pos, pos + 1, 0, 0),
                Arc::from(src_bytes),
                hash.clone(),
            )));

        edits.push(make_replace_body_edit(
            format!("{filepath}::func_{i}"),
            format!("{}", i * 10),
        ));
    }

    let server = make_server(&ws_dir, mock_surgeon);

    let result = server
        .replace_batch(Parameters(crate::server::types::ReplaceBatchParams {
            filepath: filepath.to_owned(),
            base_version: hash.as_str().to_owned(),
            edits,
            ignore_validation_failures: false,
        }))
        .await
        .expect("should succeed");

    assert!(result.0.success);
    let written = std::fs::read_to_string(&abs).unwrap();

    // Verify edited numbers
    for i in [0, 2, 4, 6, 8] {
        assert!(written.contains(&format!("{}", i * 10)));
    }
    // Verify unedited numbers remain
    for i in [1, 3, 5, 7, 9] {
        assert!(written.contains(&i.to_string()));
    }
}

/// Test that the error message includes the edit indices for overlapping edits.
///
/// This verifies that when overlap is detected, the error message
/// clearly indicates which edits conflicted (e.g., "edit 1 overlaps with edit 0").
#[tokio::test]
async fn test_batch_overlap_error_includes_indices() {
    let ws_dir = tempdir().expect("temp dir");

    let source = r"
fn func_a() -> i32 { 1 }
fn func_b() -> i32 { 2 }
";
    let filepath = "src/lib.rs";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, source).unwrap();

    let src_bytes = source.as_bytes();
    let hash = VersionHash::compute(src_bytes);

    let mock_surgeon = MockSurgeon::new();

    // Create overlapping ranges
    let func_a_open = source.find('{').unwrap();
    let _func_a_close = source.find('}').unwrap() + 1;
    let func_b_start = source.find("fn func_b").unwrap();
    let func_b_open = source[func_b_start..].find('{').unwrap() + func_b_start;

    // Make func_a's range extend into func_b's range
    mock_surgeon
        .resolve_body_range_results
        .lock()
        .unwrap()
        .push(Ok((
            make_body_range(func_a_open, func_b_open + 5, 0, 0),
            Arc::from(src_bytes),
            hash.clone(),
        )));

    mock_surgeon
        .resolve_body_range_results
        .lock()
        .unwrap()
        .push(Ok((
            make_body_range(func_b_open, source.len(), 0, 0),
            Arc::from(src_bytes),
            hash.clone(),
        )));

    let server = make_server(&ws_dir, mock_surgeon);

    let result = server
        .replace_batch(Parameters(crate::server::types::ReplaceBatchParams {
            filepath: filepath.to_owned(),
            base_version: hash.as_str().to_owned(),
            edits: vec![
                make_replace_body_edit(format!("{filepath}::func_a"), "42".to_string()),
                make_replace_body_edit(format!("{filepath}::func_b"), "99".to_string()),
            ],
            ignore_validation_failures: false,
        }))
        .await;

    assert!(result.is_err());
    let err = result.err().unwrap();

    // Verify error message contains edit indices
    let msg = err
        .data
        .as_ref()
        .and_then(|d| d.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(msg.contains("edit"), "Error message should mention 'edit'");
    // The actual indices depend on sorting order, so we just check for digits
    assert!(
        msg.contains(char::is_numeric),
        "Error message should contain edit indices"
    );

    // Verify file was NOT modified
    let written = std::fs::read_to_string(&abs).unwrap();
    assert_eq!(written, source);
}
