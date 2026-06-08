//! Pathfinder MCP Server — tool registration and dispatch.
//!
//! Implements `rmcp::ServerHandler` for all Pathfinder discovery & navigation tools.
//!
//! # Module Layout
//! - [`helpers`] — error conversion, stub builder, language detection
//! - [`types`] — all parameter and response structs
//! - [`tools`] — handler logic, one submodule per tool group:
//!   - [`tools::search`] — `search_codebase`
//!   - [`tools::repo_map`] — `get_repo_map`
//!   - [`tools::symbols`] — `read_symbol_scope`, `read_with_deep_context`
//!   - [`tools::navigation`] — `get_definition`, `find_callers_callees`
//!   - [`tools::file_ops`] — `read_file`
//!   - [`tools::source_file`] — `read_source_file`

/// Duration after which a negative probe cache entry expires.
/// Allows re-probing an LSP that was still starting when first checked.
const PROBE_NEGATIVE_TTL_SECS: u64 = 60;

/// A cached probe result with optional expiry for negative entries.
///
/// Positive entries (success) are cached indefinitely for liveness re-probe.
/// Negative entries (failure) expire after `PROBE_NEGATIVE_TTL_SECS` to allow
/// an LSP that was still starting to be re-probed later.
#[derive(Clone)]
pub(crate) struct ProbeCacheEntry {
    /// Whether the probe succeeded.
    pub(crate) success: bool,
    /// When this entry was created. Used to check TTL for negative entries and age for liveness re-probe.
    pub(crate) created_at: std::time::Instant,
    /// Optional TTL for expiration (negative entries only). Positive entries use age-based re-probe.
    pub(crate) ttl: Option<std::time::Duration>,
}

impl ProbeCacheEntry {
    pub(crate) fn new(success: bool) -> Self {
        Self {
            success,
            created_at: std::time::Instant::now(),
            ttl: if success {
                None // Positive entries: use age-based re-probe instead of expiry
            } else {
                Some(std::time::Duration::from_secs(PROBE_NEGATIVE_TTL_SECS))
            },
        }
    }

    /// Returns true if this entry is still valid.
    /// Positive entries never expire (liveness re-probe handles staleness).
    /// Negative entries expire after `PROBE_NEGATIVE_TTL_SECS`.
    pub(crate) fn is_valid(&self) -> bool {
        match self.ttl {
            Some(ttl) => self.created_at.elapsed() < ttl,
            None => true, // Positive entries never expire (liveness re-probe handles staleness)
        }
    }

    /// How old is this cache entry in seconds?
    /// Used by liveness probe to determine when to re-probe "ready" languages.
    pub(crate) fn age_secs(&self) -> u64 {
        self.created_at.elapsed().as_secs()
    }
}

mod helpers;
mod tools;
/// Module containing type definitions.
pub mod types;

use types::{
    FindCallersCalleesParams, FindSymbolParams, GetDefinitionParams, GetRepoMapParams,
    ReadFileParams, ReadFilesParams, ReadSourceFileParams, ReadSymbolScopeParams,
    ReadWithDeepContextParams, SearchCodebaseParams, SearchCodebaseResponse,
};

use pathfinder_common::config::PathfinderConfig;
use pathfinder_common::sandbox::Sandbox;
use pathfinder_common::types::WorkspaceRoot;
use pathfinder_lsp::{Lawyer, LspClient, NoOpLawyer};
use pathfinder_search::{RipgrepScout, Scout};
use pathfinder_treesitter::{Surgeon, TreeSitterSurgeon};

use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{ErrorData, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ServerHandler};

use std::sync::Arc;

/// The main Pathfinder MCP server.
///
/// Holds shared workspace state and dispatches MCP tool calls.
#[derive(Clone)]
pub struct PathfinderServer {
    workspace_root: Arc<WorkspaceRoot>,
    sandbox: Arc<Sandbox>,
    scout: Arc<dyn Scout>,
    surgeon: Arc<dyn Surgeon>,
    lawyer: Arc<dyn Lawyer>,
    // Populated by `#[tool_router]` and consumed through the generated
    // `tool_handler` trait impl. The compiler's dead-code pass cannot follow
    // the read path across the proc-macro boundary, so we suppress the lint.
    #[expect(dead_code)]
    tool_router: ToolRouter<Self>,
    /// Cache of probe results per language to avoid redundant LSP calls.
    ///
    /// Positive results (true) are cached indefinitely — once a language's LSP
    /// responds to a probe, it stays "ready" for the session.
    ///
    /// Negative results (false) are cached with a TTL of 60s. This prevents
    /// hammering a still-starting LSP with probes on every `lsp_health` call,
    /// while allowing recovery once the LSP finishes initializing.
    probe_cache: Arc<std::sync::Mutex<std::collections::HashMap<String, ProbeCacheEntry>>>,
}

impl PathfinderServer {
    /// Create a new Pathfinder server backed by the real Ripgrep scout, Tree-sitter
    /// surgeon, and `LspClient` for LSP operations.
    ///
    /// Zero-Config language detection (PRD §6.5) runs synchronously during construction.
    /// LSP processes are started **lazily** — only when the first LSP-dependent tool call
    /// is made for a given language.
    ///
    /// If Zero-Config detection fails (e.g., unreadable workspace directory), the server
    /// falls back to `NoOpLawyer` and logs a warning. All tools remain functional in
    /// degraded mode.
    #[must_use]
    pub async fn new(workspace_root: WorkspaceRoot, config: PathfinderConfig) -> Self {
        let sandbox = Sandbox::new(workspace_root.path(), &config.sandbox);

        let lawyer: Arc<dyn Lawyer> =
            match LspClient::new(workspace_root.path(), Arc::new(config.clone())).await {
                Ok(client) => {
                    // Kick off background initialization so LSP processes are
                    // already loading while the agent issues its first non-LSP
                    // tool calls (get_repo_map, search_codebase, etc.).
                    client.warm_start();
                    tracing::info!(
                        workspace = %workspace_root.path().display(),
                        "LspClient initialised (warm start in progress)"
                    );
                    Arc::new(client)
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "LSP Zero-Config detection failed — degraded mode (NoOpLawyer)"
                    );
                    Arc::new(NoOpLawyer)
                }
            };

