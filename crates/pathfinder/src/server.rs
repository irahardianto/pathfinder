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
//!   - [`tools::navigation`] — `get_definition`, `analyze_impact`
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
    AnalyzeImpactParams, GetDefinitionParams, GetRepoMapParams, ReadFileParams,
    ReadSourceFileParams, ReadSymbolScopeParams, ReadWithDeepContextParams, SearchCodebaseParams,
    SearchCodebaseResponse,
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

// ── Tool Router (defines all 9 tools) ──────────────────────────────

#[tool_router]
impl PathfinderServer {
    #[tool(
        name = "search_codebase",
        description = "Search for text or regex patterns across source code. Returns matching lines with `enclosing_semantic_path` (the symbol containing the match). Use `filter_mode` to search code_only (default), comments_only, or all. Use `path_glob` to narrow scope and `exclude_glob` to skip files (e.g. tests). Pass `known_files` to suppress content for already-read files and `group_by_file=true` to group matches."
    )]
    async fn search_codebase(
        &self,
        Parameters(params): Parameters<SearchCodebaseParams>,
    ) -> Result<Json<SearchCodebaseResponse>, ErrorData> {
        self.search_codebase_impl(params).await
    }

    #[tool(
        name = "get_repo_map",
        description = "Get the structural skeleton of the project — an indented tree of symbols with their semantic paths. IMPORTANT: Copy-paste the exact semantic paths from the output into other Pathfinder tools. Use `max_tokens` (default 16000) and `max_tokens_per_file` (default 2000) to control coverage. Use `visibility=\"all\"` for all symbols including private/internal, or `visibility=\"public\"` (default) for exported/public symbols only. Use `changed_since` (e.g. '3h', 'HEAD~5') to scope to recent changes. Use `include_extensions`/`exclude_extensions` to filter by language. Use `include_imports` with values `\"none\"`, `\"third_party\"` (default), or `\"all\"` to control whether imports are included in the skeleton."
    )]
    async fn get_repo_map(
        &self,
        Parameters(params): Parameters<GetRepoMapParams>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::model::ErrorData> {
        self.get_repo_map_impl(params).await
    }

    #[tool(
        name = "read_symbol_scope",
        description = "Extract the exact source code of one symbol (function, class, method) by semantic path. IMPORTANT: semantic_path MUST include file path + '::', e.g. 'src/auth.ts::AuthService.login'. Returns source and line range. Use this for focused reading of a single symbol without context waste."
    )]
    async fn read_symbol_scope(
        &self,
        Parameters(params): Parameters<ReadSymbolScopeParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.read_symbol_scope_impl(params).await
    }

    #[tool(
        name = "read_source_file",
        description = "Read an entire source file with its full AST symbol hierarchy. Returns source, language, and a nested symbol tree with semantic paths. **AST-only** — only for source files (.rs, .ts, .tsx, .go, .py, .vue, .jsx, .js); use `read_file` for config/docs files. detail_level: `compact` (default) = source + flat symbols, `symbols` = tree only, `full` = source + nested AST. Use `start_line`/`end_line` to restrict output."
    )]
    async fn read_source_file(
        &self,
        Parameters(params): Parameters<ReadSourceFileParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.read_source_file_impl(params).await
    }

    #[tool(
        name = "read_with_deep_context",
        description = "Read a symbol's source code PLUS the signatures of all functions it calls — its dependency graph in one call. IMPORTANT: semantic_path MUST include file path + '::' (e.g. 'src/auth.ts::AuthService.login'). LSP-powered; first call may take 5–30s during LSP warmup. Check `degraded` in response — if true, dependencies may be incomplete. Source code is always returned even when degraded."
    )]
    async fn read_with_deep_context(
        &self,
        Parameters(params): Parameters<ReadWithDeepContextParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.read_with_deep_context_impl(params).await
    }

    #[tool(
        name = "get_definition",
        description = "Jump to where a symbol is defined. Returns the definition's file, line, column, and a code preview. IMPORTANT: semantic_path MUST include file path + '::' (e.g. 'src/auth.ts::AuthService.login'). If you don't know which file defines a symbol, use search_codebase first to locate it. LSP-powered — follows imports, re-exports, and type aliases across files. Falls back to ripgrep when LSP is unavailable. Check `degraded` in response."
    )]
    async fn get_definition(
        &self,
        Parameters(params): Parameters<GetDefinitionParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.get_definition_impl(params).await
    }

    #[tool(
        name = "analyze_impact",
        description = "Map the blast radius of a symbol: all callers (incoming) and all callees (outgoing). Use before any refactor to find who depends on a function. IMPORTANT: semantic_path MUST include file path + '::' (e.g. 'src/mod.rs::func'). LSP-powered with grep fallback. Check `degraded` — when true, empty results may be due to LSP warmup, not genuinely zero callers."
    )]
    async fn analyze_impact(
        &self,
        Parameters(params): Parameters<AnalyzeImpactParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.analyze_impact_impl(params).await
    }

    #[tool(
        name = "lsp_health",
        description = "Check LSP health per language. Returns overall status (ready / warming_up / starting / unavailable) and per-language details. Use this when navigation tools return degraded results, or at session start to know which languages have full LSP support. Pass `language` to check a specific language, or omit to check all. When status is not 'ready', navigation tools may return incomplete results."
    )]
    async fn lsp_health(
        &self,
        Parameters(params): Parameters<crate::server::types::LspHealthParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.lsp_health_impl(params).await
    }

    #[tool(
        name = "read_file",
        description = "Read raw file content with optional pagination (start_line, max_lines). Use for config files (.env, YAML, TOML, Dockerfile, package.json). For source code, prefer read_symbol_scope or read_source_file instead."
    )]
    async fn read_file(
        &self,
        Parameters(params): Parameters<ReadFileParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.read_file_impl(params).await
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
        assert_eq!(t.text, expected_scope.content);

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
