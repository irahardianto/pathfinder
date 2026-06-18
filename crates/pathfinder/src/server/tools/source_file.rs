//! `read` tool (source file mode) — AST-based full file symbol extraction via Tree-sitter.

use crate::server::helpers::{
    millis_to_u64, pathfinder_to_error_data, serialize_metadata, treesitter_error_to_error_data,
};
use crate::server::types::{ReadParams, ReadSourceFileMetadata, SourceSymbol};
use crate::server::PathfinderServer;

use rmcp::model::{CallToolResult, Content, ErrorData};

fn map_symbols(
    syms: Vec<pathfinder_treesitter::surgeon::ExtractedSymbol>,
    filepath: &str,
) -> Vec<SourceSymbol> {
    syms.into_iter()
        .map(|s| SourceSymbol {
            name: s.name,
            semantic_path: format!("{}::{}", filepath, s.semantic_path),
            kind: format!("{:?}", s.kind),
            start_line: s.start_line + 1, // AST lines are 0-indexed, UI is 1-indexed
            end_line: s.end_line + 1,
            children: map_symbols(s.children, filepath),
        })
        .collect()
}

/// Render a tree-like representation of symbols for text output.
///
/// Output format:
/// ```text
/// src/main.rs (12 symbols)
/// ├── main [fn] L1-L45
/// ├── Config [struct] L47-L62
/// │   ├── name [field] L48
/// │   └── value [field] L49
/// └── parse [fn] L64-L80
/// ```
fn render_symbol_tree(symbols: &[SourceSymbol], file_path: &str) -> String {
    let mut lines = Vec::new();
    lines.push(format!("{} ({} symbols)", file_path, symbols.len()));

    // Render top-level symbols
    for (i, sym) in symbols.iter().enumerate() {
        let is_last = i == symbols.len() - 1;
        let connector = if is_last { "└── " } else { "├── " };
        let child_prefix = if is_last { "    " } else { "│   " };

        lines.push(format!(
            "{}{} [{}] L{}-L{} ({})",
            connector, sym.name, sym.kind, sym.start_line, sym.end_line, sym.semantic_path
        ));

        // Render children recursively
        render_recursive(&sym.children, child_prefix, &mut lines);
    }

    lines.join("\n")
}

/// Helper function to render symbol tree recursively.
fn render_recursive(symbols: &[SourceSymbol], prefix: &str, output: &mut Vec<String>) {
    for (i, sym) in symbols.iter().enumerate() {
        let is_last_item = i == symbols.len() - 1;
        let connector = if is_last_item {
            "└── "
        } else {
            "├── "
        };
        let child_prefix = if is_last_item { "    " } else { "│   " };

        output.push(format!(
            "{}{}{} [{}] L{}-L{}",
            prefix, connector, sym.name, sym.kind, sym.start_line, sym.end_line
        ));

        if !sym.children.is_empty() {
            render_recursive(&sym.children, &format!("{prefix}{child_prefix}"), output);
        }
    }
}

fn map_symbols_compact(
    syms: Vec<pathfinder_treesitter::surgeon::ExtractedSymbol>,
    filepath: &str,
) -> Vec<SourceSymbol> {
    syms.into_iter()
        .map(|s| SourceSymbol {
            name: s.name,
            semantic_path: format!("{}::{}", filepath, s.semantic_path),
            kind: format!("{:?}", s.kind),
            start_line: s.start_line + 1,
            end_line: s.end_line + 1,
            children: vec![],
        })
        .collect()
}

fn filter_symbols(
    syms: Vec<pathfinder_treesitter::surgeon::ExtractedSymbol>,
    start_line_0: usize,
    end_line_0: usize,
) -> Vec<pathfinder_treesitter::surgeon::ExtractedSymbol> {
    syms.into_iter()
        .filter_map(|mut s| {
            if s.end_line >= start_line_0 && s.start_line <= end_line_0 {
                s.children = filter_symbols(s.children, start_line_0, end_line_0);
                Some(s)
            } else {
                None
            }
        })
        .collect()
}

fn truncate_content(content: &str, start_line: u32, end_line: Option<u32>) -> String {
    let start_idx = start_line.saturating_sub(1) as usize;
    if start_line > 1 || end_line.is_some() {
        let lines: Vec<&str> = content.split_inclusive('\n').collect();
        let end_idx = end_line
            .map_or(lines.len(), |l| l as usize)
            .min(lines.len());

        if start_idx < lines.len() && start_idx < end_idx {
            lines[start_idx..end_idx].concat()
        } else {
            String::default()
        }
    } else {
        content.to_string()
    }
}

