//! Pathfinder MCP Server — tool registration and dispatch.
//!
//! Implements `rmcp::ServerHandler` for all Pathfinder discovery & navigation tools.
//!
//! # Module Layout
//! - [`helpers`] — error conversion, stub builder, language detection
//! - [`types`] — all parameter and response structs
//! - [`tools`] — handler logic:
//!   - The `#[tool_router]` impl block below registers 7 consolidated tools:
//!     `explore`, `search`, `read`, `inspect`, `locate`, `trace`, `health`.
//!   - Submodules (`search`, `repo_map`, `navigation`, etc.) contain
//!     the underlying implementations delegated to by these handlers.

/// A cached probe result.
///
/// Positive and negative entries are cached and evaluated based on dynamic
/// intervals that scale with the LSP process age.
#[derive(Clone)]
pub(crate) struct ProbeCacheEntry {
    /// Whether the probe succeeded.
    pub(crate) success: bool,
    /// Whether call hierarchy was verified.
    pub(crate) call_hierarchy_verified: bool,
    /// When this entry was created.
    pub(crate) created_at: std::time::Instant,
}

impl ProbeCacheEntry {
    pub(crate) fn new(success: bool, call_hierarchy_verified: bool) -> Self {
        Self {
            success,
            call_hierarchy_verified,
            created_at: std::time::Instant::now(),
        }
    }

    /// How old is this cache entry in seconds?
    pub(crate) fn age_secs(&self) -> u64 {
        self.created_at.elapsed().as_secs()
    }
}

mod helpers;
mod tools;
/// Module containing type definitions.
pub mod types;

use types::{
    ExploreParams, HealthParams, InspectParams, LocateParams, ReadParams, SearchParams, TraceParams,
};

use pathfinder_common::config::PathfinderConfig;
use pathfinder_common::sandbox::Sandbox;
use pathfinder_common::types::WorkspaceRoot;
use pathfinder_lsp::{Lawyer, LspClient, NoOpLawyer};
use pathfinder_search::{RipgrepScout, Scout};
use pathfinder_treesitter::{Surgeon, TreeSitterSurgeon};

use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
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
    /// Cache validity is evaluated dynamically based on `get_probe_interval()`,
    /// which scales with the LSP process age (10s for first 60s, 30s for
    /// 60-300s, 120s for 300s+). The effective threshold is
    /// `min(30, probe_interval)` seconds.
    probe_cache: Arc<std::sync::Mutex<std::collections::HashMap<String, ProbeCacheEntry>>>,
    /// Tracking map of when each LSP was first detected as connected (uptime is Some).
    /// Used for dynamic probe interval ramp-up schedule.
    lsp_started_at: Arc<std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>>,
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
                    // tool calls (explore, search, etc.).
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
            lsp_started_at: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }
}

// ── Tool Router (defines all 7 consolidated tools) ─────────────────

#[tool_router]
impl PathfinderServer {
    #[tool(
        name = "explore",
        description = "Get the structural skeleton of the project — directory tree, file listing, or full symbol hierarchy.

Use when: Exploring project structure, discovering available symbols, or planning navigation.
Alternative: Use `read` for a single file's content. Use `search(mode=\"symbol\")` to locate a symbol by name.

IMPORTANT: Copy-paste the exact semantic paths from the output into other Pathfinder tools.

Response format: The skeleton (directory tree, file list, or symbol hierarchy) is in the TEXT
content. Structured content contains only metadata (coverage_percent, tech_stack, version_hashes,
mode, dirs_scanned). Always read the text content for the actual output.

Metadata fields:
- `mode`: Indicates which detail mode was used: \"structure\", \"files\", or \"symbols\".
- `dirs_scanned`: Number of directories scanned (only present for `detail=\"structure\"`).
- `files_scanned`: Number of source files scanned (always 0 for structure mode).
  Check `mode` and `dirs_scanned` to confirm structure-mode calls succeeded.

Parameter guidance:
- `detail`: Controls output verbosity.
  - `\"structure\"` — directory tree + package manager files only (cheapest). Token cap: 4,000.
  - `\"files\"` — directory tree + all filenames (no symbols). Token cap: 8,000.
  - `\"symbols\"` (default) — full AST symbol hierarchy. Uses provided max_tokens (default 16,000).
- `depth=3` (default): Increase for deeply-nested monorepos.
- `max_tokens=16000` (default for symbols): Auto-scales up to 48,000 for large repos.
  NOTE: structure and files modes ignore this value — they use their own caps (4,000 / 8,000).
- `visibility`: `\"public\"` (default) or `\"all\"` (includes private/internal).

Example: `explore(path=\"src/\", detail=\"files\", depth=5)`"
    )]
    async fn explore(
        &self,
        Parameters(params): Parameters<ExploreParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.get_repo_map_impl(params).await
    }

    #[tool(
        name = "search",
        description = "Search for text patterns, regex, or resolve symbol names across the codebase.

