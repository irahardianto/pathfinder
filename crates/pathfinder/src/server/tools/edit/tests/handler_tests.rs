#![allow(clippy::unwrap_used, clippy::expect_used, clippy::needless_return)]
use super::helpers::*;
use crate::server::types::{
    DeleteSymbolParams, InsertAfterParams, InsertBeforeParams, ReplaceBodyParams,
    ReplaceFullParams, ValidateOnlyParams,
};

use pathfinder_common::types::VersionHash;
use pathfinder_treesitter::mock::MockSurgeon;
use rmcp::handler::server::wrapper::Parameters;
use std::sync::Arc;
use tempfile::tempdir;

#[tokio::test]
async fn test_replace_body_success() {
    let ws_dir = tempdir().expect("temp dir");

    // Write a simple Go file
    let src = "func Login() {\n    // old body\n}\n";
    let filepath = "src/auth.go";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, src).unwrap();

    let src_bytes = src.as_bytes();
    let hash = VersionHash::compute(src_bytes);

    // Locate braces: `{` is at position 13, `}` is at position 31 (inclusive), so length is 32.
    // Tree-sitter is exclusive of end_byte, so it should be close + 1.
    let open = src.find('{').unwrap();
    let close = src.rfind('}').unwrap() + 1;

    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .resolve_body_range_results
        .lock()
        .unwrap()
        .push(Ok((
            make_body_range(open, close, 0, 4),
            std::sync::Arc::from(src_bytes),
            hash.clone(),
        )));

    let server = make_server(&ws_dir, mock_surgeon);

    let params = ReplaceBodyParams {
        semantic_path: format!("{filepath}::Login"),
        base_version: hash.as_str().to_owned(),
        new_code: "    return nil".to_owned(),
        ignore_validation_failures: false,
    };

    let result = server
        .replace_body(Parameters(params))
        .await
        .expect("should succeed");
    let resp = result.0;

    assert!(resp.success);
    assert!(resp.new_version_hash.is_some());
    assert_eq!(resp.validation.status, "skipped");
    assert!(resp.validation_skipped);

    // Verify the file was actually written
    let written = std::fs::read_to_string(&abs).unwrap();
    assert!(written.contains("return nil"), "written: {written}");
    assert!(!written.contains("old body"), "written: {written}");
}

// ── replace_body_version_mismatch ────────────────────────────────

#[tokio::test]
async fn test_replace_body_version_mismatch() {
    let ws_dir = tempdir().expect("temp dir");

    let src = "func Login() {\n    // body\n}\n";
    let filepath = "src/auth.go";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, src).unwrap();

    let src_bytes = src.as_bytes();
    let real_hash = VersionHash::compute(src_bytes);
    let stale_hash = "sha256:stale000".to_owned();

    let open = src.find('{').unwrap();
    let close = src.rfind('}').unwrap() + 1;

    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .resolve_body_range_results
        .lock()
        .unwrap()
        .push(Ok((
            make_body_range(open, close, 0, 4),
            std::sync::Arc::from(src_bytes),
            real_hash,
        )));

    let server = make_server(&ws_dir, mock_surgeon);

    let params = ReplaceBodyParams {
        semantic_path: format!("{filepath}::Login"),
        base_version: stale_hash,
        new_code: "return nil".to_owned(),
        ignore_validation_failures: false,
    };

    let result = server.replace_body(Parameters(params)).await;
    let Err(err) = result else {
        panic!("expected VERSION_MISMATCH error");
    };

    let code = err
        .data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(code, "VERSION_MISMATCH", "got: {err:?}");
}

// ── replace_body_symbol_not_found ────────────────────────────────

