//! Pathfinder MCP Server — tool registration and dispatch.
//!
//! Implements `rmcp::ServerHandler` for all Pathfinder discovery & navigation tools.
//!
//! # Module Layout
//! - `helpers` — error conversion, stub builder, language detection
//! - `types` — all parameter and response structs
//! - `tools` — handler logic:
//!   - The `#[tool_router]` impl block below registers 3 consolidated tools:
//!     `inspect`, `locate`, `trace`.
//!   - Submodules (`inspect`, `navigation`, `semantic_path`) contain
//!     the underlying implementations delegated to by these handlers.

mod helpers;
mod tools;
/// Module containing type definitions.
pub mod types;

use types::{InspectParams, LocateParams, TraceParams};

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
}

impl PathfinderServer {
    async fn init_lsp_client(
        workspace_root: &WorkspaceRoot,
        config: &PathfinderConfig,
    ) -> Arc<dyn Lawyer> {
        let _ = pathfinder_lsp::load_workspace_toml_plugins(workspace_root.path()).await;

        match LspClient::new(workspace_root.path(), Arc::new(config.clone())).await {
            Ok(client) => {
                // Kick off background initialization so LSP processes are
                // already loading while the agent issues its first tool calls.
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
        }
    }

    /// Create a new Pathfinder server backed by the real Ripgrep scout, Tree-sitter
    /// surgeon, and `LspClient` for LSP operations.
    ///
    /// # Lifecycle
    ///
    /// Zero-Config language detection runs synchronously during construction.
    /// LSP processes are then started **eagerly**: `warm_start()` spawns every
    /// detected language in parallel background tasks before this function
    /// returns.
    ///
    /// If Zero-Config detection fails (e.g., unreadable workspace directory), the server
    /// falls back to `NoOpLawyer` and logs a warning. All tools remain functional in
    /// degraded mode.
    #[must_use]
    pub async fn new(workspace_root: WorkspaceRoot, config: PathfinderConfig) -> Self {
        let sandbox = Sandbox::new(workspace_root.path(), &config.sandbox);
        let lawyer = Self::init_lsp_client(&workspace_root, &config).await;

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
        }
    }
}

// ── Tool Router (defines all 3 consolidated tools) ─────────────────

#[tool_router]
impl PathfinderServer {
    #[tool(
        name = "inspect",
        description = "Extract symbol source by semantic path (\"file::symbol\"). Set include_dependencies=true for LSP callee signatures (default false).
Example: inspect(semantic_path=\"src/auth.ts::AuthService.login\")"
    )]
    async fn inspect(
        &self,
        Parameters(params): Parameters<InspectParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.inspect_impl(params).await
    }

    #[tool(
        name = "locate",
        description = "Jump to definition by semantic_path (\"file::symbol\") or resolve file+line to semantic path (or batch via locations).
Example: locate(semantic_path=\"src/auth.ts::AuthService.login\"), locate(file=\"src/auth.ts\", line=42)"
    )]
    async fn locate(
        &self,
        Parameters(params): Parameters<LocateParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.locate_impl(params).await
    }

    #[tool(
        name = "trace",
        description = "Trace callers/callees (scope=\"callers\"), refs (\"references\"), or \"overview\".
null = UNKNOWN; [] = zero. Heuristics marked confidence=\"heuristic\"."
    )]
    async fn trace(
        &self,
        Parameters(params): Parameters<TraceParams>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        self.trace_impl(params).await
    }
}

// ── ServerHandler trait impl ────────────────────────────────────────

#[allow(clippy::unused_async_trait_impl)]
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