Use when: Finding text/patterns, locating function calls, or resolving a bare symbol name to its semantic path.

Parameter guidance:
- `mode`: Controls search behavior.
  - `\"text\"` (default) — literal text search.
  - `\"regex\"` — regex pattern search.
  - `\"symbol\"` — resolve bare symbol name to `file::symbol` semantic paths.
    Use `kind` to filter by symbol type. Accepted values (case-insensitive):
    Canonical: function, class, struct, interface, enum, constant, module, impl.
    Aliases: method/fn → function; trait → interface; const/static/let → constant;
    mod/namespace → module; class also matches struct and interface.
    Invalid kind values return an error listing accepted values.
- `path_glob`: Limit scope (e.g., `\"**/*.rs\"`).
- `max_results=50` (default): Cap returned matches. Applies to all modes including `symbol`.
- `known_files`: Suppress full content for files already in context.

Examples:
- `search(query=\"login\", path_glob=\"**/*.rs\")` — text search
- `search(query=\"TODO|FIXME\", mode=\"regex\")` — regex search
- `search(query=\"AuthService\", mode=\"symbol\", kind=\"class\")` — symbol lookup"
    )]
    async fn search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.search_impl(params).await
    }

    #[tool(
        name = "read",
        description = "Read file contents — single file or batch. Auto-detects source vs config files.

Use when: Reading any file. Source files (.rs, .ts, .go, .py, .vue, .js, .java) get AST-parsed content. Config files (.yaml, .toml, .json, .env) get raw content.

Parameter guidance:
- `filepath`: Single file path. Use for reading one file.
- `paths`: Array of file paths (max 10). Use for batch reading.
  Exactly one of `filepath` or `paths` must be provided.
- `detail_level`: `\"source_only\"` (lowest tokens), `\"compact\"` (default), `\"symbols\"` (tree only), `\"full\"` (source + nested AST).
- `start_line`/`end_line`: Restrict output to a line range.
- `max_lines_per_file`: Maximum lines returned per file (defaults to 500). Applies to batch mode and config/raw files.

Examples:
- `read(filepath=\"src/auth.ts\", detail_level=\"compact\")` — single source file
- `read(filepath=\".env\")` — config file (raw content)
- `read(paths=[\"src/auth.ts\", \"src/config.ts\"])` — batch read"
    )]
    async fn read(
        &self,
        Parameters(params): Parameters<ReadParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.read_impl(params).await
    }

    #[tool(
        name = "inspect",
        description = r#"Extract a symbol's source code by semantic path (single or batch), optionally with its dependency graph.

Use when: You know the exact symbol and want its source code. Optionally includes signatures of all functions it calls.
Alternative: Use `read` for full file content.

IMPORTANT: `semantic_path` MUST include file path + '::' (e.g., `src/auth.ts::AuthService.login`).

Parameter guidance:
- `semantic_path`: Single semantic path.
- `semantic_paths`: Array of semantic paths (max 10) for batch inspection.
  Provide either `semantic_path` or `semantic_paths`.
- `include_dependencies=false` (default): Source code only (fast, Tree-sitter).
- `include_dependencies=true`: Source + callee signatures (LSP-powered, may take 5–30s on first call).
- `max_dependencies=50` (default): Cap dependency output.