#[tokio::test]
async fn test_replace_body_symbol_not_found() {
    let ws_dir = tempdir().expect("temp dir");

    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .resolve_body_range_results
        .lock()
        .unwrap()
        .push(Err(pathfinder_treesitter::SurgeonError::SymbolNotFound {
            path: "src/auth.go::Lgon".to_owned(),
            did_you_mean: vec!["Login".to_owned()],
        }));

    let server = make_server(&ws_dir, mock_surgeon);

    let params = ReplaceBodyParams {
        semantic_path: "src/auth.go::Lgon".to_owned(),
        base_version: "sha256:any".to_owned(),
        new_code: "return nil".to_owned(),
        ignore_validation_failures: false,
    };

    let result = server.replace_body(Parameters(params)).await;
    let Err(err) = result else {
        panic!("expected SYMBOL_NOT_FOUND error");
    };

    let code = err
        .data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(code, "SYMBOL_NOT_FOUND", "got: {err:?}");
}

// ── replace_body_access_denied ────────────────────────────────────

#[tokio::test]
async fn test_replace_body_access_denied() {
    let ws_dir = tempdir().expect("temp dir");
    let server = make_server(&ws_dir, MockSurgeon::new());

    let params = ReplaceBodyParams {
        semantic_path: ".git/config::Login".to_owned(),
        base_version: "sha256:any".to_owned(),
        new_code: "body".to_owned(),
        ignore_validation_failures: false,
    };

    let result = server.replace_body(Parameters(params)).await;
    let Err(err) = result else {
        panic!("expected ACCESS_DENIED error");
    };

    let code = err
        .data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(code, "ACCESS_DENIED", "got: {err:?}");
}

// ── replace_body_brace_leniency ───────────────────────────────────

#[tokio::test]
async fn test_replace_body_brace_leniency() {
    // LLM wraps code in braces — should be auto-stripped
    let ws_dir = tempdir().expect("temp dir");

    let src = "func Login() {\n    // old\n}\n";
    let filepath = "src/auth.go";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, src).unwrap();

    let src_bytes = src.as_bytes();
    let hash = VersionHash::compute(src_bytes);

    let open = src.find('{').unwrap();
    let close = src.rfind('}').unwrap() + 1;

    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .resolve_body_range_results
        .lock()
        .unwrap()
        .push(Ok((
            make_body_range(open, close, 0, 4),
            std::sync::Arc::from(src_bytes),
            hash.clone(),
        )));

    let server = make_server(&ws_dir, mock_surgeon);

    // Pass code wrapped in braces — brace-leniency should strip them
    let params = ReplaceBodyParams {
        semantic_path: format!("{filepath}::Login"),
        base_version: hash.as_str().to_owned(),
        new_code: "{ return nil }".to_owned(),
        ignore_validation_failures: false,
    };

    let result = server
        .replace_body(Parameters(params))
        .await
        .expect("should succeed despite outer braces");
    assert!(result.0.success);

    let written = std::fs::read_to_string(&abs).unwrap();
    // Should NOT contain `{{ return nil }}` — braces should have been stripped
    assert!(!written.contains("{ return nil }"), "written: {written}");
    assert!(written.contains("return nil"), "written: {written}");
}

// ── replace_body_bare_file_rejected ──────────────────────────────

#[tokio::test]
async fn test_replace_body_bare_file_rejected() {
    let ws_dir = tempdir().expect("temp dir");
    let server = make_server(&ws_dir, MockSurgeon::new());

    let params = ReplaceBodyParams {
        semantic_path: "src/auth.go".to_owned(), // no :: symbol
        base_version: "sha256:any".to_owned(),
        new_code: "body".to_owned(),
        ignore_validation_failures: false,
    };

    let result = server.replace_body(Parameters(params)).await;
    let Err(err) = result else {
        panic!("expected INVALID_SEMANTIC_PATH error");
    };

    let code = err
        .data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(code, "INVALID_SEMANTIC_PATH", "got: {err:?}");
}

// ── Integration Tests with Real TreeSitterSurgeon ───────────────────

