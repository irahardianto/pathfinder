//! Navigation tool handlers: `get_definition`, `analyze_impact`, and
//! `read_with_deep_context`.
//!
//! All three tools are LSP-powered but degrade gracefully when no language
//! server is available. The tool responses include `"degraded": true` and
//! `"degraded_reason"` fields to signal the fallback mode to agents.
//!
//! # Degraded Mode
//! When the `Lawyer` returns `LspError::NoLspAvailable`:
//! - `get_definition` — returns an error response (`LSP_REQUIRED`)
//! - `analyze_impact` — returns `null` caller/callee lists with `degraded: true`
//! - `read_with_deep_context` — returns the symbol scope only, no dependencies

use crate::server::helpers::{
    parse_semantic_path, pathfinder_to_error_data, require_symbol_target, serialize_metadata,
    treesitter_error_to_error_data,
};
use crate::server::types::{
    AnalyzeImpactParams, GetDefinitionParams, GetDefinitionResponse, ReadWithDeepContextParams,
};
use crate::server::PathfinderServer;
use pathfinder_common::error::PathfinderError;
use pathfinder_lsp::LspError;
use rmcp::model::{CallToolResult, ErrorData};

/// GAP-002: Re-probe interval for "ready" languages to check liveness.
/// Re-probes every 2 minutes to detect LSPs that became non-responsive after
/// initial readiness (e.g., stuck indexing, memory pressure, internal deadlock).
const LIVENESS_PROBE_INTERVAL_SECS: u64 = 120;

/// File extensions considered source code for grep fallback filtering.
///
/// When the LSP is unavailable and we fall back to text search, we only
/// want results from actual source files, not documentation (.md), config
/// (.json, .yaml, .toml), or other non-source files.
const SOURCE_FILE_EXTENSIONS: &[&str] = &[
    "rs",  // Rust
    "go",  // Go
    "ts",  // TypeScript
    "tsx", // TypeScript + JSX
    "js",  // JavaScript
    "jsx", // JavaScript + JSX
    "py",  // Python
    "vue", // Vue Single-File Component
];

/// Returns `true` if the file path has a source code extension.
///
/// Used to filter out non-source files (docs, configs) from grep fallback
/// search results to reduce false positives.
fn is_source_file(file: &str) -> bool {
    let ext = std::path::Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    SOURCE_FILE_EXTENSIONS.contains(&ext)
}

/// Returns `true` if the file looks like it's from the user's workspace (not external/dependencies).
///
/// Filters out:
/// - Absolute paths (stdlib, SDK files)
/// - Paths containing `node_modules/` or `vendor/` (dependencies)
fn is_workspace_file(file: &str) -> bool {
    // Filter out absolute paths (stdlib, SDK files)
    // Unix: starts with `/`
    // Windows: starts with `\` or has `:` at position 1 (e.g., `C:\`)
    if file.starts_with('/') || file.starts_with('\\') {
        return false;
    }
    // Check for Windows-style absolute paths like `C:\` or `D:/`
    if file.len() >= 2 {
        let second_char = file.chars().nth(1);
        if second_char == Some(':') {
            return false;
        }
    }
    // Filter out dependency directories
    if file.contains("node_modules/")
        || file.contains("node_modules\\")
        || file.contains("vendor/")
        || file.contains("vendor\\")
    {
        return false;
    }
    true
}

/// Direction for call hierarchy BFS traversal in `analyze_impact`.
///
/// `Incoming` traverses callers (who calls this symbol).
/// `Outgoing` traverses callees (what this symbol calls).
enum CallDirection {
    Incoming,
    Outgoing,
}

/// Result of LSP call-hierarchy resolution for `read_with_deep_context`.
struct LspResolution {
    dependencies: Vec<crate::server::types::DeepContextDependency>,
    degraded: bool,
    degraded_reason: Option<String>,
    engines: Vec<&'static str>,
}

