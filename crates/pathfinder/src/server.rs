//! Pathfinder MCP Server — tool registration and dispatch.
//!
//! Implements `rmcp::ServerHandler` with all 18 Pathfinder tools.
//!
//! # Module Layout
//! - [`helpers`] — error conversion, stub builder, language detection
//! - [`types`] — all parameter and response structs
//! - [`tools`] — handler logic, one submodule per tool group:
//!   - [`tools::search`] — `search_codebase`
//!   - [`tools::repo_map`] — `get_repo_map`
//!   - [`tools::symbols`] — `read_symbol_scope`, `read_with_deep_context`
//!   - [`tools::navigation`] — `get_definition`, `analyze_impact`
//!   - [`tools::file_ops`] — `create_file`, `delete_file`, `read_file`, `write_file`

mod helpers;
mod tools;
/// Module containing type definitions.
pub mod types;

use types::{
    AnalyzeImpactParams, CreateFileParams, CreateFileResponse, DeleteFileParams,
    DeleteFileResponse, DeleteSymbolParams, EditResponse, GetDefinitionParams,
    GetDefinitionResponse, GetRepoMapParams, InsertAfterParams, InsertBeforeParams, ReadFileParams,
    ReadSourceFileParams, ReadSymbolScopeParams, ReadWithDeepContextParams, ReplaceBodyParams,
    ReplaceFullParams, SearchCodebaseParams, SearchCodebaseResponse, ValidateOnlyParams,
    WriteFileParams,
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
    #[allow(dead_code)]
    config: Arc<PathfinderConfig>,
    #[allow(dead_code)]
    sandbox: Arc<Sandbox>,
    scout: Arc<dyn Scout>,
    surgeon: Arc<dyn Surgeon>,
    lawyer: Arc<dyn Lawyer>,
    tool_router: ToolRouter<Self>,
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
    pub fn with_all_engines(
        workspace_root: WorkspaceRoot,
        config: PathfinderConfig,
        sandbox: Sandbox,
        scout: Arc<dyn Scout>,
        surgeon: Arc<dyn Surgeon>,
        lawyer: Arc<dyn Lawyer>,
    ) -> Self {
        Self {
            workspace_root: Arc::new(workspace_root),
            config: Arc::new(config),
            sandbox: Arc::new(sandbox),
            scout,
            surgeon,
            lawyer,
            tool_router: Self::tool_router(),
        }
    }
}

// ── Tool Router (defines all 18 tools) ──────────────────────────────

