//! `find_symbol` tool — Resolve a bare symbol name to its `file::symbol` semantic path(s).

use crate::server::helpers::io_error_data;
use crate::server::types::{FindSymbolParams, FindSymbolResponse, FoundSymbol};
use crate::server::PathfinderServer;
use pathfinder_search::SearchParams;
use regex::Regex;
use rmcp::handler::server::wrapper::Json;
use rmcp::model::ErrorData;
use std::path::Path;

/// Helper to calculate relevance score for sorting.
///
/// - 3 points for exact name match
/// - 2 points for prefix match
/// - 1 point for contains match
#[must_use]
fn relevance_score(query: &str, match_text: &str) -> u8 {
    match match_text == query {
        true => 3,
        false if match_text.starts_with(query) => 2,
        false if match_text.contains(query) => 1,
        false => 0,
    }
}

impl PathfinderServer {
    /// Core logic for the `find_symbol` tool.
    ///
    /// Runs ripgrep across the workspace with language-aware definition patterns
    /// (from spec 007), then enriches each match with Tree-sitter to get the
    /// enclosing semantic path and symbol kind.
    ///
    /// Results are:
    /// 1. Deduplicated by semantic path
    /// 2. Sorted by relevance (exact match > prefix match > contains match)
    /// 3. Filtered by optional `kind` parameter
    /// 4. Limited to `max_results` entries
    pub(crate) async fn find_symbol_impl(
        &self,
        params: FindSymbolParams,
    ) -> Result<Json<FindSymbolResponse>, ErrorData> {
        let start = std::time::Instant::now();

        tracing::info!(
            tool = "find_symbol",
            name = %params.name,
            kind = ?params.kind,
            path_glob = %params.path_glob,
            max_results = params.max_results,
            "find_symbol: start"
        );

        if params.name.trim().is_empty() {
            return Err(io_error_data("name must not be empty"));
        }

        // Validate name doesn't contain path traversal or regex metacharacters
        let name_lower = params.name.to_lowercase();
        if name_lower.contains("..") || name_lower.contains('/') || name_lower.contains('\\') {
            return Err(io_error_data("name must not contain path separators"));
        }

        // Build regex patterns for each source file extension
        // We'll search for the symbol name in Rust, TypeScript, Python, Go files
        let mut patterns = Vec::new();

        // Check if path_glob already filters by extension
        let has_ext_filter = params.path_glob.contains("*.rs")
            || params.path_glob.contains("*.ts")
            || params.path_glob.contains("*.tsx")
            || params.path_glob.contains("*.js")
            || params.path_glob.contains("*.jsx")
            || params.path_glob.contains("*.py")
            || params.path_glob.contains("*.go");

        // Build patterns for all supported languages unless filtered
        let extensions = if has_ext_filter {
            // Let the path_glob handle filtering; use bare word pattern
            vec![("any", params.path_glob.clone())]
        } else {
            vec![
                ("rs", String::from("**/*.rs")),
                ("ts", String::from("**/*.ts")),
                ("tsx", String::from("**/*.tsx")),
                ("js", String::from("**/*.js")),
                ("jsx", String::from("**/*.jsx")),
                ("py", String::from("**/*.py")),
                ("go", String::from("**/*.go")),
            ]
        };

        for (ext, glob) in extensions {
            let ext_patterns =
                crate::server::tools::navigation::definition_patterns(ext, &params.name);
            for pattern in ext_patterns {
                patterns.push((pattern, glob.clone()));
            }
        }

        let mut all_matches: Vec<FoundSymbol> = Vec::new();
        let mut any_degraded = false;

        // When the user's path_glob is not an extension filter and is not a
        // catch-all, extract a directory prefix for post-search filtering.
        // RipgrepScout uses extension-specific globs ("**/*.rs" etc.) for the
        // search, so the user's directory-scoped path_glob is applied here as
        // an additional filter on results.
        let user_path_prefix: Option<String> =
            if !has_ext_filter && !params.path_glob.is_empty() && params.path_glob != "**/*" {
                // Strip trailing glob wildcards to get the directory prefix.
                // "crates/foo/**" → "crates/foo/",  "src/**/*.rs" → "src/"
                let trimmed = params
                    .path_glob
                    .trim_end_matches("/**")
                    .trim_end_matches("/*");
                if trimmed.is_empty() || trimmed == "." {
                    None
                } else {
                    Some(format!("{trimmed}/"))
                }
            } else {
                None
            };

        // Search for each pattern
        for (pattern, glob) in patterns {
            let search_params = SearchParams {
                workspace_root: self.workspace_root.path().to_path_buf(),
                query: pattern.clone(),
                is_regex: true,
                path_glob: glob,
                exclude_glob: "**/node_modules/**".to_string(),
                max_results: 100, // Collect many matches then filter/sort
                offset: 0,
                context_lines: 0,
            };

            match self.scout.search(&search_params).await {
                Ok(result) => {
                    for m in result.matches {
                        // Validate file path is safe (no path traversal)
                        if m.file.contains("..")
                            || m.file.starts_with('/')
                            || m.file.starts_with('\\')
                        {
                            tracing::warn!(
                                tool = "find_symbol",
                                file = %m.file,
                                "skipping potentially unsafe path"
                            );
                            continue;
                        }

                        let file_path = Path::new(&m.file);

                        // Skip non-workspace files
                        if !is_workspace_file(file_path, self.workspace_root.path()) {
                            continue;
                        }

                        // Apply sandbox policy check
                        if let Err(e) = self.sandbox.check(file_path) {
                            tracing::warn!(
                                tool = "find_symbol",
                                file = %m.file,
                                error = %e,
                                "sandbox denied access to file"
                            );
                            continue;
                        }

                        // Apply user's path_glob as directory filter when the
                        // search used extension-specific globs instead.
                        if let Some(ref prefix) = user_path_prefix {
                            if !m.file.starts_with(prefix.as_str()) {
                                continue;
                            }
                        }

                        // Enrich with Tree-sitter to get symbol name and kind
                        let (symbol_name, kind) = match self
                            .surgeon
                            .enclosing_symbol(
                                self.workspace_root.path(),
                                file_path,
                                usize::try_from(m.line).unwrap_or(0),
                            )
                            .await
                        {
                            Ok(Some(enclosing_symbol)) => {
                                // If enclosing_symbol is already a full semantic path (file::symbol),
                                // extract just the symbol part
                                let symbol_part = if enclosing_symbol.contains("::") {
                                    enclosing_symbol
                                        .split("::")
                                        .skip(1)
                                        .collect::<Vec<_>>()
                                        .join("::")
                                } else {
                                    enclosing_symbol.clone()
                                };

                                // Validate symbol name is non-empty
                                if symbol_part.is_empty() {
                                    tracing::warn!(
                                        tool = "find_symbol",
                                        file = %m.file,
                                        line = m.line,
                                        "Tree-sitter returned empty symbol name, using fallback"
                                    );
                                    any_degraded = true;
                                    (
                                        extract_name_from_line(&m.content),
                                        infer_kind_from_line(&m.content),
                                    )
                                } else {
                                    (symbol_part, infer_kind_from_line(&m.content))
                                }
                            }
                            Ok(None) => {
                                any_degraded = true;
                                // Fallback: use match content
                                let kind_str = infer_kind_from_line(&m.content);
                                (extract_name_from_line(&m.content), kind_str)
                            }
                            Err(_) => {
                                any_degraded = true;
                                (
                                    extract_name_from_line(&m.content),
                                    infer_kind_from_line(&m.content),
                                )
                            }
                        };

                        // Validate symbol name is non-empty
                        if symbol_name.trim().is_empty() {
                            tracing::warn!(
                                tool = "find_symbol",
                                file = %m.file,
                                line = m.line,
                                "empty symbol name, skipping match"
                            );
                            continue;
                        }

                        // Build semantic path - combine file with symbol name
                        let semantic_path = format!("{}::{}", m.file, symbol_name);

                        // Preview: first 100 chars (UTF-8 safe)
                        let preview = if m.content.chars().count() > 100 {
                            m.content.chars().take(100).collect::<String>() + "..."
                        } else {
                            m.content.clone()
                        };

                        // Filter by kind if specified
                        if let Some(ref filter_kind) = params.kind {
                            if !kind_matches_filter(&kind, filter_kind) {
                                continue;
                            }
                        }

                        all_matches.push(FoundSymbol {
                            semantic_path,
                            kind,
                            file: m.file.clone(),
                            line: m.line,
                            preview,
                        });
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        tool = "find_symbol",
                        error = %err,
                        pattern = %pattern,
                        "scout search failed"
                    );
                }
            }
        }

        // Deduplicate by semantic path
        let mut seen = std::collections::HashSet::new();
        let mut unique_matches: Vec<FoundSymbol> = Vec::new();
        for m in all_matches {
            if seen.insert(m.semantic_path.clone()) {
                unique_matches.push(m);
            }
        }

        // Sort by relevance (exact match > prefix match > contains match)
        // Use stable sort for deterministic results when scores tie
        unique_matches.sort_by(|a, b| {
            let score_a = relevance_score(
                &params.name,
                &extract_symbol_name_from_path(&a.semantic_path),
            );
            let score_b = relevance_score(
                &params.name,
                &extract_symbol_name_from_path(&b.semantic_path),
            );
            // Higher score first; use file path as tiebreaker for stability
            match score_b.cmp(&score_a) {
                std::cmp::Ordering::Equal => {
                    // Secondary sort by file path then line for deterministic order
                    match a.file.cmp(&b.file) {
                        std::cmp::Ordering::Equal => a.line.cmp(&b.line),
                        other => other,
                    }
                }
                other => other,
            }
        });

        // Limit results
        let total_found = unique_matches.len();
        unique_matches.truncate(params.max_results as usize);

        let duration_ms = start.elapsed().as_millis();
        // Always use ripgrep + treesitter; "degraded" means treesitter fell back to line inference
        let search_strategy = if any_degraded {
            "ripgrep+fallback".to_string()
        } else {
            "ripgrep+treesitter".to_string()
        };

        tracing::info!(
            tool = "find_symbol",
            total_found,
            returned = unique_matches.len(),
            duration_ms,
            search_strategy = %search_strategy,
            "find_symbol: complete"
        );

        Ok(Json(FindSymbolResponse {
            symbols: unique_matches,
            total_found: u32::try_from(total_found).unwrap_or(u32::MAX),
            search_strategy,
            duration_ms: Some(u64::try_from(duration_ms).unwrap_or(u64::MAX)),
        }))
    }
}

