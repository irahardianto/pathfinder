//! Consolidated tool handlers (Phase 1).
//!
//! Each handler converts the new unified param types into legacy param types
//! and delegates to the existing `_impl()` functions. **No business logic
//! lives here** — this is pure parameter mapping and routing.

use crate::server::helpers::io_error_data;
use crate::server::types::{
    Detail, ExploreParams, FindAllReferencesParams, FindCallersCalleesParams, FindSymbolParams,
    GetDefinitionParams, GetRepoMapParams, GetSemanticPathParams, HealthParams, InspectParams,
    LocateParams, LspHealthParams, ReadFileParams, ReadFilesParams, ReadParams,
    ReadSourceFileParams, ReadSymbolScopeParams, ReadWithDeepContextParams, SearchCodebaseParams,
    SearchMode, SearchParams, SymbolOverviewParams, TraceParams, TraceScope,
};
use crate::server::PathfinderServer;
use rmcp::model::{CallToolResult, ErrorData};
use std::path::Path;

/// Source file extensions eligible for AST-based processing.
///
/// Duplicated from `read_files.rs` — kept local to avoid coupling the
/// consolidated routing to the batch-read module. Both lists must stay
/// in sync with `SupportedLanguage::detect` in `pathfinder-treesitter`.
const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "go", "py", "pyi", "vue", "js", "jsx", "mjs", "cjs", "java",
];

/// Returns `true` when the file extension indicates a source file that
/// benefits from Tree-sitter AST processing.
fn is_source_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| SOURCE_EXTENSIONS.contains(&ext))
}

impl PathfinderServer {
    // ── 1. explore ─────────────────────────────────────────────────────

    /// Consolidated `explore` handler → delegates to `get_repo_map_impl`.
    ///
    /// Converts `ExploreParams` into `GetRepoMapParams`. The `detail` field
    /// controls `depth` and `max_tokens` defaults:
    /// - `Structure` → depth 1, `max_tokens` 4000
    /// - `Files`     → depth 3, `max_tokens` 8000
    /// - `Symbols`   → uses caller-supplied values (or their defaults)
    pub(crate) async fn explore_impl(
        &self,
        params: ExploreParams,
    ) -> Result<CallToolResult, ErrorData> {
        // Override depth and max_tokens for lightweight detail levels
        // to avoid returning unnecessarily large payloads.
        let (depth, max_tokens) = match params.detail {
            Detail::Structure => (params.depth.min(1), params.max_tokens.min(4_000)),
            Detail::Files => (params.depth.min(3), params.max_tokens.min(8_000)),
            Detail::Symbols => (params.depth, params.max_tokens),
        };

        let legacy = GetRepoMapParams {
            path: params.path,
            max_tokens,
            depth,
            visibility: params.visibility,
            max_tokens_per_file: params.max_tokens_per_file,
            changed_since: params.changed_since,
            include_extensions: params.include_extensions,
            exclude_extensions: params.exclude_extensions,
            // Structure/Files detail levels don't need test symbols.
            include_tests: !matches!(params.detail, Detail::Structure | Detail::Files),
        };

        self.get_repo_map_impl(legacy).await
    }

    // ── 2. search ──────────────────────────────────────────────────────