#[tokio::test]
async fn test_replace_body_real_parser_go() {
    use pathfinder_treesitter::treesitter_surgeon::TreeSitterSurgeon;
    let ws_dir = tempdir().expect("temp dir");

    let src = "package main\n\nfunc Login() {\n    // old body\n}\n";
    let filepath = "src/auth.go";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, src).unwrap();

    let hash = VersionHash::compute(src.as_bytes());

    let real_surgeon = Arc::new(TreeSitterSurgeon::new(10));
    let server = make_server_dyn(&ws_dir, real_surgeon);

    let params = ReplaceBodyParams {
        semantic_path: format!("{filepath}::Login"),
        base_version: hash.as_str().to_owned(),
        new_code: "    return nil".to_owned(),
        ignore_validation_failures: false,
    };

    let result = server
        .replace_body(Parameters(params))
        .await
        .expect("should succeed");
    assert!(result.0.success);

    let written = std::fs::read_to_string(&abs).unwrap();
    assert!(written.contains("return nil"), "written: {written}");
    assert!(!written.contains("old body"), "written: {written}");
    // Make sure braces are preserved
    assert!(written.contains("func Login() {\n"), "written: {written}");
    assert!(written.ends_with("}\n"), "written: {written}");
}

#[tokio::test]
async fn test_replace_body_real_parser_python() {
    use pathfinder_treesitter::treesitter_surgeon::TreeSitterSurgeon;
    let ws_dir = tempdir().expect("temp dir");

    let src = "def login():\n    # old body\n    pass\n";
    let filepath = "src/auth.py";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, src).unwrap();

    let hash = VersionHash::compute(src.as_bytes());

    let real_surgeon = Arc::new(TreeSitterSurgeon::new(10));
    let server = make_server_dyn(&ws_dir, real_surgeon);

    let params = ReplaceBodyParams {
        semantic_path: format!("{filepath}::login"),
        base_version: hash.as_str().to_owned(),
        new_code: "    return None".to_owned(),
        ignore_validation_failures: false,
    };

    let result = server
        .replace_body(Parameters(params))
        .await
        .expect("should succeed");
    assert!(result.0.success);

    let written = std::fs::read_to_string(&abs).unwrap();

    let expected = "def login():\n    # old body\n    return None\n";
    assert_eq!(written, expected);
}

// ── Integration Tests for New Tools ─────────────────────────────────────

#[tokio::test]
async fn test_replace_full_real_parser_go() {
    use pathfinder_treesitter::treesitter_surgeon::TreeSitterSurgeon;
    let ws_dir = tempdir().expect("temp dir");

    let src = "package main\n\n// DOC\nfunc Login() {\n    // old body\n}\n\nfunc Other() {}";
    let filepath = "src/auth.go";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, src).unwrap();

    let hash = VersionHash::compute(src.as_bytes());

    let real_surgeon = Arc::new(TreeSitterSurgeon::new(10));
    let server = make_server_dyn(&ws_dir, real_surgeon);

    let params = ReplaceFullParams {
        semantic_path: format!("{filepath}::Login"),
        base_version: hash.as_str().to_owned(),
        new_code: "func NewLogin() {\n    return nil\n}".to_owned(),
        ignore_validation_failures: false,
    };

    let result = server
        .replace_full(Parameters(params))
        .await
        .expect("should succeed");
    assert!(result.0.success);

    let written = std::fs::read_to_string(&abs).unwrap();
    assert!(written.contains("func NewLogin"));
    assert!(!written.contains("func Login"));
    assert!(
        !written.contains("// DOC"),
        "Doc comment should be replaced"
    );
}

#[tokio::test]
async fn test_insert_before_bare_file() {
    let ws_dir = tempdir().expect("temp dir");

    let src = "package main\n";
    let filepath = "src/main.go";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, src).unwrap();

    let hash = VersionHash::compute(src.as_bytes());

    let server = make_server(&ws_dir, MockSurgeon::new());

    let params = InsertBeforeParams {
        semantic_path: filepath.to_owned(), // BOF
        base_version: hash.as_str().to_owned(),
        new_code: "// License\n".to_owned(),
        ignore_validation_failures: false,
    };

    let result = server
        .insert_before(Parameters(params))
        .await
        .expect("should succeed");
    assert!(result.0.success);

    let written = std::fs::read_to_string(&abs).unwrap();
    assert!(written.starts_with("// License\n"));
    assert!(written.contains("package main"));
}