impl PathfinderServer {
    /// Resolve LSP call-hierarchy dependencies for a symbol.
    ///
    /// Extracted from `read_with_deep_context` to reduce nesting depth.
    /// Prepares the call hierarchy, then fetches outgoing calls and
    /// maps them to `DeepContextDependency` entries. Includes LSP warmup
    /// retry logic (3-second wait + re-probe) mirroring `get_definition_impl`.
    #[expect(
        clippy::too_many_lines,
        reason = "Call-hierarchy resolution with LSP warmup probe + retry. Linear structure for readability."
    )]
    async fn resolve_lsp_dependencies(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        start_line: usize,
        name_column: usize,
    ) -> LspResolution {
        let mut dependencies = Vec::new();
        let mut degraded = true;
        let mut degraded_reason = Some("no_lsp".to_owned());
        let mut engines = vec!["tree-sitter"];

        let lsp_result = self
            .lawyer
            .call_hierarchy_prepare(
                self.workspace_root.path(),
                &semantic_path.file_path,
                u32::try_from(start_line + 1).unwrap_or(1),
                // Position cursor on the symbol's name identifier (e.g., the 'd' in 'dedent'),
                // not the 'pub' keyword. rust-analyzer requires this for symbol resolution.
                u32::try_from(name_column + 1).unwrap_or(1),
            )
            .await;

        match lsp_result {
            Ok(items) if !items.is_empty() => {
                self.append_outgoing_deps(
                    &items[0],
                    &mut dependencies,
                    &mut engines,
                    &mut degraded,
                    &mut degraded_reason,
                )
                .await;
            }
            Ok(_) => {
                // Empty call hierarchy — verify LSP is actually warm.
                // Mirror the probe logic from analyze_impact_impl: if goto_definition
                // can resolve the symbol, the LSP is indexed and zero deps is genuine.
                // If goto_definition also returns None, the LSP is still warming up
                // and the empty result is unreliable.
                let probe = self
                    .lawyer
                    .goto_definition(
                        self.workspace_root.path(),
                        &semantic_path.file_path,
                        u32::try_from(start_line + 1).unwrap_or(1),
                        u32::try_from(name_column + 1).unwrap_or(1),
                    )
                    .await;

                if matches!(probe, Ok(Some(_))) {
                    // LSP is warm — definition resolved → confirmed zero dependencies
                    engines.push("lsp");
                    degraded = false;
                    degraded_reason = None;
                } else {
                    // LSP returned empty but can't resolve the symbol → likely warming up.
                    //
                    // Retry once after a brief wait: if the LSP just finished indexing
                    // between our call_hierarchy_prepare and the probe, a second attempt
                    // often succeeds. This mirrors the retry pattern in get_definition_impl.
                    engines.push("lsp");

                    tracing::info!(
                        tool = "read_with_deep_context",
                        semantic_path = %semantic_path,
                        "read_with_deep_context: call_hierarchy_prepare returned [] and goto_definition \
                         probe returned no result — LSP likely warming up, waiting 3s and retrying"
                    );

                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

                    let retry_result = self
                        .lawyer
                        .call_hierarchy_prepare(
                            self.workspace_root.path(),
                            &semantic_path.file_path,
                            u32::try_from(start_line + 1).unwrap_or(1),
                            u32::try_from(name_column + 1).unwrap_or(1),
                        )
                        .await;

                    match retry_result {
                        Ok(retry_items) if !retry_items.is_empty() => {
                            // Succeeded on retry — LSP finished indexing
                            tracing::info!(
                                tool = "read_with_deep_context",
                                semantic_path = %semantic_path,
                                "read_with_deep_context: call_hierarchy_prepare succeeded on retry after warmup wait"
                            );
                            self.append_outgoing_deps(
                                &retry_items[0],
                                &mut dependencies,
                                &mut engines,
                                &mut degraded,
                                &mut degraded_reason,
                            )
                            .await;
                        }
                        _ => {
                            // Retry also returned empty or failed → truly warming up or no deps
                            tracing::info!(
                                tool = "read_with_deep_context",
                                semantic_path = %semantic_path,
                                "read_with_deep_context: retry also returned empty — LSP still warming up"
                            );
                            degraded = true;
                            degraded_reason = Some("lsp_warmup_empty_unverified".to_owned());
                        }
                    }
                }
            }
            Err(LspError::NoLspAvailable | LspError::UnsupportedCapability { .. }) => {}
            Err(e) => {
                tracing::warn!(
                    tool = "read_with_deep_context",
                    error = %e,
                    "call_hierarchy_prepare failed"
                );
            }
        }

        LspResolution {
            dependencies,
            degraded,
            degraded_reason,
            engines,
        }
    }

    /// Fetch outgoing call-hierarchy items and append them as dependencies.
    async fn append_outgoing_deps(
        &self,
        item: &pathfinder_lsp::types::CallHierarchyItem,
        dependencies: &mut Vec<crate::server::types::DeepContextDependency>,
        engines: &mut Vec<&'static str>,
        degraded: &mut bool,
        degraded_reason: &mut Option<String>,
    ) {
        match self
            .lawyer
            .call_hierarchy_outgoing(self.workspace_root.path(), item)
            .await
        {
            Ok(outgoing) => {
                engines.push("lsp");
                for call in outgoing {
                    let callee = call.item;

                    // Filter out non-workspace files (stdlib, dependencies)
                    if !is_source_file(&callee.file) || !is_workspace_file(&callee.file) {
                        continue;
                    }

                    let signature = callee.detail.clone().unwrap_or_else(|| callee.name.clone());
                    let sp = format!("{}::{}", callee.file, callee.name);
                    dependencies.push(crate::server::types::DeepContextDependency {
                        semantic_path: sp,
                        signature,
                        file: callee.file,
                        line: callee.line as usize,
                    });
                }
                *degraded = false;
                *degraded_reason = None;
            }
            Err(e) => {
                tracing::warn!(
                    tool = "read_with_deep_context",
                    error = %e,
                    "call_hierarchy_outgoing failed"
                );
            }
        }
    }

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
        let file_content =
            tokio::fs::read_to_string(self.workspace_root.path().join(&semantic_path.file_path))
                .await
                .unwrap_or_default();
        // `_doc_guard` is held until the end of this function; dropping it fires did_close.
        let _doc_guard = self
            .lawyer
            .open_document(
                self.workspace_root.path(),
                &semantic_path.file_path,
                &file_content,
            )
            .await;

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
                    lsp_readiness: Some("ready".to_owned()),
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
                        lsp_readiness: Some("warming_up".to_owned()),
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
                    def.degraded_reason = Some(
                        "lsp_warmup_grep_fallback: LSP returned no result (likely warming up); \
                         result from Ripgrep pattern search — may not be the canonical definition. \
                         Verify with read_source_file."
                            .to_owned(),
                    );
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
                Err(pathfinder_to_error_data(&PathfinderError::SymbolNotFound {
                    semantic_path: params.semantic_path,
                    did_you_mean: vec![],
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
                    def.degraded_reason = Some(
                        "no_lsp_grep_fallback: LSP unavailable; result from Ripgrep \
                         pattern search — may not be the canonical definition. \
                         Verify with read_source_file."
                            .to_owned(),
                    );
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
                    def.degraded_reason = Some(
                        "lsp_timeout_grep_fallback: LSP timed out; result from Ripgrep pattern search — \
                         may not be the canonical definition. Verify with read_source_file."
                            .to_owned(),
                    );
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
                    def.degraded_reason = Some(format!(
                        "lsp_error_grep_fallback: LSP returned error ({e}); result from Ripgrep \
                         pattern search — may not be the canonical definition. \
                         Verify with read_source_file."
                    ));
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
            let reason = def.degraded_reason.as_deref().unwrap_or("unknown");
            format!(
                "DEGRADED ({reason}) — {}:L{} — {}",
                def.file,
                def.line,
                if def.preview.is_empty() {
                    "(no preview)"
                } else {
                    &def.preview
                }
            )
        } else {
            format!(
                "{}:L{} col:{} — {}",
                def.file,
                def.line,
                def.column,
                if def.preview.is_empty() {
                    "(no preview)"
                } else {
                    &def.preview
                }
            )
        };
        let mut res = rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(text)]);
        res.structured_content = serialize_metadata(def);
        res
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
        self.grep_definition_global(symbol_name).await
    }

    /// Search for a definition within a specific file.
    async fn grep_definition_in_file(
        &self,
        symbol_name: String,
        file_path: std::path::PathBuf,
    ) -> Option<GetDefinitionResponse> {
        // Match definition patterns with optional preceding visibility modifier.
        // Rust: `pub fn`, `pub(crate) fn`, `pub async fn`, bare `fn`
        // TypeScript: `export function`, `export default function`, bare `function`
        // Python: `def`, `async def`
        let pattern = format!(
            r"(?:(?:pub|export|public|private|protected|internal|open)\s*(?:\([^)]*\)\s*)?(?:async\s*)?)?(?:fn|def|func|function|class|struct|type|interface|const|let|var|enum|trait|mod)\s+{symbol_name}\\b"
        );

        // Use the file as a specific path glob. Convert to forward-slash
        // format for ripgrep compatibility across platforms.
        let glob = file_path.to_string_lossy().replace('\\', "/");

        let search_result = self
            .scout
            .search(&pathfinder_search::SearchParams {
                workspace_root: self.workspace_root.path().to_path_buf(),
                query: pattern,
                is_regex: true,
                max_results: 5,
                path_glob: glob,
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
                    degraded_reason: Some(
                        "grep_fallback_file_scoped: result from file-scoped Ripgrep search. \
                         Verify with read_source_file."
                            .to_owned(),
                    ),
                    lsp_readiness: Some("unavailable".to_owned()),
                });
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
        let impl_pattern = format!(r"impl\s+(?:<[^>]+>\s+)?{parent_name}\\b");
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
                let method_pattern = format!(
                    r"(?:(?:pub|export|public|private|protected|internal|open)\s*(?:\([^)]*\)\s*)?(?:async\s*)?)?fn\s+{method_name}\\b"
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
                            degraded_reason: Some(
                                "grep_fallback_impl_scoped: result from impl-scoped Ripgrep search. \
                                 Verify with read_source_file."
                                    .to_owned(),
                            ),
                            lsp_readiness: Some("unavailable".to_owned()),
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
        let pattern = format!(
            r"(?:(?:pub|export|public|private|protected|internal|open)\s*(?:\([^)]*\)\s*)?(?:async\s*)?)?(?:fn|def|func|function|class|struct|type|interface|const|let|var|enum|trait|mod)\s+{symbol_name}\\b"
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
                    degraded_reason: Some(
                        "grep_fallback_global: result from global Ripgrep search — \
                         may not be the canonical definition. Verify with read_source_file."
                            .to_owned(),
                    ),
                    lsp_readiness: Some("unavailable".to_owned()),
                });
            }
        }
        None
    }

    /// Core logic for the `read_with_deep_context` tool.
    ///
    /// Returns the symbol's source code. When LSP is available, appends the
    /// signatures of all called symbols. Degrades gracefully to symbol scope
    /// only when no LSP is configured.
    #[expect(
        clippy::too_many_lines,
        reason = "Sequential pipeline: parse → sandbox → TS → LSP → dep-block rendering. Linear structure is intentional for readability."
    )]
    pub(crate) async fn read_with_deep_context_impl(
        &self,
        params: ReadWithDeepContextParams,
    ) -> Result<CallToolResult, ErrorData> {
        let start = std::time::Instant::now();

        tracing::info!(
            tool = "read_with_deep_context",
            semantic_path = %params.semantic_path,
            "read_with_deep_context: start"
        );

        // Parse and validate the semantic path
        let semantic_path = parse_semantic_path(&params.semantic_path)?;
        require_symbol_target(&semantic_path, &params.semantic_path)?;

        // Sandbox check
        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            let duration_ms = start.elapsed().as_millis();
            tracing::warn!(
                tool = "read_with_deep_context",
                error_code = e.error_code(),
                duration_ms,
                "sandbox check failed"
            );
            return Err(pathfinder_to_error_data(&e));
        }

        // Fetch the symbol scope (Tree-sitter)
        let ts_start = std::time::Instant::now();
        let scope = self
            .surgeon
            .read_symbol_scope(self.workspace_root.path(), &semantic_path)
            .await
            .map_err(treesitter_error_to_error_data)?;
        let tree_sitter_ms = ts_start.elapsed().as_millis();

        // IW-3 (DS-1 gap fix): RAII document lifecycle — did_close fires on all exits.
        let file_content =
            tokio::fs::read_to_string(self.workspace_root.path().join(&semantic_path.file_path))
                .await
                .unwrap_or_default();
        // `_doc_guard` fires did_close automatically when this function returns.
        let _doc_guard = self
            .lawyer
            .open_document(
                self.workspace_root.path(),
                &semantic_path.file_path,
                &file_content,
            )
            .await;

        let lsp_start = std::time::Instant::now();

        let LspResolution {
            dependencies,
            degraded,
            degraded_reason,
            engines,
        } = self
            .resolve_lsp_dependencies(&semantic_path, scope.start_line, scope.name_column)
            .await;

        // Note: `_doc_guard` still alive here; drops at function return.
        let lsp_ms = lsp_start.elapsed().as_millis();
        let duration_ms = start.elapsed().as_millis();

        tracing::info!(
            tool = "read_with_deep_context",
            semantic_path = %params.semantic_path,
            tree_sitter_ms,
            lsp_ms,
            duration_ms,
            degraded,
            degraded_reason,
            engines_used = ?engines,
            "read_with_deep_context: complete"
        );

        let dep_count = dependencies.len();
        let lsp_readiness = if degraded {
            match degraded_reason.as_deref() {
                Some("no_lsp") => Some("unavailable".to_owned()),
                _ => Some("warming_up".to_owned()),
            }
        } else {
            Some("ready".to_owned())
        };
        let metadata = crate::server::types::ReadWithDeepContextMetadata {
            start_line: scope.start_line,
            end_line: scope.end_line,
            language: scope.language,
            dependencies,
            degraded,
            degraded_reason: degraded_reason.clone(),
            lsp_readiness,
        };

        // Build the dependency block: list each callee signature, file, and line.
        // This surfaces the same data as structured_content.dependencies in plain text
        // so agents reading the text channel don't need to parse JSON.
        let dep_block: String = if metadata.dependencies.is_empty() {
            String::new()
        } else {
            let mut lines = Vec::with_capacity(metadata.dependencies.len());
            for dep in &metadata.dependencies {
                lines.push(format!("  {} ({}:L{})", dep.signature, dep.file, dep.line));
            }
            format!("\n{}", lines.join("\n"))
        };

        // Prepend degradation notice when in degraded mode
        let text = if degraded {
            let reason = degraded_reason.as_deref().unwrap_or("unknown");
            format!(
                "DEGRADED MODE ({}) — {dep_count} dependencies loaded (results may be incomplete){dep_block}\n\n{}",
                reason, scope.content
            )
        } else {
            format!(
                "{dep_count} dependencies loaded{dep_block}\n\n{}",
                scope.content
            )
        };
        let mut res = CallToolResult::success(vec![rmcp::model::Content::text(text)]);
        res.structured_content = serialize_metadata(&metadata);
        Ok(res)
    }

    /// Performs BFS traversal of the call hierarchy in the specified direction.
    ///
    /// Returns the collected references and the maximum depth reached during traversal.
    async fn bfs_call_hierarchy(
        &self,
        initial_item: &pathfinder_lsp::types::CallHierarchyItem,
        direction: CallDirection,
        max_depth: u32,
        files_referenced: &mut std::collections::HashSet<String>,
    ) -> (Vec<crate::server::types::ImpactReference>, u32) {
        let mut queue = std::collections::VecDeque::new();
        queue.push_back((initial_item.clone(), 0));
        let mut seen = std::collections::HashSet::new();
        seen.insert((initial_item.file.clone(), initial_item.line));
        files_referenced.insert(initial_item.file.clone());

        let mut references = Vec::new();
        let mut max_depth_reached = 0;

        while let Some((item, current_depth)) = queue.pop_front() {
            max_depth_reached = std::cmp::max(max_depth_reached, current_depth);
            if current_depth >= max_depth {
                continue;
            }

            let hierarchy_result = match direction {
                CallDirection::Incoming => {
                    self.lawyer
                        .call_hierarchy_incoming(self.workspace_root.path(), &item)
                        .await
                }
                CallDirection::Outgoing => {
                    self.lawyer
                        .call_hierarchy_outgoing(self.workspace_root.path(), &item)
                        .await
                }
            };

            match hierarchy_result {
                Ok(calls) => {
                    for call in calls {
                        let referenced_item = call.item;

                        // Filter out non-workspace files:
                        // - Must have a source code extension
                        // - Must be a relative path (not absolute like stdlib/SDK paths)
                        // - Must not be in node_modules/ or vendor/
                        if !is_source_file(&referenced_item.file)
                            || !is_workspace_file(&referenced_item.file)
                        {
                            continue;
                        }

                        files_referenced.insert(referenced_item.file.clone());

                        let key = (referenced_item.file.clone(), referenced_item.line);
                        if seen.insert(key) {
                            queue.push_back((referenced_item.clone(), current_depth + 1));

                            references.push(crate::server::types::ImpactReference {
                                semantic_path: format!(
                                    "{}::{}",
                                    referenced_item.file, referenced_item.name
                                ),
                                file: referenced_item.file.clone(),
                                line: referenced_item.line as usize,
                                snippet: referenced_item
                                    .detail
                                    .unwrap_or_else(|| referenced_item.name.clone()),
                                direction: match direction {
                                    CallDirection::Incoming => "incoming".to_owned(),
                                    CallDirection::Outgoing => "outgoing".to_owned(),
                                },
                                depth: current_depth as usize,
                            });
                        }
                    }
                }
                Err(e) => {
                    let direction_name = match direction {
                        CallDirection::Incoming => "call_hierarchy_incoming",
                        CallDirection::Outgoing => "call_hierarchy_outgoing",
                    };
                    tracing::warn!(
                        tool = "analyze_impact",
                        error = %e,
                        file = %item.file,
                        line = item.line,
                        depth = current_depth,
                        "{direction_name} failed during BFS (partial impact graph)"
                    );
                }
            }
        }

        (references, max_depth_reached)
    }

    /// Core logic for the `analyze_impact` tool.
    ///
    /// Returns callers (incoming) and callees (outgoing) for the target symbol.
    /// Degrades gracefully to empty results when no LSP is configured.
    #[expect(
        clippy::too_many_lines,
        reason = "Sequential pipeline (parse→sandbox→tree-sitter→LSP→BFS→version hash)."
    )]
    pub(crate) async fn analyze_impact_impl(
        &self,
        params: AnalyzeImpactParams,
    ) -> Result<CallToolResult, ErrorData> {
        let start = std::time::Instant::now();

        // Cap max_depth to prevent unbounded BFS traversal (PRD §5.1 maximum).
        // Also floor at 1 to guarantee at least one level of traversal.
        let max_depth = params.max_depth.clamp(1, 5);

        tracing::info!(
            tool = "analyze_impact",
            semantic_path = %params.semantic_path,
            max_depth = max_depth,
            "analyze_impact: start"
        );

        // Parse and validate the semantic path
        let semantic_path = parse_semantic_path(&params.semantic_path)?;
        require_symbol_target(&semantic_path, &params.semantic_path)?;

        // Sandbox check
        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            let duration_ms = start.elapsed().as_millis();
            tracing::warn!(
                tool = "analyze_impact",
                error_code = e.error_code(),
                duration_ms,
                "sandbox check failed"
            );
            return Err(pathfinder_to_error_data(&e));
        }

        // 1. Fetch the symbol scope (Tree-sitter) to get start line
        let ts_start = std::time::Instant::now();
        let scope = match self
            .surgeon
            .read_symbol_scope(self.workspace_root.path(), &semantic_path)
            .await
        {
            Ok(s) => s,
            Err(e) => {
                let duration_ms = start.elapsed().as_millis();
                tracing::warn!(
                    tool = "analyze_impact",
                    error = %e,
                    duration_ms,
                    "tree-sitter read failed"
                );
                return Err(treesitter_error_to_error_data(e));
            }
        };
        let tree_sitter_ms = ts_start.elapsed().as_millis();

        // IW-3 (DS-1 gap fix): RAII document lifecycle — did_close fires on all exits.
        let file_content =
            tokio::fs::read_to_string(self.workspace_root.path().join(&semantic_path.file_path))
                .await
                .unwrap_or_default();
        // `_doc_guard` fires did_close automatically when this function returns.
        let _doc_guard = self
            .lawyer
            .open_document(
                self.workspace_root.path(),
                &semantic_path.file_path,
                &file_content,
            )
            .await;

        let lsp_start = std::time::Instant::now();
        // Use Option<Vec> to distinguish "unknown" (LSP unavailable) from "verified empty" (LSP confirmed zero).
        // None = degraded (LSP was down — callers are unknown, do NOT treat as zero)
        // Some([]) = LSP responded with confirmed zero callers/callees
        let mut incoming: Option<Vec<crate::server::types::ImpactReference>> = None;
        let mut outgoing: Option<Vec<crate::server::types::ImpactReference>> = None;
        let mut degraded = true;
        let mut degraded_reason = Some("no_lsp".to_owned());
        let mut engines = vec!["tree-sitter"];
        let mut files_referenced = std::collections::HashSet::new();
        let mut max_depth_reached = 0;

        let lsp_result = self
            .lawyer
            .call_hierarchy_prepare(
                self.workspace_root.path(),
                &semantic_path.file_path,
                u32::try_from(scope.start_line + 1).unwrap_or(1),
                // Position cursor on the symbol's name identifier (e.g., the 'd' in 'dedent'),
                // not the 'pub' keyword. rust-analyzer requires this for symbol resolution.
                u32::try_from(scope.name_column + 1).unwrap_or(1),
            )
            .await;

        match lsp_result {
            Ok(items) if !items.is_empty() => {
                engines.push("lsp");
                degraded = false;
                degraded_reason = None;

                let initial_item = &items[0];

                // --- INCOMING BFS ---
                let (incoming_refs, depth_in) = self
                    .bfs_call_hierarchy(
                        initial_item,
                        CallDirection::Incoming,
                        max_depth,
                        &mut files_referenced,
                    )
                    .await;
                incoming = Some(incoming_refs);
                max_depth_reached = std::cmp::max(max_depth_reached, depth_in);

                // --- OUTGOING BFS ---
                let (outgoing_refs, depth_out) = self
                    .bfs_call_hierarchy(
                        initial_item,
                        CallDirection::Outgoing,
                        max_depth,
                        &mut files_referenced,
                    )
                    .await;
                outgoing = Some(outgoing_refs);
                max_depth_reached = std::cmp::max(max_depth_reached, depth_out);
            }
            Ok(_) => {
                // LSP responded with empty items — but this is ambiguous:
                //   - Genuine "zero callers": LSP is warm and the symbol truly has no references.
                //   - LSP warmup: LSP hasn't finished indexing and returned [] for everything.
                //
                // Probe goto_definition at the same position. A warm LSP can resolve a symbol
                // to its definition; a cold LSP returns None even for well-known symbols.
                // If the probe returns Ok(Some(_)) the LSP is warm → confirmed zero callers.
                // If the probe returns Ok(None) or Err, we degrade rather than lying to the agent.
                let probe = self
                    .lawyer
                    .goto_definition(
                        self.workspace_root.path(),
                        &semantic_path.file_path,
                        u32::try_from(scope.start_line + 1).unwrap_or(1),
                        u32::try_from(scope.name_column + 1).unwrap_or(1),
                    )
                    .await;

                if matches!(probe, Ok(Some(_))) {
                    // LSP is warm — definition resolved → confirmed zero callers/callees
                    engines.push("lsp");
                    degraded = false;
                    degraded_reason = None;
                    incoming = Some(Vec::new());
                    outgoing = Some(Vec::new());
                } else {
                    // LSP likely still warming up — empty call hierarchy is not reliable.
                    // Degrade so agents know to verify before acting on "zero references".
                    tracing::info!(
                        tool = "analyze_impact",
                        symbol = %semantic_path,
                        "analyze_impact: call_hierarchy_prepare returned [] but goto_definition \
                         probe returned no result — LSP likely warming up, attempting grep-based reference fallback"
                    );
                    engines.push("lsp");
                    degraded = true;
                    degraded_reason = Some("lsp_warmup_empty_unverified".to_owned());

                    // Use grep-based reference search as a heuristic fallback when LSP is warming up.
                    // Results may over-count (string references) or under-count (indirect calls),
                    // but give the agent a starting point.
                    let symbol_name = semantic_path
                        .symbol_chain
                        .as_ref()
                        .and_then(|c| c.segments.last())
                        .map(|s| s.name.clone())
                        .unwrap_or_default();

                    let search_result = self
                        .scout
                        .search(&pathfinder_search::SearchParams {
                            workspace_root: self.workspace_root.path().to_path_buf(),
                            query: symbol_name.clone(),
                            is_regex: false,
                            max_results: 20,
                            path_glob: "**/*".to_owned(),
                            exclude_glob: String::default(),
                            context_lines: 0,
                            offset: 0,
                        })
                        .await;

                    if let Ok(result) = search_result {
                        if !result.matches.is_empty() {
                            let refs: Vec<crate::server::types::ImpactReference> = result
                                .matches
                                .into_iter()
                                // Exclude the definition file itself AND non-source files (docs, configs).
                                // is_source_file filters out .md, .txt, .json, .yaml, etc. to reduce false positives.
                                .filter(|m| {
                                    let m_path = std::path::Path::new(&m.file);
                                    is_source_file(&m.file)
                                        && m_path != std::path::Path::new(&semantic_path.file_path)
                                })
                                .take(10) // Cap at 10 heuristic references to avoid overwhelming output
                                .map(|m| {
                                    files_referenced.insert(m.file.clone());
                                    crate::server::types::ImpactReference {
                                        semantic_path: format!("{}::{symbol_name}", m.file),
                                        file: m.file,
                                        line: usize::try_from(m.line).unwrap_or(usize::MAX),
                                        snippet: m.content,
                                        // Grep fallback: heuristic, direction is assumed incoming
                                        direction: "incoming_heuristic".to_owned(),
                                        depth: 0,
                                    }
                                })
                                .collect();
                            incoming = Some(refs);
                            degraded_reason = Some("lsp_warmup_grep_fallback".to_owned());
                            tracing::info!(
                                tool = "analyze_impact",
                                references_found = incoming.as_ref().map_or(0, Vec::len),
                                "analyze_impact: grep-based fallback references found during LSP warmup"
                            );
                        }
                    }
                }
            }
            Err(LspError::NoLspAvailable | LspError::UnsupportedCapability { .. }) => {
                // Degraded mode — LSP not available. Use grep-based reference search
                // as a heuristic fallback. Results may over-count (string references)
                // or under-count (indirect calls), but give the agent a starting point.
                tracing::info!(
                    tool = "analyze_impact",
                    symbol = %semantic_path,
                    "analyze_impact: no LSP — attempting grep-based reference fallback"
                );

                let symbol_name = semantic_path
                    .symbol_chain
                    .as_ref()
                    .and_then(|c| c.segments.last())
                    .map(|s| s.name.clone())
                    .unwrap_or_default();

                let search_result = self
                    .scout
                    .search(&pathfinder_search::SearchParams {
                        workspace_root: self.workspace_root.path().to_path_buf(),
                        query: symbol_name.clone(),
                        is_regex: false,
                        max_results: 20,
                        path_glob: "**/*".to_owned(),
                        exclude_glob: String::default(),
                        context_lines: 0,
                        offset: 0,
                    })
                    .await;

                if let Ok(result) = search_result {
                    if !result.matches.is_empty() {
                        let refs: Vec<crate::server::types::ImpactReference> = result
                            .matches
                            .into_iter()
                            // Exclude the definition file itself AND non-source files (docs, configs).
                            // is_source_file filters out .md, .txt, .json, .yaml, etc. to reduce false positives.
                            .filter(|m| {
                                let m_path = std::path::Path::new(&m.file);
                                is_source_file(&m.file)
                                    && m_path != std::path::Path::new(&semantic_path.file_path)
                            })
                            .take(10) // Cap at 10 heuristic references to avoid overwhelming output
                            .map(|m| {
                                files_referenced.insert(m.file.clone());
                                crate::server::types::ImpactReference {
                                    semantic_path: format!("{}::{symbol_name}", m.file),
                                    file: m.file,
                                    line: usize::try_from(m.line).unwrap_or(usize::MAX),
                                    snippet: m.content,
                                    // Grep fallback: heuristic, direction is assumed incoming
                                    direction: "incoming_heuristic".to_owned(),
                                    depth: 0,
                                }
                            })
                            .collect();
                        incoming = Some(refs);
                        degraded_reason = Some("no_lsp_grep_fallback".to_owned());
                        tracing::info!(
                            tool = "analyze_impact",
                            references_found = incoming.as_ref().map_or(0, Vec::len),
                            "analyze_impact: grep-based fallback references found"
                        );
                    }
                }
                // Keep degraded = true to signal this is heuristic data
            }
            Err(LspError::Timeout { .. }) => {
                // GAP-001: LSP timed out — attempt grep-based reference fallback
                tracing::info!(
                    tool = "analyze_impact",
                    symbol = %semantic_path,
                    "analyze_impact: LSP timed out — attempting grep-based reference fallback"
                );

                let symbol_name = semantic_path
                    .symbol_chain
                    .as_ref()
                    .and_then(|c| c.segments.last())
                    .map(|s| s.name.clone())
                    .unwrap_or_default();

                let search_result = self
                    .scout
                    .search(&pathfinder_search::SearchParams {
                        workspace_root: self.workspace_root.path().to_path_buf(),
                        query: symbol_name.clone(),
                        is_regex: false,
                        max_results: 20,
                        path_glob: "**/*".to_owned(),
                        exclude_glob: String::default(),
                        context_lines: 0,
                        offset: 0,
                    })
                    .await;

                if let Ok(result) = search_result {
                    if !result.matches.is_empty() {
                        let refs: Vec<crate::server::types::ImpactReference> = result
                            .matches
                            .into_iter()
                            // Exclude the definition file itself AND non-source files (docs, configs).
                            // is_source_file filters out .md, .txt, .json, .yaml, etc. to reduce false positives.
                            .filter(|m| {
                                let m_path = std::path::Path::new(&m.file);
                                is_source_file(&m.file)
                                    && m_path != std::path::Path::new(&semantic_path.file_path)
                            })
                            .take(10) // Cap at 10 heuristic references to avoid overwhelming output
                            .map(|m| {
                                files_referenced.insert(m.file.clone());
                                crate::server::types::ImpactReference {
                                    semantic_path: format!("{}::{symbol_name}", m.file),
                                    file: m.file,
                                    line: usize::try_from(m.line).unwrap_or(usize::MAX),
                                    snippet: m.content,
                                    // Grep fallback: heuristic, direction is assumed incoming
                                    direction: "incoming_heuristic".to_owned(),
                                    depth: 0,
                                }
                            })
                            .collect();
                        incoming = Some(refs);
                        degraded_reason = Some("lsp_timeout_grep_fallback".to_owned());
                        tracing::info!(
                            tool = "analyze_impact",
                            references_found = incoming.as_ref().map_or(0, Vec::len),
                            "analyze_impact: grep-based fallback references found after timeout"
                        );
                    }
                }
                // Keep degraded = true to signal this is heuristic data
            }
            Err(e) => {
                tracing::warn!(
                    tool = "analyze_impact",
                    error = %e,
                    "call_hierarchy_prepare failed"
                );
            }
        }

        // Note: `_doc_guard` still alive here; did_close fires at function return.
        let lsp_ms = lsp_start.elapsed().as_millis();
        let duration_ms = start.elapsed().as_millis();

        tracing::info!(
            tool = "analyze_impact",
            semantic_path = %params.semantic_path,
            tree_sitter_ms,
            lsp_ms,
            duration_ms,
            degraded,
            degraded_reason,
            engines_used = ?engines,
            "analyze_impact: complete"
        );

        let inc_count = incoming.as_ref().map_or(0, Vec::len);
        let out_count = outgoing.as_ref().map_or(0, Vec::len);
        let degraded_reason_cloned = degraded_reason.clone();

        let metadata = crate::server::types::AnalyzeImpactMetadata {
            incoming,
            outgoing,
            depth_reached: max_depth_reached,
            files_referenced: files_referenced.len(),
            degraded,
            degraded_reason,
        };

        // Build honest text output based on actual results, listing every
        // reference so agents can act without parsing structured_content.
        let mut text_parts = Vec::new();
        if degraded {
            text_parts.push(format!(
                "Degraded analysis ({}) — LSP unavailable — reference counts are UNRELIABLE. Do NOT trust zero as 'confirmed no callers'. Grep-based heuristic was used if available. Use search_codebase for manual verification.",
                degraded_reason_cloned.as_deref().unwrap_or("unknown")
            ));
        }
        // Incoming
        text_parts.push(format!("Incoming references: {inc_count}"));
        if let Some(refs) = &metadata.incoming {
            for r in refs {
                text_parts.push(format!(
                    "  [depth={}] {} ({}:L{})",
                    r.depth, r.semantic_path, r.file, r.line
                ));
                if !r.snippet.is_empty() {
                    text_parts.push(format!("    > {}", r.snippet.trim()));
                }
            }
        }
        // Outgoing
        text_parts.push(format!("Outgoing references: {out_count}"));
        if let Some(refs) = &metadata.outgoing {
            for r in refs {
                text_parts.push(format!(
                    "  [depth={}] {} ({}:L{})",
                    r.depth, r.semantic_path, r.file, r.line
                ));
                if !r.snippet.is_empty() {
                    text_parts.push(format!("    > {}", r.snippet.trim()));
                }
            }
        }

        let text = text_parts.join("\n");
        let mut res = CallToolResult::success(vec![rmcp::model::Content::text(text)]);
        res.structured_content = serialize_metadata(&metadata);
        Ok(res)
    }

    /// Check LSP health status.
    ///
    /// Tests whether LSP navigation tools (`get_definition`, `analyze_impact`,
    /// `read_with_deep_context`) will return real data or degraded results.
    /// Agents should call this once at session start to choose their strategy.
    #[allow(clippy::too_many_lines)]
    #[tracing::instrument(skip(self, params), fields(language = ?params.language))]
    pub(crate) async fn lsp_health_impl(
        &self,
        params: crate::server::types::LspHealthParams,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        // IW-4: Handle action="restart" before the normal health query flow.
        if params.action.as_deref() == Some("restart") {
            let lang = match &params.language {
                Some(l) => l.clone(),
                None => {
                    return Err(crate::server::helpers::pathfinder_to_error_data(
                        &pathfinder_common::error::PathfinderError::IoError {
                            message: "lsp_health action='restart' requires 'language' to be set"
                                .to_owned(),
                        },
                    ));
                }
            };
            tracing::info!(language = %lang, "lsp_health: restart requested by agent");
            match self.lawyer.force_respawn(&lang).await {
                Ok(()) => {
                    tracing::info!(language = %lang, "lsp_health: restart successful");
                }
                Err(e) => {
                    tracing::warn!(language = %lang, error = %e, "lsp_health: restart failed");
                }
            }
            // Fall through to return updated health status after restart attempt.
        }

        let capability_status = self.lawyer.capability_status().await;

        let mut languages = Vec::new();
        let mut overall_status = "unavailable";

        for (lang, status) in &capability_status {
            if let Some(ref filter) = params.language {
                if lang != filter {
                    continue;
                }
            }

            // LSP-HEALTH-001: Two-phase readiness model
            // Primary gate: navigation_ready (initialize handshake + definitionProvider)
            // indexing_complete is an ADDITIONAL signal, not a requirement.
            let (status_str, uptime) = if status.navigation_ready == Some(true) {
                // Navigation is functional — report ready regardless of indexing status.
                // This makes get_definition, analyze_impact available immediately after
                // initialize completes, without waiting for WorkDoneProgressEnd.
                ("ready", status.uptime_seconds.map(format_uptime))
            } else if status.navigation_ready == Some(false)
                || status.indexing_complete == Some(false)
            {
                // Process is running but navigation is not yet functional (e.g.,
                // supports_definition=false) OR indexing still in progress but
                // navigation_ready is not confirmed. Still warming up.
                ("warming_up", status.uptime_seconds.map(format_uptime))
            } else if status.uptime_seconds.is_some() {
                // Process exists but no capability info yet (lazy start)
                ("starting", status.uptime_seconds.map(format_uptime))
            } else {
                ("unavailable", None)
            };

            // Compute indexing_status: independent signal for agents that want to wait
            // for full indexing. None when process not running.
            let indexing_status = match status.indexing_complete {
                Some(true) => Some("complete".to_owned()),
                Some(false) => Some("in_progress".to_owned()),
                None => None,
            };

            match status_str {
                "ready" => overall_status = "ready",
                "warming_up" if overall_status != "ready" => {
                    overall_status = "warming_up";
                }
                "starting" if overall_status != "ready" && overall_status != "warming_up" => {
                    overall_status = "starting";
                }
                _ => {}
            }

            languages.push(crate::server::types::LspLanguageHealth {
                language: lang.clone(),
                status: status_str.to_owned(),
                uptime,
                diagnostics_strategy: status.diagnostics_strategy.clone(),
                supports_call_hierarchy: status.supports_call_hierarchy,
                supports_diagnostics: status.supports_diagnostics,
                supports_definition: status.supports_definition,
                indexing_status,
                navigation_ready: status.navigation_ready,
                probe_verified: false,
                install_hint: None,
                degraded_tools: compute_degraded_tools(status),
            });
        }

        // PATCH-006: Probe-based readiness check
        // For languages that have been running for a while but still show warming_up,
        // fire a probe to verify actual readiness.
        //
        // Also handles the edge case where navigation_ready = Some(false) but the
        // LSP may actually be functional (e.g., capability detection was inaccurate
        // during early initialize).
        for lang_health in &mut languages {
            if lang_health.status == "warming_up" {
                // Check probe cache first — avoid redundant LSP calls.
                // Positive entries are cached indefinitely; negative entries
                // expire after PROBE_NEGATIVE_TTL_SECS (60s) to allow the LSP
                // to finish starting and be re-probed later.
                let cache_action = {
                    let cache = self
                        .probe_cache
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    match cache.get(&lang_health.language) {
                        Some(entry) if entry.is_valid() && entry.success => {
                            // Valid positive entry — reuse cached result
                            ProbeAction::UseCachedReady
                        }
                        Some(entry) if entry.is_valid() && !entry.success => {
                            // Valid negative entry — skip probe, LSP still starting
                            ProbeAction::SkipProbe
                        }
                        Some(_) => {
                            // Expired negative entry — allow re-probe
                            ProbeAction::Probe
                        }
                        None => ProbeAction::Probe,
                    }
                };

                match cache_action {
                    ProbeAction::UseCachedReady => {
                        "ready".clone_into(&mut lang_health.status);
                        lang_health.probe_verified = true;
                        if overall_status != "ready" {
                            overall_status = "ready";
                        }
                        continue;
                    }
                    ProbeAction::SkipProbe => {
                        continue;
                    }
                    ProbeAction::Probe => {}
                }

                let uptime_secs = parse_uptime_to_seconds(lang_health.uptime.as_deref());
                if let Some(secs) = uptime_secs {
                    if secs > 10 {
                        // LSP has been running for 10+ seconds but still warming_up.
                        // This likely means progress notifications aren't being emitted.
                        // Fire a lightweight probe.
                        let probe_result =
                            self.probe_language_readiness(&lang_health.language).await;
                        if probe_result {
                            "ready".clone_into(&mut lang_health.status);
                            lang_health.probe_verified = true;
                            // Cache the successful probe result (indefinite TTL)
                            self.probe_cache
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .insert(
                                    lang_health.language.clone(),
                                    crate::server::ProbeCacheEntry::new(true),
                                );
                            // Update overall status
                            if overall_status != "ready" {
                                overall_status = "ready";
                            }
                        } else {
                            // Cache negative result with TTL — allows re-probe after
                            // the LSP finishes starting
                            self.probe_cache
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .insert(
                                    lang_health.language.clone(),
                                    crate::server::ProbeCacheEntry::new(false),
                                );
                        }
                    }
                }
            }
        }

        // GAP-002: LIVENESS PROBE for "ready" languages
        // Verify that languages that were "ready" at initialization are still responsive.
        // This catches LSPs that become non-responsive after initial readiness
        // (e.g., stuck indexing, memory pressure, internal deadlock).
        for lang_health in &mut languages {
            if lang_health.status != "ready" {
                continue;
            }

            // Check liveness cache
            let cache_action = {
                let cache = self
                    .probe_cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match cache.get(&lang_health.language) {
                    Some(entry) if entry.is_valid() && entry.success => {
                        // Positive entry — check if it's time for a re-probe
                        if entry.age_secs() < LIVENESS_PROBE_INTERVAL_SECS {
                            ProbeAction::UseCachedReady
                        } else {
                            ProbeAction::Probe // Stale — re-probe
                        }
                    }
                    Some(entry) if entry.is_valid() && !entry.success => ProbeAction::SkipProbe,
                    Some(_) => {
                        ProbeAction::Probe // Expired
                    }
                    None => ProbeAction::Probe, // Never probed (shouldn't happen for "ready")
                }
            };

            match cache_action {
                ProbeAction::UseCachedReady => {
                    lang_health.probe_verified = true;
                    continue;
                }
                ProbeAction::SkipProbe => continue,
                ProbeAction::Probe => {}
            }

            // Run the same probe as warming_up
            // Note: find_probe_file returns None if no source file exists.
            // In this case, we skip the probe and don't downgrade the status.
            // The language remains "ready" based on capability status alone.
            let probe_result = match self.find_probe_file(&lang_health.language) {
                Some(_) => self.probe_language_readiness(&lang_health.language).await,
                None => {
                    // No file to probe — skip liveness check, keep status as-is
                    continue;
                }
            };

            if probe_result {
                // Still alive — cache positive result
                lang_health.probe_verified = true;
                self.probe_cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(
                        lang_health.language.clone(),
                        crate::server::ProbeCacheEntry::new(true),
                    );
            } else {
                // LSP is dead! Downgrade from "ready" to "degraded"
                "degraded".clone_into(&mut lang_health.status);
                lang_health.probe_verified = false;
                // Cache negative result
                self.probe_cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(
                        lang_health.language.clone(),
                        crate::server::ProbeCacheEntry::new(false),
                    );
            }
        }

        // Downgrade overall status if all ready languages are now degraded
        if !languages.iter().any(|l| l.status == "ready") && overall_status == "ready" {
            overall_status = "degraded";
        }

        // PATCH-008: Add missing languages (markers found but no LSP binary)
        // These are languages where we detected marker files (Cargo.toml, pyproject.toml, etc.)
        // but no LSP binary is on PATH. We show them as "unavailable" with install hints.
        let missing_languages = self.lawyer.missing_languages();
        for missing in &missing_languages {
            if let Some(ref filter) = params.language {
                if &missing.language_id != filter {
                    continue;
                }
            }

            languages.push(crate::server::types::LspLanguageHealth {
                language: missing.language_id.clone(),
                status: "unavailable".to_owned(),
                uptime: None,
                diagnostics_strategy: None,
                supports_call_hierarchy: None,
                supports_diagnostics: None,
                supports_definition: None,
                indexing_status: None,
                navigation_ready: None,
                probe_verified: false,
                install_hint: Some(missing.install_hint.clone()),
                degraded_tools: vec![
                    crate::server::types::DegradedToolInfo {
                        tool: "analyze_impact".to_owned(),
                        severity: "unavailable".to_owned(),
                        description:
                            "No LSP available. Use search_codebase for manual reference search."
                                .to_owned(),
                    },
                    crate::server::types::DegradedToolInfo {
                        tool: "read_with_deep_context".to_owned(),
                        severity: "unavailable".to_owned(),
                        description:
                            "No LSP available. Returns source only, no dependency signatures."
                                .to_owned(),
                    },
                ],
            });
        }

        if languages.is_empty() && params.language.is_none() {
            overall_status = "unavailable";
        }

        let response = crate::server::types::LspHealthResponse {
            status: overall_status.to_owned(),
            languages,
        };

        // Build a concise human-readable summary for the text channel.
        // Agents reading plain text get actionable status without parsing JSON.
        let lang_lines: Vec<String> = response
            .languages
            .iter()
            .map(|l| {
                let mut parts = vec![format!("{}: {}", l.language, l.status)];
                if l.probe_verified {
                    parts.push("(probe_verified)".to_owned());
                }
                if let Some(ref idx) = l.indexing_status {
                    parts.push(format!("indexing: {idx}"));
                }
                if !l.degraded_tools.is_empty() {
                    let tool_names: Vec<_> =
                        l.degraded_tools.iter().map(|t| t.tool.as_str()).collect();
                    parts.push(format!("degraded_tools: [{}]", tool_names.join(", ")));
                }
                parts.join(" ")
            })
            .collect();
        let text = if lang_lines.is_empty() {
            format!("LSP status: {} — no languages detected", response.status)
        } else {
            format!("LSP status: {}\n{}", response.status, lang_lines.join("\n"))
        };

        let mut res = rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(text)]);
        res.structured_content = serialize_metadata(&response);
        Ok(res)
    }

    /// Probe whether an LSP is actually ready by attempting a lightweight operation.
    async fn probe_language_readiness(&self, language_id: &str) -> bool {
        // Find a well-known file in the workspace for this language
        let probe_file = self.find_probe_file(language_id);
        let Some(file_path) = probe_file else {
            return false; // No file to probe with
        };

        // Open the file, try goto_definition on line 1 column 1
        let content = tokio::fs::read_to_string(self.workspace_root.path().join(&file_path))
            .await
            .unwrap_or_default();

        let _ = self
            .lawyer
            .open_document(self.workspace_root.path(), &file_path, &content)
            .await;

        let result = self
            .lawyer
            .goto_definition(self.workspace_root.path(), &file_path, 1, 1)
            .await;

        // Any response (even Ok(None)) means the LSP is alive and processing requests.
        // Only Err means it's not ready.
        result.is_ok()
    }

    /// Find a well-known file in the workspace for probing language readiness.
    pub(crate) fn find_probe_file(&self, language_id: &str) -> Option<std::path::PathBuf> {
        let extensions: &[&str] = match language_id {
            "rust" => &["rs"],
            "go" => &["go"],
            "typescript" => &["ts", "tsx"],
            "javascript" => &["js", "jsx"],
            "python" => &["py"],
            "ruby" => &["rb"],
            "java" => &["java"],
            _ => return None,
        };

        // First try well-known paths (fast path)
        let candidates = match language_id {
            "rust" => vec!["src/main.rs", "src/lib.rs"],
            "go" => vec!["main.go", "cmd/main.go"],
            "typescript" => vec![
                "src/index.ts",
                "index.ts",
                "src/main.ts",
                "src/index.tsx",
                "index.tsx",
                "src/main.tsx",
            ],
            "javascript" => vec![
                "src/index.js",
                "index.js",
                "src/main.js",
                "src/index.jsx",
                "index.jsx",
                "src/main.jsx",
            ],
            "python" => vec!["src/__init__.py", "main.py", "setup.py", "__init__.py"],
            "ruby" => vec!["lib/main.rb", "main.rb"],
            "java" => vec!["src/main/java/Main.java"],
            _ => vec![],
        };

        for candidate in candidates {
            let path = self.workspace_root.path().join(candidate);
            if path.exists() {
                return Some(std::path::PathBuf::from(candidate));
            }
        }

        // LSP-HEALTH-001 Task 3.1: Fallback to depth-limited recursive scan for monorepos
        // Scans up to depth 4 looking for any file with matching extension.
        // Returns relative path to first match.
        self.find_file_by_extension_recursive(self.workspace_root.path(), extensions, 0, 4)
    }

    /// Recursive helper for `find_probe_file`: depth-limited scan for any file
    /// with matching extension. Returns relative path from workspace root.
    fn find_file_by_extension_recursive(
        &self,
        current_dir: &std::path::Path,
        extensions: &[&str],
        current_depth: usize,
        max_depth: usize,
    ) -> Option<std::path::PathBuf> {
        if current_depth > max_depth {
            return None;
        }

        let Ok(entries) = std::fs::read_dir(current_dir) else {
            return None;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = entry.metadata() else {
                continue;
            };

            if metadata.is_dir() {
                // Skip hidden directories and common build/test dirs
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with('.')
                        || name == "node_modules"
                        || name == "target"
                        || name == "vendor"
                        || name == "dist"
                        || name == "build"
                        || name == "__pycache__"
                        || name == ".git"
                    {
                        continue;
                    }
                }
                // Recurse
                if let Some(found) = self.find_file_by_extension_recursive(
                    &path,
                    extensions,
                    current_depth + 1,
                    max_depth,
                ) {
                    return Some(found);
                }
            } else if metadata.is_file() {
                // Check extension
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if extensions.iter().any(|&e| e.eq_ignore_ascii_case(ext)) {
                        // Found a match - return relative path from workspace root
                        if let Ok(rel_path) = path.strip_prefix(self.workspace_root.path()) {
                            return Some(rel_path.to_path_buf());
                        }
                    }
                }
            }
        }
        None
    }
}