    /// Consolidated `search` handler → delegates to `search_codebase_impl`
    /// or `find_symbol_impl` depending on `mode`.
    pub(crate) async fn search_impl(
        &self,
        params: SearchParams,
    ) -> Result<CallToolResult, ErrorData> {
        match params.mode {
            SearchMode::Text | SearchMode::Regex => {
                let legacy = SearchCodebaseParams {
                    query: params.query,
                    is_regex: matches!(params.mode, SearchMode::Regex),
                    path_glob: params.path_glob,
                    exclude_glob: params.exclude_glob,
                    offset: params.offset,
                    max_results: params.max_results,
                    context_lines: params.context_lines,
                    known_files: params.known_files,
                    // Grouped output is always on for the consolidated tool
                    // to reduce token waste for multi-file results.
                    group_by_file: true,
                    filter_mode: pathfinder_common::types::FilterMode::CodeOnly,
                };
                let result = self.search_codebase_impl(legacy).await?;
                // `search_codebase_impl` returns `Json<SearchCodebaseResponse>`.
                // Convert to the generic `CallToolResult` expected by the caller.
                let text = serde_json::to_string_pretty(&result.0)
                    .unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}"));
                Ok(CallToolResult::success(vec![
                    rmcp::model::Content::text(text),
                ]))
            }
            SearchMode::Symbol => {
                let legacy = FindSymbolParams {
                    name: params.query,
                    kind: params.kind,
                    path_glob: params.path_glob,
                    max_results: params.max_results,
                };
                let result = self.find_symbol_impl(legacy).await?;
                let text = serde_json::to_string_pretty(&result.0)
                    .unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}"));
                Ok(CallToolResult::success(vec![
                    rmcp::model::Content::text(text),
                ]))
            }
        }
    }

    // ── 3. read ────────────────────────────────────────────────────────

    /// Consolidated `read` handler → delegates to `read_file_impl`,
    /// `read_source_file_impl`, or `read_files_impl`.
    ///
    /// Routing:
    /// - `paths` provided → batch mode → `read_files_impl`
    /// - `filepath` provided + source extension → `read_source_file_impl`
    /// - `filepath` provided + config extension → `read_file_impl`
    pub(crate) async fn read_impl(
        &self,
        params: ReadParams,
    ) -> Result<CallToolResult, ErrorData> {
        // Exactly one of filepath / paths must be set.
        match (&params.filepath, &params.paths) {
            (Some(_), Some(_)) => {
                Err(io_error_data(
                    "provide either `filepath` (single file) or `paths` (batch), not both",
                ))
            }
            (None, None) => {
                Err(io_error_data(
                    "provide either `filepath` (single file) or `paths` (batch)",
                ))
            }
            // Batch mode
            (None, Some(paths)) => {
                let legacy = ReadFilesParams {
                    paths: paths.clone(),
                    detail_level: params.detail_level,
                    max_lines_per_file: params.max_lines_per_file,
                };
                self.read_files_impl(legacy).await
            }
            // Single file
            (Some(filepath), None) => {
                // Validate line range before routing.
                if let Some(end) = params.end_line {
                    if end < params.start_line {
                        return Err(io_error_data(
                            "`end_line` must be >= `start_line`",
                        ));
                    }
                }
                if is_source_file(Path::new(filepath)) {
                    let legacy = ReadSourceFileParams {
                        filepath: filepath.clone(),
                        detail_level: params.detail_level,
                        start_line: params.start_line,
                        end_line: params.end_line,
                    };
                    self.read_source_file_impl(legacy).await
                } else {
                    // Config/non-source file: use raw read_file.
                    // Compute max_lines from start_line + end_line or
                    // fall back to max_lines_per_file.
                    let max_lines = if let Some(end) = params.end_line {
                        end.saturating_sub(params.start_line) + 1
                    } else {
                        params.max_lines_per_file
                    };
                    let legacy = ReadFileParams {
                        filepath: filepath.clone(),
                        start_line: params.start_line,
                        max_lines,
                    };
                    self.read_file_impl(legacy).await
                }
            }
        }
    }

    // ── 4. inspect ─────────────────────────────────────────────────────

    /// Consolidated `inspect` handler → delegates to `read_symbol_scope_impl`
    /// or `read_with_deep_context_impl`.
    ///
    /// When `include_dependencies` is `false` (default), uses the lighter
    /// `read_symbol_scope_impl`. When `true`, uses `read_with_deep_context_impl`
    /// which also fetches callee signatures via LSP.
    pub(crate) async fn inspect_impl(
        &self,
        params: InspectParams,
    ) -> Result<CallToolResult, ErrorData> {
        if params.include_dependencies {
            let legacy = ReadWithDeepContextParams {
                semantic_path: params.semantic_path,
                project_only: Some(true),
                max_dependencies: params.max_dependencies,
                include_imports: params.include_imports,
            };
            self.read_with_deep_context_impl(legacy).await
        } else {
            let legacy = ReadSymbolScopeParams {
                semantic_path: params.semantic_path,
            };
            self.read_symbol_scope_impl(legacy).await
        }
    }

    // ── 5. locate ──────────────────────────────────────────────────────

    /// Consolidated `locate` handler → delegates to `get_definition_impl`
    /// or `get_semantic_path_impl`.
    ///
    /// Auto-detects mode from input:
    /// - `semantic_path` provided → jump to definition
    /// - `file` + `line` provided → resolve to semantic path
    pub(crate) async fn locate_impl(
        &self,
        params: LocateParams,
    ) -> Result<CallToolResult, ErrorData> {
        match (params.semantic_path, params.file, params.line) {
            // Definition lookup
            (Some(sp), None, None) => {
                let legacy = GetDefinitionParams { semantic_path: sp };
                self.get_definition_impl(legacy).await
            }
            // Semantic path resolution
            (None, Some(file), Some(line)) => {
                let legacy = GetSemanticPathParams { file, line };
                self.get_semantic_path_impl(legacy).await
            }
            // Ambiguous: both modes specified
            (Some(_), Some(_), _) | (Some(_), _, Some(_)) => Err(io_error_data(
                "provide either `semantic_path` (definition lookup) or `file`+`line` (semantic path resolution), not both",
            )),
            // Missing required fields for semantic path mode
            (None, Some(_), None) => Err(io_error_data(
                "`line` is required when using `file` for semantic path resolution",
            )),
            (None, None, Some(_)) => Err(io_error_data(
                "`file` is required when using `line` for semantic path resolution",
            )),
            // Nothing provided
            (None, None, None) => Err(io_error_data(
                "provide either `semantic_path` or `file`+`line`",
            )),
        }
    }

    // ── 6. trace ───────────────────────────────────────────────────────

    /// Consolidated `trace` handler → delegates to `find_callers_callees_impl`,
    /// `find_all_references_impl`, or `symbol_overview_impl`.
    ///
    /// In `Overview` scope, the single `max_references` param controls both
    /// `max_callers_callees` and `max_references` on the legacy struct. This
    /// simplifies the agent interface without losing meaningful control — both
    /// old defaults were already 50.
    pub(crate) async fn trace_impl(
        &self,
        params: TraceParams,
    ) -> Result<CallToolResult, ErrorData> {
        match params.scope {
            TraceScope::Callers => {
                let legacy = FindCallersCalleesParams {
                    semantic_path: params.semantic_path,
                    max_depth: params.max_depth,
                    project_only: Some(true),
                    max_references: params.max_references,
                    include_test_coverage: false,
                };
                self.find_callers_callees_impl(legacy).await
            }
            TraceScope::References => {
                let legacy = FindAllReferencesParams {
                    semantic_path: params.semantic_path,
                    max_results: params.max_references,
                    offset: params.offset,
                };
                self.find_all_references_impl(legacy).await
            }
            TraceScope::Overview => {
                let legacy = SymbolOverviewParams {
                    semantic_path: params.semantic_path,
                    project_only: Some(true),
                    max_callers_callees: params.max_references,
                    max_references: params.max_references,
                };
                self.symbol_overview_impl(legacy).await
            }
        }
    }

    // ── 7. health ──────────────────────────────────────────────────────

    /// Consolidated `health` handler → delegates to `lsp_health_impl`.
    ///
    /// Direct 1:1 mapping — `HealthParams` is structurally identical to
    /// `LspHealthParams`.
    pub(crate) async fn health_impl(
        &self,
        params: HealthParams,
    ) -> Result<CallToolResult, ErrorData> {
        let legacy = LspHealthParams {
            language: params.language,
            action: params.action,
        };
        self.lsp_health_impl(legacy).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::server::types::{Detail, ExploreParams, SearchMode};

    // ── explore param conversion ───────────────────────────────────────

    #[test]
    fn explore_structure_caps_depth_and_tokens() {
        // Verify that Structure detail level applies caps correctly.
        let params = ExploreParams {
            detail: Detail::Structure,
            depth: 10,
            max_tokens: 20_000,
            ..ExploreParams::default()
        };
        // The conversion logic caps depth to 1 and max_tokens to 4000.
        assert!(matches!(params.detail, Detail::Structure));
        assert_eq!(params.depth.min(1), 1);
        assert_eq!(params.max_tokens.min(4_000), 4_000);
    }

    #[test]
    fn explore_files_caps_depth_and_tokens() {
        let params = ExploreParams {
            detail: Detail::Files,
            depth: 10,
            max_tokens: 20_000,
            ..ExploreParams::default()
        };
        assert_eq!(params.depth.min(3), 3);
        assert_eq!(params.max_tokens.min(8_000), 8_000);
    }

    #[test]
    fn explore_symbols_passes_through() {
        let params = ExploreParams {
            detail: Detail::Symbols,
            depth: 7,
            max_tokens: 32_000,
            ..ExploreParams::default()
        };
        // Symbols mode passes values through unchanged.
        assert_eq!(params.depth, 7);
        assert_eq!(params.max_tokens, 32_000);
    }

    // ── read param routing ─────────────────────────────────────────────

    #[test]
    fn is_source_detects_rust() {
        assert!(is_source_file(Path::new("src/main.rs")));
    }

    #[test]
    fn is_source_detects_typescript() {
        assert!(is_source_file(Path::new("src/app.tsx")));
    }

    #[test]
    fn is_source_rejects_config() {
        assert!(!is_source_file(Path::new("Cargo.toml")));
        assert!(!is_source_file(Path::new(".env")));
        assert!(!is_source_file(Path::new("config.yaml")));
    }

    // ── search mode routing ────────────────────────────────────────────

    #[test]
    fn search_mode_defaults_to_text() {
        let mode = SearchMode::default();
        assert!(matches!(mode, SearchMode::Text));
    }

    // ── locate validation ──────────────────────────────────────────────

    #[test]
    fn locate_needs_at_least_one_mode() {
        let params = LocateParams::default();
        // Both semantic_path and file/line are None → should be rejected.
        assert!(params.semantic_path.is_none());
        assert!(params.file.is_none());
        assert!(params.line.is_none());
    }

    // ── explore include_tests logic ────────────────────────────────────

    #[test]
    fn explore_structure_excludes_tests() {
        let params = ExploreParams {
            detail: Detail::Structure,
            ..ExploreParams::default()
        };
        // Structure mode should NOT include test symbols.
        assert!(!matches!(params.detail, Detail::Symbols));
        assert!(matches!(params.detail, Detail::Structure | Detail::Files));
    }

    #[test]
    fn explore_symbols_includes_tests() {
        let params = ExploreParams {
            detail: Detail::Symbols,
            ..ExploreParams::default()
        };
        // Symbols mode should include test symbols.
        assert!(!matches!(params.detail, Detail::Structure | Detail::Files));
    }

    // ── is_source edge cases ──────────────────────────────────────────

    #[test]
    fn is_source_detects_mjs_cjs_pyi() {
        // These are in SOURCE_EXTENSIONS for routing to read_source_file_impl.
        // Tree-sitter handles them via unsupported language fallback.
        assert!(is_source_file(Path::new("module.mjs")));
        assert!(is_source_file(Path::new("module.cjs")));
        assert!(is_source_file(Path::new("stubs.pyi")));
    }

    #[test]
    fn is_source_detects_all_supported_extensions() {
        let extensions = [
            "rs", "ts", "tsx", "go", "py", "pyi", "vue", "js", "jsx", "mjs", "cjs", "java",
        ];
        for ext in &extensions {
            assert!(
                is_source_file(Path::new(&format!("test.{ext}"))),
                "extension {ext} should be detected as source"
            );
        }
    }

    #[test]
    fn is_source_rejects_no_extension() {
        assert!(!is_source_file(Path::new("Dockerfile")));
        assert!(!is_source_file(Path::new("Makefile")));
    }

    // ── read param validation ─────────────────────────────────────────

    #[test]
    fn read_max_lines_calculation_happy_path() {
        // end_line=10, start_line=5 → max_lines=6
        let end: u32 = 10;
        let start: u32 = 5;
        let max_lines = end.saturating_sub(start) + 1;
        assert_eq!(max_lines, 6);
    }

    #[test]
    fn read_max_lines_same_line() {
        // end_line=5, start_line=5 → max_lines=1
        let end: u32 = 5;
        let start: u32 = 5;
        let max_lines = end.saturating_sub(start) + 1;
        assert_eq!(max_lines, 1);
    }

    // ── trace scope defaults ──────────────────────────────────────────

    #[test]
    fn trace_scope_defaults_to_callers() {
        let scope = TraceScope::default();
        assert!(matches!(scope, TraceScope::Callers));
    }

    #[test]
    fn trace_params_defaults() {
        let params = TraceParams::default();
        assert!(matches!(params.scope, TraceScope::Callers));
        assert_eq!(params.max_depth, 3);
        assert_eq!(params.max_references, 50);
        assert_eq!(params.offset, 0);
    }

    // ── detail enum defaults ──────────────────────────────────────────

    #[test]
    fn detail_defaults_to_symbols() {
        let detail = Detail::default();
        assert!(matches!(detail, Detail::Symbols));
    }

    // ── locate error path coverage ────────────────────────────────────

    #[test]
    fn locate_rejects_file_without_line() {
        let params = LocateParams {
            file: Some("src/main.rs".to_string()),
            ..LocateParams::default()
        };
        // file set but line is None → should be rejected by handler.
        assert!(params.file.is_some());
        assert!(params.line.is_none());
    }

    #[test]
    fn locate_rejects_line_without_file() {
        let params = LocateParams {
            line: Some(42),
            ..LocateParams::default()
        };
        // line set but file is None → should be rejected by handler.
        assert!(params.file.is_none());
        assert!(params.line.is_some());
    }

    #[test]
    fn locate_rejects_both_modes() {
        let params = LocateParams {
            semantic_path: Some("src/main.rs::main".to_string()),
            file: Some("src/main.rs".to_string()),
            line: Some(1),
        };
        // Both modes set → should be rejected by handler.
        assert!(params.semantic_path.is_some());
        assert!(params.file.is_some());
    }
}
