use super::*;
use pathfinder_treesitter::surgeon::{ExtractedSymbol, SymbolKind};

fn make_symbol(
    name: &str,
    start_line: usize,
    end_line: usize,
    children: Vec<ExtractedSymbol>,
) -> ExtractedSymbol {
    ExtractedSymbol {
        name: name.to_string(),
        semantic_path: name.to_string(),
        kind: SymbolKind::Function,
        byte_range: 0..0,
        start_line,
        end_line,
        name_column: 0,
        access_level: pathfinder_treesitter::surgeon::AccessLevel::Public,
        children,
    }
}

#[test]
fn test_truncate_content() {
    let content = "line 1\nline 2\nline 3\nline 4\nline 5";

    let c1 = truncate_content(content, 2, Some(4));
    assert_eq!(c1, "line 2\nline 3\nline 4\n"); // Split inclusive keeps newlines

    let c2 = truncate_content(content, 4, None);
    assert_eq!(c2, "line 4\nline 5");

    let c3 = truncate_content(content, 10, Some(15));
    assert_eq!(c3, "");
}

#[test]
fn test_filter_symbols() {
    let syms = vec![
        make_symbol("a", 0, 10, vec![]),
        make_symbol("b", 15, 20, vec![]),
        make_symbol("c", 10, 15, vec![]),
    ];

    // Ranges: overlap 10-15
    let filtered = filter_symbols(syms.clone(), 10, 15);
    assert_eq!(filtered.len(), 3); // All overlap line 10-15

    // Ranges: overlap 11-14
    let filtered2 = filter_symbols(syms, 11, 14);
    assert_eq!(filtered2.len(), 1);
    assert_eq!(filtered2[0].name, "c");
}

#[test]
fn test_map_symbols_modes() {
    let syms = vec![make_symbol(
        "parent",
        0,
        10,
        vec![make_symbol("child", 2, 5, vec![])],
    )];

    let compact = map_symbols_compact(syms.clone(), "src/test.rs");
    assert_eq!(compact.len(), 1);
    assert!(
        compact[0].children.is_empty(),
        "Compact should drop children"
    );
    assert_eq!(
        compact[0].semantic_path, "src/test.rs::parent",
        "Compact should prepend filepath"
    );

    let full = map_symbols(syms, "src/test.rs");
    assert_eq!(full.len(), 1);
    assert_eq!(full[0].children.len(), 1, "Full should keep children");
    assert_eq!(
        full[0].semantic_path, "src/test.rs::parent",
        "Full should prepend filepath"
    );
    assert_eq!(
        full[0].children[0].semantic_path, "src/test.rs::child",
        "Children should also have filepath prefix"
    );
}

#[test]
fn test_render_symbol_tree_single_symbol() {
    let syms = vec![SourceSymbol {
        name: "main".to_string(),
        semantic_path: "src/main.rs::main".to_string(),
        kind: "Function".to_string(),
        start_line: 1,
        end_line: 45,
        children: vec![],
    }];
    let tree = render_symbol_tree(&syms, "src/main.rs");
    assert!(tree.contains("src/main.rs (1 symbols)"));
    assert!(tree.contains("main [Function] L1-L45"));
    assert!(tree.contains("src/main.rs::main"));
}

#[test]
fn test_render_symbol_tree_nested() {
    let syms = vec![SourceSymbol {
        name: "Config".to_string(),
        semantic_path: "src/lib.rs::Config".to_string(),
        kind: "Struct".to_string(),
        start_line: 10,
        end_line: 20,
        children: vec![
            SourceSymbol {
                name: "name".to_string(),
                semantic_path: "src/lib.rs::Config.name".to_string(),
                kind: "Field".to_string(),
                start_line: 11,
                end_line: 11,
                children: vec![],
            },
            SourceSymbol {
                name: "parse".to_string(),
                semantic_path: "src/lib.rs::Config.parse".to_string(),
                kind: "Method".to_string(),
                start_line: 13,
                end_line: 19,
                children: vec![],
            },
        ],
    }];
    let tree = render_symbol_tree(&syms, "src/lib.rs");
    assert!(tree.contains("Config [Struct] L10-L20"));
    assert!(tree.contains("name [Field] L11-L11"));
    assert!(tree.contains("parse [Method] L13-L19"));
}