/// Extract just the symbol name from a semantic path (e.g., "`src/auth.ts::AuthService.login`" -> "AuthService.login")
fn extract_symbol_name_from_path(semantic_path: &str) -> String {
    semantic_path
        .split("::")
        .skip(1)
        .collect::<Vec<_>>()
        .join("::")
}

/// Infer symbol kind from line content (fallback when Tree-sitter unavailable)
fn infer_kind_from_line(line: &str) -> String {
    let lower = line.to_lowercase();
    if lower.contains("fn ") || lower.contains("function ") || lower.contains("def ") {
        "function".to_string()
    } else if lower.contains("struct ") {
        "struct".to_string()
    } else if lower.contains("class ") || lower.contains("type ") {
        "class".to_string()
    } else if lower.contains("enum ") {
        "enum".to_string()
    } else if lower.contains("trait ") || lower.contains("interface ") {
        "interface".to_string()
    } else if lower.contains("const ")
        || lower.contains("static ")
        || lower.contains("var ")
        || lower.contains("let ")
    {
        "constant".to_string()
    } else if lower.contains("mod ") || lower.contains("module ") {
        "module".to_string()
    } else if lower.contains("impl ") {
        "impl".to_string()
    } else {
        "unknown".to_string()
    }
}

/// Extract symbol name from line content (fallback when Tree-sitter unavailable)
fn extract_name_from_line(line: &str) -> String {
    let trimmed = line.trim();

    // Try to extract name after keywords
    if let Some(captures) = Regex::new(r"(?:fn|function|def|struct|class|interface|type|enum|trait|const|static|var|let|mod|impl)\s+([a-zA-Z_][a-zA-Z0-9_]*)").ok()
        .and_then(|re| re.captures(trimmed))
    {
        captures.get(1).map(|m| m.as_str().to_string()).unwrap_or_default()
    } else {
        // Fallback: take first word
        trimmed
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string()
    }
}