        Self::with_all_engines(
            workspace_root,
            config,
            sandbox,
            Arc::new(RipgrepScout),
            Arc::new(TreeSitterSurgeon::new(100)), // Cache capacity of 100 files
            lawyer,
        )
    }

    /// Create a server with injected Scout and Surgeon engines (for testing).
    ///
    /// Uses a `NoOpLawyer` for LSP operations — keeps existing tests unchanged.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn with_engines(
        workspace_root: WorkspaceRoot,
        config: PathfinderConfig,
        sandbox: Sandbox,
        scout: Arc<dyn Scout>,
        surgeon: Arc<dyn Surgeon>,
    ) -> Self {
        Self::with_all_engines(
            workspace_root,
            config,
            sandbox,
            scout,
            surgeon,
            Arc::new(NoOpLawyer),
        )
    }

    /// Create a server with all three engines injected (for testing with a `MockLawyer`).
    #[must_use]
    #[allow(clippy::needless_pass_by_value)] // Preserve API compatibility; 20+ call sites in tests
    pub fn with_all_engines(
        workspace_root: WorkspaceRoot,
        _config: PathfinderConfig,
        sandbox: Sandbox,
        scout: Arc<dyn Scout>,
        surgeon: Arc<dyn Surgeon>,
        lawyer: Arc<dyn Lawyer>,
    ) -> Self {
        Self {
            workspace_root: Arc::new(workspace_root),
            sandbox: Arc::new(sandbox),
            scout,
            surgeon,
            lawyer,
            tool_router: Self::tool_router(),
            probe_cache: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }
}

// ── Tool Router (defines all 13 tools) ──────────────────────────────

#[tool_router]
impl PathfinderServer {
    #[tool(
        name = "search_codebase",
        description = "Search for text or regex patterns across source code. Returns matching lines with `enclosing_semantic_path` (the symbol containing the match).

Use when: Finding text/patterns across the codebase, finding where functions are called or mentioned.
Alternative: Use `find_symbol` to resolve a bare name to semantic paths. Use `get_definition` to jump to where a symbol is defined (not just mentioned).

Parameter guidance:
- `max_results=50` (default): Increase for exhaustive searches; decrease for quick lookups.
- `filter_mode=\"code_only\"` (default): Use \"all\" to include comments/strings.
- `group_by_file=true`: Consolidate matches (recommended for multi-match edits).

Common issues:
- Too many matches: Narrow with `path_glob=\"**/*.rs\"` or similar.
- No matches: Try `filter_mode=\"all\"` (match may be in comment/string).
- `degraded=true` results: Use `search_codebase` as fallback when LSP tools return degraded.

Examples:
- `search_codebase(query=\"login\", path_glob=\"**/*.rs\", filter_mode=\"code_only\")`
- `search_codebase(query=\"TODO|FIXME\", is_regex=true)`"
    )]
    async fn search_codebase(
        &self,
        Parameters(params): Parameters<SearchCodebaseParams>,
    ) -> Result<Json<SearchCodebaseResponse>, ErrorData> {
        self.search_codebase_impl(params).await
    }

    #[tool(
        name = "get_repo_map",
        description = "Get the structural skeleton of the project — an indented tree of symbols with their semantic paths.

Use when: Exploring project structure, discovering available symbols, or planning navigation.
Alternative: Use `read_source_file(detail_level=\"symbols\")` for a single file's structure. Use `find_symbol` to locate a specific symbol by name.

IMPORTANT: Copy-paste the exact semantic paths from the output into other Pathfinder tools.

Navigation quick reference:
- Find a symbol's file: find_symbol(name=\"SymbolName\")
- Read one function: read_symbol_scope(semantic_path=\"file.rs::function\")
- Read full file: read_source_file(filepath=\"file.rs\")
- Find definition: get_definition(semantic_path=\"file.rs::symbol\")
- Find all callers: find_callers_callees(semantic_path=\"file.rs::symbol\")
- Find all usages: find_all_references(semantic_path=\"file.rs::symbol\")
- Search code: search_codebase(query=\"pattern\")

Parameter guidance:
- `max_tokens` (default 16000): Total token budget. Increase for more coverage.
- `max_tokens_per_file` (default 2000): Per-file cap. Increase if files show [TRUNCATED].
- `visibility`: \"public\" (default) or \"all\" (includes private/internal).
- `include_imports`: \"none\", \"third_party\" (default), or \"all\".
- `changed_since`: Filter to recently modified files (e.g., '3h', 'HEAD~5').

Example: `get_repo_map(path=\"src/\", visibility=\"all\")`"
    )]
    async fn get_repo_map(
        &self,
        Parameters(params): Parameters<GetRepoMapParams>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::model::ErrorData> {
        self.get_repo_map_impl(params).await
    }

    #[tool(
        name = "read_symbol_scope",
        description = "Extract the exact source code of one symbol (function, class, method) by semantic path.

Use when: You know the exact symbol and want only its source code (no surrounding context).
Alternative: Use `read_source_file` for full file content, or `read_file` for config/non-source files.

IMPORTANT: semantic_path MUST include file path + '::' (e.g., 'src/auth.ts::AuthService.login').

Example: `read_symbol_scope(semantic_path=\"src/auth.ts::AuthService.login\")`

Error format:
- SYMBOL_NOT_FOUND: includes `details.did_you_mean` and `hint` with recovery guidance. Use `find_symbol(name=...)` or `search_codebase(query=...)` to locate the correct path.
- FILE_NOT_FOUND: includes `details.path`. Use `search_codebase(query=\"...\")` to find the correct path.
- INVALID_SEMANTIC_PATH: includes `details.issue`. Ensure path uses `file::symbol` format.

Common issues:
- SYMBOL_NOT_FOUND: Use `find_symbol(name=\"SymbolName\")` to discover the correct path, or `read_source_file(filepath, detail_level=\"symbols\")` to see available symbols.
- FILE_NOT_FOUND: Use `search_codebase` to find the correct path."
    )]
    async fn read_symbol_scope(
        &self,
        Parameters(params): Parameters<ReadSymbolScopeParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.read_symbol_scope_impl(params).await
    }

    #[tool(
        name = "read_source_file",
        description = "Read an entire file with optional AST symbol hierarchy. For supported languages, returns source, language, and nested symbol tree with semantic paths. For unsupported languages (.sql, .yaml, .toml, etc.), gracefully returns raw content with `unsupported_language: true` in metadata.

Use when: You need to explore a file's structure, find symbols in a file, or read a large file efficiently. Also works for config files and non-source types when you want consistent response format.
Alternative: Use `read_symbol_scope` for a single symbol, or `read_file` for raw config-only reading with line counts.

Supported languages for AST parsing: .rs, .ts, .tsx, .go, .py, .vue, .jsx, .js. Other file types return raw content only.

Parameter guidance:
- `detail_level`: \"source_only\" (lowest tokens), \"compact\" (default, source + flat symbols when available), \"symbols\" (tree only), \"full\" (source + nested AST when available).
- `start_line`/`end_line`: Restrict output to a line range. Works for both supported and unsupported languages.

Example: `read_source_file(filepath=\"src/auth.ts\", detail_level=\"compact\")`"
    )]
    async fn read_source_file(
        &self,
        Parameters(params): Parameters<ReadSourceFileParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.read_source_file_impl(params).await
    }

    #[tool(
        name = "read_with_deep_context",
        description = "Read a symbol's source code PLUS the signatures of all functions it calls — its dependency graph in one call.

Use when: Understanding what a function calls and how it fits into the codebase.
Alternative: Use `read_symbol_scope` for source only (no deps), or `symbol_overview` for comprehensive info.

IMPORTANT: semantic_path MUST include file path + '::' (e.g. 'src/auth.ts::AuthService.login').

LSP-powered; first call may take 5–30s during LSP warmup. Check `degraded` in response — if true, dependencies may be incomplete. Source code is always returned even when degraded.

Error format:
- SYMBOL_NOT_FOUND: includes `details.did_you_mean` and `hint`. Use `find_symbol(name=...)` to discover the correct path.
- FILE_NOT_FOUND: includes `details.path`. Use `search_codebase` to find the correct path.

Common issues:
- No dependencies found with `degraded=true`: LSP not ready. Source code is still valid, but dependencies are heuristic or missing.
- SYMBOL_NOT_FOUND: Use `find_symbol(name=\"SymbolName\")` to discover the correct path.
- DEGRADED results: Check `lsp_health` for warmup status. Use `search_codebase(query=\"function_name\")` as heuristic fallback.

Example: `read_with_deep_context(semantic_path=\"src/auth.ts::AuthService.login\")`"
    )]
    async fn read_with_deep_context(
        &self,
        Parameters(params): Parameters<ReadWithDeepContextParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.read_with_deep_context_impl(params).await
    }

    #[tool(
        name = "get_definition",
        description = "Jump to where a symbol is defined. Returns the definition's file, line, column, and a code preview.