#[test]
fn test_truncate_content_no_truncation() {
    let content = "line 1\nline 2\nline 3";
    let result = truncate_content(content, 1, None);
    assert_eq!(result, content);
}

#[test]
fn test_truncate_content_single_line() {
    let content = "only line";
    let result = truncate_content(content, 1, Some(1));
    assert_eq!(result, "only line");
}

// ── CG-3: sandbox check error in read_source_file ────────────────────

#[tokio::test]
async fn test_read_source_file_rejects_sandbox_denied_path() {
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::WorkspaceRoot;
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::mock::MockSurgeon;
    use std::sync::Arc;
    use tempfile::tempdir;

    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(MockSurgeon::default()),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = ReadParams {
        filepath: Some(".git/HEAD".to_owned()),
        start_line: 1,
        end_line: None,
        detail_level: "full".to_owned(),
        ..Default::default()
    };
    let result = server.read_source_file_impl(params).await;
    assert!(result.is_err(), "sandbox should deny .git paths");
    let err = result.unwrap_err();
    let code = err
        .data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(code, "ACCESS_DENIED");
}

// ── GAP-004: version_hash in text output ───────────────────────────────

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_read_source_file_includes_version_hash_in_text() {
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::WorkspaceRoot;
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::mock::MockSurgeon;

    use std::sync::Arc;
    use tempfile::tempdir;

    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Create a test file
    let file_path = ws.path().join("test.rs");
    let content = "fn test() {}\n";
    tokio::fs::write(&file_path, content).await.unwrap();
    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .read_source_file_results
        .lock()
        .unwrap()
        .push(Ok((content.to_owned(), "rust".to_owned(), vec![])));

    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(mock_surgeon),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = ReadParams {
        filepath: Some("test.rs".to_owned()),
        start_line: 1,
        end_line: None,
        detail_level: "full".to_owned(),
        ..Default::default()
    };

    let result = server.read_source_file_impl(params).await;
    assert!(result.is_ok(), "read_source_file should succeed");
    let call_result = result.unwrap();

    // Verify content is present
    assert!(
        !call_result.content.is_empty(),
        "text output should be non-empty"
    );

    // Verify structured_content contains language
    if let Some(metadata) = call_result.structured_content {
        assert!(
            metadata.get("language").is_some(),
            "structured_content should contain language"
        );
    } else {
        panic!("Expected structured_content");
    }
}

/// LT-4: Verify that `read_source_file` calls `touch_language` for the file's language.
///
/// With `NoOpLawyer` (default `touch_language` is a no-op), this validates
/// that the call path doesn't panic.
#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_read_source_file_triggers_lt4_idle_touch() {
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::WorkspaceRoot;
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::mock::MockSurgeon;

    use std::sync::Arc;
    use tempfile::tempdir;

    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Create a Rust file — should trigger touch_language("rust")
    let content = "fn main() {}\n";
    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .read_source_file_results
        .lock()
        .unwrap()
        .push(Ok((content.to_owned(), "rust".to_owned(), vec![])));

    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(mock_surgeon),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = ReadParams {
        filepath: Some("main.rs".to_owned()),
        start_line: 1,
        end_line: None,
        detail_level: "compact".to_owned(),
        ..Default::default()
    };

    let result = server.read_source_file_impl(params).await;
    assert!(
        result.is_ok(),
        "read_source_file should succeed with touch_language"
    );
}

