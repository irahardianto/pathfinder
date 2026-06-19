//! `locate` tool handler (definition mode) and grep-based fallback strategies.

use crate::server::helpers::{
    format_degraded_notice, millis_to_u64, parse_semantic_path, pathfinder_to_error_data,
    require_symbol_target, serialize_metadata, treesitter_error_to_error_data,
};
use crate::server::types::{GetDefinitionResponse, LocateParams};
use crate::server::PathfinderServer;
use pathfinder_common::error::PathfinderError;
use pathfinder_common::types::DegradedReason;
use pathfinder_lsp::LspError;
use pathfinder_treesitter::symbols::did_you_mean;
use rmcp::model::ErrorData;

use super::definition_patterns;

impl PathfinderServer {
    /// Core logic for the `get_definition` tool.
    ///
    /// Resolves the semantic path to a file position, queries the LSP for the
    /// definition location, and returns the result.
    ///
    /// **Degraded mode:** Returns a `LSP_REQUIRED` error when no LSP is configured.
    // This function coordinates Tree-sitter (position resolution), LSP (goto_definition),
    // and Ripgrep (degraded fallback). It has multiple outcome paths:
    // 1. Happy path: LSP returns Some(def)
    // 2. Warmup path: LSP returns None → 3s wait → retry → grep fallback
    // 3. Degraded path: NoLspAvailable → grep fallback
    // 4. Error path: Other LspError
    // The linear structure makes the orchestration easier to understand.
    #[expect(
        clippy::too_many_lines,
        reason = "Sequential pipeline: parse → sandbox → TS → LSP (with warmup retry) → grep fallback. Extraction done at helper level; remaining orchestration is linear."
    )]
    pub(crate) async fn get_definition_impl(
        &self,
        params: LocateParams,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        let start = std::time::Instant::now();
        let semantic_path_str = params.semantic_path.clone().unwrap_or_default();

        tracing::info!(
            tool = "get_definition",
            semantic_path = %semantic_path_str,
            "get_definition: start"
        );

        // Parse and validate the semantic path
        let semantic_path = parse_semantic_path(&semantic_path_str)?;
        require_symbol_target(&semantic_path, &semantic_path_str)?;

        // Sandbox check
        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            let duration_ms = start.elapsed().as_millis();
            tracing::warn!(
                tool = "get_definition",
                error_code = e.error_code(),
                duration_ms,
                "sandbox check failed"
            );
            return Err(pathfinder_to_error_data(&e));
        }

        // Early file existence check — avoid tree-sitter parse on nonexistent files
        let abs_file = self.workspace_root.path().join(&semantic_path.file_path);
        if !abs_file.exists() {
            let err = pathfinder_common::error::PathfinderError::FileNotFound {
                path: abs_file.clone(),
            };
            tracing::warn!(
                tool = "get_definition",
                path = %abs_file.display(),
                "file not found"
            );
            return Err(pathfinder_to_error_data(&err));
        }

        // Resolve the symbol position via Tree-sitter to get line/column
        let ts_start = std::time::Instant::now();
        let symbol_scope = self
            .surgeon
            .read_symbol_scope(self.workspace_root.path(), &semantic_path)
            .await
            .map_err(treesitter_error_to_error_data)?;
        let tree_sitter_ms = ts_start.elapsed().as_millis();

        // IW-3 (DS-1 gap fix): Open the file via RAII guard so did_close is
        // guaranteed on all exit paths (success, error, early return).
        // rust-analyzer requires files to be in its document buffer to resolve
        // definitions. Without this, it returns null for all navigation.
        let file_path = self.workspace_root.path().join(&semantic_path.file_path);
        let file_content = match tokio::fs::read_to_string(&file_path).await {
            Ok(content) => content,
            Err(e) => {
                tracing::warn!(
                    tool = "get_definition",
                    path = %file_path.display(),
                    error = %e,
                    "file read failed — LSP will receive empty content, goto_definition may return null"
                );
                String::new()
            }
        };
        // `_doc_guard` is held until the end of this function; dropping it fires did_close.
        let _doc_guard = match self
            .lawyer
            .open_document(
                self.workspace_root.path(),
                &semantic_path.file_path,
                &file_content,
            )
            .await
        {
            Ok(guard) => Some(guard),
            Err(e) => {
                tracing::warn!(
                    tool = "get_definition",
                    semantic_path = %semantic_path,
                    error = %e,
                    "open_document failed — LSP queries may return degraded results"
                );
                None
            }
        };

        // Query LSP for the definition location at the symbol's start line
        let lsp_start = std::time::Instant::now();
        let lsp_result = self
            .lawyer
            .goto_definition(
                self.workspace_root.path(),
                &semantic_path.file_path,
                // Convert 0-indexed start_line from SymbolScope to 1-indexed for Lawyer
                u32::try_from(symbol_scope.start_line + 1).unwrap_or(1),
                // Position cursor on the symbol's name identifier (e.g., the 'd' in 'dedent'),
                // not the 'pub' keyword. rust-analyzer requires this for symbol resolution.
                u32::try_from(symbol_scope.name_column + 1).unwrap_or(1),
            )
            .await;
        let lsp_ms = lsp_start.elapsed().as_millis();

        // Note: `_doc_guard` is still in scope here and will fire did_close on drop.
        let duration_ms = start.elapsed().as_millis();

        match lsp_result {
            Ok(Some(def)) => {
                tracing::info!(
                    tool = "get_definition",
                    file = %def.file,
                    definition_line = def.line,
                    tree_sitter_ms,
                    lsp_ms,
                    duration_ms,
                    engines_used = ?["tree-sitter", "lsp"],
                    "get_definition: complete"
                );
                Ok(Self::get_def_to_call_result(&GetDefinitionResponse {
                    file: def.file,
                    line: def.line,
                    column: def.column,
                    preview: def.preview,
                    degraded: false,
                    degraded_reason: None,
                    actionable_guidance: None,
                    lsp_readiness: Some("ready".to_owned()),
                    warm_start_in_progress: Some(false),
                    duration_ms: Some(millis_to_u64(duration_ms)),
                    resolution_strategy: Some("lsp".to_owned()),
                }))
            }
            Ok(None) => {
                // Symbol has no definition (e.g., built-in, external) or LSP is still warming up.
                //
                // Retry once after a brief wait: if the LSP just finished indexing
                // between our did_open and the query, a second attempt often succeeds.
                // 1s is sufficient — if LSP still isn't ready, grep fallback handles it.
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;

                let retry_lsp_result = self
                    .lawyer
                    .goto_definition(
                        self.workspace_root.path(),
                        &semantic_path.file_path,
                        u32::try_from(symbol_scope.start_line + 1).unwrap_or(1),
                        u32::try_from(symbol_scope.name_column + 1).unwrap_or(1),
                    )
                    .await;

                if let Ok(Some(def)) = retry_lsp_result {
                    tracing::info!(
                        tool = "get_definition",
                        file = %def.file,
                        definition_line = def.line,
                        tree_sitter_ms,
                        lsp_ms,
                        duration_ms = start.elapsed().as_millis(),
                        engines_used = ?["tree-sitter", "lsp"],
                        "get_definition: complete (succeeded on retry after warmup wait)"
                    );
                    return Ok(Self::get_def_to_call_result(&GetDefinitionResponse {
                        file: def.file,
                        line: def.line,
                        column: def.column,
                        preview: def.preview,
                        degraded: false,
                        degraded_reason: None,
                        actionable_guidance: None,
                        lsp_readiness: Some("warming_up".to_owned()),
                        warm_start_in_progress: Some(true),
                        duration_ms: Some(millis_to_u64(start.elapsed().as_millis())),
                        resolution_strategy: Some("lsp_retry".to_owned()),
                    }));
                }

                // Re-capture duration after the 3s sleep + retry attempt
                // so downstream logs reflect the full elapsed time.
                let duration_ms = start.elapsed().as_millis();

                tracing::info!(
                    tool = "get_definition",
                    semantic_path = %semantic_path_str,
                    tree_sitter_ms,
                    lsp_ms,
                    duration_ms,
                    "get_definition: no definition found via LSP — attempting grep-based fallback"
                );

                if let Some(mut def) = self.fallback_definition_grep(&semantic_path).await {
                    if !matches!(
                        def.degraded_reason,
                        Some(DegradedReason::GrepFallbackImplScoped)
                    ) {
                        def.degraded_reason = Some(DegradedReason::LspWarmupGrepFallback);
                    }
                    def.duration_ms = Some(millis_to_u64(start.elapsed().as_millis()));
                    let duration_ms = def.duration_ms.unwrap_or(0);
                    tracing::info!(
                        tool = "get_definition",
                        file = %def.file,
                        line = def.line,
                        duration_ms,
                        degraded = true,
                        degraded_reason = "lsp_warmup_grep_fallback",
                        engines_used = ?["tree-sitter", "lsp", "ripgrep"],
                        "get_definition: degraded complete (grep fallback after LSP None)"
                    );
                    return Ok(Self::get_def_to_call_result(&def));
                }

                tracing::info!(
                    tool = "get_definition",
                    semantic_path = %semantic_path_str,
                    tree_sitter_ms,
                    lsp_ms,
                    duration_ms,
                    "get_definition: no definition found (LSP None, grep empty)"
                );
                let did_you_mean_suggestions = self.compute_did_you_mean(&semantic_path).await;

                // Spec 2.4: Check if LSP is still warming up and suggest retry delay
                let retry_after = if self.lawyer.is_warm_start_complete() {
                    None
                } else {
                    Some(10u32) // 10 seconds when warmup is in progress
                };

                Err(pathfinder_to_error_data(&PathfinderError::SymbolNotFound {
                    semantic_path: semantic_path_str,
                    did_you_mean: did_you_mean_suggestions,
                    retry_after_seconds: retry_after,
                }))
            }
            Err(LspError::NoLspAvailable) => {
                // Degraded mode — LSP not available. Use a grep-based heuristic to
                // find a likely definition location. This is not LSP-accurate but
                // gives the agent a starting point without requiring a full
                // `search_codebase` call.
                tracing::info!(
                    tool = "get_definition",
                    symbol = %semantic_path,
                    "get_definition: no LSP — attempting grep-based fallback"
                );

                if let Some(mut def) = self.fallback_definition_grep(&semantic_path).await {
                    if !matches!(
                        def.degraded_reason,
                        Some(DegradedReason::GrepFallbackImplScoped)
                    ) {
                        def.degraded_reason = Some(DegradedReason::NoLspGrepFallback);
                    }
                    def.duration_ms = Some(millis_to_u64(start.elapsed().as_millis()));
                    let duration_ms = def.duration_ms.unwrap_or(0);
                    tracing::info!(
                        tool = "get_definition",
                        file = %def.file,
                        line = def.line,
                        duration_ms,
                        degraded = true,
                        degraded_reason = "no_lsp_grep_fallback",
                        engines_used = ?["tree-sitter", "ripgrep"],
                        "get_definition: degraded complete (grep fallback)"
                    );
                    return Ok(Self::get_def_to_call_result(&def));
                }

                // No grep match either — return the original LSP error
                let duration_ms = start.elapsed().as_millis();
                tracing::info!(
                    tool = "get_definition",
                    duration_ms,
                    degraded = true,
                    degraded_reason = "no_lsp",
                    engines_used = ?["none"],
                    "get_definition: degraded (no LSP, grep fallback also empty)"
                );
                Err(pathfinder_to_error_data(&PathfinderError::NoLspAvailable {
                    language: symbol_scope.language,
                }))
            }
            Err(LspError::Timeout { .. }) => {
                // LSP timed out — attempt grep-based fallback
                tracing::info!(
                    tool = "get_definition",
                    semantic_path = %semantic_path_str,
                    "get_definition: LSP timed out — attempting grep-based fallback"
                );

                if let Some(mut def) = self.fallback_definition_grep(&semantic_path).await {
                    if !matches!(
                        def.degraded_reason,
                        Some(DegradedReason::GrepFallbackImplScoped)
                    ) {
                        def.degraded_reason = Some(DegradedReason::LspTimeoutGrepFallback);
                    }
                    def.duration_ms = Some(millis_to_u64(start.elapsed().as_millis()));
                    let duration_ms = def.duration_ms.unwrap_or(0);
                    tracing::info!(
                        tool = "get_definition",
                        file = %def.file,
                        line = def.line,
                        duration_ms,
                        degraded = true,
                        degraded_reason = "lsp_timeout_grep_fallback",
                        engines_used = ?["tree-sitter", "lsp", "ripgrep"],
                        "get_definition: degraded complete (grep fallback after timeout)"
                    );
                    return Ok(Self::get_def_to_call_result(&def));
                }

                tracing::warn!(
                    tool = "get_definition",
                    semantic_path = %semantic_path_str,
                    "get_definition: LSP timed out and grep fallback found no match"
                );
                Err(pathfinder_to_error_data(&PathfinderError::LspError {
                    message: "LSP timed out and grep fallback found no match".to_owned(),
                }))
            }
            Err(e) => {
                // Generic LSP error — attempt grep fallback before giving up.
                // Covers connection resets, protocol errors, and any other LspError variants
                // not handled by the specific arms above. This prevents agent stalls when
                // an unexpected LSP failure occurs mid-session.
                tracing::warn!(
                    tool = "get_definition",
                    error = %e,
                    tree_sitter_ms,
                    lsp_ms,
                    duration_ms,
                    engines_used = ?["lsp"],
                    "get_definition: LSP error — attempting grep fallback"
                );

                if let Some(mut def) = self.fallback_definition_grep(&semantic_path).await {
                    // Preserve strategy-specific reasons (e.g., GrepFallbackImplScoped)
                    // but override the generic GrepFallbackFileScoped with the
                    // context-specific reason from the LSP error path.
                    if !matches!(
                        def.degraded_reason,
                        Some(DegradedReason::GrepFallbackImplScoped)
                    ) {
                        def.degraded_reason = Some(DegradedReason::LspErrorGrepFallback);
                    }
                    def.degraded = true;
                    def.duration_ms = Some(millis_to_u64(start.elapsed().as_millis()));
                    let duration_ms = def.duration_ms.unwrap_or(0);
                    tracing::info!(
                        tool = "get_definition",
                        file = %def.file,
                        line = def.line,
                        duration_ms,
                        degraded = true,
                        degraded_reason = "lsp_error_grep_fallback",
                        engines_used = ?["tree-sitter", "lsp", "ripgrep"],
                        "get_definition: degraded complete (grep fallback after LSP error)"
                    );
                    return Ok(Self::get_def_to_call_result(&def));
                }

                let duration_ms = start.elapsed().as_millis();
                tracing::warn!(
                    tool = "get_definition",
                    error = %e,
                    duration_ms,
                    "get_definition: LSP error and grep fallback found no match"
                );
                Err(pathfinder_to_error_data(&PathfinderError::LspError {
                    message: e.to_string(),
                }))
            }
        }
    }

    /// Convert a `GetDefinitionResponse` into a `CallToolResult` with a
    /// human-readable text summary and the struct in `structured_content`.
    ///
    /// Mirrors the pattern used by all other tools in the suite.
    fn get_def_to_call_result(def: &GetDefinitionResponse) -> rmcp::model::CallToolResult {
        let text = if def.degraded {
            let notice = def
                .degraded_reason
                .as_ref()
                .map_or_else(|| "DEGRADED (unknown)".to_owned(), format_degraded_notice);
            format!(
                "{notice}\n{}:L{} — {}\n[completed in {}ms]",
                def.file,
                def.line,
                if def.preview.is_empty() {
                    "(no preview)"
                } else {
                    &def.preview
                },
                def.duration_ms.unwrap_or(0),
            )
        } else {
            format!(
                "{}:L{} col:{} — {}\n[completed in {}ms]",
                def.file,
                def.line,
                def.column,
                if def.preview.is_empty() {
                    "(no preview)"
                } else {
                    &def.preview
                },
                def.duration_ms.unwrap_or(0),
            )
        };
        let mut res = rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(text)]);
        res.structured_content = serialize_metadata(def);
        res
    }

    async fn compute_did_you_mean(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
    ) -> Vec<String> {
        let Some(ref symbol_chain) = semantic_path.symbol_chain else {
            return Vec::new();
        };
        let symbols = self
            .surgeon
            .extract_symbols(self.workspace_root.path(), &semantic_path.file_path)
            .await;
        let Ok(symbols) = symbols else {
            return Vec::new();
        };
        did_you_mean(&symbols, symbol_chain, 3)
    }

    /// Grep-based fallback for definition resolution when LSP is unavailable or warming up.
    ///
    /// Uses a multi-strategy approach:
    /// 1. Search the expected file first (if known from the semantic path)
    /// 2. Search for struct-qualified patterns (e.g., `impl Struct` + `fn method`)
    /// 3. Fall back to a global search (excludes test/mock files, returns first match)
    /// 4. Broad symbol search as last resort
    async fn fallback_definition_grep(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
    ) -> Option<GetDefinitionResponse> {
        let symbol_chain = semantic_path.symbol_chain.as_ref()?;
        let symbol_name = symbol_chain.segments.last()?.name.clone();
        let expected_file = &semantic_path.file_path;

        // Strategy 1: Search the expected file first (highest confidence)
        if let Some(result) = self
            .grep_definition_in_file(symbol_name.clone(), expected_file.clone())
            .await
        {
            return Some(result);
        }

        // Strategy 2: For method lookups (impl Struct), search for the impl block
        if symbol_chain.segments.len() >= 2 {
            let parent_name = symbol_chain.segments[symbol_chain.segments.len() - 2]
                .name
                .clone();
            let ext = expected_file
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            let path_glob = format!("**/*.{ext}");
            if let Some(result) = self
                .grep_impl_method(&parent_name, &symbol_name, &path_glob)
                .await
            {
                return Some(result);
            }
        }

        // Strategy 3: Global search (excludes test/mock files, returns first match)
        if let Some(result) = self.grep_definition_global(symbol_name.clone()).await {
            return Some(result);
        }

        // Spec 2.3: Strategy 4: Broad symbol search when definition patterns fail
        self.grep_symbol_broad(&symbol_name).await
    }

    /// Search for a definition within a specific file.
    ///
    /// Uses language-aware patterns from `definition_patterns` (SPEC 007).
    async fn grep_definition_in_file(
        &self,
        symbol_name: String,
        file_path: std::path::PathBuf,
    ) -> Option<GetDefinitionResponse> {
        // Extract file extension to determine which language patterns to use
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");

        // Get language-specific definition patterns
        let patterns = definition_patterns(ext, &symbol_name);

        // Use the file as a specific path glob. Convert to forward-slash
        // format for ripgrep compatibility across platforms.
        let glob = file_path.to_string_lossy().replace('\\', "/");

        // Try each pattern in sequence until a match is found
        for pattern in patterns {
            let search_result = self
                .scout
                .search(&pathfinder_search::SearchParams {
                    workspace_root: self.workspace_root.path().to_path_buf(),
                    query: pattern,
                    is_regex: true,
                    max_results: 5,
                    path_glob: glob.clone(),
                    exclude_glob: Vec::new(),
                    context_lines: 0,
                    offset: 0,
                })
                .await;

            if let Ok(result) = search_result {
                if !result.matches.is_empty() {
                    let m = &result.matches[0];
                    return Some(GetDefinitionResponse {
                        file: m.file.clone(),
                        line: u32::try_from(m.line).unwrap_or(u32::MAX),
                        column: u32::try_from(m.column).unwrap_or(1),
                        preview: m.content.clone(),
                        degraded: true,
                        degraded_reason: Some(DegradedReason::GrepFallbackFileScoped),
                        actionable_guidance: Some(
                            DegradedReason::GrepFallbackFileScoped.guidance(),
                        ),
                        lsp_readiness: Some("unavailable".to_owned()),
                        warm_start_in_progress: None,
                        duration_ms: None,
                        resolution_strategy: Some("grep_file".to_owned()),
                    });
                }
            } else if let Err(e) = search_result {
                tracing::warn!(
                    tool = "get_definition",
                    strategy = "grep_definition_in_file",
                    error = %e,
                    "scout.search failed during grep fallback"
                );
            }
        }
        None
    }

    /// Search for a method within an impl block (e.g., `impl Sandbox` containing `fn check`).
    async fn grep_impl_method(
        &self,
        parent_name: &str,
        method_name: &str,
        path_glob: &str,
    ) -> Option<GetDefinitionResponse> {
        // First find files containing the impl block
        let parent_escaped = regex::escape(parent_name);
        let impl_pattern = format!(r"impl\s+(?:<[^>]+>\s+)?{parent_escaped}\b");
        let search_result = self
            .scout
            .search(&pathfinder_search::SearchParams {
                workspace_root: self.workspace_root.path().to_path_buf(),
                query: impl_pattern,
                is_regex: true,
                max_results: 10,
                path_glob: path_glob.to_owned(),
                exclude_glob: Vec::new(),
                context_lines: 0,
                offset: 0,
            })
            .await;

        if let Ok(result) = search_result {
            for m in &result.matches {
                // Now search within this specific file for the method.
                // Use language-aware patterns based on file extension.
                let ext = std::path::Path::new(&m.file)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");
                let method_escaped = regex::escape(method_name);
                let method_pattern = match ext {
                    "rs" => format!(
                        r"(?:(?:pub|crate)\s*(?:\([^)]*\)\s*)?(?:async\s*)?)?fn\s+{method_escaped}\b"
                    ),
                    "ts" | "js" | "tsx" | "jsx" => format!(
                        r"(?:(?:export\s+(?:default\s*)?)?(?:async\s+)?)?(?:function\s+{method_escaped}\b|{method_escaped}\s*[=:])"
                    ),
                    "py" => format!(r"(?:async\s+)?def\s+{method_escaped}\b"),
                    "go" => format!(r"func\s+(?:\([^)]*\)\s+)?{method_escaped}\b"),
                    "java" => format!(
                        r"(?:public\s+|private\s+|protected\s+|static\s+|final\s+|abstract\s+)*(?:<[^>]*>\s+)?[a-zA-Z_][a-zA-Z0-9_<>\[\],\s]+\s+{method_escaped}\s*\("
                    ),
                    _ => format!(r"\b{method_escaped}\b"),
                };
                let file_search = self
                    .scout
                    .search(&pathfinder_search::SearchParams {
                        workspace_root: self.workspace_root.path().to_path_buf(),
                        query: method_pattern,
                        is_regex: true,
                        max_results: 5,
                        path_glob: m.file.clone(),
                        exclude_glob: Vec::new(),
                        context_lines: 0,
                        offset: 0,
                    })
                    .await;

                if let Ok(file_result) = file_search {
                    if !file_result.matches.is_empty() {
                        let hit = &file_result.matches[0];
                        return Some(GetDefinitionResponse {
                            file: hit.file.clone(),
                            line: u32::try_from(hit.line).unwrap_or(u32::MAX),
                            column: u32::try_from(hit.column).unwrap_or(1),
                            preview: hit.content.clone(),
                            degraded: true,
                            degraded_reason: Some(DegradedReason::GrepFallbackImplScoped),
                            actionable_guidance: Some(
                                DegradedReason::GrepFallbackImplScoped.guidance(),
                            ),
                            lsp_readiness: Some("unavailable".to_owned()),
                            warm_start_in_progress: None,
                            duration_ms: None,
                            resolution_strategy: Some("grep_impl".to_owned()),
                        });
                    }
                } else if let Err(e) = file_search {
                    tracing::warn!(
                        tool = "get_definition",
                        strategy = "grep_impl_method",
                        error = %e,
                        "scout.search failed during impl-method grep fallback"
                    );
                }
            }
        } else if let Err(e) = search_result {
            tracing::warn!(
                tool = "get_definition",
                strategy = "grep_impl_block",
                error = %e,
                "scout.search failed during impl-block grep fallback"
            );
        }
        None
    }

    /// Global search for a definition when file-scoped and impl-scoped searches fail.
    /// Avoids matching in test files and mock implementations.
    async fn grep_definition_global(&self, symbol_name: String) -> Option<GetDefinitionResponse> {
        // Match definition patterns with optional preceding visibility modifier.
        // Rust: `pub fn`, `pub(crate) fn`, `pub async fn`, bare `fn`
        // TypeScript: `export function`, `export default function`, bare `function`
        // Python: `def`, `async def`
        let name = regex::escape(&symbol_name);
        let pattern = format!(
            r"(?:(?:pub|export|public|private|protected|internal|open)\s*(?:\([^)]*\)\s*)?(?:async\s*)?)?(?:fn|def|func|function|class|struct|type|interface|const|let|var|enum|trait|mod)\s+{name}\b"
        );

        let search_result = self
            .scout
            .search(&pathfinder_search::SearchParams {
                workspace_root: self.workspace_root.path().to_path_buf(),
                query: pattern,
                is_regex: true,
                max_results: 10,
                path_glob: "**/*".to_owned(),
                // Exclude test files and mock implementations to prefer real definitions
                exclude_glob: vec!["**/{test,tests,mock}*/**".to_owned()],
                offset: 0,
                context_lines: 0,
            })
            .await;

        if let Ok(result) = search_result {
            if !result.matches.is_empty() {
                let m = &result.matches[0];
                return Some(GetDefinitionResponse {
                    file: m.file.clone(),
                    line: u32::try_from(m.line).unwrap_or(u32::MAX),
                    column: u32::try_from(m.column).unwrap_or(1),
                    preview: m.content.clone(),
                    degraded: true,
                    degraded_reason: Some(DegradedReason::GrepFallbackGlobal),
                    actionable_guidance: Some(DegradedReason::GrepFallbackGlobal.guidance()),
                    lsp_readiness: Some("unavailable".to_owned()),
                    warm_start_in_progress: None,
                    duration_ms: None,
                    resolution_strategy: Some("grep_global".to_owned()),
                });
            }
        } else if let Err(e) = search_result {
            tracing::warn!(
                tool = "get_definition",
                strategy = "grep_definition_global",
                error = %e,
                "scout.search failed during global grep fallback"
            );
        }
        None
    }

    /// Spec 2.3: Broad cross-file symbol search fallback when definition patterns fail.
    ///
    /// Searches for the bare symbol name (not just definition patterns) across all
    /// source files. Returns the first match that looks like a symbol definition or reference.
    async fn grep_symbol_broad(&self, symbol_name: &str) -> Option<GetDefinitionResponse> {
        let name = regex::escape(symbol_name);
        let pattern = format!(r"\b{name}\b");

        let search_result = self
            .scout
            .search(&pathfinder_search::SearchParams {
                workspace_root: self.workspace_root.path().to_path_buf(),
                query: pattern,
                is_regex: true,
                max_results: 20,
                path_glob: "**/*".to_owned(),
                exclude_glob: vec!["**/{test,tests,mock}*/**".to_owned()],
                offset: 0,
                context_lines: 0,
            })
            .await;

        if let Ok(result) = search_result {
            if !result.matches.is_empty() {
                let m = &result.matches[0];
                return Some(GetDefinitionResponse {
                    file: m.file.clone(),
                    line: u32::try_from(m.line).unwrap_or(u32::MAX),
                    column: u32::try_from(m.column).unwrap_or(1),
                    preview: m.content.clone(),
                    degraded: true,
                    degraded_reason: Some(DegradedReason::GrepFallbackGlobal),
                    actionable_guidance: Some(DegradedReason::GrepFallbackGlobal.guidance()),
                    lsp_readiness: Some("unavailable".to_owned()),
                    warm_start_in_progress: None,
                    duration_ms: None,
                    resolution_strategy: Some("grep_broad".to_owned()),
                });
            }
        } else if let Err(e) = search_result {
            tracing::warn!(
                tool = "get_definition",
                strategy = "grep_symbol_broad",
                error = %e,
                "scout.search failed during broad grep fallback"
            );
        }
        None
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "definition_test.rs"]
mod tests;