Use when: You need to navigate to a symbol's definition site.
Alternative: Use `find_symbol` when you don't know which file defines it.

IMPORTANT: semantic_path MUST include file path + '::' (e.g. 'src/auth.ts::AuthService.login'). If you don't know which file defines a symbol, use `find_symbol` first.

LSP-powered — follows imports, re-exports, and type aliases across files. Falls back to ripgrep when LSP is unavailable. Check `degraded` in response. When SYMBOL_NOT_FOUND includes `retry_after_seconds`, the LSP is still warming up — retry after the indicated delay.

Error format:
- SYMBOL_NOT_FOUND: includes `details.did_you_mean`, `details.retry_after_seconds`, and `hint`. Check `did_you_mean` for alternatives; retry after `retry_after_seconds` if LSP is warming up.
- FILE_NOT_FOUND: includes `details.path`. Use `search_codebase` to find the correct path.

Common issues:
- Returns wrong definition: Grep fallback found a similar name. Check `degraded_reason`. Wait for LSP warmup and retry.
- Returns no results: Symbol may be in a different file. Use `find_symbol(name=\"SymbolName\")` to locate it.
- SYMBOL_NOT_FOUND: Use `find_symbol(name=\"SymbolName\")` to discover the correct path, or `read_source_file(filepath, detail_level=\"symbols\")` to see available symbols.

Example: `get_definition(semantic_path=\"src/auth.ts::AuthService.login\")`"
    )]
    async fn get_definition(
        &self,
        Parameters(params): Parameters<GetDefinitionParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.get_definition_impl(params).await
    }

    #[tool(
        name = "find_symbol",
        description = "Resolve a bare symbol name to its file::symbol semantic path(s). Use when you know a symbol's name but not its file. Returns matching definitions with file, line, kind, and a code preview.

Use when: You know a symbol name but not its file path, or need to discover the semantic path for other tools.
Alternative: Use `get_definition` when you already have the full semantic_path. Use `search_codebase` for text pattern matching.

Filter by `kind` (e.g. \"class\", \"function\", \"struct\") to narrow results. Use `path_glob` to limit search scope. Faster than `get_repo_map` + `search_codebase` for symbol lookup.

Example: `find_symbol(name=\"AuthService\", kind=\"class\")`"
    )]
    async fn find_symbol(
        &self,
        Parameters(params): Parameters<FindSymbolParams>,
    ) -> Result<Json<types::FindSymbolResponse>, ErrorData> {
        self.find_symbol_impl(params).await
    }

    #[tool(
        name = "find_callers_callees",
        description = "Find all callers (incoming) and callees (outgoing) of a symbol — who calls this function and what does it call? Use this to understand the blast radius before refactoring.

ALWAYS run this tool before recommending a refactor to check for unexpected callers.

Use when: Understanding blast radius before refactoring. Shows who calls this symbol and what it calls.
Alternative: Use `find_all_references` for exhaustive reference enumeration (including non-call references).

IMPORTANT: semantic_path MUST include file path + '::' (e.g., 'src/mod.rs::func'). If unsure of the path, use `find_symbol(name=\"func\")` to discover it first.

Parameter guidance:
- `max_depth=3` (default): Standard refactoring. Shows direct + 1-hop callers/callees.
- `max_depth=4-5`: Large-scale API changes. Shows full transitive blast radius.
- `max_references=50` (default): Caps output to prevent context overflow. Increase to 100-200 for exhaustive analysis on small codebases.
- `project_only=true` (default): Excludes stdlib/vendor references.

LSP-backed with grep fallback. Check `degraded` flag in response:
- `degraded=false`: LSP-confirmed, authoritative results
- `degraded=true`: grep heuristic, may over/under-count. Use `search_codebase` to verify.
- When `degraded=true`, `incoming`/`outgoing` are `null` (not `[]`) — do NOT treat empty as \"confirmed no callers\". When not degraded: empty arrays `[]` = LSP confirmed zero callers/callees.

Error format:
- SYMBOL_NOT_FOUND: includes `details.did_you_mean` and `hint`. Use `find_symbol(name=...)` to discover the correct path.
- FILE_NOT_FOUND: includes `details.path`.

Example: `find_callers_callees(semantic_path=\"src/auth.ts::AuthService.login\", max_depth=3)`"
    )]
    async fn find_callers_callees(
        &self,
        Parameters(params): Parameters<FindCallersCalleesParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.find_callers_callees_impl(params).await
    }

    #[tool(
        name = "find_all_references",
        description = "Find all references to a symbol across the entire codebase. Uses LSP textDocument/references to find all usages (function calls, field accesses, imports, etc.). Unlike `find_callers_callees` (call hierarchy only), this returns every reference including type annotations, imports, and field access.

Use when: Finding all usages of a symbol (not just callers), or when `find_callers_callees` misses references.
Alternative: Use `find_callers_callees` for call hierarchy (incoming/outgoing callers). Use `search_codebase` for text pattern matching.

Supports `max_results` (default 50) and `offset` for pagination through large result sets. IMPORTANT: semantic_path MUST include file path + '::' (e.g., 'src/mod.rs::func'). LSP-powered. When degraded, use `search_codebase` as fallback.

Error format:
- SYMBOL_NOT_FOUND: includes `details.did_you_mean` and `hint`. Use `find_symbol(name=...)` to discover the correct path.
- FILE_NOT_FOUND: includes `details.path`.