impl PathfinderServer {
    /// Core logic for the `read_source_file` tool.
    ///
    /// Performs a sandbox check, then delegates to the `Surgeon` to extract
    /// the AST hierarchy and read the full source context.
    #[tracing::instrument(skip(self, params), fields(file = %params.filepath.as_deref().unwrap_or("")))]
    pub(crate) async fn read_source_file_impl(
        &self,
        params: ReadParams,
    ) -> Result<CallToolResult, ErrorData> {
        let start = std::time::Instant::now();
        tracing::info!(tool = "read_source_file", "read_source_file: start");

        let filepath = params
            .filepath
            .as_ref()
            .ok_or_else(|| rmcp::model::ErrorData::invalid_params("filepath is required", None))?;
        let file_path = std::path::Path::new(filepath);

        if let Err(e) = self.sandbox.check(file_path) {
            tracing::warn!(tool = "read_source_file", error = %e, "sandbox check failed");
            return Err(pathfinder_to_error_data(&e));
        }

        let ts_start = std::time::Instant::now();
        match self
            .surgeon
            .read_source_file(self.workspace_root.path(), file_path)
            .await
        {
            Ok((content, language, symbols)) => {
                let tree_sitter_ms = ts_start.elapsed().as_millis();
                let duration_ms = start.elapsed().as_millis();
                self.lawyer_touch_language_for_file(file_path);

                Ok(build_supported_response(
                    content,
                    language,
                    symbols,
                    &params,
                    duration_ms,
                    tree_sitter_ms,
                ))
            }
            Err(e) => {
                let tree_sitter_ms = ts_start.elapsed().as_millis();
                let duration_ms = start.elapsed().as_millis();

                if let pathfinder_treesitter::error::SurgeonError::UnsupportedLanguage(_) = e {
                    self.handle_unsupported_language_fallback(
                        file_path,
                        &params,
                        tree_sitter_ms,
                        start,
                    )
                    .await
                } else {
                    tracing::warn!(
                        tool = "read_source_file",
                        error = %e,
                        tree_sitter_ms,
                        duration_ms,
                        engines_used = ?["tree-sitter"],
                        "read_source_file: failed"
                    );
                    Err(treesitter_error_to_error_data(e))
                }
            }
        }
    }

    /// LT-4: Touch LSP idle timer for supported languages.
    fn lawyer_touch_language_for_file(&self, file_path: &std::path::Path) {
        if let Some(ext) = file_path.extension().and_then(|e| e.to_str()) {
            let lang_id = match ext {
                "rs" => Some("rust"),
                "go" => Some("go"),
                "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "vue" => Some("typescript"),
                "py" | "pyi" => Some("python"),
                "java" => Some("java"),
                _ => None,
            };
            if let Some(lang) = lang_id {
                self.lawyer.touch_language(lang);
            }
        }
    }

    /// Graceful fallback: read raw file content without AST parsing.
    #[tracing::instrument(skip(self, params, start), fields(file = %params.filepath.as_deref().unwrap_or("")))]
    async fn handle_unsupported_language_fallback(
        &self,
        file_path: &std::path::Path,
        params: &ReadParams,
        tree_sitter_ms: u128,
        start: std::time::Instant,
    ) -> Result<CallToolResult, ErrorData> {
        let abs_path = self.workspace_root.path().join(file_path);
        let language = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("unknown")
            .to_lowercase();

        let raw_content = match tokio::fs::read_to_string(&abs_path).await {
            Ok(c) => c,
            Err(io_err) => {
                let duration_ms = start.elapsed().as_millis();
                tracing::warn!(
                    tool = "read_source_file",
                    error = %io_err,
                    tree_sitter_ms,
                    duration_ms,
                    "read_source_file: unsupported language + failed to read raw"
                );
                return Err(treesitter_error_to_error_data(
                    pathfinder_treesitter::error::SurgeonError::Io(std::sync::Arc::new(io_err)),
                ));
            }
        };

        let content = truncate_content(&raw_content, params.start_line, params.end_line);
        let duration_ms = start.elapsed().as_millis();

        tracing::info!(
            tool = "read_source_file",
            tree_sitter_ms,
            duration_ms,
            %language,
            engines_used = ?["raw_file"],
            "read_source_file: graceful fallback for unsupported language"
        );

        let text = format!(
            "{content}\n[completed in {duration_ms}ms; unsupported language: raw content only]"
        );

        let metadata = ReadSourceFileMetadata {
            language,
            content: Some(content),
            symbols: vec![],
            duration_ms: Some(millis_to_u64(duration_ms)),
            unsupported_language: Some(true),
        };

        let mut result = CallToolResult::success(vec![Content::text(text)]);
        result.structured_content = serialize_metadata(&metadata);

        Ok(result)
    }
}

/// Build response for successfully-parsed file with AST symbols.
fn build_supported_response(
    mut content: String,
    language: String,
    mut symbols: Vec<pathfinder_treesitter::surgeon::ExtractedSymbol>,
    params: &ReadParams,
    duration_ms: u128,
    tree_sitter_ms: u128,
) -> CallToolResult {
    tracing::info!(
        tool = "read_source_file",
        tree_sitter_ms,
        duration_ms,
        engines_used = ?["tree-sitter"],
        "read_source_file: complete"
    );

    let start_idx = params.start_line.saturating_sub(1) as usize;
    if params.start_line > 1 || params.end_line.is_some() {
        content = truncate_content(&content, params.start_line, params.end_line);
        let end_line_0 = params
            .end_line
            .map_or(usize::MAX, |l| l.saturating_sub(1) as usize);
        symbols = filter_symbols(symbols, start_idx, end_line_0);
    }

    let filepath_str = params.filepath.as_deref().unwrap_or("");

    let (final_content, final_symbols) = match params.detail_level.as_str() {
        "source_only" => (Some(content), vec![]),
        "symbols" => {
            let syms = map_symbols(symbols, filepath_str);
            let tree_text = render_symbol_tree(&syms, filepath_str);
            (Some(tree_text), syms)
        }
        "full" => (Some(content), map_symbols(symbols, filepath_str)),
        _ => (Some(content), map_symbols_compact(symbols, filepath_str)),
    };

    let mut contents = Vec::new();
    if let Some(ref text) = final_content {
        contents.push(Content::text(format!(
            "{text}\n[completed in {duration_ms}ms]"
        )));
    }

    let metadata = ReadSourceFileMetadata {
        language,
        content: final_content,
        symbols: final_symbols,
        duration_ms: Some(millis_to_u64(duration_ms)),
        unsupported_language: None,
    };

    let mut result = CallToolResult::success(contents);
    result.structured_content = serialize_metadata(&metadata);

    result
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
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
}