#[tokio::test]
async fn test_insert_after_bare_file() {
    let ws_dir = tempdir().expect("temp dir");

    let src = "package main\n";
    let filepath = "src/main.go";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, src).unwrap();

    let hash = VersionHash::compute(src.as_bytes());

    let server = make_server(&ws_dir, MockSurgeon::new());

    let params = InsertAfterParams {
        semantic_path: filepath.to_owned(), // EOF
        base_version: hash.as_str().to_owned(),
        new_code: "func append() {}".to_owned(),
        ignore_validation_failures: false,
    };

    let result = server
        .insert_after(Parameters(params))
        .await
        .expect("should succeed");
    assert!(result.0.success);

    let written = std::fs::read_to_string(&abs).unwrap();
    assert!(written.contains("package main\n\nfunc append() {}"));
}

// ── insert_into tests ───────────────────────────────────────────────────

#[tokio::test]
async fn test_insert_into_mod_tests_appends_inside() {
    use pathfinder_treesitter::treesitter_surgeon::TreeSitterSurgeon;
    let ws_dir = tempdir().expect("temp dir");

    let src = "mod tests {\n    fn test_a() {}\n}\n";
    let filepath = "src/lib.rs";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, src).unwrap();

    let hash = VersionHash::compute(src.as_bytes());

    let real_surgeon = Arc::new(TreeSitterSurgeon::new(10));
    let server = make_server_dyn(&ws_dir, real_surgeon);

    let params = crate::server::types::InsertIntoParams {
        semantic_path: format!("{filepath}::tests"),
        base_version: hash.as_str().to_owned(),
        new_code: "fn test_b() {}".to_owned(),
        ignore_validation_failures: false,
    };

    let result = server
        .insert_into(Parameters(params))
        .await
        .expect("should succeed");
    assert!(result.0.success);

    let written = std::fs::read_to_string(&abs).unwrap();
    assert!(written.contains("fn test_b() {}"));
    assert!(written.ends_with("}\n"));
    assert!(written.find("fn test_b() {}").unwrap() > written.find("fn test_a() {}").unwrap());
    assert!(written.find("fn test_b() {}").unwrap() < written.rfind('}').unwrap());
}

#[tokio::test]
async fn test_insert_into_bare_file_is_rejected() {
    let ws_dir = tempdir().expect("temp dir");
    let server = make_server(&ws_dir, MockSurgeon::new());

    let params = crate::server::types::InsertIntoParams {
        semantic_path: "src/lib.rs".to_owned(),
        base_version: "sha256:any".to_owned(),
        new_code: "fn b() {}".to_owned(),
        ignore_validation_failures: false,
    };

    let result = server.insert_into(Parameters(params)).await;
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
async fn test_insert_into_function_symbol_is_rejected() {
    use pathfinder_treesitter::treesitter_surgeon::TreeSitterSurgeon;
    let ws_dir = tempdir().expect("temp dir");

    let src = "fn test_a() {}\n";
    let filepath = "src/lib.rs";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, src).unwrap();

    let hash = VersionHash::compute(src.as_bytes());

    let real_surgeon = Arc::new(TreeSitterSurgeon::new(10));
    let server = make_server_dyn(&ws_dir, real_surgeon);

    let params = crate::server::types::InsertIntoParams {
        semantic_path: format!("{filepath}::test_a"),
        base_version: hash.as_str().to_owned(),
        new_code: "let x = 1;".to_owned(),
        ignore_validation_failures: false,
    };

    let result = server.insert_into(Parameters(params)).await;
    let Err(err) = result else {
        panic!("expected INVALID_TARGET error");
    };

    let code = err
        .data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(code, "INVALID_TARGET");
}