Common issues:
- Empty results with `degraded=true`: LSP not ready. Results are unknown, NOT confirmed zero.
- Too many results: Use `max_results` to cap output.
- Missing references: Use `search_codebase(query=\"symbol_name\")` as heuristic fallback.
- SYMBOL_NOT_FOUND: Use `find_symbol(name=\"SymbolName\")` to discover the correct path.

Example: `find_all_references(semantic_path=\"src/auth.ts::AuthService.login\", max_results=50)`"
    )]
    async fn find_all_references(
        &self,
        Parameters(params): Parameters<crate::server::types::FindAllReferencesParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.find_all_references_impl(params).await
    }

    #[tool(
        name = "symbol_overview",
        description = "Get comprehensive information about a symbol in one call: source code, callers, callees, and references. Combines `read_symbol_scope` + `find_callers_callees` + `find_all_references`.

Use when: Initial analysis before refactoring, or when you need a complete picture of a symbol.
Alternative: Use individual tools (`read_symbol_scope`, `find_callers_callees`, `find_all_references`) for more control over parameters.

Use `project_only=true` (default) to filter out stdlib/vendor references. Use `max_callers_callees` and `max_references` to cap output. IMPORTANT: semantic_path MUST include file path + '::' (e.g. 'src/mod.rs::func'). When degraded, partial results are returned with LSP fallback indicators.

Error format:
- SYMBOL_NOT_FOUND: includes `details.did_you_mean` and `hint`. Use `find_symbol(name=...)` to discover the correct path.
- FILE_NOT_FOUND: includes `details.path`.

Common issues:
- Partial results with `degraded=true`: LSP not ready. Source code is always valid; callers/callees/references may be incomplete.
- SYMBOL_NOT_FOUND: Use `find_symbol(name=\"SymbolName\")` to discover the correct path.
- Callers/callees are null (not empty arrays): LSP degraded. Use `search_codebase` as fallback.

Example: `symbol_overview(semantic_path=\"src/auth.ts::AuthService.login\")`"
    )]
    async fn symbol_overview(
        &self,
        Parameters(params): Parameters<crate::server::types::SymbolOverviewParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.symbol_overview_impl(params).await
    }

    #[tool(
        name = "lsp_health",
        description = "Check LSP health per language. Returns overall status (ready / warming_up / starting / unavailable) and per-language details including `navigation_ready`, `indexing_status`, `supports_call_hierarchy`, and `degraded_tools`.

Use when: Diagnosing why a navigation tool returned degraded results, or checking if LSP is ready before calling navigation tools.
Alternative: Individual tool responses include `degraded` and `lsp_readiness` fields.

Pass `language` to check a specific language, or omit to check all. Pass `action=\"restart\"` with `language` to force-restart a stuck LSP process.

Example: `lsp_health(language=\"rust\")`"
    )]
    async fn lsp_health(
        &self,
        Parameters(params): Parameters<crate::server::types::LspHealthParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.lsp_health_impl(params).await
    }

    #[tool(
        name = "read_file",
        description = "Read raw file content with optional pagination (start_line, max_lines).

Use when: Reading config files (.env, YAML, TOML, Dockerfile, package.json) or non-source files.
Alternative: Use `read_source_file` for source code with AST metadata. Use `read_symbol_scope` for a single symbol.

Example: `read_file(filepath=\".env\", start_line=1, max_lines=50)`"
    )]
    async fn read_file(
        &self,
        Parameters(params): Parameters<ReadFileParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.read_file_impl(params).await
    }

    #[tool(
        name = "read_files",
        description = "Batch read multiple files in a single call with per-file error resilience. Max 10 files per call.

Use when: Reading multiple files at once (e.g., comparing implementations, gathering context from several files).
Alternative: Use `read_file` for a single file, or `read_source_file` for source files with full AST metadata.

For source files (.rs, .ts, .tsx, .go, .py, .vue, .js, .jsx), returns AST-parsed content. For config files (.json, .yaml, .toml, .env, Dockerfile), returns raw content.

Parameter guidance:
- `detail_level`: \"source_only\" (lowest tokens), \"compact\" (default), \"full\" (AST + source).
- `max_lines_per_file`: Cap output per file (default 500).

Example: `read_files(paths=[\"src/auth.ts\", \"src/config.ts\"], detail_level=\"compact\")`"
    )]
    async fn read_files(
        &self,
        Parameters(params): Parameters<ReadFilesParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.read_files_impl(params).await
    }
}

// ── ServerHandler trait impl ────────────────────────────────────────

#[tool_handler]
impl ServerHandler for PathfinderServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("pathfinder", env!("CARGO_PKG_VERSION")))
    }
}