Examples:
- `inspect(semantic_path="src/auth.ts::AuthService.login")` — source only
- `inspect(semantic_paths=["src/auth.ts::AuthService.login", "src/auth.ts::AuthService.logout"])` — batch inspect"#
    )]
    async fn inspect(
        &self,
        Parameters(params): Parameters<InspectParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.inspect_impl(params).await
    }

    #[tool(
        name = "locate",
        description = r#"Jump to a symbol's definition, or resolve a file+line to its semantic path (single or batch).

Use when: Navigating to where a symbol is defined, or converting stack trace locations to semantic paths.

Three modes (auto-detected from input):
1. **Definition lookup**: Provide `semantic_path` → returns definition file, line, column, and code preview.
2. **Semantic path resolution**: Provide `file` + `line` → returns the `file::symbol` semantic path of the enclosing symbol.
3. **Batch locate**: Provide `locations` array containing up to 10 entries (each with either `semantic_path` or `file` + `line`).

LSP-powered with ripgrep fallback. Check `degraded` in response.

Examples:
- `locate(semantic_path="src/auth.ts::AuthService.login")` — jump to definition
- `locate(file="src/auth.ts", line=42)` — resolve to semantic path
- `locate(locations=[{semantic_path: "src/auth.ts::AuthService.login"}, {file: "src/auth.ts", line: 42}])` — batch locate"#
    )]
    async fn locate(
        &self,
        Parameters(params): Parameters<LocateParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.locate_impl(params).await
    }

    #[tool(
        name = "trace",
        description = "Trace a symbol's relationships — callers/callees, all references, or full overview.

ALWAYS run this before recommending a refactor to check for unexpected callers.

Use when: Understanding blast radius, finding all usages, or getting a complete picture before refactoring.

Parameter guidance:
- `scope`: What to trace.
  - `\"callers\"` (default) — call hierarchy: who calls this and what it calls. Use `max_depth` to control traversal.
  - `\"references\"` — all references: calls, imports, type annotations, field access.
  - `\"overview\"` — combined: source + callers + callees + references in one call. Uses `max_references` for both callers/callees and references caps.
- `max_depth=3` (default): For call hierarchy traversal. Increase to 4-5 for large API changes.
- `max_references=50` (default): Cap output for all scopes. In `overview` mode, applies to both caller/callee and reference limits.
- `offset`: Pagination offset. Applies to `scope=\"references\"` only; ignored for `callers` and `overview`.

LSP-powered. Response field semantics when `degraded=true`:
- `incoming`/`outgoing` are `null` — LSP was unavailable and no heuristic results found. Callers are **unknown**.
- `incoming`/`outgoing` contain results with `confidence: \"heuristic\"` — grep-based fallback found candidates (may include false positives).
- `incoming`/`outgoing` are `[]` — LSP confirmed zero callers/callees exist (only when `degraded=false`).
Never treat `null` as \"no callers\" — it means the answer is unknown. Use `search` as a fallback.

⚠️ CRITICAL — null vs empty array are NOT equivalent:
  null  = UNKNOWN (degraded — callers may exist but LSP couldn't confirm)
  []    = CONFIRMED ZERO (LSP verified — safe to conclude no callers)
Mistaking null for \"no callers\" leads to dangerous refactoring decisions.

Examples:
- `trace(semantic_path=\"src/auth.ts::AuthService.login\")` — callers/callees
- `trace(semantic_path=\"src/auth.ts::AuthService.login\", scope=\"references\")` — all usages
- `trace(semantic_path=\"src/auth.ts::AuthService.login\", scope=\"overview\")` — full picture"
    )]
    async fn trace(
        &self,
        Parameters(params): Parameters<TraceParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.trace_impl(params).await
    }

    #[tool(
        name = "health",
        description = "Check LSP health and readiness per language.

Use when: Diagnosing why navigation tools returned degraded results, or checking LSP status.

Pass `language` to check a specific language, or omit to check all.
Pass `action=\"restart\"` with `language` to force-restart a stuck LSP process.
Pass `force_probe=true` to force a live liveness check regardless of cache age.

Example: `health(language=\"rust\", force_probe=true)`"
    )]
    async fn health(
        &self,
        Parameters(params): Parameters<HealthParams>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::model::ErrorData> {
        self.health_impl(params).await
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

#[cfg(test)]
#[path = "server_test.rs"]
mod tests;