#[tokio::test]
async fn test_insert_into_occ_mismatch() {
    use pathfinder_treesitter::treesitter_surgeon::TreeSitterSurgeon;
    let ws_dir = tempdir().expect("temp dir");

    let src = "mod tests {}\n";
    let filepath = "src/lib.rs";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, src).unwrap();

    let real_surgeon = Arc::new(TreeSitterSurgeon::new(10));
    let server = make_server_dyn(&ws_dir, real_surgeon);

    let params = crate::server::types::InsertIntoParams {
        semantic_path: format!("{filepath}::tests"),
        base_version: "sha256:stale000".to_owned(),
        new_code: "fn test_b() {}".to_owned(),
        ignore_validation_failures: false,
    };

    let result = server.insert_into(Parameters(params)).await;
    let Err(err) = result else {
        panic!("expected VERSION_MISMATCH error");
    };

    let code = err
        .data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(code, "VERSION_MISMATCH");
}

#[tokio::test]
async fn test_delete_symbol_real_parser_go() {
    use pathfinder_treesitter::treesitter_surgeon::TreeSitterSurgeon;
    let ws_dir = tempdir().expect("temp dir");

    let src = "package main\n\n// DOC\nfunc Login() {\n    // body\n}\n\nfunc Next() {}";
    let filepath = "src/auth.go";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, src).unwrap();

    let hash = VersionHash::compute(src.as_bytes());

    let real_surgeon = Arc::new(TreeSitterSurgeon::new(10));
    let server = make_server_dyn(&ws_dir, real_surgeon);

    let params = DeleteSymbolParams {
        semantic_path: format!("{filepath}::Login"),
        base_version: hash.as_str().to_owned(),
        ignore_validation_failures: false,
    };

    let result = server
        .delete_symbol(Parameters(params))
        .await
        .expect("should succeed");
    assert!(result.0.success);

    let written = std::fs::read_to_string(&abs).unwrap();
    assert!(!written.contains("Login"));
    assert!(!written.contains("// DOC"));
    assert_eq!(written, "package main\n\nfunc Next() {}");
}

// ── validate_only tests ─────────────────────────────────────────────────

#[tokio::test]
async fn test_validate_only_replace_body() {
    let ws_dir = tempdir().expect("temp dir");
    let src = "func Login() {\n    // old body\n}\n";
    let filepath = "src/auth.go";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, src).unwrap();

    let src_bytes = src.as_bytes();
    let hash = VersionHash::compute(src_bytes);

    let open = src.find('{').unwrap();
    let close = src.rfind('}').unwrap() + 1;

    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .resolve_body_range_results
        .lock()
        .unwrap()
        .push(Ok((
            make_body_range(open, close, 0, 4),
            std::sync::Arc::from(src_bytes),
            hash.clone(),
        )));

    let server = make_server(&ws_dir, mock_surgeon);

    let params = ValidateOnlyParams {
        semantic_path: format!("{filepath}::Login"),
        edit_type: "replace_body".to_string(),
        new_code: Some("    return nil".to_string()),
        base_version: hash.as_str().to_owned(),
    };

    let result = server
        .validate_only(Parameters(params))
        .await
        .expect("should succeed");
    let resp = result.0;

    assert!(resp.success);
    assert!(resp.new_version_hash.is_none());
    assert_eq!(resp.validation.status, "skipped");
    assert!(resp.validation_skipped);

    // Verify the file was NOT written
    let written = std::fs::read_to_string(&abs).unwrap();
    assert!(
        !written.contains("return nil"),
        "File should not be modified"
    );
    assert!(
        written.contains("old body"),
        "File should retain original content"
    );
}

#[tokio::test]
async fn test_validate_only_version_mismatch() {
    let ws_dir = tempdir().expect("temp dir");
    let src = "func Login() {\n    // old body\n}\n";
    let filepath = "src/auth.go";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, src).unwrap();

    let src_bytes = src.as_bytes();
    let real_hash = VersionHash::compute(src_bytes);
    let stale_hash = "sha256:stale000".to_owned();

    let open = src.find('{').unwrap();
    let close = src.rfind('}').unwrap() + 1;

    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .resolve_body_range_results
        .lock()
        .unwrap()
        .push(Ok((
            make_body_range(open, close, 0, 4),
            std::sync::Arc::from(src_bytes),
            real_hash,
        )));

    let server = make_server(&ws_dir, mock_surgeon);

    let params = ValidateOnlyParams {
        semantic_path: format!("{filepath}::Login"),
        edit_type: "replace_body".to_string(),
        new_code: Some("return nil".to_string()),
        base_version: stale_hash,
    };

    let result = server.validate_only(Parameters(params)).await;
    let Err(err) = result else {
        panic!("expected VERSION_MISMATCH error");
    };

    let code = err
        .data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(code, "VERSION_MISMATCH");
}

