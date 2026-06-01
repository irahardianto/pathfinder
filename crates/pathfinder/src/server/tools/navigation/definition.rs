//! `get_definition` tool handler and grep-based fallback strategies.

use crate::server::helpers::{
    format_degraded_notice, millis_to_u64, parse_semantic_path, pathfinder_to_error_data,
    require_symbol_target, serialize_metadata, treesitter_error_to_error_data,
};
use crate::server::types::{GetDefinitionParams, GetDefinitionResponse};
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
        params: GetDefinitionParams,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        let start = std::time::Instant::now();

        tracing::info!(
            tool = "get_definition",
            semantic_path = %params.semantic_path,
            "get_definition: start"
        );

        // Parse and validate the semantic path
        let semantic_path = parse_semantic_path(&params.semantic_path)?;
        require_symbol_target(&semantic_path, &params.semantic_path)?;

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
                // This is the single most impactful fix for warmup-period reliability.
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;

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

                tracing::info!(
                    tool = "get_definition",
                    semantic_path = %params.semantic_path,
                    tree_sitter_ms,
                    lsp_ms,
                    duration_ms,
                    "get_definition: no definition found via LSP — attempting grep-based fallback"
                );

                if let Some(mut def) = self.fallback_definition_grep(&semantic_path).await {
                    def.degraded_reason = Some(DegradedReason::LspWarmupGrepFallback);
                    def.duration_ms = Some(millis_to_u64(start.elapsed().as_millis()));
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
                    semantic_path = %params.semantic_path,
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
                    semantic_path: params.semantic_path,
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
                    def.degraded_reason = Some(DegradedReason::NoLspGrepFallback);
                    def.duration_ms = Some(millis_to_u64(start.elapsed().as_millis()));
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
                // GAP-001: LSP timed out — attempt grep-based fallback
                tracing::info!(
                    tool = "get_definition",
                    semantic_path = %params.semantic_path,
                    "get_definition: LSP timed out — attempting grep-based fallback"
                );

                if let Some(mut def) = self.fallback_definition_grep(&semantic_path).await {
                    def.degraded_reason = Some(DegradedReason::LspTimeoutGrepFallback);
                    def.duration_ms = Some(millis_to_u64(start.elapsed().as_millis()));
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
                    semantic_path = %params.semantic_path,
                    "get_definition: LSP timed out and grep fallback found no match"
                );
                Err(pathfinder_to_error_data(&PathfinderError::LspError {
                    message: "LSP timed out and grep fallback found no match".to_owned(),
                }))
            }
            Err(e) => {
                // GAP-C2: Generic LSP error — attempt grep fallback before giving up.
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
                    def.degraded = true;
                    def.degraded_reason = Some(DegradedReason::LspErrorGrepFallback);
                    def.duration_ms = Some(millis_to_u64(start.elapsed().as_millis()));
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
    /// 3. Fall back to a global search with scoring by file proximity
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
            if let Some(result) = self.grep_impl_method(&parent_name, &symbol_name).await {
                return Some(result);
            }
        }

        // Strategy 3: Global search with file-proximity scoring
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
                    exclude_glob: String::default(),
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
            }
        }
        None
    }

    /// Search for a method within an impl block (e.g., `impl Sandbox` containing `fn check`).
    async fn grep_impl_method(
        &self,
        parent_name: &str,
        method_name: &str,
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
                path_glob: "**/*.rs".to_owned(),
                exclude_glob: String::default(),
                context_lines: 0,
                offset: 0,
            })
            .await;

        if let Ok(result) = search_result {
            for m in &result.matches {
                // Now search within this specific file for the method
                let method_escaped = regex::escape(method_name);
                let method_pattern = format!(
                    r"(?:(?:pub|export|public|private|protected|internal|open)\s*(?:\([^)]*\)\s*)?(?:async\s*)?)?fn\s+{method_escaped}\b"
                );
                let file_search = self
                    .scout
                    .search(&pathfinder_search::SearchParams {
                        workspace_root: self.workspace_root.path().to_path_buf(),
                        query: method_pattern,
                        is_regex: true,
                        max_results: 5,
                        path_glob: m.file.clone(),
                        exclude_glob: String::default(),
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
                }
            }
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
                exclude_glob: "**/{test,tests,mock}*/**".to_owned(),
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
                exclude_glob: "**/{test,tests,mock}*/**".to_owned(),
                offset: 0,
                context_lines: 1,
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
        }
        None
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use super::super::test_helpers::{make_scope, make_server_with_lawyer, make_temp_workspace};
    use crate::server::types::GetDefinitionParams;
    use crate::server::PathfinderServer;
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::{DegradedReason, WorkspaceRoot};
    use pathfinder_lsp::{DefinitionLocation, MockLawyer};
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::mock::MockSurgeon;
    use std::sync::Arc;

    /// Extract `GetDefinitionResponse` from a `CallToolResult.structured_content`.
    /// Replaces the old `call_res.0` tuple-unwrap from the `Json<T>` era.
    fn unpack_def(res: rmcp::model::CallToolResult) -> crate::server::types::GetDefinitionResponse {
        serde_json::from_value(res.structured_content.expect("structured_content")).unwrap()
    }

    // ── get_definition ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_definition_routes_to_lawyer_success() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        lawyer.set_goto_definition_result(Ok(Some(DefinitionLocation {
            file: "src/auth.rs".into(),
            line: 42,
            column: 5,
            preview: "pub fn login() -> bool {".into(),
        })));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer.clone());
        let params = GetDefinitionParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };

        let result = server.get_definition_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_def(call_res);

        assert_eq!(val.file, "src/auth.rs");
        assert_eq!(val.line, 42);
        assert_eq!(val.preview, "pub fn login() -> bool {");
        assert!(!val.degraded);
        assert_eq!(lawyer.goto_definition_call_count(), 1);
    }

    #[tokio::test]
    async fn test_get_definition_degrades_when_no_lsp() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        // Default MockLawyer returns Ok(None); use NoOpLawyer for NoLspAvailable
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

        let params = GetDefinitionParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };
        let result = server.get_definition_impl(params).await;
        // Should return NO_LSP_AVAILABLE error
        let Err(err) = result else {
            panic!("expected error but got Ok");
        };
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "NO_LSP_AVAILABLE");
    }

    #[tokio::test]
    async fn test_get_definition_rejects_empty_semantic_path() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(MockLawyer::default());
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = GetDefinitionParams {
            semantic_path: String::default(), // empty is truly invalid
        };
        let result = server.get_definition_impl(params).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_definition_rejects_sandbox_denied_path() {
        let surgeon = Arc::new(MockSurgeon::new());
        let lawyer = Arc::new(MockLawyer::default());
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = GetDefinitionParams {
            semantic_path: ".git/objects/abc::def".to_owned(), // sandbox should deny
        };
        let result = server.get_definition_impl(params).await;
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

    // ── get_definition LSP error path ──────────────────────────────────

    #[tokio::test]
    async fn test_get_definition_lsp_error_no_grep_match_returns_lsp_error() {
        // When a generic LSP error fires AND grep returns nothing, the original error is surfaced.
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        // Simulate an LSP protocol error (not NoLspAvailable, not None)
        lawyer.set_goto_definition_result(Err(LspError::Protocol("LSP protocol error".to_string())));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);
        let params = GetDefinitionParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };

        let result = server.get_definition_impl(params).await;
        let Err(err) = result else {
            panic!("expected error but got Ok");
        };
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "LSP_ERROR");
    }

    // ── GAP-C2: catch-all Err(e) grep fallback ───────────────────────────────

    #[tokio::test]
    async fn test_get_definition_generic_lsp_error_falls_back_to_grep() {
        // When a generic LSP error fires and grep DOES find a match,
        // the result should be Ok with degraded=true and reason containing "lsp_error_grep_fallback".
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let ws_dir = make_temp_workspace();
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/auth.rs"),
            "fn login() -> bool { true }",
        )
        .unwrap();

        // Scout returns a match so the fallback succeeds
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
                is_definition: None,
                version_hash: "sha256:abc".to_string(),
                known: Some(false),
            }],
            total_matches: 1,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
        }));

        // Lawyer returns a generic LSP error (not NoLspAvailable)
        let lawyer = Arc::new(MockLawyer::default());
        lawyer.set_goto_definition_result(Err(LspError::Protocol("protocol violation".to_string())));

        let server =
            PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

        let params = GetDefinitionParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };
        let result = server.get_definition_impl(params).await;
        let Ok(res) = result else {
            panic!("expected Ok with grep fallback, got Err");
        };
        let val = unpack_def(res);
        assert!(val.degraded, "should be degraded");
        assert_eq!(val.file, "src/auth.rs");
        assert_eq!(
            val.degraded_reason,
            Some(DegradedReason::LspErrorGrepFallback),
            "degraded_reason should be lsp_error_grep_fallback: {:?}",
            val.degraded_reason
        );
    }

    #[tokio::test]
    async fn test_get_definition_connection_lost_falls_back_to_grep() {
        // Same as above but with a "connection lost" error message — exercises
        // the same code path with a different error variant text.
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let ws_dir = make_temp_workspace();
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/auth.rs"),
            "fn login() -> bool { true }",
        )
        .unwrap();

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
                is_definition: None,
                version_hash: "sha256:abc".to_string(),
                known: Some(false),
            }],
            total_matches: 1,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
        }));

        let lawyer = Arc::new(MockLawyer::default());
        lawyer.set_goto_definition_result(Err(LspError::ConnectionLost));

        let server =
            PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

        let params = GetDefinitionParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };
        let result = server.get_definition_impl(params).await;
        let Ok(res) = result else {
            panic!("expected Ok with grep fallback, got Err");
        };
        let val = unpack_def(res);
        assert!(val.degraded, "should be degraded");
        assert_eq!(
            val.degraded_reason,
            Some(DegradedReason::LspErrorGrepFallback),
            "degraded_reason: {:?}",
            val.degraded_reason
        );
    }

    #[tokio::test]
    async fn test_get_definition_lsp_none_no_grep_fallback_returns_symbol_not_found() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));
        // Set up extract_symbols to return empty list for did_you_mean
        surgeon
            .extract_symbols_results
            .lock()
            .unwrap()
            .push(Ok(Vec::new()));

        // Default MockLawyer returns Ok(None) for goto_definition.
        // MockScout returns empty results → no grep fallback.
        let lawyer = Arc::new(MockLawyer::default());
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = GetDefinitionParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };
        let result = server.get_definition_impl(params).await;
        let Err(err) = result else {
            panic!("expected error but got Ok");
        };
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "SYMBOL_NOT_FOUND");
    }

    #[tokio::test]
    async fn test_get_definition_grep_fallback_with_mock_scout() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        // MockLawyer returns Ok(None) — triggers grep fallback
        let _lawyer = Arc::new(MockLawyer::default());

        // Use NoOpLawyer (NoLspAvailable path) + MockScout with results
        let ws_dir = make_temp_workspace();
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        // Write a file so search can find it
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/other.rs"),
            "fn login() -> bool { true }",
        )
        .unwrap();

        let scout = Arc::new(MockScout::default());
        scout.set_result(Ok(pathfinder_search::SearchResult {
            matches: vec![pathfinder_search::SearchMatch {
                file: "src/other.rs".to_string(),
                line: 1,
                column: 1,
                content: "fn login() -> bool { true }".to_string(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:abc".to_string(),
                known: Some(false),
            }],
            total_matches: 1,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
        }));

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            scout,
            surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params = GetDefinitionParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };
        let result = server.get_definition_impl(params).await;
        let Ok(res) = result else {
            panic!("expected Ok with grep fallback, got Err");
        };
        // Should return degraded result from grep
        let val = unpack_def(res);
        assert!(val.degraded);
        assert_eq!(val.file, "src/other.rs");
        assert!(val
            .degraded_reason
            .as_ref()
            .unwrap()
            .to_string()
            .contains("grep_fallback"));
    }

    // ── DS-1: DocumentGuard lifecycle tests ──────────────────────────────────

    #[tokio::test]
    async fn test_get_definition_closes_document_on_success() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        lawyer.set_goto_definition_result(Ok(Some(DefinitionLocation {
            file: "src/auth.rs".into(),
            line: 42,
            column: 5,
            preview: "pub fn login() -> bool {".into(),
        })));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer.clone());
        let params = GetDefinitionParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };

        let _ = server.get_definition_impl(params).await;

        // Yield so the spawned `did_close` task (from MockDocumentLease Drop) runs.
        tokio::task::yield_now().await;

        assert_eq!(
            lawyer.did_open_call_count(),
            lawyer.did_close_call_count(),
            "DS-1: did_open and did_close must be symmetric on success"
        );
    }

    #[tokio::test]
    async fn test_get_definition_closes_document_on_lsp_error() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        // Simulate an LSP protocol error after the document is opened
        lawyer.set_goto_definition_result(Err(LspError::Protocol("LSP crashed".to_string())));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer.clone());
        let params = GetDefinitionParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };

        let _ = server.get_definition_impl(params).await;

        tokio::task::yield_now().await;

        assert_eq!(
            lawyer.did_open_call_count(),
            lawyer.did_close_call_count(),
            "DS-1: did_close must be called even when LSP returns an error"
        );
    }

    // ── TASK-3: did_you_mean suggestions ─────────────────────────────────────

    /// When `get_definition` fails (LSP None, grep empty), and `extract_symbols`
    /// returns close-but-not-exact symbol names, the error payload should contain
    /// `did_you_mean` suggestions computed by Levenshtein distance.
    #[tokio::test]
    async fn test_get_definition_returns_did_you_mean_suggestions_on_symbol_not_found() {
        use pathfinder_treesitter::surgeon::{ExtractedSymbol, SymbolKind};

        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        // Provide close symbol names so did_you_mean can produce suggestions.
        // The caller is looking for "login" — we provide "logIn" and "logon" as candidates.
        let symbols = vec![
            ExtractedSymbol {
                name: "logIn".to_owned(),
                semantic_path: "logIn".to_owned(),
                kind: SymbolKind::Function,
                byte_range: 0..5,
                start_line: 0,
                end_line: 0,
                name_column: 0,
                access_level: pathfinder_treesitter::surgeon::AccessLevel::Public,
                children: vec![],
            },
            ExtractedSymbol {
                name: "logon".to_owned(),
                semantic_path: "logon".to_owned(),
                kind: SymbolKind::Function,
                byte_range: 10..15,
                start_line: 1,
                end_line: 1,
                name_column: 0,
                access_level: pathfinder_treesitter::surgeon::AccessLevel::Public,
                children: vec![],
            },
        ];
        surgeon
            .extract_symbols_results
            .lock()
            .unwrap()
            .push(Ok(symbols));

        // MockLawyer returns Ok(None) — triggers warmup retry → grep fallback → did_you_mean path.
        // MockScout returns empty results → grep fallback finds nothing → SymbolNotFound.
        let lawyer = Arc::new(MockLawyer::default());
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = GetDefinitionParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };
        let result = server.get_definition_impl(params).await;
        let Err(err) = result else {
            panic!("expected SYMBOL_NOT_FOUND error, got Ok");
        };

        // Verify error code
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(
            code, "SYMBOL_NOT_FOUND",
            "error code must be SYMBOL_NOT_FOUND"
        );

        // Verify did_you_mean field is non-empty and contains expected candidates.
        // The suggestions are nested in data.details.did_you_mean (via `to_details()`).
        let suggestions = err
            .data
            .as_ref()
            .and_then(|d| d.get("details"))
            .and_then(|d| d.get("did_you_mean"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(
            !suggestions.is_empty(),
            "did_you_mean must contain suggestions when similar symbols exist"
        );
        let has_login_variant = suggestions
            .iter()
            .any(|s| s.as_str().is_some_and(|s| s.contains("log")));
        assert!(
            has_login_variant,
            "suggestions should include close matches like 'logIn' or 'logon', got: {suggestions:?}"
        );
    }

    // ── get_definition grep fallback ────────────────────────────────────

    #[tokio::test]
    async fn test_get_definition_grep_fallback_when_lsp_returns_none() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        // MockLawyer with no result set returns Ok(None) by default
        let lawyer = Arc::new(MockLawyer::default());

        // Configure MockScout to return a search result for the grep fallback
        let scout = Arc::new(MockScout::default());
        scout.set_result(Ok(pathfinder_search::SearchResult {
            matches: vec![pathfinder_search::SearchMatch {
                file: "src/auth.rs".to_owned(),
                line: 10,
                column: 4,
                content: "pub fn login() -> bool {".to_owned(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: Some("src/auth.rs::login".to_owned()),
                is_definition: Some(true),
                version_hash: "hash".to_owned(),
                known: None,
            }],
            total_matches: 1,
            truncated: false,
            files_searched: 1,
            files_in_scope: 1,
        }));

        let ws_dir = make_temp_workspace();
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);
        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            scout,
            surgeon,
            lawyer,
        );

        let params = GetDefinitionParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };
        let result = server.get_definition_impl(params).await;
        let call_res = result.expect("should succeed via grep fallback");
        let val = unpack_def(call_res);

        assert_eq!(val.file, "src/auth.rs");
        assert_eq!(val.line, 10);
        assert!(val.degraded, "should be degraded when using grep fallback");
        assert!(
            val.degraded_reason.is_some(),
            "degraded_reason must be set"
        );
    }

    #[tokio::test]
    async fn test_get_definition_grep_fallback_when_no_lsp() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        // NoOpLawyer returns NoLspAvailable for all methods
        let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);

        // Configure MockScout to return a search result for the grep fallback
        let scout = Arc::new(MockScout::default());
        scout.set_result(Ok(pathfinder_search::SearchResult {
            matches: vec![pathfinder_search::SearchMatch {
                file: "src/auth.rs".to_owned(),
                line: 10,
                column: 4,
                content: "pub fn login() -> bool {".to_owned(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: Some("src/auth.rs::login".to_owned()),
                is_definition: Some(true),
                version_hash: "hash".to_owned(),
                known: None,
            }],
            total_matches: 1,
            truncated: false,
            files_searched: 1,
            files_in_scope: 1,
        }));

        let ws_dir = make_temp_workspace();
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);
        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            scout,
            surgeon,
            lawyer,
        );

        let params = GetDefinitionParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };
        let result = server.get_definition_impl(params).await;
        let call_res = result.expect("should succeed via grep fallback");
        let val = unpack_def(call_res);

        assert_eq!(val.file, "src/auth.rs");
        assert_eq!(val.line, 10);
        assert!(val.degraded, "should be degraded when using grep fallback");
    }
}