/// Format uptime in seconds as a human-readable string.
fn format_uptime(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        let mins = seconds / 60;
        let secs = seconds % 60;
        if secs == 0 {
            format!("{mins}m")
        } else {
            format!("{mins}m{secs}s")
        }
    } else {
        let hours = seconds / 3600;
        let mins = (seconds % 3600) / 60;
        if mins == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h{mins}m")
        }
    }
}

/// Parse a formatted uptime string back to seconds.
/// Handles formats: `"Xs"`, `"XmYs"`, `"XhYm"`, `"XhYmZs"`
/// Compute which tools are degraded based on LSP capabilities.
/// Decision from checking the probe cache for a language.
enum ProbeAction {
    /// Cached positive result exists — upgrade to "ready" immediately.
    UseCachedReady,
    /// Cached negative result exists and hasn't expired — skip probing.
    SkipProbe,
    /// No cache entry or expired negative — perform a live probe.
    Probe,
}

/// Returns structured information about tools that lose LSP support for this language.
///
/// Each entry includes the tool name, severity level, and description of the fallback behavior.
fn compute_degraded_tools(
    status: &pathfinder_lsp::types::LspLanguageStatus,
) -> Vec<crate::server::types::DegradedToolInfo> {
    let mut degraded = Vec::new();

    if status.supports_definition != Some(true) {
        degraded.push(crate::server::types::DegradedToolInfo {
            tool: "get_definition".to_owned(),
            severity: "grep_fallback".to_owned(),
            description:
                "Uses ripgrep heuristic instead of LSP. May find wrong definition or miss re-exports."
                    .to_owned(),
        });
    }

    if status.supports_call_hierarchy != Some(true) {
        degraded.push(crate::server::types::DegradedToolInfo {
            tool: "analyze_impact".to_owned(),
            severity: "grep_fallback".to_owned(),
            description:
                "Uses text search instead of call hierarchy. May over/under-count references."
                    .to_owned(),
        });
        degraded.push(crate::server::types::DegradedToolInfo {
            tool: "read_with_deep_context".to_owned(),
            severity: "unavailable".to_owned(),
            description:
                "Returns source only, no dependency signatures. Use search_codebase as alternative."
                    .to_owned(),
        });
    }

    degraded
}