#[tokio::test]
async fn test_validate_only_invalid_edit_type() {
    let ws_dir = tempdir().expect("temp dir");
    let server = make_server(&ws_dir, MockSurgeon::new());

    let params = ValidateOnlyParams {
        semantic_path: "src/auth.go::Login".to_string(),
        edit_type: "foo_bar".to_string(),
        new_code: Some("return nil".to_string()),
        base_version: "sha256:any".to_owned(),
    };

    let result = server.validate_only(Parameters(params)).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_validate_only_delete() {
    let ws_dir = tempdir().expect("temp dir");
    let src = "func Login() {}";
    let filepath = "src/auth.go";
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

    let server = make_server(&ws_dir, mock_surgeon);

    let params = ValidateOnlyParams {
        semantic_path: format!("{filepath}::Login"),
        edit_type: "delete".to_string(),
        new_code: None,
        base_version: hash.as_str().to_owned(),
    };

    let result = server
        .validate_only(Parameters(params))
        .await
        .expect("should succeed");
    assert!(result.0.success);
    assert!(result.0.new_version_hash.is_none());
}

#[tokio::test]
async fn test_validate_only_real_parser_go() {
    use pathfinder_treesitter::treesitter_surgeon::TreeSitterSurgeon;
    let ws_dir = tempdir().expect("temp dir");

    let src = "package main\n\nfunc Login() {\n    // old body\n}\n";
    let filepath = "src/auth.go";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, src).unwrap();

    let hash = VersionHash::compute(src.as_bytes());

    let real_surgeon = Arc::new(TreeSitterSurgeon::new(10));
    let server = make_server_dyn(&ws_dir, real_surgeon);

    let params = ValidateOnlyParams {
        semantic_path: format!("{filepath}::Login"),
        edit_type: "replace_full".to_string(),
        new_code: Some("func NewLogin() {}".to_string()),
        base_version: hash.as_str().to_owned(),
    };

    let result = server
        .validate_only(Parameters(params))
        .await
        .expect("should succeed");
    assert!(result.0.success);
    assert!(result.0.new_version_hash.is_none());

    // Ensure disk untouched
    let written = std::fs::read_to_string(&abs).unwrap();
    assert!(written.contains("func Login() {"));
}

// ── delete_symbol_bare_file_rejected ──────────────────────────────

#[tokio::test]
async fn test_delete_symbol_bare_file_rejected() {
    // delete_symbol requires a symbol target — bare files must return INVALID_SEMANTIC_PATH.
    // This exercises the require_symbol_target guard inside resolve_edit_content("delete").
    let ws_dir = tempdir().expect("temp dir");
    let server = make_server(&ws_dir, MockSurgeon::new());

    let params = DeleteSymbolParams {
        semantic_path: "src/auth.go".to_owned(), // no :: — bare file
        base_version: "sha256:any".to_owned(),
        ignore_validation_failures: false,
    };

    let result = server.delete_symbol(Parameters(params)).await;
    let Err(err) = result else {
        panic!("expected INVALID_SEMANTIC_PATH error for bare file");
    };

    let code = err
        .data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(code, "INVALID_SEMANTIC_PATH", "got: {err:?}");
}

// ── delete_symbol_cross_file_reference_warning ────────────────────