// ── Language Detection ──────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use pathfinder_common::types::FilterMode;
    use pathfinder_search::{MockScout, SearchMatch, SearchResult};
    use pathfinder_treesitter::mock::MockSurgeon;
    use pathfinder_treesitter::surgeon::{AccessLevel, ExtractedSymbol, SymbolKind};
    use rmcp::model::ErrorCode;
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_get_repo_map_success() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let mock_surgeon = MockSurgeon::new();
        mock_surgeon
            .generate_skeleton_results
            .lock()
            .unwrap()
            .push(Ok(pathfinder_treesitter::repo_map::RepoMapResult {
                skeleton: "class Mock {}".to_string(),
                tech_stack: vec!["TypeScript".to_string()],
                files_scanned: 1,
                files_truncated: 0,
                truncated_paths: vec![],
                files_in_scope: 1,
                coverage_percent: 100,
                version_hashes: std::collections::HashMap::default(),
            }));

        let server = PathfinderServer::with_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            Arc::new(mock_surgeon),
        );

        let params = GetRepoMapParams {
            path: ".".to_owned(),
            max_tokens: 16_000,
            depth: 3,
            visibility: pathfinder_common::types::Visibility::Public,
            max_tokens_per_file: 2000,
            changed_since: String::default(),
            include_extensions: vec![],
            exclude_extensions: vec![],
            include_imports: pathfinder_common::types::IncludeImports::None,
            include_tests: true,
        };

        let result = server.get_repo_map(Parameters(params)).await;
        assert!(result.is_ok());
        let call_res = result.unwrap();
        let skeleton = match &call_res.content[0].raw {
            rmcp::model::RawContent::Text(t) => t.text.clone(),
            _ => panic!("expected text content"),
        };
        let response: crate::server::types::GetRepoMapMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();
        assert!(
            skeleton.starts_with("class Mock {}"),
            "skeleton: {skeleton}"
        );
        assert_eq!(response.files_scanned, 1);
        assert_eq!(response.coverage_percent, 100);
        // Visibility filtering is now implemented via name-convention heuristics.
        assert_eq!(response.visibility_degraded, None);
    }

    #[tokio::test]
    async fn test_get_repo_map_visibility_not_degraded() {
        // Both visibility modes should return visibility_degraded: None
        // because visibility filtering is now implemented via name-convention heuristics.
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let mock_surgeon = MockSurgeon::new();
        mock_surgeon
            .generate_skeleton_results
            .lock()
            .unwrap()
            .push(Ok(pathfinder_treesitter::repo_map::RepoMapResult {
                skeleton: String::default(),
                tech_stack: vec![],
                files_scanned: 0,
                files_truncated: 0,
                truncated_paths: vec![],
                files_in_scope: 0,
                coverage_percent: 100,
                version_hashes: std::collections::HashMap::default(),
            }));

        let server = PathfinderServer::with_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            Arc::new(mock_surgeon),
        );

        let params = GetRepoMapParams {
            visibility: pathfinder_common::types::Visibility::All,
            ..Default::default()
        };
        let result = server
            .get_repo_map(Parameters(params))
            .await
            .expect("should succeed");
        let meta: crate::server::types::GetRepoMapMetadata =
            serde_json::from_value(result.structured_content.unwrap()).unwrap();
        assert_eq!(
            meta.visibility_degraded, None,
            "visibility filtering is implemented; visibility_degraded must be None"
        );
    }

    #[tokio::test]
    async fn test_get_repo_map_access_denied() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let mock_surgeon = MockSurgeon::new();
        let server = PathfinderServer::with_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            Arc::new(mock_surgeon),
        );

        let params = GetRepoMapParams {
            path: ".env".to_string(), // Sandbox should deny this
            ..Default::default()
        };

        let Err(err) = server.get_repo_map(Parameters(params)).await else {
            panic!("Expected ACCESS_DENIED error");
        };
        assert_eq!(err.code, ErrorCode(-32001));
    }

    #[tokio::test]
    async fn test_search_codebase_routes_to_scout_and_handles_success() {
        let ws = WorkspaceRoot::new(std::env::temp_dir()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let mock_scout = MockScout::default();
        mock_scout.set_result(Ok(SearchResult {
            matches: vec![SearchMatch {
                file: "src/main.rs".to_owned(),
                line: 10,
                column: 5,
                content: "test_query()".to_owned(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:123".to_owned(),
                known: None,
            }],
            total_matches: 1,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }));

        let mock_surgeon = Arc::new(MockSurgeon::new());
        mock_surgeon
            .enclosing_symbol_results
            .lock()
            .unwrap()
            .push(Ok(Some("test_query_func".to_owned())));
        mock_surgeon
            .enclosing_symbol_detail_results
            .lock()
            .unwrap()
            .push(Ok(Some(ExtractedSymbol {
                name: "test_query_func".to_owned(),
                semantic_path: "test_query_func".to_owned(),
                kind: SymbolKind::Function,
                byte_range: 0..1,
                start_line: 0,
                end_line: 0,
                name_column: 0,
                access_level: AccessLevel::Public,
                children: vec![],
            })));

        let server = PathfinderServer::with_engines(
            ws,
            config,
            sandbox,
            Arc::new(mock_scout.clone()),
            mock_surgeon.clone(),
        );
        let params = SearchCodebaseParams {
            query: "test_query".to_owned(),
            is_regex: true,
            ..Default::default()
        };

        let result = server.search_codebase(Parameters(params)).await;
        // Json(val) gives us val.0
        let val = result.expect("search_codebase should succeed").0;

        assert_eq!(val.total_matches, 1);
        assert!(!val.truncated);
        let matches = val.matches;
        assert_eq!(matches[0].file, "src/main.rs");
        assert_eq!(matches[0].content, "test_query()");
        assert_eq!(
            matches[0].enclosing_semantic_path.as_deref(),
            Some("src/main.rs::test_query_func")
        );

        let calls = mock_scout.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].query, "test_query");
        assert!(calls[0].is_regex);

        let surgeon_calls = mock_surgeon.enclosing_symbol_detail_calls.lock().unwrap();
        assert_eq!(surgeon_calls.len(), 1);
        assert_eq!(surgeon_calls[0].1, std::path::PathBuf::from("src/main.rs"));
        assert_eq!(surgeon_calls[0].2, 10);
    }

    #[tokio::test]
    async fn test_search_codebase_handles_scout_error() {
        let ws = WorkspaceRoot::new(std::env::temp_dir()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let mock_scout = MockScout::default();
        mock_scout.set_result(Err("simulated engine error".to_owned()));

        let server = PathfinderServer::with_engines(
            ws,
            config,
            sandbox,
            Arc::new(mock_scout),
            Arc::new(MockSurgeon::new()),
        );
        let params = SearchCodebaseParams {
            query: "test".to_owned(),
            ..Default::default()
        };

        let result = server.search_codebase(Parameters(params)).await;

        let err = result
            .err()
            .expect("search_codebase should return error on scout failure");
        assert_eq!(err.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(err.message, "search engine error: simulated engine error");
    }

    // ── filter_mode unit tests ────────────────────────────────────────

    fn make_search_match(file: &str, line: u64, content: &str) -> SearchMatch {
        SearchMatch {
            file: file.to_owned(),
            line,
            column: 0,
            content: content.to_owned(),
            context_before: vec![],
            context_after: vec![],
            enclosing_semantic_path: None,
            is_definition: None,
            version_hash: "sha256:abc".to_owned(),
            known: None,
        }
    }

    #[tokio::test]
    async fn test_search_codebase_filter_mode_code_only_drops_comments() {
        let ws = WorkspaceRoot::new(std::env::temp_dir()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let mock_scout = MockScout::default();
        mock_scout.set_result(Ok(SearchResult {
            matches: vec![
                make_search_match("src/a.go", 1, "code line"),
                make_search_match("src/a.go", 2, "// comment line"),
                make_search_match("src/a.go", 3, "another code line"),
            ],
            total_matches: 3,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }));

        let mock_surgeon = Arc::new(MockSurgeon::new());
        // 3 matches → 3 calls: code, comment, code
        // enclosing_symbol called 3 times → return None each (default "code" below)
        // enclosing_symbol_detail called 3 times → return None each
        // node_type_at_position called 3 times → pre-configure results
        mock_surgeon
            .enclosing_symbol_results
            .lock()
            .unwrap()
            .extend([Ok(None), Ok(None), Ok(None)]);
        mock_surgeon
            .enclosing_symbol_detail_results
            .lock()
            .unwrap()
            .extend([Ok(None), Ok(None), Ok(None)]);
        mock_surgeon
            .node_type_at_position_results
            .lock()
            .unwrap()
            .extend([
                Ok("code".to_owned()),
                Ok("comment".to_owned()),
                Ok("code".to_owned()),
            ]);

        let server =
            PathfinderServer::with_engines(ws, config, sandbox, Arc::new(mock_scout), mock_surgeon);

        let params = SearchCodebaseParams {
            query: "line".to_owned(),
            filter_mode: FilterMode::CodeOnly,
            ..Default::default()
        };

        let result = server
            .search_codebase(Parameters(params))
            .await
            .expect("should succeed")
            .0;

        // Only the 2 code matches should survive
        assert_eq!(result.matches.len(), 2, "code_only should drop comments");
        assert_eq!(result.matches[0].content, "code line");
        assert_eq!(result.matches[1].content, "another code line");
        // raw_match_count reflects the ORIGINAL ripgrep count (before filtering)
        assert_eq!(result.raw_match_count, 3);
        // total_matches reflects the FILTERED count (after filtering)
        assert_eq!(result.total_matches, 2);
        // No degraded flag — filtering was real
        assert!(!result.degraded);
    }

    #[tokio::test]
    async fn test_search_codebase_filter_mode_comments_only_keeps_comments() {
        let ws = WorkspaceRoot::new(std::env::temp_dir()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let mock_scout = MockScout::default();
        mock_scout.set_result(Ok(SearchResult {
            matches: vec![
                make_search_match("src/b.go", 1, "func HelloWorld() {}"),
                make_search_match("src/b.go", 2, "// HelloWorld says hello"),
                make_search_match("src/b.go", 3, r#"msg := "Hello World""#),
            ],
            total_matches: 3,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }));

        let mock_surgeon = Arc::new(MockSurgeon::new());
        mock_surgeon
            .enclosing_symbol_results
            .lock()
            .unwrap()
            .extend([Ok(None), Ok(None), Ok(None)]);
        mock_surgeon
            .enclosing_symbol_detail_results
            .lock()
            .unwrap()
            .extend([Ok(None), Ok(None), Ok(None)]);
        mock_surgeon
            .node_type_at_position_results
            .lock()
            .unwrap()
            .extend([
                Ok("code".to_owned()),
                Ok("comment".to_owned()),
                Ok("string".to_owned()),
            ]);

        let server =
            PathfinderServer::with_engines(ws, config, sandbox, Arc::new(mock_scout), mock_surgeon);

        let params = SearchCodebaseParams {
            query: "Hello".to_owned(),
            filter_mode: FilterMode::CommentsOnly,
            ..Default::default()
        };

        let result = server
            .search_codebase(Parameters(params))
            .await
            .expect("should succeed")
            .0;

        // Comment and string matches should survive; code match should be dropped
        assert_eq!(result.matches.len(), 2, "comments_only should drop code");
        assert_eq!(result.matches[0].content, "// HelloWorld says hello");
        assert_eq!(result.matches[1].content, r#"msg := "Hello World""#);
        assert!(!result.degraded);
    }

    #[tokio::test]
    async fn test_search_codebase_filter_mode_all_returns_everything() {
        let ws = WorkspaceRoot::new(std::env::temp_dir()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let mock_scout = MockScout::default();
        mock_scout.set_result(Ok(SearchResult {
            matches: vec![
                make_search_match("src/c.go", 1, "code"),
                make_search_match("src/c.go", 2, "// comment"),
                make_search_match("src/c.go", 3, r#"\"string\""#),
            ],
            total_matches: 3,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }));

        let mock_surgeon = Arc::new(MockSurgeon::default());
        // enclosing_symbol: all return None
        mock_surgeon
            .enclosing_symbol_results
            .lock()
            .unwrap()
            .extend([Ok(None), Ok(None), Ok(None)]);
        // enclosing_symbol_detail: all return None
        mock_surgeon
            .enclosing_symbol_detail_results
            .lock()
            .unwrap()
            .extend([Ok(None), Ok(None), Ok(None)]);
        // node_type_at_position: will use default "code" since queue is empty
        // (FilterMode::All skips classification entirely — but mock still gets called;
        // the default return value is "code" so no pre-configuration needed)

        let server =
            PathfinderServer::with_engines(ws, config, sandbox, Arc::new(mock_scout), mock_surgeon);

        let params = SearchCodebaseParams {
            query: "test".to_owned(),
            filter_mode: FilterMode::All,
            ..Default::default()
        };

        let result = server
            .search_codebase(Parameters(params))
            .await
            .expect("should succeed")
            .0;

        // All 3 matches returned, no filtering
        assert_eq!(result.matches.len(), 3);
        assert!(!result.degraded);
    }

    // ── read_file tests ──────────────────────────────────────

    #[tokio::test]
    async fn test_read_file_pagination() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);
        let server = PathfinderServer::with_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            Arc::new(MockSurgeon::new()),
        );

        // Write a 10-line file
        let filepath = "config.yaml";
        let lines: Vec<String> = (1..=10).map(|i| format!("line{i}: value")).collect();
        let content = lines.join("\n");
        fs::write(ws_dir.path().join(filepath), &content).expect("write");

        // Full read
        let result = server
            .read_file(Parameters(ReadFileParams {
                filepath: filepath.to_owned(),
                start_line: 1,
                max_lines: 500,
            }))
            .await
            .expect("should succeed");
        let val: crate::server::types::ReadFileMetadata =
            serde_json::from_value(result.structured_content.unwrap()).unwrap();
        assert_eq!(val.total_lines, 10);
        assert_eq!(val.lines_returned, 10);
        assert!(!val.truncated);
        assert_eq!(val.language, "yaml");

        // Paginated read — lines 3-5
        let result2 = server
            .read_file(Parameters(ReadFileParams {
                filepath: filepath.to_owned(),
                start_line: 3,
                max_lines: 3,
            }))
            .await
            .expect("should succeed");
        let val2: crate::server::types::ReadFileMetadata =
            serde_json::from_value(result2.structured_content.unwrap()).unwrap();
        assert_eq!(val2.start_line, 3);
        assert_eq!(val2.lines_returned, 3);
        assert!(val2.truncated);
        let text_content = match &result2.content[0].raw {
            rmcp::model::RawContent::Text(t) => t.text.clone(),
            _ => panic!("expected text content"),
        };
        assert!(text_content.contains("line3"));
        assert!(text_content.contains("line5"));
        assert!(!text_content.contains("line6"));

        // FILE_NOT_FOUND
        let result3 = server
            .read_file(Parameters(ReadFileParams {
                filepath: "nonexistent.yaml".to_owned(),
                start_line: 1,
                max_lines: 500,
            }))
            .await;
        assert!(result3.is_err());
        let Err(err) = result3 else {
            panic!("expected error")
        };
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "FILE_NOT_FOUND", "got: {err:?}");
    }

    // ── read_symbol_scope tests ─────────────────────────────────────

    #[tokio::test]
    async fn test_read_symbol_scope_routes_to_surgeon_and_handles_success() {
        let ws_dir = tempdir().expect("temp dir");
        // Create test file so file existence check passes
        let src_dir = ws_dir.path().join("src");
        std::fs::create_dir_all(&src_dir).expect("create src dir");
        std::fs::write(src_dir.join("auth.go"), "func Login() {}").expect("create auth.go");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);
        let mock_surgeon = Arc::new(MockSurgeon::new());

        let content = "func Login() {}";
        let expected_scope = pathfinder_common::types::SymbolScope {
            content: content.to_owned(),
            start_line: 5,
            end_line: 7,
            name_column: 0,
            language: "go".to_owned(),
        };
        mock_surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(expected_scope.clone()));

        let server = PathfinderServer::with_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            mock_surgeon.clone(),
        );

        let params = ReadSymbolScopeParams {
            semantic_path: "src/auth.go::Login".to_owned(),
        };

        let result = server.read_symbol_scope(Parameters(params)).await;
        let val = result.expect("should succeed");

        let rmcp::model::RawContent::Text(t) = &val.content[0].raw else {
            panic!("Expected text content");
        };
        assert!(
            t.text.starts_with(&expected_scope.content),
            "text: {}",
            t.text
        );

        let metadata: crate::server::types::ReadSymbolScopeMetadata =
            serde_json::from_value(val.structured_content.expect("missing structured_content"))
                .expect("valid metadata");

        assert_eq!(metadata.start_line, expected_scope.start_line);
        assert_eq!(metadata.end_line, expected_scope.end_line);
        assert_eq!(metadata.language, expected_scope.language);

        let calls = mock_surgeon.read_symbol_scope_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
    }

    #[tokio::test]
    async fn test_read_symbol_scope_handles_surgeon_error() {
        let ws_dir = tempdir().expect("temp dir");
        // Create test file so file existence check passes
        let src_dir = ws_dir.path().join("src");
        std::fs::create_dir_all(&src_dir).expect("create src dir");
        std::fs::write(src_dir.join("auth.go"), "func Login() {}").expect("create auth.go");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);
        let mock_surgeon = Arc::new(MockSurgeon::new());

        mock_surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Err(pathfinder_treesitter::SurgeonError::SymbolNotFound {
                path: "src/auth.go::Login".to_owned(),
                did_you_mean: vec!["Logout".to_owned()],
            }));

        let server = PathfinderServer::with_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            mock_surgeon,
        );

        let params = ReadSymbolScopeParams {
            semantic_path: "src/auth.go::Login".to_owned(),
        };

        let Err(err) = server.read_symbol_scope(Parameters(params)).await else {
            panic!("Expected failed response");
        };

        assert_eq!(err.code, ErrorCode::INVALID_PARAMS); // SymbolNotFound maps to INVALID_PARAMS
        let code = err
            .data
            .as_ref()
            .unwrap()
            .get("error")
            .unwrap()
            .as_str()
            .unwrap();
        assert_eq!(code, "SYMBOL_NOT_FOUND");
    }

    // \u2500\u2500 E4 tests \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500

    // ── E4 tests ─────────────────────────────────────────────────────

    /// E4.1: Matches in `known_files` must have content + context stripped,
    /// while matches in other files must retain full content.
    #[tokio::test]
    async fn test_search_codebase_known_files_suppresses_context() {
        let ws = WorkspaceRoot::new(std::env::temp_dir()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let mock_scout = MockScout::default();
        mock_scout.set_result(Ok(SearchResult {
            matches: vec![
                SearchMatch {
                    file: "src/auth.ts".to_owned(),
                    line: 10,
                    column: 1,
                    content: "secret content".to_owned(),
                    context_before: vec!["before".to_owned()],
                    context_after: vec!["after".to_owned()],
                    enclosing_semantic_path: None,
                    is_definition: None,
                    version_hash: "sha256:abc".to_owned(),
                    known: None,
                },
                SearchMatch {
                    file: "src/main.ts".to_owned(),
                    line: 5,
                    column: 1,
                    content: "visible content".to_owned(),
                    context_before: vec!["ctx_before".to_owned()],
                    context_after: vec!["ctx_after".to_owned()],
                    enclosing_semantic_path: None,
                    is_definition: None,
                    version_hash: "sha256:xyz".to_owned(),
                    known: None,
                },
            ],
            total_matches: 2,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }));

        let mock_surgeon = Arc::new(MockSurgeon::new());
        // Two matches → two enrichment calls
        mock_surgeon
            .enclosing_symbol_results
            .lock()
            .unwrap()
            .extend([Ok(None), Ok(None)]);
        mock_surgeon
            .enclosing_symbol_detail_results
            .lock()
            .unwrap()
            .extend([Ok(None), Ok(None)]);

        let server =
            PathfinderServer::with_engines(ws, config, sandbox, Arc::new(mock_scout), mock_surgeon);

        let params = SearchCodebaseParams {
            query: "content".to_owned(),
            known_files: vec!["src/auth.ts".to_owned()],
            ..Default::default()
        };

        let result = server
            .search_codebase(Parameters(params))
            .await
            .expect("should succeed")
            .0;

        assert_eq!(result.matches.len(), 2);

        // Known file match — content + context stripped, known=true
        let known_match = result
            .matches
            .iter()
            .find(|m| m.file == "src/auth.ts")
            .unwrap();
        assert!(
            known_match.content.is_empty(),
            "content should be suppressed for known file"
        );
        assert!(
            known_match.context_before.is_empty(),
            "context_before should be empty"
        );
        assert!(
            known_match.context_after.is_empty(),
            "context_after should be empty"
        );
        assert_eq!(
            known_match.known,
            Some(true),
            "known flag must be set for known-file matches"
        );

        // Unknown file match — content retained, no known flag
        let normal_match = result
            .matches
            .iter()
            .find(|m| m.file == "src/main.ts")
            .unwrap();
        assert_eq!(normal_match.content, "visible content");
        assert_eq!(normal_match.context_before, vec!["ctx_before"]);
        assert_eq!(normal_match.context_after, vec!["ctx_after"]);
        assert_eq!(
            normal_match.known, None,
            "unknown-file matches must not have known flag"
        );
    }

    /// E4.1: `known_files` path normalisation — `./src/auth.ts` must match `src/auth.ts`.
    #[tokio::test]
    async fn test_search_codebase_known_files_path_normalisation() {
        let ws = WorkspaceRoot::new(std::env::temp_dir()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let mock_scout = MockScout::default();
        mock_scout.set_result(Ok(SearchResult {
            matches: vec![SearchMatch {
                file: "src/auth.ts".to_owned(),
                line: 1,
                column: 1,
                content: "should be stripped".to_owned(),
                context_before: vec!["before".to_owned()],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:abc".to_owned(),
                known: None,
            }],
            total_matches: 1,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }));

        let mock_surgeon = Arc::new(MockSurgeon::new());
        mock_surgeon
            .enclosing_symbol_results
            .lock()
            .unwrap()
            .push(Ok(None));
        mock_surgeon
            .enclosing_symbol_detail_results
            .lock()
            .unwrap()
            .push(Ok(None));

        let server =
            PathfinderServer::with_engines(ws, config, sandbox, Arc::new(mock_scout), mock_surgeon);

        // Pass with leading "./" — should still match "src/auth.ts"
        let params = SearchCodebaseParams {
            query: "stripped".to_owned(),
            known_files: vec!["./src/auth.ts".to_owned()],
            ..Default::default()
        };

        let result = server
            .search_codebase(Parameters(params))
            .await
            .expect("should succeed")
            .0;

        let m = &result.matches[0];
        assert!(
            m.content.is_empty(),
            "content should be suppressed despite ./ prefix"
        );
        assert!(m.context_before.is_empty());
        assert_eq!(m.known, Some(true), "known flag must be set");
    }

    /// E4.2: `group_by_file=true` groups matches by file with shared `version_hash`;
    /// known files go into `known_matches` with minimal info.
    #[tokio::test]
    async fn test_search_codebase_group_by_file() {
        let ws = WorkspaceRoot::new(std::env::temp_dir()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let mock_scout = MockScout::default();
        mock_scout.set_result(Ok(SearchResult {
            matches: vec![
                // Two matches in the same known file
                SearchMatch {
                    file: "src/auth.ts".to_owned(),
                    line: 1,
                    column: 1,
                    content: "known line 1".to_owned(),
                    context_before: vec![],
                    context_after: vec![],
                    enclosing_semantic_path: None,
                    is_definition: None,
                    version_hash: "sha256:auth".to_owned(),
                    known: None,
                },
                SearchMatch {
                    file: "src/auth.ts".to_owned(),
                    line: 2,
                    column: 1,
                    content: "known line 2".to_owned(),
                    context_before: vec![],
                    context_after: vec![],
                    enclosing_semantic_path: None,
                    is_definition: None,
                    version_hash: "sha256:auth".to_owned(),
                    known: None,
                },
                // One match in a normal file
                SearchMatch {
                    file: "src/main.ts".to_owned(),
                    line: 5,
                    column: 1,
                    content: "main content".to_owned(),
                    context_before: vec!["prev".to_owned()],
                    context_after: vec![],
                    enclosing_semantic_path: None,
                    is_definition: None,
                    version_hash: "sha256:main".to_owned(),
                    known: None,
                },
            ],
            total_matches: 3,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }));

        let mock_surgeon = Arc::new(MockSurgeon::new());
        // 3 enrichments
        mock_surgeon
            .enclosing_symbol_results
            .lock()
            .unwrap()
            .extend([Ok(None), Ok(None), Ok(None)]);
        mock_surgeon
            .enclosing_symbol_detail_results
            .lock()
            .unwrap()
            .extend([Ok(None), Ok(None), Ok(None)]);

        let server =
            PathfinderServer::with_engines(ws, config, sandbox, Arc::new(mock_scout), mock_surgeon);

        let params = SearchCodebaseParams {
            query: "line".to_owned(),
            known_files: vec!["src/auth.ts".to_owned()],
            group_by_file: true,
            ..Default::default()
        };

        let result = server
            .search_codebase(Parameters(params))
            .await
            .expect("should succeed")
            .0;

        let groups = result
            .file_groups
            .expect("file_groups should be Some when group_by_file=true");
        assert_eq!(groups.len(), 2);

        let auth_group = groups.iter().find(|g| g.file == "src/auth.ts").unwrap();
        assert_eq!(auth_group.version_hash, "sha256:auth");
        assert!(
            auth_group.matches.is_empty(),
            "known file should have no full matches"
        );
        assert_eq!(
            auth_group.known_matches.len(),
            2,
            "known file should have 2 known_matches"
        );
        assert!(auth_group.known_matches[0].known);

        let main_group = groups.iter().find(|g| g.file == "src/main.ts").unwrap();
        assert_eq!(main_group.version_hash, "sha256:main");
        assert_eq!(main_group.matches.len(), 1);
        // GroupedMatch has no file/version_hash — those are at group level only
        assert_eq!(main_group.matches[0].content, "main content");
        assert_eq!(main_group.matches[0].line, 5);
        assert!(main_group.known_matches.is_empty());
    }

    /// E4.3: `exclude_glob` is forwarded to the scout as part of `SearchParams`.
    #[tokio::test]
    async fn test_search_codebase_exclude_glob_forwarded_to_scout() {
        let ws = WorkspaceRoot::new(std::env::temp_dir()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let mock_scout = MockScout::default();
        mock_scout.set_result(Ok(SearchResult {
            matches: vec![],
            total_matches: 0,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }));

        let server = PathfinderServer::with_engines(
            ws,
            config,
            sandbox,
            Arc::new(mock_scout.clone()),
            Arc::new(MockSurgeon::new()),
        );

        let params = SearchCodebaseParams {
            query: "anything".to_owned(),
            exclude_glob: "**/*.test.*".to_owned(),
            ..Default::default()
        };

        server
            .search_codebase(Parameters(params))
            .await
            .expect("should succeed");

        let calls = mock_scout.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].exclude_glob, "**/*.test.*",
            "exclude_glob must be forwarded to the scout"
        );
    }

    // ── Server constructor tests (WP-5) ─────────────────────────────────

    #[tokio::test]
    async fn test_with_all_engines_constructs_functional_server() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            Arc::new(MockSurgeon::new()),
            Arc::new(pathfinder_lsp::MockLawyer::default()),
        );

        // Verify server functions — get_info should work
        let info = server.get_info();
        assert_eq!(info.server_info.name, "pathfinder");
    }

    #[tokio::test]
    async fn test_with_engines_uses_no_op_lawyer() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        // Create a Rust file for surgeon to read
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(ws_dir.path().join("src/lib.rs"), "fn hello() -> i32 { 1 }").unwrap();

        let mock_surgeon = Arc::new(MockSurgeon::new());
        mock_surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(pathfinder_common::types::SymbolScope {
                content: "fn hello() -> i32 { 1 }".to_owned(),
                start_line: 0,
                end_line: 0,
                name_column: 0,
                language: "rust".to_owned(),
            }));

        let server = PathfinderServer::with_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            mock_surgeon,
        );

        // Navigation with NoOpLawyer should degrade gracefully
        let params = crate::server::types::GetDefinitionParams {
            semantic_path: "src/lib.rs::hello".to_owned(),
        };
        let result = server.get_definition_impl(params).await;
        // Should fail because NoOpLawyer returns NoLspAvailable and no grep fallback match
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_read_file_not_found() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            Arc::new(MockSurgeon::new()),
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params = crate::server::types::ReadFileParams {
            filepath: "missing.txt".to_owned(),
            start_line: 1,
            max_lines: 100,
        };
        let result = server.read_file_impl(params).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "FILE_NOT_FOUND");
    }
}