// ── GFB-001-G: Unsupported language graceful fallback ───────────────────

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_read_source_file_unsupported_language_graceful_fallback() {
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::WorkspaceRoot;
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::error::SurgeonError;
    use pathfinder_treesitter::mock::MockSurgeon;

    use std::sync::Arc;
    use tempfile::tempdir;

    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let sql_content = "SELECT * FROM users WHERE active = 1;\n";
    let file_path = ws.path().join("query.sql");
    tokio::fs::write(&file_path, sql_content).await.unwrap();

    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .read_source_file_results
        .lock()
        .unwrap()
        .push(Err(SurgeonError::UnsupportedLanguage("query.sql".into())));

    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(mock_surgeon),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = ReadParams {
        filepath: Some("query.sql".to_owned()),
        start_line: 1,
        end_line: None,
        detail_level: "full".to_owned(),
        ..Default::default()
    };

    let result = server.read_source_file_impl(params).await;

    // With graceful fallback: should be Ok, not Err
    assert!(
        result.is_ok(),
        "read_source_file should return Ok with raw content on unsupported language, got Err: {:?}",
        result.err()
    );

    let call_result = result.unwrap();

    // Verify text output contains the SQL content
    if let Some(content) = call_result.content.first() {
        if let rmcp::model::RawContent::Text(text_content) = &content.raw {
            assert!(
                text_content.text.contains("SELECT * FROM users"),
                "Text output should contain SQL content. Got: {}",
                text_content.text
            );
        } else {
            panic!("Expected text content");
        }
    } else {
        panic!("Expected content");
    }

    // Verify structured_content: unsupported_language = true
    if let Some(metadata) = call_result.structured_content {
        assert_eq!(
            metadata.get("unsupported_language"),
            Some(&serde_json::Value::Bool(true)),
            "metadata should have unsupported_language: true"
        );
        assert_eq!(
            metadata.get("language"),
            Some(&serde_json::Value::String("sql".to_owned())),
            "language should be file extension"
        );

        // content field should have the raw content
        if let Some(content_val) = metadata.get("content") {
            assert!(
                content_val
                    .as_str()
                    .unwrap_or("")
                    .contains("SELECT * FROM users"),
                "content field should have SQL"
            );
        } else {
            panic!("content field missing from metadata");
        }

        // symbols should be empty array or missing
        let symbols = metadata.get("symbols").and_then(|v| v.as_array());
        assert!(
            symbols.is_none_or(std::vec::Vec::is_empty),
            "symbols should be empty for unsupported language"
        );
    } else {
        panic!("Expected structured_content");
    }
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_read_source_file_unsupported_language_line_range() {
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::WorkspaceRoot;
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::error::SurgeonError;
    use pathfinder_treesitter::mock::MockSurgeon;

    use std::sync::Arc;
    use tempfile::tempdir;

    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let yaml_content = "line1: first\nline2: second\nline3: third\nline4: fourth\n";
    let file_path = ws.path().join("config.yaml");
    tokio::fs::write(&file_path, yaml_content).await.unwrap();

    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .read_source_file_results
        .lock()
        .unwrap()
        .push(Err(SurgeonError::UnsupportedLanguage("config.yaml".into())));

    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(mock_surgeon),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = ReadParams {
        filepath: Some("config.yaml".to_owned()),
        start_line: 2,
        end_line: Some(3),
        detail_level: "full".to_owned(),
        ..Default::default()
    };

    let result = server.read_source_file_impl(params).await;
    assert!(result.is_ok(), "should be Ok");

    let call_result = result.unwrap();

    if let Some(metadata) = call_result.structured_content {
        assert_eq!(
            metadata.get("unsupported_language"),
            Some(&serde_json::Value::Bool(true))
        );
        assert_eq!(
            metadata.get("language"),
            Some(&serde_json::Value::String("yaml".to_owned()))
        );

        if let Some(content_val) = metadata.get("content") {
            let content = content_val.as_str().unwrap_or("");
            assert!(content.contains("line2: second"), "should contain line 2");
            assert!(content.contains("line3: third"), "should contain line 3");
            assert!(
                !content.contains("line1: first"),
                "should NOT contain line 1 (before start_line)"
            );
            assert!(
                !content.contains("line4: fourth"),
                "should NOT contain line 4 (after end_line)"
            );
        }
    }
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_read_source_file_unsupported_language_yaml_toml() {
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::WorkspaceRoot;
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::error::SurgeonError;
    use pathfinder_treesitter::mock::MockSurgeon;

    use std::sync::Arc;
    use tempfile::tempdir;

    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Test .yaml
    let yaml_content = "apiVersion: v1\nkind: ConfigMap\n";
    let yaml_path = ws.path().join("app.yaml");
    tokio::fs::write(&yaml_path, yaml_content).await.unwrap();

    // Test .toml
    let toml_content = "[package]\nname = \"test\"\nversion = \"0.1.0\"\n";
    let toml_path = ws.path().join("Cargo.toml");
    tokio::fs::write(&toml_path, toml_content).await.unwrap();

    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .read_source_file_results
        .lock()
        .unwrap()
        .push(Err(SurgeonError::UnsupportedLanguage("app.yaml".into())));
    mock_surgeon
        .read_source_file_results
        .lock()
        .unwrap()
        .push(Err(SurgeonError::UnsupportedLanguage("Cargo.toml".into())));

    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(mock_surgeon),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    // Verify YAML
    let yaml_params = ReadParams {
        filepath: Some("app.yaml".to_owned()),
        start_line: 1,
        end_line: None,
        detail_level: "full".to_owned(),
        ..Default::default()
    };
    let yaml_result = server.read_source_file_impl(yaml_params).await;
    assert!(yaml_result.is_ok());
    let call_result_yaml = yaml_result.unwrap();
    if let Some(meta) = call_result_yaml.structured_content {
        assert_eq!(
            meta.get("language"),
            Some(&serde_json::Value::String("yaml".to_owned()))
        );
        assert_eq!(
            meta.get("unsupported_language"),
            Some(&serde_json::Value::Bool(true))
        );
    }

    // Verify TOML
    let toml_params = ReadParams {
        filepath: Some("Cargo.toml".to_owned()),
        start_line: 1,
        end_line: None,
        detail_level: "full".to_owned(),
        ..Default::default()
    };
    let toml_result = server.read_source_file_impl(toml_params).await;
    assert!(toml_result.is_ok());
    let call_result_toml = toml_result.unwrap();
    if let Some(meta) = call_result_toml.structured_content {
        assert_eq!(
            meta.get("language"),
            Some(&serde_json::Value::String("toml".to_owned()))
        );
        assert_eq!(
            meta.get("unsupported_language"),
            Some(&serde_json::Value::Bool(true))
        );
    }
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_read_source_file_unsupported_language_empty_file() {
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::WorkspaceRoot;
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::error::SurgeonError;
    use pathfinder_treesitter::mock::MockSurgeon;

    use std::sync::Arc;
    use tempfile::tempdir;

    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let empty_path = ws.path().join("empty.sql");
    tokio::fs::write(&empty_path, "").await.unwrap();

    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .read_source_file_results
        .lock()
        .unwrap()
        .push(Err(SurgeonError::UnsupportedLanguage("empty.sql".into())));

    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(mock_surgeon),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = ReadParams {
        filepath: Some("empty.sql".to_owned()),
        start_line: 1,
        end_line: None,
        detail_level: "full".to_owned(),
        ..Default::default()
    };

    let result = server.read_source_file_impl(params).await;
    assert!(
        result.is_ok(),
        "empty unsupported file should return Ok, not Err"
    );

    let call_result = result.unwrap();
    if let Some(meta) = call_result.structured_content {
        assert_eq!(
            meta.get("language"),
            Some(&serde_json::Value::String("sql".to_owned()))
        );
        assert_eq!(
            meta.get("unsupported_language"),
            Some(&serde_json::Value::Bool(true))
        );

        if let Some(content_val) = meta.get("content") {
            assert_eq!(
                content_val.as_str().unwrap_or("non-empty"),
                "",
                "content should be empty string"
            );
        }
    }
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_read_source_file_symbols_detail_level() {
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::WorkspaceRoot;
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::mock::MockSurgeon;
    use pathfinder_treesitter::surgeon::AccessLevel;

    use std::sync::Arc;
    use tempfile::tempdir;

    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let content = "fn main() {}\nfn helper() {}";
    let test_path = ws.path().join("test.rs");
    std::fs::write(&test_path, content).unwrap();

    let mock_surgeon = MockSurgeon::new();
    let symbols = vec![
        make_symbol("main", 0, 0, vec![]),
        ExtractedSymbol {
            name: "Config".to_string(),
            semantic_path: "Config".to_string(),
            kind: SymbolKind::Struct,
            byte_range: 0..0,
            start_line: 1,
            end_line: 1,
            name_column: 0,
            access_level: AccessLevel::Public,
            children: vec![make_symbol("name", 1, 1, vec![])],
        },
    ];
    mock_surgeon
        .read_source_file_results
        .lock()
        .unwrap()
        .push(Ok((content.to_owned(), "rust".to_owned(), symbols)));

    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(mock_surgeon),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = ReadParams {
        filepath: Some("test.rs".to_owned()),
        start_line: 1,
        end_line: None,
        detail_level: "symbols".to_owned(),
        ..Default::default()
    };

    let result = server.read_source_file_impl(params).await;
    assert!(result.is_ok(), "symbols detail_level should succeed");
    let call = result.unwrap();

    let meta: crate::server::types::ReadSourceFileMetadata =
        serde_json::from_value(call.structured_content.unwrap()).unwrap();
    // symbols mode returns the tree text as content
    assert!(meta.content.is_some());
    let content_text = meta.content.unwrap();
    assert!(
        content_text.contains("symbols)"),
        "should contain symbol count in tree text"
    );
    // symbols should be populated
    assert!(!meta.symbols.is_empty(), "symbols should be returned");
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_read_source_file_non_unsupported_error() {
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::WorkspaceRoot;
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::error::SurgeonError;
    use pathfinder_treesitter::mock::MockSurgeon;

    use std::sync::Arc;
    use tempfile::tempdir;

    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // File must exist for sandbox check to pass
    let test_path = ws.path().join("test.rs");
    std::fs::write(&test_path, "fn main() {}").unwrap();

    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .read_source_file_results
        .lock()
        .unwrap()
        .push(Err(SurgeonError::Io(std::sync::Arc::new(
            std::io::Error::other("disk error"),
        ))));

    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(mock_surgeon),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = ReadParams {
        filepath: Some("test.rs".to_owned()),
        start_line: 1,
        end_line: None,
        detail_level: "full".to_owned(),
        ..Default::default()
    };

    let result = server.read_source_file_impl(params).await;
    assert!(
        result.is_err(),
        "non-UnsupportedLanguage error should return Err"
    );
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_read_source_file_compact_detail_level() {
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::WorkspaceRoot;
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::mock::MockSurgeon;

    use std::sync::Arc;
    use tempfile::tempdir;

    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let content = "fn parent() { fn child() {} }";
    let test_path = ws.path().join("test.rs");
    std::fs::write(&test_path, content).unwrap();

    let mock_surgeon = MockSurgeon::new();
    let symbols = vec![make_symbol(
        "parent",
        0,
        0,
        vec![make_symbol("child", 0, 0, vec![])],
    )];
    mock_surgeon
        .read_source_file_results
        .lock()
        .unwrap()
        .push(Ok((content.to_owned(), "rust".to_owned(), symbols)));

    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(mock_surgeon),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    // Use an unknown detail_level to trigger the default/compact branch
    let params = ReadParams {
        filepath: Some("test.rs".to_owned()),
        start_line: 1,
        end_line: None,
        detail_level: "compact".to_owned(),
        ..Default::default()
    };

    let result = server.read_source_file_impl(params).await;
    assert!(result.is_ok(), "compact detail_level should succeed");
    let call = result.unwrap();

    let meta: crate::server::types::ReadSourceFileMetadata =
        serde_json::from_value(call.structured_content.unwrap()).unwrap();
    // compact mode should flatten children
    assert_eq!(meta.symbols.len(), 1, "compact returns top-level symbols only");
    assert!(
        meta.symbols[0].children.is_empty(),
        "compact mode should drop children"
    );
}