#[tokio::test]
async fn test_delete_symbol_cross_file_reference_warning() {
    // When `ignore_validation_failures: false` and `rg` detects the symbol name
    // in another file, delete_symbol must return INVALID_TARGET.
    //
    // We set up two files:
    //   src/auth.go   — defines `func Login() {}`
    //   src/main.go   — references `Login` (causing a cross-file hit)
    //
    // NOTE: This test requires `rg` (ripgrep) on PATH. If absent the guard
    // silently skips, so we check and early-return to avoid a misleading pass.
    use pathfinder_treesitter::treesitter_surgeon::TreeSitterSurgeon;

    if std::process::Command::new("rg")
        .arg("--version")
        .output()
        .is_err()
    {
        // rg not available — skip gracefully.
        return;
    }

    let ws_dir = tempdir().expect("temp dir");

    let auth_src = "package main\n\nfunc Login() {\n    // body\n}\n";
    let main_src = "package main\n\nfunc main() {\n    Login()\n}\n";

    let auth_path = "src/auth.go";
    let main_path = "src/main.go";

    let auth_abs = ws_dir.path().join(auth_path);
    let main_abs = ws_dir.path().join(main_path);
    std::fs::create_dir_all(auth_abs.parent().unwrap()).unwrap();
    std::fs::write(&auth_abs, auth_src).unwrap();
    std::fs::write(&main_abs, main_src).unwrap();

    let hash = VersionHash::compute(auth_src.as_bytes());
    let real_surgeon = Arc::new(TreeSitterSurgeon::new(10));
    let ws = pathfinder_common::types::WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = pathfinder_common::config::PathfinderConfig::default();
    let sandbox = pathfinder_common::sandbox::Sandbox::new(ws.path(), &config.sandbox);
    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        std::sync::Arc::new(pathfinder_search::RipgrepScout),
        real_surgeon,
        std::sync::Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    // 1. Without override — should be blocked (Login is referenced in main.go)
    let params = DeleteSymbolParams {
        semantic_path: format!("{auth_path}::Login"),
        base_version: hash.as_str().to_owned(),
        ignore_validation_failures: false,
    };

    let result = server.delete_symbol(Parameters(params)).await;
    let Err(err) = result else {
        panic!("expected INVALID_TARGET: cross-file reference should block delete");
    };

    let code = err
        .data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(code, "INVALID_TARGET", "got: {err:?}");
    // The human-readable reason is in data["message"], not in err.message
    // (err.message holds the MCP error code string, e.g. "INVALID_TARGET").
    let reason = err
        .data
        .as_ref()
        .and_then(|d| d.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        reason.contains("Login"),
        "Error reason should name the symbol: {reason}"
    );

    // 2. With override — should succeed despite cross-file reference
    let rehash = VersionHash::compute(auth_src.as_bytes());
    let params_override = DeleteSymbolParams {
        semantic_path: format!("{auth_path}::Login"),
        base_version: rehash.as_str().to_owned(),
        ignore_validation_failures: true,
    };

    let result2 = server
        .delete_symbol(Parameters(params_override))
        .await
        .expect("ignore_validation_failures:true should bypass reference check");
    assert!(result2.0.success);

    let written = std::fs::read_to_string(&auth_abs).unwrap();
    assert!(!written.contains("func Login"), "written: {written}");

    // main_abs exists to set up the cross-file reference; not read post-test.
    let _ = main_abs;
}

// ── GAP-006: Warn when insert_into targets a Rust struct ───────────

// ── GAP-006: Warn when insert_into targets a Rust struct ───────────