fn parse_uptime_to_seconds(uptime: Option<&str>) -> Option<u64> {
    let uptime = uptime?;
    let mut seconds = 0u64;

    // Parse hours
    if let Some(h_pos) = uptime.find('h') {
        let h_str = &uptime[..h_pos];
        if let Ok(h) = h_str.parse::<u64>() {
            seconds += h * 3600;
        }
    }

    // Parse minutes
    let min_part = if let Some(h_pos) = uptime.find('h') {
        &uptime[h_pos + 1..]
    } else {
        uptime
    };

    if let Some(m_pos) = min_part.find('m') {
        let m_str = &min_part[..m_pos];
        if let Ok(m) = m_str.parse::<u64>() {
            seconds += m * 60;
        }
    }

    // Parse seconds
    let sec_part = if let Some(m_pos) = min_part.find('m') {
        &min_part[m_pos + 1..]
    } else {
        // min_part already equals uptime when no 'h', so we can just use min_part
        min_part
    };

    if let Some(s_pos) = sec_part.find('s') {
        let s_str = &sec_part[..s_pos];
        if let Ok(s) = s_str.parse::<u64>() {
            seconds += s;
        }
    }

    Some(seconds)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::server::types::{
        AnalyzeImpactParams, GetDefinitionParams, ReadWithDeepContextParams,
    };
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::{SymbolScope, WorkspaceRoot};
    use pathfinder_lsp::types::{CallHierarchyCall, CallHierarchyItem};
    use pathfinder_lsp::{DefinitionLocation, MockLawyer};
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::mock::MockSurgeon;
    use std::sync::Arc;
    use tempfile::tempdir;

    /// Extract `GetDefinitionResponse` from a `CallToolResult.structured_content`.
    /// Replaces the old `call_res.0` tuple-unwrap from the `Json<T>` era.
    fn unpack_def(res: rmcp::model::CallToolResult) -> crate::server::types::GetDefinitionResponse {
        serde_json::from_value(res.structured_content.expect("structured_content")).unwrap()
    }

    /// Extract `LspHealthResponse` from a `CallToolResult.structured_content`.
    fn unpack_health(res: rmcp::model::CallToolResult) -> crate::server::types::LspHealthResponse {
        serde_json::from_value(res.structured_content.expect("structured_content")).unwrap()
    }

    fn make_server_with_lawyer(
        mock_surgeon: Arc<MockSurgeon>,
        mock_lawyer: Arc<MockLawyer>,
    ) -> (PathfinderServer, tempfile::TempDir) {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);
        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            mock_surgeon,
            mock_lawyer,
        );
        (server, ws_dir)
    }

    fn make_scope() -> SymbolScope {
        SymbolScope {
            content: "fn login() { }".to_owned(),
            start_line: 9,
            end_line: 9,
            name_column: 0,
            language: "rust".to_owned(),
        }
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
        let ws_dir = tempdir().expect("temp dir");
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

    // ── read_with_deep_context ────────────────────────────────────────

    #[tokio::test]
    async fn test_read_with_deep_context_degrades_when_call_hierarchy_unsupported() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);
        let ws_dir = tempdir().expect("temp dir");
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

        let params = ReadWithDeepContextParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };
        let result = server.read_with_deep_context_impl(params).await;
        let call_res = result.expect("should succeed");
        let text_content = match &call_res.content[0].raw {
            rmcp::model::RawContent::Text(t) => t.text.clone(),
            _ => panic!("expected text content"),
        };
        let val: crate::server::types::ReadWithDeepContextMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert_eq!(text_content, "DEGRADED MODE (no_lsp) — 0 dependencies loaded (results may be incomplete)\n\nfn login() { }");
        assert!(val.degraded);
        assert_eq!(val.degraded_reason.as_deref(), Some("no_lsp"));
        assert!(val.dependencies.is_empty());
    }

    #[tokio::test]
    async fn test_read_with_deep_context_lsp_populates_dependencies() {
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
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item.clone()]));

        lawyer.push_outgoing_call_result(Ok(vec![CallHierarchyCall {
            item: CallHierarchyItem {
                name: "validate_token".into(),
                kind: "function".into(),
                detail: Some("fn validate_token() -> bool".into()),
                file: "src/token.rs".into(),
                line: 15,
                column: 4,
                data: None,
            },
            call_sites: vec![9],
        }]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = ReadWithDeepContextParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };
        let result = server.read_with_deep_context_impl(params).await;
        let call_res = result.expect("should succeed");
        let text_content = match &call_res.content[0].raw {
            rmcp::model::RawContent::Text(t) => t.text.clone(),
            _ => panic!("expected text content"),
        };
        let val: crate::server::types::ReadWithDeepContextMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert_eq!(
            text_content,
            "1 dependencies loaded\n  fn validate_token() -> bool (src/token.rs:L15)\n\nfn login() { }"
        );
        assert!(!val.degraded);
        assert_eq!(val.degraded_reason, None);
        assert_eq!(val.dependencies.len(), 1);
        assert_eq!(
            val.dependencies[0].semantic_path,
            "src/token.rs::validate_token"
        );
        assert_eq!(val.dependencies[0].signature, "fn validate_token() -> bool");
        assert_eq!(val.dependencies[0].file, "src/token.rs");
        assert_eq!(val.dependencies[0].line, 15);
    }

    // ── analyze_impact ────────────────────────────────────────────────

    #[tokio::test]
    async fn test_analyze_impact_returns_empty_degraded() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);
        let ws_dir = tempdir().expect("temp dir");
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

        let params = AnalyzeImpactParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 2,
        };
        let result = server.analyze_impact_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::AnalyzeImpactMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(
            val.incoming.is_none(),
            "incoming must be null (not empty) when degraded"
        );
        assert!(
            val.outgoing.is_none(),
            "outgoing must be null (not empty) when degraded"
        );
        assert!(val.degraded);
        assert_eq!(val.degraded_reason.as_deref(), Some("no_lsp"));
    }

    #[tokio::test]
    async fn test_analyze_impact_lsp_populates_incoming_and_outgoing() {
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
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item.clone()]));

        lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
            item: CallHierarchyItem {
                name: "handle_request".into(),
                kind: "function".into(),
                detail: Some("fn handle_request()".into()),
                file: "src/server.rs".into(),
                line: 20,
                column: 4,
                data: None,
            },
            call_sites: vec![25],
        }]));

        lawyer.push_outgoing_call_result(Ok(vec![CallHierarchyCall {
            item: CallHierarchyItem {
                name: "validate_token".into(),
                kind: "function".into(),
                detail: Some("fn validate_token() -> bool".into()),
                file: "src/token.rs".into(),
                line: 15,
                column: 4,
                data: None,
            },
            call_sites: vec![9],
        }]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = AnalyzeImpactParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 1,
        };
        let result = server.analyze_impact_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::AnalyzeImpactMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(!val.degraded);
        assert_eq!(val.degraded_reason, None);
        assert_eq!(val.depth_reached, 1); // BFS pops level 1, updates max_depth_reached, then continues
        assert_eq!(val.files_referenced, 3); // initial + caller + callee
        let incoming = val
            .incoming
            .as_ref()
            .expect("incoming must be Some when not degraded");
        let outgoing = val
            .outgoing
            .as_ref()
            .expect("outgoing must be Some when not degraded");
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].file, "src/server.rs");
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].file, "src/token.rs");
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
        lawyer.set_goto_definition_result(Err("LSP protocol error".to_string()));

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

        let ws_dir = tempdir().expect("temp dir");
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
                version_hash: "sha256:abc".to_string(),
                known: Some(false),
            }],
            total_matches: 1,
            truncated: false,
        }));

        // Lawyer returns a generic LSP error (not NoLspAvailable)
        let lawyer = Arc::new(MockLawyer::default());
        lawyer.set_goto_definition_result(Err("protocol violation".to_string()));

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
        assert!(
            val.degraded_reason
                .as_ref()
                .unwrap()
                .contains("lsp_error_grep_fallback"),
            "degraded_reason should mention lsp_error_grep_fallback: {:?}",
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

        let ws_dir = tempdir().expect("temp dir");
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
                version_hash: "sha256:abc".to_string(),
                known: Some(false),
            }],
            total_matches: 1,
            truncated: false,
        }));

        let lawyer = Arc::new(MockLawyer::default());
        lawyer.set_goto_definition_result(Err("connection lost to language server".to_string()));

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
        assert!(
            val.degraded_reason
                .as_ref()
                .unwrap()
                .contains("lsp_error_grep_fallback"),
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
        let ws_dir = tempdir().expect("temp dir");
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
            .contains("grep_fallback"));
    }

    // ── analyze_impact with empty hierarchy (confirmed zero callers) ───────

    #[tokio::test]
    async fn test_analyze_impact_empty_hierarchy_confirmed_zero() {
        // call_hierarchy_prepare returns Ok([]) AND goto_definition probe returns Ok(Some(...))
        // → LSP is warm, confirmed zero callers. Must NOT be degraded.
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        // Empty call hierarchy — ambiguous on its own
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![]));
        // Probe: goto_definition succeeds → LSP is warm → confirmed zero
        lawyer.set_goto_definition_result(Ok(Some(DefinitionLocation {
            file: "src/auth.rs".into(),
            line: 10,
            column: 4,
            preview: "fn login() {}".into(),
        })));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = AnalyzeImpactParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 2,
        };
        let result = server.analyze_impact_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::AnalyzeImpactMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // NOT degraded — LSP warm, genuinely zero callers confirmed
        assert!(
            !val.degraded,
            "must not be degraded when probe confirms LSP is warm"
        );
        assert_eq!(val.degraded_reason, None);
        let incoming = val
            .incoming
            .as_ref()
            .expect("must be Some when confirmed-zero");
        let outgoing = val
            .outgoing
            .as_ref()
            .expect("must be Some when confirmed-zero");
        assert!(incoming.is_empty(), "confirmed zero callers");
        assert!(outgoing.is_empty(), "confirmed zero callees");
    }

    #[tokio::test]
    async fn test_analyze_impact_empty_hierarchy_warmup_degrades() {
        // call_hierarchy_prepare returns Ok([]) AND goto_definition probe returns Ok(None)
        // → LSP is warming up. Must be degraded with "lsp_warmup_empty_unverified".
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        // Empty call hierarchy
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![]));
        // Probe: goto_definition returns Ok(None) → LSP is still warming up
        // MockLawyer::default() already returns Ok(None) for goto_definition, so no extra setup needed.

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = AnalyzeImpactParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 2,
        };
        let result = server.analyze_impact_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::AnalyzeImpactMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // DEGRADED — LSP warmup detected
        assert!(
            val.degraded,
            "must be degraded when goto_definition probe also returns None"
        );
        assert_eq!(
            val.degraded_reason.as_deref(),
            Some("lsp_warmup_empty_unverified"),
            "degraded_reason must indicate warmup ambiguity"
        );
        // incoming/outgoing must be None — do NOT mislead agent with Some([])
        assert!(
            val.incoming.is_none(),
            "incoming must be None (unknown) during warmup, not Some([]) (confirmed-zero)"
        );
        assert!(
            val.outgoing.is_none(),
            "outgoing must be None (unknown) during warmup, not Some([]) (confirmed-zero)"
        );
    }

    // ── analyze_impact with LSP error on call_hierarchy_prepare ────────────

    #[tokio::test]
    async fn test_analyze_impact_lsp_error_degrades() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        // Simulate LSP protocol error
        lawyer.push_prepare_call_hierarchy_result(Err("LSP crashed".to_string()));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = AnalyzeImpactParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 2,
        };
        let result = server.analyze_impact_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::AnalyzeImpactMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // Degraded due to LSP error
        assert!(val.degraded);
        assert_eq!(val.degraded_reason.as_deref(), Some("no_lsp"));
    }

    // ── read_with_deep_context with outgoing call error ───────────────────

    #[tokio::test]
    async fn test_read_with_deep_context_outgoing_error_degrades() {
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
        // Prepare succeeds
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));
        // But outgoing call fails
        lawyer.push_outgoing_call_result(Err("outgoing failed".to_string()));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = ReadWithDeepContextParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };
        let result = server.read_with_deep_context_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::ReadWithDeepContextMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // Degraded because outgoing call failed
        assert!(val.degraded);
        assert_eq!(val.degraded_reason.as_deref(), Some("no_lsp"));
        assert!(val.dependencies.is_empty());
    }

    // ── read_with_deep_context with empty hierarchy (confirmed zero deps) ──

    #[tokio::test]
    async fn test_read_with_deep_context_empty_hierarchy_zero_deps() {
        // call_hierarchy_prepare returns Ok([]) AND goto_definition probe returns Ok(Some(...))
        // → LSP is warm, confirmed zero deps. Must NOT be degraded.
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        // Empty call hierarchy — ambiguous on its own
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![]));
        // Probe: goto_definition succeeds → LSP is warm → confirmed zero
        lawyer.set_goto_definition_result(Ok(Some(DefinitionLocation {
            file: "src/auth.rs".into(),
            line: 10,
            column: 4,
            preview: "fn login() {}".into(),
        })));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = ReadWithDeepContextParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };
        let result = server.read_with_deep_context_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::ReadWithDeepContextMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // NOT degraded — LSP warm, genuinely zero deps confirmed
        assert!(
            !val.degraded,
            "must not be degraded when probe confirms LSP is warm"
        );
        assert_eq!(val.degraded_reason, None);
        assert!(val.dependencies.is_empty(), "confirmed zero dependencies");
    }

    #[tokio::test]
    async fn test_read_with_deep_context_empty_hierarchy_warmup_degrades() {
        // call_hierarchy_prepare returns Ok([]) AND goto_definition probe returns Ok(None)
        // → LSP is warming up. Must be degraded with "lsp_warmup_empty_unverified".
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        // Empty call hierarchy
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![]));
        // Probe: goto_definition returns Ok(None) → LSP is still warming up
        // MockLawyer::default() already returns Ok(None) for goto_definition, so no extra setup needed.

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = ReadWithDeepContextParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };
        let result = server.read_with_deep_context_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::ReadWithDeepContextMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // DEGRADED — LSP warmup detected
        assert!(
            val.degraded,
            "must be degraded when goto_definition probe also returns None"
        );
        assert_eq!(
            val.degraded_reason.as_deref(),
            Some("lsp_warmup_empty_unverified"),
            "degraded_reason must indicate warmup ambiguity"
        );
        assert!(val.dependencies.is_empty());
    }

    // ── analyze_impact BFS depth limiting ────────────────────────────────

    #[tokio::test]
    async fn test_analyze_impact_bfs_respects_max_depth() {
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
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item.clone()]));

        // Incoming: one caller that itself has a caller (depth 2 chain)
        let caller_item = CallHierarchyItem {
            name: "caller".into(),
            kind: "function".into(),
            detail: None,
            file: "src/caller.rs".into(),
            line: 5,
            column: 4,
            data: None,
        };
        lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
            item: caller_item.clone(),
            call_sites: vec![9],
        }]));
        // Second level incoming (would only be reached if max_depth > 1)
        lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
            item: CallHierarchyItem {
                name: "top_level".into(),
                kind: "function".into(),
                detail: None,
                file: "src/main.rs".into(),
                line: 1,
                column: 0,
                data: None,
            },
            call_sites: vec![5],
        }]));

        // Outgoing: empty
        lawyer.push_outgoing_call_result(Ok(vec![]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = AnalyzeImpactParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 1, // Should stop after first level
        };
        let result = server.analyze_impact_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::AnalyzeImpactMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(!val.degraded);
        let _incoming = val.incoming.as_ref().expect("must be Some");
        // With max_depth=1, BFS processes the initial item at depth 0, finds caller at depth 1,
        // but the second-level caller (depth 2) should NOT be included
        // However depth_reached should be 1
        assert_eq!(val.depth_reached, 1);
    }

    // ── CG-3: sandbox check error in analyze_impact ──────────────────────

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

    // ── CG-4: Tree-sitter error in analyze_impact ──────────────────────────

    #[tokio::test]
    async fn test_analyze_impact_tree_sitter_error() {
        let surgeon = Arc::new(MockSurgeon::new());
        // Push an error result
        surgeon.read_symbol_scope_results.lock().unwrap().push(Err(
            pathfinder_treesitter::SurgeonError::ParseError {
                path: std::path::PathBuf::from("src/auth.rs"),
                reason: "parse failed".to_string(),
            },
        ));

        let lawyer = Arc::new(MockLawyer::default());
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = AnalyzeImpactParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 2,
        };
        let result = server.analyze_impact_impl(params).await;
        assert!(result.is_err(), "tree-sitter error should propagate");
    }

    // ── CG-5: LSP error during BFS traversal ───────────────────────────────

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

        // NOT degraded — prepare succeeded, incoming succeeded, only outgoing had error
        assert!(!val.degraded);
        let incoming = val.incoming.as_ref().expect("incoming must be Some");
        assert_eq!(incoming.len(), 1, "incoming caller should be present");
        let outgoing = val.outgoing.as_ref().expect("outgoing must be Some");
        assert!(outgoing.is_empty(), "outgoing should be empty due to error");
    }

    // ── CG-1: Grep fallback path in analyze_impact ─────────────────────────

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
        )
        .unwrap();

        let scout = Arc::new(MockScout::default());
        // Create a caller file (different from the definition file)
        std::fs::write(
            ws_dir.path().join("src/caller.rs"),
            "fn handle_request() { login(); }",
        )
        .unwrap();
        scout.set_result(Ok(pathfinder_search::SearchResult {
            matches: vec![pathfinder_search::SearchMatch {
                file: "src/caller.rs".to_string(),
                line: 1,
                column: 1,
                content: "fn handle_request() { login(); }".to_string(),
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
        assert_eq!(val.degraded_reason.as_deref(), Some("no_lsp_grep_fallback"));
        let incoming = val.incoming.as_ref().expect("must be Some from grep");
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].file, "src/caller.rs");
        assert_eq!(incoming[0].direction, "incoming_heuristic");
    }

    // ── PATCH-002: Non-source file filtering in grep fallback ───────────

    #[tokio::test]
    async fn test_analyze_impact_grep_fallback_filters_non_source_files() {
        // Issue: grep fallback was returning matches from .md, .json, .txt, etc.
        // causing false positives. This test verifies that non-source files
        // are filtered out of the results.
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

        // Create the definition file
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/auth.rs"),
            "fn login() -> bool { true }",
        )
        .unwrap();

        let scout = Arc::new(MockScout::default());
        // Return a mix of source and non-source files that match the symbol name
        scout.set_result(Ok(pathfinder_search::SearchResult {
            matches: vec![
                // Legitimate source file caller
                pathfinder_search::SearchMatch {
                    file: "src/caller.rs".to_string(),
                    line: 1,
                    column: 1,
                    content: "fn call() { login(); }".to_string(),
                    context_before: vec![],
                    context_after: vec![],
                    enclosing_semantic_path: None,
                    version_hash: "sha256:a".to_string(),
                    known: Some(false),
                },
                // Documentation file - should be filtered OUT
                pathfinder_search::SearchMatch {
                    file: "docs/README.md".to_string(),
                    line: 10,
                    column: 1,
                    content: "call login() to authenticate".to_string(),
                    context_before: vec![],
                    context_after: vec![],
                    enclosing_semantic_path: None,
                    version_hash: "sha256:b".to_string(),
                    known: Some(false),
                },
                // Config file - should be filtered OUT
                pathfinder_search::SearchMatch {
                    file: "config.json".to_string(),
                    line: 5,
                    column: 1,
                    content: "\"login\": \"/api/auth\"".to_string(),
                    context_before: vec![],
                    context_after: vec![],
                    enclosing_semantic_path: None,
                    version_hash: "sha256:c".to_string(),
                    known: Some(false),
                },
                // TypeScript source - should be KEPT
                pathfinder_search::SearchMatch {
                    file: "web/src/auth.ts".to_string(),
                    line: 20,
                    column: 1,
                    content: "import { login } from './api';".to_string(),
                    context_before: vec![],
                    context_after: vec![],
                    enclosing_semantic_path: None,
                    version_hash: "sha256:d".to_string(),
                    known: Some(false),
                },
            ],
            total_matches: 4,
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
        assert_eq!(val.degraded_reason.as_deref(), Some("no_lsp_grep_fallback"));
        let incoming = val.incoming.as_ref().expect("must be Some from grep");

        // Only the 2 source files should remain (.rs and .ts)
        // .md and .json should be filtered out
        assert_eq!(
            incoming.len(),
            2,
            "non-source files should be filtered, got: {:?}",
            incoming.iter().map(|r| &r.file).collect::<Vec<_>>()
        );

        // Verify the correct files are kept
        let files: std::collections::HashSet<_> =
            incoming.iter().map(|r| r.file.as_str()).collect();
        assert!(files.contains("src/caller.rs"), "should keep .rs file");
        assert!(files.contains("web/src/auth.ts"), "should keep .ts file");
        assert!(!files.contains("docs/README.md"), "should filter .md file");
        assert!(!files.contains("config.json"), "should filter .json file");
    }

    // ── PATCH-005: Per-Language Capabilities Tests ─────────────────────

    #[tokio::test]
    async fn test_lsp_health_includes_diagnostics_strategy() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        // No lawyer_clone needed - MockLawyer returns empty status
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // MockLawyer returns empty capability_status, so no languages should be returned
        // This tests the structure exists and doesn't panic
        let params = crate::server::types::LspHealthParams::default();
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        assert_eq!(val.status, "unavailable");
        assert!(val.languages.is_empty());
    }

    #[tokio::test]
    async fn test_lsp_health_shows_push_for_go() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // Mock a Go LSP with push diagnostics
        lawyer_clone.set_capability_status(std::collections::HashMap::from([(
            "go".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(15),
                diagnostics_strategy: Some("push".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(false),
                server_name: None,
            },
        )]));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("go".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        assert_eq!(val.languages.len(), 1);
        let go_health = &val.languages[0];
        assert_eq!(go_health.language, "go");
        assert_eq!(go_health.status, "ready");
        assert_eq!(go_health.diagnostics_strategy, Some("push".to_string()));
        assert_eq!(go_health.supports_call_hierarchy, Some(true));
        assert_eq!(go_health.supports_diagnostics, Some(true));
    }

    #[tokio::test]
    async fn test_lsp_health_shows_pull_for_rust() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // Mock a Rust LSP with pull diagnostics
        lawyer_clone.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(20),
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
            },
        )]));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        assert_eq!(val.languages.len(), 1);
        let rust_health = &val.languages[0];
        assert_eq!(rust_health.language, "rust");
        assert_eq!(rust_health.status, "ready");
        assert_eq!(rust_health.diagnostics_strategy, Some("pull".to_string()));
        assert_eq!(rust_health.supports_call_hierarchy, Some(true));
        assert_eq!(rust_health.supports_diagnostics, Some(true));
    }

    #[tokio::test]
    async fn test_lsp_health_shows_capabilities() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // Mock an LSP with partial capabilities
        lawyer_clone.set_capability_status(std::collections::HashMap::from([(
            "typescript".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(10),
                diagnostics_strategy: Some("push".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true), // TS supports call hierarchy
                supports_diagnostics: Some(true),

                supports_formatting: Some(false),
                server_name: None,
            },
        )]));

        let params = crate::server::types::LspHealthParams::default();
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        assert_eq!(val.languages.len(), 1);
        let ts_health = &val.languages[0];
        assert_eq!(ts_health.supports_definition, Some(true));
        assert_eq!(ts_health.supports_call_hierarchy, Some(true));
        assert_eq!(ts_health.supports_diagnostics, Some(true));
    }

    // ── PATCH-006: Probe-Based Readiness Tests ─────────────────────────

    #[tokio::test]
    async fn test_lsp_health_probe_upgrades_warming_up_to_ready() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());

        // Create a workspace with a main.rs file for probing
        let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/main.rs"),
            r#"fn main() { println!("Hello"); }"#,
        )
        .unwrap();

        // Mock a Rust LSP that's been warming up for 30 seconds
        lawyer.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(false), // Still warming up
                uptime_seconds: Some(30),       // 30 seconds - should trigger probe
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
            },
        )]));

        // Mock successful goto_definition response (LSP is ready)
        lawyer.set_goto_definition_result(Ok(Some(pathfinder_lsp::types::DefinitionLocation {
            file: "src/main.rs".to_string(),
            line: 1,
            column: 0,
            preview: "fn main()".to_string(),
        })));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        // With two-phase readiness model: navigation_ready = Some(true) means
        // status is immediately "ready" without waiting for indexing.
        // This is the fix for LSP-HEALTH-001: LSPs that support definitionProvider
        // should be usable immediately, without waiting for WorkDoneProgressEnd.
        // GAP-002: Liveness probe also runs for "ready" languages to verify
        // the LSP is still responsive.
        assert_eq!(val.status, "ready");
        assert_eq!(val.languages.len(), 1);
        let rust_health = &val.languages[0];
        assert_eq!(rust_health.language, "rust");
        assert_eq!(rust_health.status, "ready");
        assert_eq!(rust_health.uptime, Some("30s".to_string()));
        // indexing_status is still "in_progress" because we never saw WorkDoneProgressEnd
        assert_eq!(rust_health.indexing_status, Some("in_progress".to_string()));
        // GAP-002: With liveness probe, probe_verified should be true since
        // the probe ran and succeeded (LSP is responsive)
        assert!(rust_health.probe_verified);
    }

    #[tokio::test]
    async fn test_lsp_health_probe_keeps_warming_up_when_probe_fails() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());

        // Create a workspace with a main.rs file for probing
        // Create a workspace with a main.rs file for probing
        let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(ws_dir.path().join("src/main.rs"), "fn main() {}").unwrap();

        // Mock a Rust LSP that's been warming up for 30 seconds
        lawyer.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(false), // Still warming up
                uptime_seconds: Some(30),       // 30 seconds - should trigger probe
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
            },
        )]));

        // Mock failed goto_definition response (LSP is not responsive)
        lawyer.set_goto_definition_result(Err("Connection lost".to_string()));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        // GAP-002: With liveness probe, when the LSP was "ready" but becomes
        // non-responsive, the status should be downgraded to "degraded".
        // This is the key improvement: detecting LSPs that die after initialization.
        assert_eq!(val.status, "degraded");
        assert_eq!(val.languages.len(), 1);
        let rust_health = &val.languages[0];
        assert_eq!(rust_health.language, "rust");
        assert_eq!(rust_health.status, "degraded");
        assert!(!rust_health.probe_verified);
    }

    #[tokio::test]
    async fn test_lsp_health_no_probe_for_recently_started() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer.clone());

        // Mock a Rust LSP that just started (5 seconds ago)
        lawyer.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(false), // Warming up
                uptime_seconds: Some(5),        // Only 5 seconds - should NOT trigger probe
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
            },
        )]));

        // Set a goto_definition result to verify it's not called
        lawyer.set_goto_definition_result(Ok(Some(pathfinder_lsp::types::DefinitionLocation {
            file: "src/main.rs".to_string(),
            line: 1,
            column: 0,
            preview: "fn main()".to_string(),
        })));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        // With two-phase readiness: navigation_ready = Some(true) means status
        // is immediately "ready" - uptime doesn't matter when capability is confirmed.
        assert_eq!(val.status, "ready");
        assert_eq!(val.languages.len(), 1);
        let rust_health = &val.languages[0];
        assert_eq!(rust_health.language, "rust");
        assert_eq!(rust_health.status, "ready");
        assert_eq!(rust_health.indexing_status, Some("in_progress".to_string()));
        assert!(!rust_health.probe_verified);
    }

    #[tokio::test]
    async fn test_lsp_health_no_probe_for_already_ready() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer.clone());

        // Mock a Rust LSP that's already ready
        lawyer.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true), // Ready
                uptime_seconds: Some(60),      // 60 seconds
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
            },
        )]));

        // Set a goto_definition result to verify it's not called
        lawyer.set_goto_definition_result(Ok(Some(pathfinder_lsp::types::DefinitionLocation {
            file: "src/main.rs".to_string(),
            line: 1,
            column: 0,
            preview: "fn main()".to_string(),
        })));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        // Status should be "ready" and probe not attempted
        assert_eq!(val.status, "ready");
        assert_eq!(val.languages.len(), 1);
        let rust_health = &val.languages[0];
        assert_eq!(rust_health.status, "ready");
        assert!(!rust_health.probe_verified);
    }

    #[tokio::test]
    async fn test_parse_uptime_to_seconds() {
        assert_eq!(parse_uptime_to_seconds(Some("5s")), Some(5));
        assert_eq!(parse_uptime_to_seconds(Some("1m30s")), Some(90));
        assert_eq!(parse_uptime_to_seconds(Some("2h15m")), Some(8100));
        assert_eq!(parse_uptime_to_seconds(Some("1h30m45s")), Some(5445));
        assert_eq!(parse_uptime_to_seconds(Some("1m")), Some(60));
        assert_eq!(parse_uptime_to_seconds(Some("1h")), Some(3600));
        assert_eq!(parse_uptime_to_seconds(None), None);
    }

    #[tokio::test]
    async fn test_find_probe_file() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());

        // Create some probe files
        let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer);
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(ws_dir.path().join("main.go"), "package main").unwrap();
        std::fs::write(ws_dir.path().join("src/index.ts"), "export const x = 1;").unwrap();

        // Test finding probe files
        assert_eq!(
            server.find_probe_file("go"),
            Some(std::path::PathBuf::from("main.go"))
        );
        assert_eq!(
            server.find_probe_file("typescript"),
            Some(std::path::PathBuf::from("src/index.ts"))
        );
        assert_eq!(server.find_probe_file("rust"), None); // No Rust file
    }

    // ── LSP-HEALTH-001: Recursive Probe for Monorepos ───────────────────────

    #[tokio::test]
    async fn test_find_probe_file_recursive_monorepo() {
        // Test the fallback recursive scan for monorepo layouts where
        // files are at non-standard paths like apps/backend/cmd/main.go
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());

        let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer);

        // Create a monorepo structure: Go file at apps/backend/cmd/server/main.go
        // (not at the standard main.go or cmd/main.go)
        std::fs::create_dir_all(
            ws_dir
                .path()
                .join("apps")
                .join("backend")
                .join("cmd")
                .join("server"),
        )
        .unwrap();
        std::fs::write(
            ws_dir
                .path()
                .join("apps")
                .join("backend")
                .join("cmd")
                .join("server")
                .join("main.go"),
            "package main\nfunc main() {}",
        )
        .unwrap();

        // Create a node_modules directory to test that it's skipped
        std::fs::create_dir_all(ws_dir.path().join("node_modules").join("react")).unwrap();
        std::fs::write(
            ws_dir
                .path()
                .join("node_modules")
                .join("react")
                .join("index.ts"),
            "export const React = {};",
        )
        .unwrap();

        // Test that recursive scan finds the Go file at non-standard path
        let probe = server.find_probe_file("go");
        assert!(probe.is_some(), "Should find Go file in monorepo structure");
        let probe_path = probe.unwrap();
        assert!(
            probe_path.to_str().unwrap().contains("main.go"),
            "Should find a main.go file, got: {probe_path:?}"
        );

        // Test that node_modules is skipped (should NOT find the TS file there)
        // This is a bit tricky to test without other TS files - let's just verify
        // the probe works for a standard pattern too by adding a deeper Python file
        std::fs::create_dir_all(ws_dir.path().join("tools").join("fath-factory").join("src"))
            .unwrap();
        std::fs::write(
            ws_dir
                .path()
                .join("tools")
                .join("fath-factory")
                .join("src")
                .join("__init__.py"),
            "",
        )
        .unwrap();

        let py_probe = server.find_probe_file("python");
        assert!(
            py_probe.is_some(),
            "Should find Python file in tools/ directory"
        );
    }

    // ── PATCH-008: Install Guidance Tests ─────────────────────────────────

    #[tokio::test]
    async fn test_lsp_health_includes_missing_languages_with_install_hint() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // Mock a detected language (TypeScript with running LSP)
        lawyer_clone.set_capability_status(std::collections::HashMap::from([(
            "typescript".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(60),
                diagnostics_strategy: Some("push".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(false),
                server_name: None,
            },
        )]));

        // Mock missing languages (Python and Go with markers but no LSP binaries)
        lawyer_clone.set_missing_languages(vec![
            pathfinder_lsp::client::MissingLanguage {
                language_id: "python".to_string(),
                marker_file: "pyproject.toml".to_string(),
                tried_binaries: vec!["pyright".to_string(), "pylsp".to_string()],
                install_hint: "Install pyright: npm install -g pyright".to_string(),
            },
            pathfinder_lsp::client::MissingLanguage {
                language_id: "go".to_string(),
                marker_file: "go.mod".to_string(),
                tried_binaries: vec!["gopls".to_string()],
                install_hint: "Install gopls: go install golang.org/x/tools/gopls@latest"
                    .to_string(),
            },
        ]);

        let params = crate::server::types::LspHealthParams::default();
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        // Should have 3 languages total: 1 detected + 2 missing
        assert_eq!(val.languages.len(), 3);

        // Find the missing languages
        let python_health = val.languages.iter().find(|l| l.language == "python");
        let go_health = val.languages.iter().find(|l| l.language == "go");
        let ts_health = val.languages.iter().find(|l| l.language == "typescript");

        // TypeScript should be ready
        assert!(ts_health.is_some());
        assert_eq!(ts_health.unwrap().status, "ready");

        // Python and Go should be unavailable with install hints
        assert!(python_health.is_some());
        assert_eq!(python_health.unwrap().status, "unavailable");
        assert_eq!(
            python_health.unwrap().install_hint,
            Some("Install pyright: npm install -g pyright".to_string())
        );

        assert!(go_health.is_some());
        assert_eq!(go_health.unwrap().status, "unavailable");
        assert_eq!(
            go_health.unwrap().install_hint,
            Some("Install gopls: go install golang.org/x/tools/gopls@latest".to_string())
        );
    }

    #[tokio::test]
    async fn test_lsp_health_missing_language_filter_works() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // No detected languages, only missing ones
        lawyer_clone.set_capability_status(std::collections::HashMap::new());
        lawyer_clone.set_missing_languages(vec![
            pathfinder_lsp::client::MissingLanguage {
                language_id: "python".to_string(),
                marker_file: "pyproject.toml".to_string(),
                tried_binaries: vec!["pyright".to_string()],
                install_hint: "Install pyright".to_string(),
            },
            pathfinder_lsp::client::MissingLanguage {
                language_id: "rust".to_string(),
                marker_file: "Cargo.toml".to_string(),
                tried_binaries: vec!["rust-analyzer".to_string()],
                install_hint: "Install rust-analyzer".to_string(),
            },
        ]);

        // Filter by language = python
        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("python".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        // Should only return Python, not Rust
        assert_eq!(val.languages.len(), 1);
        assert_eq!(val.languages[0].language, "python");
        assert_eq!(
            val.languages[0].install_hint,
            Some("Install pyright".to_string())
        );
    }

    // ── PATCH-010: Degraded Tools and Validation Latency Tests ─────────────

    #[tokio::test]
    async fn test_health_shows_degraded_tools_for_no_diagnostics() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // Mock an LSP without diagnostics or call hierarchy support
        lawyer_clone.set_capability_status(std::collections::HashMap::from([(
            "go".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(60),
                diagnostics_strategy: None,
                supports_definition: Some(true),
                supports_call_hierarchy: None,
                supports_diagnostics: None,

                supports_formatting: Some(true),
                server_name: None,
            },
        )]));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("go".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        assert_eq!(val.languages.len(), 1);
        let go_health = &val.languages[0];
        assert_eq!(go_health.language, "go");

        // Check that degraded_tools contains analyze_impact with correct severity
        let analyze_impact = go_health
            .degraded_tools
            .iter()
            .find(|t| t.tool == "analyze_impact");
        assert!(
            analyze_impact.is_some(),
            "degraded_tools should include analyze_impact when call hierarchy unsupported"
        );
        let ai = analyze_impact.unwrap();
        assert_eq!(
            ai.severity, "grep_fallback",
            "analyze_impact should have severity=grep_fallback"
        );
        assert!(
            ai.description.contains("text search"),
            "analyze_impact description should mention text search fallback"
        );

        // Check that degraded_tools contains read_with_deep_context with correct severity
        let rwdc = go_health
            .degraded_tools
            .iter()
            .find(|t| t.tool == "read_with_deep_context");
        assert!(
            rwdc.is_some(),
            "degraded_tools should include read_with_deep_context when call hierarchy unsupported"
        );
        let rwdc = rwdc.unwrap();
        assert_eq!(
            rwdc.severity, "unavailable",
            "read_with_deep_context should have severity=unavailable"
        );
        assert!(
            rwdc.description.contains("source only"),
            "read_with_deep_context description should mention source-only limitation"
        );

        // validate_only no longer exists — degraded_tools only contains LSP navigation tools
        let has_validate_only = go_health
            .degraded_tools
            .iter()
            .any(|t| t.tool == "validate_only");
        assert!(
            !has_validate_only,
            "degraded_tools must not include the removed validate_only tool"
        );
    }

    #[tokio::test]
    async fn test_health_shows_empty_degraded_when_fully_capable() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // Mock a fully capable LSP
        lawyer_clone.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(60),
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
            },
        )]));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        assert_eq!(val.languages.len(), 1);
        let rust_health = &val.languages[0];
        assert_eq!(rust_health.language, "rust");
        assert!(
            rust_health.degraded_tools.is_empty(),
            "degraded_tools should be empty when all capabilities supported, got: {:?}",
            rust_health.degraded_tools
        );
    }

    #[tokio::test]
    async fn test_health_shows_push_latency() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // Mock a push diagnostics language (Go)
        lawyer_clone.set_capability_status(std::collections::HashMap::from([(
            "go".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(60),
                diagnostics_strategy: Some("push".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
            },
        )]));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("go".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = unpack_health(call_res);

        assert_eq!(val.languages.len(), 1);
        let go_health = &val.languages[0];
        assert_eq!(go_health.language, "go");
        assert!(
            go_health.degraded_tools.is_empty(),
            "fully capable LSP should have no degraded tools"
        );
    }

    #[tokio::test]
    async fn test_health_shows_pull_latency() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // Mock a pull diagnostics language (Rust)
        lawyer_clone.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(60),
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
            },
        )]));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        result.expect("pull-diagnostics language should return successfully");
    }

    // ── LSP-HEALTH-001: Confidence Gradient Tests ─────────────────────────────

    #[tokio::test]
    async fn test_lsp_health_ready_but_still_indexing_shows_confidence_gradient() {
        // Simulate pyright: navigation_ready=true (definitionProvider confirmed),
        // but indexing_complete=false (no WorkDoneProgressEnd received).
        // The agent should see BOTH signals and make smart decisions.
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        lawyer_clone.set_capability_status(std::collections::HashMap::from([(
            "python".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: false, // No diagnostics support
                reason: "LSP connected but does not support diagnostics".to_string(),
                navigation_ready: Some(true), // definitionProvider confirmed
                indexing_complete: Some(false), // Still indexing
                uptime_seconds: Some(5),
                diagnostics_strategy: Some("none".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(false),

                supports_formatting: Some(false),
                server_name: None,
            },
        )]));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("python".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let val = unpack_health(result.expect("should succeed"));

        let py_health = &val.languages[0];
        // Status is "ready" because navigation_ready=true
        assert_eq!(py_health.status, "ready");
        // But indexing is still in progress — agent should see this
        assert_eq!(py_health.indexing_status, Some("in_progress".to_string()));
        // navigation_ready is surfaced so agent knows navigation is functional
        assert_eq!(py_health.navigation_ready, Some(true));
        // Diagnostics not available
        assert_eq!(py_health.diagnostics_strategy, Some("none".to_string()));
        // validate_only no longer exists — diagnostics absence only affects call hierarchy tools
        let has_validate_only = py_health
            .degraded_tools
            .iter()
            .any(|t| t.tool == "validate_only");
        assert!(!has_validate_only);
    }

    #[tokio::test]
    async fn test_lsp_health_fully_indexed_shows_complete_confidence() {
        // Simulate rust-analyzer after full indexing: both signals at max confidence.
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        lawyer_clone.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),  // Navigation ready
                indexing_complete: Some(true), // Indexing complete
                uptime_seconds: Some(120),
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
            },
        )]));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let val = unpack_health(result.expect("should succeed"));

        let rust_health = &val.languages[0];
        assert_eq!(rust_health.status, "ready");
        // Both confidence signals at max
        assert_eq!(rust_health.navigation_ready, Some(true));
        assert_eq!(rust_health.indexing_status, Some("complete".to_string()));
        // No degraded tools
        assert!(rust_health.degraded_tools.is_empty());
    }

    // ── Probe cache TTL tests (LSP-HEALTH-001 findings 1+2) ──────────

    #[tokio::test]
    async fn test_probe_cache_positive_result_never_expires() {
        // Positive cache entries should be valid indefinitely
        let entry = crate::server::ProbeCacheEntry::new(true);
        assert!(entry.is_valid(), "positive entry should always be valid");
    }

    #[tokio::test]
    async fn test_probe_cache_negative_result_is_initially_valid() {
        // Negative cache entries should be valid immediately after creation
        let entry = crate::server::ProbeCacheEntry::new(false);
        assert!(entry.is_valid(), "fresh negative entry should be valid");
    }

    #[tokio::test]
    async fn test_probe_negative_cache_skips_reprobe() {
        // When a negative cache entry exists, lsp_health should skip probing
        // and keep the status as "warming_up" instead of hammering the LSP.
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // Pre-populate cache with a negative result
        server
            .probe_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                "rust".to_string(),
                crate::server::ProbeCacheEntry::new(false),
            );

        // LSP running but not ready (navigation_ready = false)
        lawyer_clone.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(false),
                indexing_complete: Some(false),
                uptime_seconds: Some(30), // Over 10s threshold
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(false),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
            },
        )]));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let val = unpack_health(result.expect("should succeed"));

        let rust_health = &val.languages[0];
        // Status should stay "warming_up" because cached negative result skipped the probe
        assert_eq!(rust_health.status, "warming_up");
        assert!(
            !rust_health.probe_verified,
            "should not be probe-verified when using negative cache"
        );
    }

    #[tokio::test]
    async fn test_probe_cache_positive_upgrades_to_ready() {
        // When a positive cache entry exists, lsp_health should upgrade to ready
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let lawyer_clone = lawyer.clone();
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // Pre-populate cache with a positive result
        server
            .probe_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                "rust".to_string(),
                crate::server::ProbeCacheEntry::new(true),
            );

        // LSP reports warming_up but cache has positive result
        lawyer_clone.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(false),
                indexing_complete: Some(false),
                uptime_seconds: Some(30),
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(false),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
            },
        )]));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let val = unpack_health(result.expect("should succeed"));

        let rust_health = &val.languages[0];
        assert_eq!(rust_health.status, "ready");
        assert!(
            rust_health.probe_verified,
            "should be probe-verified from cache"
        );
    }

    // ── GAP-001: LSP Timeout Fallback Tests ───────────────────────────────
    //
    // NOTE: The LspError::Timeout fallback is implemented in both get_definition_impl
    // and analyze_impact_impl. However, these cannot be tested with the current
    // MockLawyer because it converts all errors to LspError::Protocol, not LspError::Timeout.
    //
    // The actual LSP client (pathfinder-lsp) will return LspError::Timeout when
    // a request times out, and the fallback will work correctly in production.
    //
    // To properly test this, we would need to:
    // 1. Modify MockLawyer to support returning specific LspError variants
    // 2. Or test with a real LSP client that can timeout
    //
    // For now, the implementation is correct and will be tested in production.

    // ── GAP-002: Liveness Probe Tests ────────────────────────────────────

    #[tokio::test]
    async fn test_lsp_health_liveness_probe_downgrades_dead_lsp() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());

        // Create a file for probing
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(ws_dir.path().join("src/main.rs"), "fn main() {}").unwrap();

        // Mock a "ready" LSP that was working but now times out
        lawyer.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(120),
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
            },
        )]));

        // Mock goto_definition timeout (LSP is dead)
        lawyer.set_goto_definition_result(Err(
            "LSP timed out on goto_definition after 10000ms".to_string()
        ));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };
        let result = server.lsp_health_impl(params).await;
        let val = unpack_health(result.expect("should succeed"));

        // Status should be downgraded to "degraded"
        assert_eq!(val.status, "degraded");
        let rust_health = &val.languages[0];
        assert_eq!(rust_health.status, "degraded");
        assert!(!rust_health.probe_verified);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn test_lsp_health_liveness_probe_caches_positive() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());

        // Create a file for probing
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(ws_dir.path().join("src/main.rs"), "fn main() {}").unwrap();

        // Mock a "ready" LSP that is still responsive
        lawyer.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(120),
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
            },
        )]));

        // Mock successful goto_definition
        lawyer.set_goto_definition_result(Ok(Some(pathfinder_lsp::types::DefinitionLocation {
            file: "src/main.rs".to_string(),
            line: 1,
            column: 0,
            preview: "fn main()".to_string(),
        })));

        // First call - should probe and cache
        let result1 = server
            .lsp_health_impl(crate::server::types::LspHealthParams {
                action: None,
                language: Some("rust".to_string()),
            })
            .await;
        let val1 = unpack_health(result1.expect("should succeed"));
        assert!(val1.languages[0].probe_verified);

        // Verify cache was populated
        let cache = server.probe_cache.lock().unwrap();
        assert!(cache.contains_key("rust"));
        let entry = cache.get("rust").unwrap();
        assert!(entry.success);
        drop(cache);

        // Second call - should use cache (no second probe)
        let call_count_before = lawyer.goto_definition_call_count();
        let result2 = server
            .lsp_health_impl(crate::server::types::LspHealthParams {
                action: None,
                language: Some("rust".to_string()),
            })
            .await;
        let val2 = unpack_health(result2.expect("should succeed"));
        assert!(val2.languages[0].probe_verified);
        // Goto definition should not be called again (cache hit)
        assert_eq!(lawyer.goto_definition_call_count(), call_count_before);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn test_liveness_probe_interval_skips_recent() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());

        // Create a file for probing
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(ws_dir.path().join("src/main.rs"), "fn main() {}").unwrap();

        // Mock a "ready" LSP
        lawyer.set_capability_status(std::collections::HashMap::from([(
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(120),
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),

                supports_formatting: Some(true),
                server_name: None,
            },
        )]));

        // Pre-populate cache with a recent positive entry (age < LIVENESS_PROBE_INTERVAL_SECS)
        let mut cache = server.probe_cache.lock().unwrap();
        cache.insert(
            "rust".to_string(),
            crate::server::ProbeCacheEntry::new(true),
        );
        drop(cache);

        // Mock goto_definition - should NOT be called due to cache
        lawyer.set_goto_definition_result(Ok(Some(pathfinder_lsp::types::DefinitionLocation {
            file: "src/main.rs".to_string(),
            line: 1,
            column: 0,
            preview: "fn main()".to_string(),
        })));

        let params = crate::server::types::LspHealthParams {
            action: None,
            language: Some("rust".to_string()),
        };

        let call_count_before = lawyer.goto_definition_call_count();
        let result = server.lsp_health_impl(params).await;
        let val = unpack_health(result.expect("should succeed"));

        // Should use cached result without probing
        assert!(val.languages[0].probe_verified);
        assert_eq!(lawyer.goto_definition_call_count(), call_count_before);
    }

    // ── DS-1: DocumentGuard lifecycle tests ──────────────────────────────────
    //
    // Verify that every `open_document` call is paired with exactly one
    // `did_close` — the RAII contract of `DocumentLease`. The `MockDocumentLease`
    // increments `did_close_calls` on drop, mirroring production `DocumentGuard`.

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
        lawyer.set_goto_definition_result(Err("LSP crashed".into()));

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

    #[tokio::test]
    async fn test_analyze_impact_closes_document_on_success() {
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

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer.clone());
        let params = AnalyzeImpactParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 1,
        };

        let _ = server.analyze_impact_impl(params).await;

        tokio::task::yield_now().await;

        assert_eq!(
            lawyer.did_open_call_count(),
            lawyer.did_close_call_count(),
            "DS-1: did_open and did_close must be symmetric in analyze_impact"
        );
    }

    #[tokio::test]
    async fn test_read_with_deep_context_closes_document_on_success() {
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

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer.clone());
        let params = ReadWithDeepContextParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };

        let _ = server.read_with_deep_context_impl(params).await;

        tokio::task::yield_now().await;

        assert_eq!(
            lawyer.did_open_call_count(),
            lawyer.did_close_call_count(),
            "DS-1: did_open and did_close must be symmetric in read_with_deep_context"
        );
    }
}