/// Check if symbol kind matches the filter
///
/// Uses exact match for primary kinds, and allows cross-language mappings:
/// - "function" matches: function, method, fn
/// - "class" matches: class, struct, interface
fn kind_matches_filter(kind: &str, filter: &str) -> bool {
    let kind_lower = kind.to_lowercase();
    let filter_lower = filter.to_lowercase();

    if kind_lower == filter_lower {
        return true;
    }

    // Cross-language kind mappings
    match filter_lower.as_str() {
        "function" => matches!(kind_lower.as_str(), "function" | "method" | "fn"),
        "class" => matches!(kind_lower.as_str(), "class" | "struct" | "interface"),
        "struct" => kind_lower == "struct",
        "interface" => kind_lower == "interface",
        "enum" => kind_lower == "enum",
        "constant" => matches!(kind_lower.as_str(), "constant" | "const" | "static" | "let"),
        "module" => matches!(kind_lower.as_str(), "module" | "mod" | "namespace"),
        _ => false,
    }
}

/// Check if file is in workspace (not in `node_modules`, target, .git, etc.)
fn is_workspace_file(path: &Path, workspace_root: &Path) -> bool {
    let path_str = path.to_string_lossy();

    // Skip common vendored/generated directories
    let skip_patterns = [
        "/node_modules/",
        "/target/",
        "/vendor/",
        "/.git/",
        "/dist/",
        "/build/",
    ];

    for pattern in &skip_patterns {
        if path_str.contains(pattern) {
            return false;
        }
    }

    // For relative paths (from ripgrep), skip the prefix check —
    // the scout already scopes results to the workspace root.
    if path.is_relative() {
        return true;
    }

    // For absolute paths, verify they're under the workspace root.
    let root_str = workspace_root.to_string_lossy();
    path_str.starts_with(root_str.as_ref())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::WorkspaceRoot;
    use pathfinder_search::RipgrepScout;
    use pathfinder_treesitter::mock::MockSurgeon;
    use std::sync::Arc;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_find_symbol_exact_match() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        // Create a Rust file with PathfinderServer struct
        std::fs::create_dir_all(ws_dir.path().join("crates/pathfinder/src/server")).unwrap();
        std::fs::write(
            ws_dir.path().join("crates/pathfinder/src/server.rs"),
            "pub struct PathfinderServer {}",
        )
        .unwrap();

        let scout = Arc::new(RipgrepScout);
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .enclosing_symbol_results
            .lock()
            .unwrap()
            .push(Ok(Some("PathfinderServer".to_string())));

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            scout,
            surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params = FindSymbolParams {
            name: "PathfinderServer".to_owned(),
            kind: None,
            path_glob: "**/*.rs".to_owned(),
            max_results: 10,
        };

        let result = server.find_symbol_impl(params).await;
        let response = result.expect("find_symbol should succeed");

        assert_eq!(response.0.total_found, 1);
        assert_eq!(response.0.symbols.len(), 1);
        assert_eq!(
            response.0.symbols[0].semantic_path,
            "crates/pathfinder/src/server.rs::PathfinderServer"
        );
        assert_eq!(response.0.search_strategy, "ripgrep+treesitter");
    }

    #[tokio::test]
    async fn test_find_symbol_with_kind_filter() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        // Create a Rust file with struct and function
        std::fs::create_dir_all(ws_dir.path().join("crates/pathfinder-lsp/src/client")).unwrap();
        std::fs::write(
            ws_dir
                .path()
                .join("crates/pathfinder-lsp/src/client/mod.rs"),
            "pub struct LspClient {}
pub fn LspClient() {}",
        )
        .unwrap();

        let scout = Arc::new(RipgrepScout);
        let surgeon = Arc::new(MockSurgeon::new());
        // 2 matches expected (struct + fn lines), each triggers enclosing_symbol
        surgeon
            .enclosing_symbol_results
            .lock()
            .unwrap()
            .push(Ok(Some("LspClient".to_string())));
        surgeon
            .enclosing_symbol_results
            .lock()
            .unwrap()
            .push(Ok(Some("LspClient".to_string())));

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            scout,
            surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        // Filter by struct kind
        let params = FindSymbolParams {
            name: "LspClient".to_owned(),
            kind: Some("struct".to_owned()),
            path_glob: "**/*.rs".to_owned(),
            max_results: 10,
        };

        let result = server.find_symbol_impl(params).await;
        let response = result.expect("find_symbol should succeed");

        // Should only return the struct, not the function
        assert!(response.0.symbols.iter().any(|s| s.kind == "struct"));
        assert!(!response
            .0
            .symbols
            .iter()
            .any(|s| s.kind == "function" || s.kind == "fn"));
    }

    #[tokio::test]
    async fn test_find_symbol_with_path_glob() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        // Create LspClient in multiple files
        std::fs::create_dir_all(ws_dir.path().join("crates/pathfinder-lsp/src/client")).unwrap();
        std::fs::write(
            ws_dir
                .path()
                .join("crates/pathfinder-lsp/src/client/mod.rs"),
            "pub struct LspClient {}",
        )
        .unwrap();

        std::fs::create_dir_all(ws_dir.path().join("crates/pathfinder/src")).unwrap();
        std::fs::write(
            ws_dir.path().join("crates/pathfinder/src/server.rs"),
            "pub struct LspClient {}",
        )
        .unwrap();

        let scout = Arc::new(RipgrepScout);
        let surgeon = Arc::new(MockSurgeon::new());
        // After path_glob filtering, only the LSP file match triggers enclosing_symbol
        surgeon
            .enclosing_symbol_results
            .lock()
            .unwrap()
            .push(Ok(Some("LspClient".to_string())));

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            scout,
            surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        // Search scoped to LSP crate only
        let params = FindSymbolParams {
            name: "LspClient".to_owned(),
            kind: None,
            path_glob: "crates/pathfinder-lsp/**".to_owned(),
            max_results: 10,
        };

        let result = server.find_symbol_impl(params).await;
        let response = result.expect("find_symbol should succeed");

        assert!(response.0.total_found >= 1);
        // All results should be in the LSP crate
        for symbol in &response.0.symbols {
            assert!(
                symbol.file.contains("pathfinder-lsp"),
                "All results should be in pathfinder-lsp crate: {}",
                symbol.file
            );
        }
    }

    #[tokio::test]
    async fn test_find_symbol_no_results() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let scout = Arc::new(RipgrepScout);
        let surgeon = Arc::new(MockSurgeon::new());

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            scout,
            surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params = FindSymbolParams {
            name: "NonExistentSymbol12345".to_owned(),
            kind: None,
            path_glob: "**/*".to_owned(),
            max_results: 10,
        };

        let result = server.find_symbol_impl(params).await;
        let response = result.expect("find_symbol should succeed");

        assert_eq!(response.0.total_found, 0);
        assert_eq!(response.0.symbols.len(), 0);
    }

    #[tokio::test]
    async fn test_find_symbol_sandbox_enforced() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        // Create a file in .git directory (should be filtered out)
        std::fs::create_dir_all(ws_dir.path().join(".git/hooks")).unwrap();
        std::fs::write(
            ws_dir.path().join(".git/hooks/post-commit"),
            "pub struct SandboxTest {}",
        )
        .unwrap();

        let scout = Arc::new(RipgrepScout);
        let surgeon = Arc::new(MockSurgeon::new());

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            scout,
            surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params = FindSymbolParams {
            name: "SandboxTest".to_owned(),
            kind: None,
            path_glob: "**/*".to_owned(),
            max_results: 10,
        };

        let result = server.find_symbol_impl(params).await;
        let response = result.expect("find_symbol should succeed");

        // Results from .git should be filtered out
        for symbol in &response.0.symbols {
            assert!(
                !symbol.file.contains(".git"),
                ".git files should be filtered out: {}",
                symbol.file
            );
        }
    }

    #[tokio::test]
    async fn test_find_symbol_deduplication() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        // Create a struct that matches multiple patterns (struct and keyword)
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(ws_dir.path().join("src/lib.rs"), "pub struct TestStruct {}").unwrap();

        let scout = Arc::new(RipgrepScout);
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .enclosing_symbol_results
            .lock()
            .unwrap()
            .push(Ok(Some("TestStruct".to_string())));

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            scout,
            surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params = FindSymbolParams {
            name: "TestStruct".to_owned(),
            kind: None,
            path_glob: "**/*.rs".to_owned(),
            max_results: 10,
        };

        let result = server.find_symbol_impl(params).await;
        let response = result.expect("find_symbol should succeed");

        // Should have at most 1 result (deduplicated)
        assert_eq!(response.0.total_found, 1);
        assert_eq!(response.0.symbols.len(), 1);
    }

    #[test]
    fn test_relevance_score_exact() {
        assert_eq!(relevance_score("AuthService", "AuthService"), 3);
    }

    #[test]
    fn test_relevance_score_prefix() {
        assert_eq!(relevance_score("Auth", "AuthService"), 2);
    }

    #[test]
    fn test_relevance_score_contains() {
        assert_eq!(relevance_score("Service", "AuthService"), 1);
    }

    #[test]
    fn test_relevance_score_no_match() {
        assert_eq!(relevance_score("Foo", "Bar"), 0);
    }

    #[test]
    fn test_infer_kind_from_line_fn() {
        assert_eq!(infer_kind_from_line("fn my_function() {}"), "function");
        assert_eq!(infer_kind_from_line("function myFunction() {}"), "function");
        assert_eq!(infer_kind_from_line("def my_function():"), "function");
    }

    #[test]
    fn test_infer_kind_from_line_class() {
        assert_eq!(infer_kind_from_line("struct MyStruct {}"), "struct");
        assert_eq!(infer_kind_from_line("class MyClass {}"), "class");
        assert_eq!(infer_kind_from_line("type MyType = i32;"), "class");
    }

    #[test]
    fn test_infer_kind_from_line_constant() {
        assert_eq!(infer_kind_from_line("const MAX: i32 = 100;"), "constant");
        assert_eq!(infer_kind_from_line("static COUNT: u64 = 0;"), "constant");
        assert_eq!(infer_kind_from_line("let x = 1;"), "constant");
    }

    #[test]
    fn test_extract_name_from_line() {
        assert_eq!(extract_name_from_line("fn my_function() {}"), "my_function");
        assert_eq!(extract_name_from_line("struct MyStruct {}"), "MyStruct");
        assert_eq!(extract_name_from_line("class MyClass {}"), "MyClass");
    }

    #[test]
    fn test_extract_symbol_name_from_path() {
        assert_eq!(
            extract_symbol_name_from_path("src/auth.ts::AuthService.login"),
            "AuthService.login"
        );
        assert_eq!(
            extract_symbol_name_from_path("crates/pathfinder/src/server.rs::PathfinderServer"),
            "PathfinderServer"
        );
    }

    #[test]
    fn test_kind_matches_filter() {
        assert!(kind_matches_filter("struct", "struct"));
        assert!(kind_matches_filter("Struct", "struct")); // Case insensitive
        assert!(kind_matches_filter("function", "function"));
        assert!(kind_matches_filter("fn", "function")); // fn matches function
        assert!(kind_matches_filter("method", "function")); // method matches function
        assert!(kind_matches_filter("struct", "class")); // struct matches class
        assert!(kind_matches_filter("interface", "class")); // interface matches class
    }

    #[test]
    fn test_is_workspace_file() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");

        // Create test files
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(ws_dir.path().join("src/main.rs"), "").unwrap();

        std::fs::create_dir_all(ws_dir.path().join("node_modules/pkg")).unwrap();
        std::fs::write(ws_dir.path().join("node_modules/pkg/index.js"), "").unwrap();

        std::fs::create_dir_all(ws_dir.path().join(".git/hooks")).unwrap();
        std::fs::write(ws_dir.path().join(".git/hooks/post-commit"), "").unwrap();

        let src_main = ws_dir.path().join("src/main.rs");
        let node_modules_index = ws_dir.path().join("node_modules/pkg/index.js");
        let git_hooks_postcommit = ws_dir.path().join(".git/hooks/post-commit");

        assert!(is_workspace_file(&src_main, ws.path()));
        assert!(!is_workspace_file(&node_modules_index, ws.path()));
        assert!(!is_workspace_file(&git_hooks_postcommit, ws.path()));
    }
}