#[tokio::test]
async fn test_insert_into_rust_struct_includes_warning() {
    use pathfinder_treesitter::treesitter_surgeon::TreeSitterSurgeon;
    let ws_dir = tempdir().expect("temp dir");

    // Write a Rust file with a struct
    let src = "pub struct Calculator {\n    pub last_result: i32,\n}\n";
    let filepath = "src/calc.rs";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, src).unwrap();

    let hash = VersionHash::compute(src.as_bytes());

    let server = crate::server::PathfinderServer::with_all_engines(
        pathfinder_common::types::WorkspaceRoot::new(ws_dir.path()).unwrap(),
        pathfinder_common::config::PathfinderConfig::default(),
        pathfinder_common::sandbox::Sandbox::new(
            ws_dir.path(),
            &pathfinder_common::config::PathfinderConfig::default().sandbox,
        ),
        std::sync::Arc::new(pathfinder_search::RipgrepScout),
        std::sync::Arc::new(TreeSitterSurgeon::new(10)),
        std::sync::Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = crate::server::types::InsertIntoParams {
        semantic_path: format!("{filepath}::Calculator"),
        base_version: hash.as_str().to_owned(),
        new_code: "    pub fn reset(&mut self) { self.last_result = 0; }".to_owned(),
        ignore_validation_failures: true,
    };

    let result = server
        .insert_into(Parameters(params))
        .await
        .expect("insert_into should succeed");

    assert!(result.0.success, "edit should succeed");
    assert!(
        result.0.warning.is_some(),
        "should include warning for Rust struct target"
    );
    let warning = result.0.warning.unwrap();
    assert!(
        warning.contains("impl"),
        "warning should mention impl blocks: {warning}"
    );
}

#[tokio::test]
async fn test_insert_into_non_rust_no_warning() {
    use pathfinder_treesitter::treesitter_surgeon::TreeSitterSurgeon;
    let ws_dir = tempdir().expect("temp dir");

    // Write a TypeScript file with a class
    let src = "class Calculator {\n    result: number = 0;\n}\n";
    let filepath = "src/calc.ts";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, src).unwrap();

    let hash = VersionHash::compute(src.as_bytes());

    let server = crate::server::PathfinderServer::with_all_engines(
        pathfinder_common::types::WorkspaceRoot::new(ws_dir.path()).unwrap(),
        pathfinder_common::config::PathfinderConfig::default(),
        pathfinder_common::sandbox::Sandbox::new(
            ws_dir.path(),
            &pathfinder_common::config::PathfinderConfig::default().sandbox,
        ),
        std::sync::Arc::new(pathfinder_search::RipgrepScout),
        std::sync::Arc::new(TreeSitterSurgeon::new(10)),
        std::sync::Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = crate::server::types::InsertIntoParams {
        semantic_path: format!("{filepath}::Calculator"),
        base_version: hash.as_str().to_owned(),
        new_code: "    reset() { this.result = 0; }".to_owned(),
        ignore_validation_failures: true,
    };

    let result = server
        .insert_into(Parameters(params))
        .await
        .expect("insert_into should succeed");

    assert!(result.0.success, "edit should succeed");
    assert!(
        result.0.warning.is_none(),
        "non-Rust file should not produce struct warning"
    );
}

#[tokio::test]
async fn test_insert_into_rust_impl_no_warning() {
    use pathfinder_treesitter::treesitter_surgeon::TreeSitterSurgeon;
    let ws_dir = tempdir().expect("temp dir");

    // Write a Rust file with an impl block
    let src = "pub struct Calc {}\nimpl Calc {\n    pub fn add(&self) {}\n}\n";
    let filepath = "src/calc.rs";
    let abs = ws_dir.path().join(filepath);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, src).unwrap();

    let hash = VersionHash::compute(src.as_bytes());

    let server = crate::server::PathfinderServer::with_all_engines(
        pathfinder_common::types::WorkspaceRoot::new(ws_dir.path()).unwrap(),
        pathfinder_common::config::PathfinderConfig::default(),
        pathfinder_common::sandbox::Sandbox::new(
            ws_dir.path(),
            &pathfinder_common::config::PathfinderConfig::default().sandbox,
        ),
        std::sync::Arc::new(pathfinder_search::RipgrepScout),
        std::sync::Arc::new(TreeSitterSurgeon::new(10)),
        std::sync::Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = crate::server::types::InsertIntoParams {
        semantic_path: format!("{filepath}::impl Calc"),
        base_version: hash.as_str().to_owned(),
        new_code: "    pub fn reset(&mut self) { }".to_owned(),
        ignore_validation_failures: true,
    };

    let result = server
        .insert_into(Parameters(params))
        .await
        .expect("insert_into should succeed");

    assert!(result.0.success, "edit should succeed");
    assert!(
        result.0.warning.is_none(),
        "impl block target should not produce struct warning"
    );
}