#[tool_router]
impl PathfinderServer {
    #[tool(
        name = "search_codebase",
        description = "Search the codebase for a text pattern. Returns matching lines with surrounding context. Each match includes an 'enclosing_semantic_path' (the AST symbol containing the match) and 'version_hash' (for immediate editing without a separate read). The version_hash in each match is immediately usable as base_version for edit tools — no additional read required. Use path_glob to narrow the search scope.\n\n**E4 parameters (token efficiency):**\n- `exclude_glob` — Glob pattern for files to exclude before search (e.g. `**/*.test.*`). Applied at the file-walk level so excluded files are never read.\n- `known_files` — List of file paths already in agent context. Matches in these files are returned with minimal metadata only (`file`, `line`, `column`, `enclosing_semantic_path`, `version_hash`) — `content` and context lines are omitted.\n- `group_by_file` — When `true`, results are returned in `file_groups` (one group per file with a single shared `version_hash`). Known-file matches appear in `known_matches`; others in `matches` inside each group."
    )]
    async fn search_codebase(
        &self,
        Parameters(params): Parameters<SearchCodebaseParams>,
    ) -> Result<Json<SearchCodebaseResponse>, ErrorData> {
        self.search_codebase_impl(params).await
    }

    #[tool(
        name = "get_repo_map",
        description = "Returns the structural skeleton of the project as an indented tree of classes, functions, and type signatures. IMPORTANT: Each symbol has its full semantic path in a trailing comment. You MUST copy-paste these EXACT paths into read/edit tools. Also returns version_hashes per file for immediate editing. The version_hashes are immediately usable as base_version for edit tools — no additional read required. Two budget knobs control coverage: `max_tokens` is the total token budget (default 16000); `max_tokens_per_file` caps detail per file before collapsing to a stub (default 2000). When `coverage_percent` is low, increase `max_tokens`. When files show `[TRUNCATED DUE TO SIZE]`, increase `max_tokens_per_file`. Use `visibility=all` to include private symbols for auditing. Module scopes (e.g., Rust `mod tests`, `mod types`) are only shown when `visibility` is set to `\"all\"`. They are hidden in public-only maps. The `depth` parameter (default 5) controls directory traversal depth; increase it for deeply-nested repos when `coverage_percent` is low.\n\n**Temporal & extension filters (Epic E6):**\n- `changed_since` — Git ref or duration to show only recently-modified files (e.g., `HEAD~5`, `3h`, `2024-01-01`). Useful for reviewing what changed in a PR or recent session. When git is unavailable the parameter is silently ignored and `degraded: true` is set in the response.\n- `include_extensions` — Only include files with these extensions (e.g., `[\"ts\", \"tsx\"]`). Mutually exclusive with `exclude_extensions`.\n- `exclude_extensions` — Exclude files with these extensions (e.g., `[\"md\", \"json\"]`). Mutually exclusive with `include_extensions`."
    )]
    #[allow(clippy::unused_self)]
    async fn get_repo_map(
        &self,
        Parameters(params): Parameters<GetRepoMapParams>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::model::ErrorData> {
        self.get_repo_map_impl(params).await
    }

    #[tool(
        name = "read_symbol_scope",
        description = "Extract the exact source code of a single symbol (function, class, method) by its semantic path. IMPORTANT: semantic_path must ALWAYS include the file path and '::', e.g., 'src/client/process.rs::send'. Returns the code, line range, and version_hash for OCC. The version_hash is immediately usable as base_version for any edit tool — no additional read required."
    )]
    async fn read_symbol_scope(
        &self,
        Parameters(params): Parameters<ReadSymbolScopeParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.read_symbol_scope_impl(params).await
    }

    #[tool(
        name = "read_source_file",
        description = "**AST-only.** Only call this on source code files (.rs, .ts, .tsx, .go, .py, .vue, .jsx, .js). For configuration or documentation files (YAML, TOML, JSON, Markdown, Dockerfile, .env, XML), use `read_file` instead — calling this tool on those file types returns UNSUPPORTED_LANGUAGE.\n\nRead an entire source file and extract its complete AST symbol hierarchy. Returns the full file context, the language detected, OCC hashes, and a nested tree of symbols with their semantic paths. Use this instead of read_symbol_scope when you need broader context beyond a single symbol. The version_hash is immediately usable as base_version for any edit tool — no additional read required.\n\n**detail_level parameter:** `compact` (default) — full source + flat symbol list; `symbols` — symbol tree only, no source; `full` — full source + complete nested AST (v4 behaviour). Use `start_line`/`end_line` to restrict output to a region of interest."
    )]
    async fn read_source_file(
        &self,
        Parameters(params): Parameters<ReadSourceFileParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.read_source_file_impl(params).await
    }

    #[tool(
        name = "replace_batch",
        description = "Apply multiple AST-aware edits sequentially within a single source file using a single atomic write. Accepts a list of edits, applies them from the end of the file backwards to prevent offset shifting, and uses a single OCC base_version guard. Use this for refactors touching multiple non-contiguous symbols in one file. IMPORTANT: For each edit, semantic_path must ALWAYS include the file path and '::' (e.g. 'src/mod.rs::func').\n\n**Two targeting modes per edit (E3.1 — Hybrid Batch):**\n\n**Option A — Semantic targeting (existing):** Set `semantic_path`, `edit_type`, and optionally `new_code`. Use for source-code constructs that have a parseable AST symbol.\n\n**Option B — Text targeting (new):** Set `old_text`, `context_line`, and optionally `replacement_text`. Use for Vue `<template>`/`<style>` zones or any region with no usable semantic path. The search scans ±25 lines around `context_line` (1-indexed) for an exact match of `old_text`. Set `normalize_whitespace: true` to collapse `\\s+` → single space before matching (useful for HTML where indentation may vary; do NOT use for Python or YAML).\n\nBoth targeting modes can appear in the same batch — the batch is fully atomic (all-or-nothing). If any edit fails (e.g., `TEXT_NOT_FOUND`), the entire batch is rolled back.\n\n**Schema quick-reference:**\n  Option A: { \"semantic_path\": \"src/file.rs::MyStruct.my_fn\", \"edit_type\": \"replace_body\", \"new_code\": \"...\" }\n  edit_type values: replace_body | replace_full | insert_before | insert_after | insert_into | delete\n  Option B: { \"old_text\": \"<old html>\", \"context_line\": 42, \"replacement_text\": \"<new html>\" }\n  Both modes may be mixed in one batch. `context_line` is required for text targeting.\n\nbase_version accepts either the full SHA-256 hash (e.g., \"sha256:4ec5a8a...\") or a short 7-character prefix (e.g., \"sha256:4ec5a8a\"), matching Git convention."
    )]
    async fn replace_batch(
        &self,
        Parameters(params): Parameters<crate::server::types::ReplaceBatchParams>,
    ) -> Result<Json<EditResponse>, ErrorData> {
        self.replace_batch_impl(params).await
    }

    #[tool(
        name = "read_with_deep_context",
        description = "Extract a symbol's source code PLUS the signatures of all functions it calls. Use this when you need to understand a function's dependencies before editing it. IMPORTANT: semantic_path must ALWAYS include the file path and '::' (e.g. 'src/auth.ts::AuthService.login').\n\nReturns a hybrid response: raw source code in `content[0].text` for direct reading, and structured metadata in `structured_content` (JSON) containing `version_hash`, `start_line`, `end_line`, `language`, `dependencies` (callee signatures), `degraded`, and `degraded_reason`.\n\n**Latency note:** The first call may take significantly longer (30–120 seconds) due to LSP server warm-up. Subsequent calls are fast. This is expected behaviour — not a bug.\n\n**Degraded mode:** When no LSP is available, `degraded=true` and `degraded_reason=\"no_lsp\"`. The response still returns source code and Tree-sitter context, but `dependencies` will be empty. Check `degraded` before relying on dependency data."
    )]
    async fn read_with_deep_context(
        &self,
        Parameters(params): Parameters<ReadWithDeepContextParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.read_with_deep_context_impl(params).await
    }

    #[tool(
        name = "get_definition",
        description = "Jump to where a symbol is defined. Provide a semantic path to a reference and get back the definition's file, line, and a code preview. IMPORTANT: semantic_path must ALWAYS include the file path and '::' (e.g. 'src/auth.ts::AuthService.login')."
    )]
    async fn get_definition(
        &self,
        Parameters(params): Parameters<GetDefinitionParams>,
    ) -> Result<Json<GetDefinitionResponse>, ErrorData> {
        self.get_definition_impl(params).await
    }

    #[tool(
        name = "analyze_impact",
        description = "Find all callers of a symbol (incoming) and all symbols it calls (outgoing). Use this BEFORE refactoring to understand the blast radius of a change. IMPORTANT: semantic_path must ALWAYS include the file path and '::' (e.g. 'src/mod.rs::func'). Returns version_hashes for all referenced files. The version_hashes are immediately usable as base_version for edit tools — no additional read required."
    )]
    async fn analyze_impact(
        &self,
        Parameters(params): Parameters<AnalyzeImpactParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.analyze_impact_impl(params).await
    }

    #[tool(
        name = "replace_body",
        description = "Replace the internal logic of a block-scoped construct (function, method, class body, impl block), keeping the signature intact. Provide ONLY the body content — DO NOT include the outer braces or function signature. DO NOT wrap your code in markdown code blocks. IMPORTANT: semantic_path must ALWAYS include the file path and '::' (e.g. 'src/mod.rs::func').\n\n**LSP validation:** Edit responses include a `validation` field. If `validation_skipped` is true, check `validation_skipped_reason` for why (e.g., `no_lsp`, `lsp_crash`). To see LSP status before editing, call `get_repo_map` and inspect `capabilities.lsp.per_language`.\n\nbase_version accepts either the full SHA-256 hash (e.g., \"sha256:4ec5a8a...\") or a short 7-character prefix (e.g., \"sha256:4ec5a8a\"), matching Git convention."
    )]
    async fn replace_body(
        &self,
        Parameters(params): Parameters<ReplaceBodyParams>,
    ) -> Result<Json<EditResponse>, ErrorData> {
        self.replace_body_impl(params).await
    }

    #[tool(
        name = "replace_full",
        description = "Replace an entire declaration including its signature, body, decorators, and doc comments. Provide the COMPLETE replacement — anything you omit (decorators, doc comments) will be removed. DO NOT wrap your code in markdown code blocks. IMPORTANT: semantic_path must ALWAYS include the file path and '::' (e.g. 'src/mod.rs::func').\n\n**LSP validation:** Edit responses include a `validation` field. If `validation_skipped` is true, check `validation_skipped_reason` for why (e.g., `no_lsp`, `lsp_crash`). To see LSP status before editing, call `get_repo_map` and inspect `capabilities.lsp.per_language`.\n\nbase_version accepts either the full SHA-256 hash (e.g., \"sha256:4ec5a8a...\") or a short 7-character prefix (e.g., \"sha256:4ec5a8a\"), matching Git convention."
    )]
    async fn replace_full(
        &self,
        Parameters(params): Parameters<ReplaceFullParams>,
    ) -> Result<Json<EditResponse>, ErrorData> {
        self.replace_full_impl(params).await
    }

    #[tool(
        name = "insert_before",
        description = "Insert new code BEFORE a target symbol. IMPORTANT: To target a symbol, semantic_path must include the file path and '::' (e.g. 'src/mod.rs::func'). To insert at the TOP of a file (e.g., adding imports), use a bare file path without '::' (e.g. 'src/mod.rs'). Pathfinder automatically adds one blank line between your code and the target.\n\n**LSP validation:** Edit responses include a `validation` field. If `validation_skipped` is true, check `validation_skipped_reason` for why. Call `get_repo_map` and inspect `capabilities.lsp.per_language` to see LSP status upfront.\n\nbase_version accepts either the full SHA-256 hash (e.g., \"sha256:4ec5a8a...\") or a short 7-character prefix (e.g., \"sha256:4ec5a8a\"), matching Git convention."
    )]
    async fn insert_before(
        &self,
        Parameters(params): Parameters<InsertBeforeParams>,
    ) -> Result<Json<EditResponse>, ErrorData> {
        self.insert_before_impl(params).await
    }

    #[tool(
        name = "insert_after",
        description = "Insert new code AFTER a target symbol. IMPORTANT: To target a symbol, semantic_path must include the file path and '::' (e.g. 'src/mod.rs::func'). To append to the BOTTOM of a file (e.g., adding new classes), use a bare file path without '::' (e.g. 'src/mod.rs'). Pathfinder automatically adds one blank line between the target and your code.\n\n**LSP validation:** Edit responses include a `validation` field. If `validation_skipped` is true, check `validation_skipped_reason` for why. Call `get_repo_map` and inspect `capabilities.lsp.per_language` to see LSP status upfront.\n\nbase_version accepts either the full SHA-256 hash (e.g., \"sha256:4ec5a8a...\") or a short 7-character prefix (e.g., \"sha256:4ec5a8a\"), matching Git convention."
    )]
    async fn insert_after(
        &self,
        Parameters(params): Parameters<InsertAfterParams>,
    ) -> Result<Json<EditResponse>, ErrorData> {
        self.insert_after_impl(params).await
    }

    #[tool(
        name = "insert_into",
        description = "Insert new code at the END of a container symbol's body \
            (Module, Class, Struct, Impl, Interface). This is the correct tool \
            for adding new functions to a test module, new methods to a struct, \
            or new items to any scope. IMPORTANT: semantic_path must target a \
            container symbol (e.g. 'src/lib.rs::tests'), NOT a bare file path. \
            For inserting before/after a specific sibling symbol, use insert_before \
            or insert_after instead.\n\nbase_version accepts either the full SHA-256 hash \
            (e.g., \"sha256:4ec5a8a...\") or a short 7-character prefix (e.g., \"sha256:4ec5a8a\"), \
            matching Git convention."
    )]
    async fn insert_into(
        &self,
        Parameters(params): Parameters<crate::server::types::InsertIntoParams>,
    ) -> Result<Json<EditResponse>, ErrorData> {
        self.insert_into_impl(params).await
    }

    #[tool(
        name = "delete_symbol",
        description = "Delete a symbol and all its associated decorators, attributes, and doc comments. If the target is a class, the ENTIRE class is deleted. If the target is a method, only that method is deleted. IMPORTANT: semantic_path must ALWAYS include the file path and '::' (e.g. 'src/auth.ts::AuthService.login').\n\n**LSP validation:** Edit responses include a `validation` field. If `validation_skipped` is true, check `validation_skipped_reason` for why. Call `get_repo_map` and inspect `capabilities.lsp.per_language` to see LSP status upfront.\n\nbase_version accepts either the full SHA-256 hash (e.g., \"sha256:4ec5a8a...\") or a short 7-character prefix (e.g., \"sha256:4ec5a8a\"), matching Git convention."
    )]
    async fn delete_symbol(
        &self,
        Parameters(params): Parameters<DeleteSymbolParams>,
    ) -> Result<Json<EditResponse>, ErrorData> {
        self.delete_symbol_impl(params).await
    }

    #[tool(
        name = "validate_only",
        description = "Dry-run an edit WITHOUT writing to disk. Use this to pre-check risky changes. Returns the same validation results as a real edit. IMPORTANT: semantic_path must ALWAYS include the file path and '::' (e.g. 'src/mod.rs::func'). new_version_hash will be null because nothing was written. Reuse your original base_version for the real edit.\n\n**LSP validation:** If `validation_skipped` is true, check `validation_skipped_reason` for why (e.g., `no_lsp`, `lsp_crash`). Call `get_repo_map` and inspect `capabilities.lsp.per_language` to see LSP status upfront.\n\nbase_version accepts either the full SHA-256 hash (e.g., \"sha256:4ec5a8a...\") or a short 7-character prefix (e.g., \"sha256:4ec5a8a\"), matching Git convention."
    )]
    async fn validate_only(
        &self,
        Parameters(params): Parameters<ValidateOnlyParams>,
    ) -> Result<Json<EditResponse>, ErrorData> {
        self.validate_only_impl(params).await
    }

    #[tool(
        name = "create_file",
        description = "Create a new file with initial content. Parent directories are created automatically. Returns a version_hash for subsequent edits."
    )]
    async fn create_file(
        &self,
        Parameters(params): Parameters<CreateFileParams>,
    ) -> Result<Json<CreateFileResponse>, ErrorData> {
        self.create_file_impl(params).await
    }

    #[tool(
        name = "delete_file",
        description = "Delete a file. Requires base_version (OCC) to prevent deleting a file that was modified after you last read it. base_version accepts either the full SHA-256 hash (e.g., \"sha256:4ec5a8a...\") or a short 7-character prefix (e.g., \"sha256:4ec5a8a\"), matching Git convention."
    )]
    async fn delete_file(
        &self,
        Parameters(params): Parameters<DeleteFileParams>,
    ) -> Result<Json<DeleteFileResponse>, ErrorData> {
        self.delete_file_impl(params).await
    }

    #[tool(
        name = "read_file",
        description = "Read raw file content. Use ONLY for configuration files (.env, Dockerfile, YAML, TOML, package.json). For source code, use read_symbol_scope instead. Supports pagination via start_line for large files."
    )]
    async fn read_file(
        &self,
        Parameters(params): Parameters<ReadFileParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.read_file_impl(params).await
    }

    #[tool(
        name = "write_file",
        description = "WARNING: This bypasses AST validation and formatting. DO NOT use for source code (TypeScript, Python, Go, Rust). ONLY use for configuration files (.env, .gitignore, Dockerfile, YAML). For source code, use replace_body or replace_full instead. Provide EITHER 'content' for full replacement OR 'replacements' for surgical search-and-replace edits (e.g., {old_text: 'postgres:15', new_text: 'postgres:16'}). Use replacements when changing specific text in large files. Requires base_version (OCC).\n\nbase_version accepts either the full SHA-256 hash (e.g., \"sha256:4ec5a8a...\") or a short 7-character prefix (e.g., \"sha256:4ec5a8a\"), matching Git convention."
    )]
    async fn write_file(
        &self,
        Parameters(params): Parameters<WriteFileParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.write_file_impl(params).await
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
    use crate::server::types::Replacement;
    use pathfinder_common::types::{FilterMode, VersionHash};
    use pathfinder_search::{MockScout, SearchMatch, SearchResult};
    use pathfinder_treesitter::mock::MockSurgeon;
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
        assert_eq!(skeleton, "class Mock {}");
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
    async fn test_create_file_success_and_already_exists() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);
        let mock_scout = MockScout::default();
        let server = PathfinderServer::with_engines(
            ws,
            config,
            sandbox,
            Arc::new(mock_scout),
            Arc::new(MockSurgeon::new()),
        );

        let filepath = "src/new_file.ts";
        let content = "console.log('hello');";
        let params = CreateFileParams {
            filepath: filepath.to_owned(),
            content: content.to_owned(),
        };

        // 1. First creation should succeed
        let result = server.create_file(Parameters(params.clone())).await;
        assert!(result.is_ok(), "Expected success, got {:#?}", result.err());
        let val = result.expect("create_file should succeed").0;
        assert!(val.success);
        assert_eq!(val.validation.status, "passed");

        let expected_hash = VersionHash::compute(content.as_bytes());
        assert_eq!(val.version_hash, expected_hash.as_str());

        // Verify file is on disk
        let absolute_path = ws_dir.path().join(filepath);
        assert!(absolute_path.exists());
        let read_content = fs::read_to_string(&absolute_path).expect("read file");
        assert_eq!(read_content, content);

        // 2. Second creation should fail (FILE_ALREADY_EXISTS)
        let result2 = server.create_file(Parameters(params)).await;
        assert!(result2.is_err());
        if let Err(err) = result2 {
            let code = err
                .data
                .as_ref()
                .and_then(|d| d.get("error"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            assert_eq!(code, "FILE_ALREADY_EXISTS", "got data: {:?}", err.data);
        } else {
            panic!("Expected error mapping to FILE_ALREADY_EXISTS");
        }

        // 3. Attempt to create file in a denied location
        let deny_params = CreateFileParams {
            filepath: ".git/objects/some_file".to_owned(),
            content: "payload".to_owned(),
        };
        let result3 = server.create_file(Parameters(deny_params)).await;
        assert!(result3.is_err());
        if let Err(err) = result3 {
            let code = err
                .data
                .as_ref()
                .and_then(|d| d.get("error"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            assert_eq!(code, "ACCESS_DENIED", "got data: {:?}", err.data);
        } else {
            panic!("Expected error mapping to ACCESS_DENIED");
        }
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
                version_hash: "sha256:123".to_owned(),
                known: None,
            }],
            total_matches: 1,
            truncated: false,
        }));

        let mock_surgeon = Arc::new(MockSurgeon::new());
        mock_surgeon
            .enclosing_symbol_results
            .lock()
            .unwrap()
            .push(Ok(Some("test_query_func".to_owned())));

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

        let surgeon_calls = mock_surgeon.enclosing_symbol_calls.lock().unwrap();
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
        let params = SearchCodebaseParams::default();

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
        }));

        let mock_surgeon = Arc::new(MockSurgeon::new());
        // 3 matches → 3 calls: code, comment, code
        // enclosing_symbol called 3 times → return None each (default "code" below)
        // node_type_at_position called 3 times → pre-configure results
        mock_surgeon
            .enclosing_symbol_results
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
        // total_matches reflects the ORIGINAL ripgrep count, not filtered count
        assert_eq!(result.total_matches, 3);
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
        }));

        let mock_surgeon = Arc::new(MockSurgeon::new());
        mock_surgeon
            .enclosing_symbol_results
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
        }));

        let mock_surgeon = Arc::new(MockSurgeon::default());
        // enclosing_symbol: all return None
        mock_surgeon
            .enclosing_symbol_results
            .lock()
            .unwrap()
            .extend([Ok(None), Ok(None), Ok(None)]);
        // node_type_at_position: will use default "code" since queue is empty
        // (FilterMode::All skips classification entirely — but mock still gets called;
        // the default return value is "code" so no pre-configuration needed)

        let server =
            PathfinderServer::with_engines(ws, config, sandbox, Arc::new(mock_scout), mock_surgeon);

        let params = SearchCodebaseParams {
            query: String::default(),
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

    // ── delete_file tests ────────────────────────────────────────────

    #[tokio::test]
    async fn test_delete_file_success_and_occ_failure() {
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

        // Create a file to delete
        let filepath = "to_delete.txt";
        let content = "goodbye";
        let abs = ws_dir.path().join(filepath);
        fs::write(&abs, content).expect("write");
        let hash = VersionHash::compute(content.as_bytes());

        // Happy path
        let result = server
            .delete_file(Parameters(DeleteFileParams {
                filepath: filepath.to_owned(),
                base_version: hash.as_str().to_owned(),
            }))
            .await;
        assert!(result.is_ok(), "Expected success, got {:?}", result.err());
        assert!(!abs.exists(), "File should be gone");

        // FILE_NOT_FOUND — file is already deleted, now handled via tfs::read NotFound (no pre-check race)
        let result2 = server
            .delete_file(Parameters(DeleteFileParams {
                filepath: filepath.to_owned(),
                base_version: hash.as_str().to_owned(),
            }))
            .await;
        assert!(result2.is_err());
        let Err(err) = result2 else {
            panic!("expected error")
        };
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "FILE_NOT_FOUND", "got: {err:?}");

        // VERSION_MISMATCH — recreate file, pass wrong hash
        fs::write(&abs, content).expect("write");
        let result3 = server
            .delete_file(Parameters(DeleteFileParams {
                filepath: filepath.to_owned(),
                base_version: "sha256:wrong".to_owned(),
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
        assert_eq!(code, "VERSION_MISMATCH", "got: {err:?}");

        // ACCESS_DENIED — sandbox-protected path
        let result4 = server
            .delete_file(Parameters(DeleteFileParams {
                filepath: ".git/objects/x".to_owned(),
                base_version: "sha256:any".to_owned(),
            }))
            .await;
        assert!(result4.is_err());
        let Err(err) = result4 else {
            panic!("expected error")
        };
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "ACCESS_DENIED", "got: {err:?}");
    }

    // ── read_file tests ──────────────────────────────────────────────

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
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);
        let mock_surgeon = Arc::new(MockSurgeon::new());

        let content = "func Login() {}";
        let expected_scope = pathfinder_common::types::SymbolScope {
            content: content.to_owned(),
            start_line: 5,
            end_line: 7,
            version_hash: VersionHash::compute(content.as_bytes()),
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
        assert_eq!(t.text, expected_scope.content);

        let metadata: crate::server::types::ReadSymbolScopeMetadata =
            serde_json::from_value(val.structured_content.expect("missing structured_content"))
                .expect("valid metadata");

        assert_eq!(metadata.start_line, expected_scope.start_line);
        assert_eq!(metadata.end_line, expected_scope.end_line);
        assert_eq!(metadata.version_hash, expected_scope.version_hash.as_str());
        assert_eq!(metadata.language, expected_scope.language);

        let calls = mock_surgeon.read_symbol_scope_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
    }

    #[tokio::test]
    async fn test_read_symbol_scope_handles_surgeon_error() {
        let ws_dir = tempdir().expect("temp dir");
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

    // ── write_file tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_write_file_full_replacement() {
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

        let filepath = "config.toml";
        let original = "[server]\nport = 8080";
        let abs = ws_dir.path().join(filepath);
        fs::write(&abs, original).expect("write");
        let hash = VersionHash::compute(original.as_bytes());

        // Happy path — full replacement
        let replacement = "[server]\nport = 9090";
        let result = server
            .write_file(Parameters(WriteFileParams {
                filepath: filepath.to_owned(),
                base_version: hash.as_str().to_owned(),
                content: Some(replacement.to_owned()),
                replacements: None,
            }))
            .await
            .expect("should succeed");
        let val: crate::server::types::WriteFileMetadata =
            serde_json::from_value(result.structured_content.unwrap()).unwrap();
        assert!(val.success);
        let on_disk = fs::read_to_string(&abs).expect("read");
        assert_eq!(on_disk, replacement);
        let new_hash = VersionHash::compute(replacement.as_bytes());
        assert_eq!(val.new_version_hash, new_hash.as_str());

        // VERSION_MISMATCH — use old hash
        let result2 = server
            .write_file(Parameters(WriteFileParams {
                filepath: filepath.to_owned(),
                base_version: hash.as_str().to_owned(), // stale
                content: Some("something else".to_owned()),
                replacements: None,
            }))
            .await;
        assert!(result2.is_err());
        let Err(err) = result2 else {
            panic!("expected error")
        };
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "VERSION_MISMATCH", "got: {err:?}");
    }

    #[tokio::test]
    async fn test_write_file_search_and_replace() {
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

        let filepath = "docker-compose.yml";
        let original = "image: postgres:15\nports:\n  - 5432:5432";
        let abs = ws_dir.path().join(filepath);
        fs::write(&abs, original).expect("write");
        let hash = VersionHash::compute(original.as_bytes());

        // Happy path — single match
        let result = server
            .write_file(Parameters(WriteFileParams {
                filepath: filepath.to_owned(),
                base_version: hash.as_str().to_owned(),
                content: None,
                replacements: Some(vec![Replacement {
                    old_text: "postgres:15".to_owned(),
                    new_text: "postgres:16-alpine".to_owned(),
                }]),
            }))
            .await
            .expect("should succeed");
        let val: crate::server::types::WriteFileMetadata =
            serde_json::from_value(result.structured_content.unwrap()).unwrap();
        assert!(val.success);
        let on_disk = fs::read_to_string(&abs).expect("read");
        assert!(on_disk.contains("postgres:16-alpine"));
        let new_hash_val = val.new_version_hash;

        // MATCH_NOT_FOUND — old text no longer exists
        let result2 = server
            .write_file(Parameters(WriteFileParams {
                filepath: filepath.to_owned(),
                base_version: new_hash_val.clone(),
                content: None,
                replacements: Some(vec![Replacement {
                    old_text: "postgres:15".to_owned(), // already replaced
                    new_text: "postgres:17".to_owned(),
                }]),
            }))
            .await;
        assert!(result2.is_err());
        let Err(err) = result2 else {
            panic!("expected error")
        };
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "MATCH_NOT_FOUND", "got: {err:?}");

        // AMBIGUOUS_MATCH — inject a file where old_text appears twice
        let ambiguous = "tag: v1\ntag: v1";
        fs::write(&abs, ambiguous).expect("write");
        let ambig_hash = VersionHash::compute(ambiguous.as_bytes());
        let result3 = server
            .write_file(Parameters(WriteFileParams {
                filepath: filepath.to_owned(),
                base_version: ambig_hash.as_str().to_owned(),
                content: None,
                replacements: Some(vec![Replacement {
                    old_text: "tag: v1".to_owned(),
                    new_text: "tag: v2".to_owned(),
                }]),
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
        assert_eq!(code, "AMBIGUOUS_MATCH", "got: {err:?}");
    }

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
                    version_hash: "sha256:xyz".to_owned(),
                    known: None,
                },
            ],
            total_matches: 2,
            truncated: false,
        }));

        let mock_surgeon = Arc::new(MockSurgeon::new());
        // Two matches → two enrichment calls
        mock_surgeon
            .enclosing_symbol_results
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
                version_hash: "sha256:abc".to_owned(),
                known: None,
            }],
            total_matches: 1,
            truncated: false,
        }));

        let mock_surgeon = Arc::new(MockSurgeon::new());
        mock_surgeon
            .enclosing_symbol_results
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
                    version_hash: "sha256:main".to_owned(),
                    known: None,
                },
            ],
            total_matches: 3,
            truncated: false,
        }));

        let mock_surgeon = Arc::new(MockSurgeon::new());
        // 3 enrichments
        mock_surgeon
            .enclosing_symbol_results
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
                version_hash: VersionHash::compute(b"fn hello() -> i32 { 1 }"),
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

    // ── file_ops edge case tests (WP-6) ──────────────────────────────────

    #[tokio::test]
    async fn test_create_file_broadcasts_watched_file_event() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            Arc::new(MockSurgeon::new()),
            lawyer.clone(),
        );

        let params = crate::server::types::CreateFileParams {
            filepath: "src/new_file.rs".to_owned(),
            content: "fn new() {}".to_owned(),
        };
        let result = server.create_file_impl(params).await;
        let res = result.expect("should succeed");
        assert!(res.0.success);

        // Verify the file was created
        assert!(ws_dir.path().join("src/new_file.rs").exists());

        // Verify watched file event was broadcast
        assert_eq!(lawyer.watched_file_changes_count(), 1);
    }

    #[tokio::test]
    async fn test_delete_file_broadcasts_watched_file_event() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());

        // Create a file to delete
        std::fs::write(ws_dir.path().join("to_delete.txt"), "content").unwrap();
        let hash = VersionHash::compute(b"content");

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            Arc::new(MockSurgeon::new()),
            lawyer.clone(),
        );

        let params = crate::server::types::DeleteFileParams {
            filepath: "to_delete.txt".to_owned(),
            base_version: hash.as_str().to_owned(),
        };
        let result = server.delete_file_impl(params).await;
        let res = result.expect("should succeed");
        assert!(res.0.success);

        // Verify the file was deleted
        assert!(!ws_dir.path().join("to_delete.txt").exists());

        // Verify watched file event was broadcast
        assert_eq!(lawyer.watched_file_changes_count(), 1);
    }

    #[tokio::test]
    async fn test_delete_file_not_found() {
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

        let params = crate::server::types::DeleteFileParams {
            filepath: "nonexistent.txt".to_owned(),
            base_version: "sha256:any".to_owned(),
        };
        let result = server.delete_file_impl(params).await;
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
            Arc::new(pathfinder_lsp::MockLawyer::default()),
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

    #[tokio::test]
    async fn test_write_file_broadcasts_watched_file_event() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        // Write initial file
        let initial_content = "initial content";
        std::fs::write(ws_dir.path().join("config.toml"), initial_content).unwrap();
        let hash = VersionHash::compute(initial_content.as_bytes());

        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            Arc::new(MockSurgeon::new()),
            lawyer.clone(),
        );

        let params = crate::server::types::WriteFileParams {
            filepath: "config.toml".to_owned(),
            base_version: hash.as_str().to_owned(),
            content: Some("updated content".to_owned()),
            replacements: None,
        };
        let result = server.write_file_impl(params).await;
        assert!(result.is_ok(), "write should succeed");

        // Verify content updated
        let written = std::fs::read_to_string(ws_dir.path().join("config.toml")).unwrap();
        assert_eq!(written, "updated content");

        // Verify watched file event was broadcast
        assert_eq!(lawyer.watched_file_changes_count(), 1);
    }

    #[tokio::test]
    async fn test_write_file_invalid_params_both_modes() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        std::fs::write(ws_dir.path().join("test.txt"), "content").unwrap();

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            Arc::new(MockSurgeon::new()),
            Arc::new(pathfinder_lsp::MockLawyer::default()),
        );

        // Both content and replacements set — invalid
        let hash = VersionHash::compute(b"content");
        let params = crate::server::types::WriteFileParams {
            filepath: "test.txt".to_owned(),
            base_version: hash.as_str().to_owned(),
            content: Some("new".to_owned()),
            replacements: Some(vec![crate::server::types::Replacement {
                old_text: "a".to_string(),
                new_text: "b".to_string(),
            }]),
        };
        let result = server.write_file_impl(params).await;
        assert!(result.is_err(), "should reject both modes");
    }

    #[tokio::test]
    async fn test_write_file_invalid_params_neither_mode() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        std::fs::write(ws_dir.path().join("test.txt"), "content").unwrap();

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            Arc::new(MockSurgeon::new()),
            Arc::new(pathfinder_lsp::MockLawyer::default()),
        );

        // Neither content nor replacements — invalid
        let hash = VersionHash::compute(b"content");
        let params = crate::server::types::WriteFileParams {
            filepath: "test.txt".to_owned(),
            base_version: hash.as_str().to_owned(),
            content: None,
            replacements: None,
        };
        let result = server.write_file_impl(params).await;
        assert!(result.is_err(), "should reject neither mode");
    }
}
