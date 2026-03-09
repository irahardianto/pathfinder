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
//! - `analyze_impact` — returns empty caller/callee lists with `degraded: true`
//! - `read_with_deep_context` — returns the symbol scope only, no dependencies

use crate::server::helpers::{
    io_error_data, pathfinder_to_error_data, treesitter_error_to_error_data,
};
use crate::server::types::{
    AnalyzeImpactParams, AnalyzeImpactResponse, GetDefinitionParams, GetDefinitionResponse,
    ImpactReference, ReadWithDeepContextParams, ReadWithDeepContextResponse,
};
use crate::server::PathfinderServer;
use pathfinder_common::error::PathfinderError;
use pathfinder_common::types::SemanticPath;
use pathfinder_lsp::LspError;
use rmcp::handler::server::wrapper::Json;
use rmcp::model::ErrorData;

impl PathfinderServer {
    /// Core logic for the `get_definition` tool.
    ///
    /// Resolves the semantic path to a file position, queries the LSP for the
    /// definition location, and returns the result.
    ///
    /// **Degraded mode:** Returns a `LSP_REQUIRED` error when no LSP is configured.
    #[expect(
        clippy::too_many_lines,
        reason = "Sequential pipeline (parse→sandbox→tree-sitter→LSP→match 4 result variants) plus per-engine timing."
    )]
    pub(crate) async fn get_definition_impl(
        &self,
        params: GetDefinitionParams,
    ) -> Result<Json<GetDefinitionResponse>, ErrorData> {
        let start = std::time::Instant::now();

        tracing::info!(
            tool = "get_definition",
            semantic_path = %params.semantic_path,
            "get_definition: start"
        );

        // Parse and validate the semantic path
        let Some(semantic_path) = SemanticPath::parse(&params.semantic_path) else {
            let duration_ms = start.elapsed().as_millis();
            tracing::warn!(
                tool = "get_definition",
                error_code = "INVALID_TARGET",
                duration_ms,
                "invalid semantic path"
            );
            return Err(io_error_data("invalid semantic path format".to_string()));
        };

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

        // Query LSP for the definition location at the symbol's start line
        let lsp_start = std::time::Instant::now();
        let lsp_result = self
            .lawyer
            .goto_definition(
                self.workspace_root.path(),
                &semantic_path.file_path,
                // Convert 0-indexed start_line from SymbolScope to 1-indexed for Lawyer
                u32::try_from(symbol_scope.start_line + 1).unwrap_or(1),
                1, // Column 1 — start of the identifier line
            )
            .await;
        let lsp_ms = lsp_start.elapsed().as_millis();

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
                Ok(Json(GetDefinitionResponse {
                    file: def.file,
                    line: def.line,
                    column: def.column,
                    preview: def.preview,
                    degraded: None,
                    degraded_reason: None,
                }))
            }
            Ok(None) => {
                // Symbol has no definition (e.g., built-in, external)
                tracing::info!(
                    tool = "get_definition",
                    semantic_path = %params.semantic_path,
                    tree_sitter_ms,
                    lsp_ms,
                    duration_ms,
                    "get_definition: no definition found (built-in or external)"
                );
                Err(pathfinder_to_error_data(&PathfinderError::SymbolNotFound {
                    semantic_path: params.semantic_path,
                    did_you_mean: vec![],
                }))
            }
            Err(LspError::NoLspAvailable) => {
                // Degraded mode — LSP not configured
                tracing::info!(
                    tool = "get_definition",
                    tree_sitter_ms,
                    lsp_ms,
                    duration_ms,
                    degraded = true,
                    degraded_reason = "no_lsp",
                    engines_used = ?["none"],
                    "get_definition: degraded (no LSP)"
                );
                Err(pathfinder_to_error_data(&PathfinderError::NoLspAvailable {
                    language: symbol_scope.language,
                }))
            }
            Err(e) => {
                tracing::warn!(
                    tool = "get_definition",
                    error = %e,
                    tree_sitter_ms,
                    lsp_ms,
                    duration_ms,
                    engines_used = ?["lsp"],
                    "get_definition: LSP error"
                );
                Err(pathfinder_to_error_data(&PathfinderError::LspError {
                    message: e.to_string(),
                }))
            }
        }
    }

    /// Core logic for the `read_with_deep_context` tool.
    ///
    /// Returns the symbol's source code. When LSP is available, appends the
    /// signatures of all called symbols. Degrades gracefully to symbol scope
    /// only when no LSP is configured.
    pub(crate) async fn read_with_deep_context_impl(
        &self,
        params: ReadWithDeepContextParams,
    ) -> Result<Json<ReadWithDeepContextResponse>, ErrorData> {
        let start = std::time::Instant::now();

        tracing::info!(
            tool = "read_with_deep_context",
            semantic_path = %params.semantic_path,
            "read_with_deep_context: start"
        );

        // Parse and validate the semantic path
        let Some(semantic_path) = SemanticPath::parse(&params.semantic_path) else {
            let duration_ms = start.elapsed().as_millis();
            tracing::warn!(
                tool = "read_with_deep_context",
                error_code = "INVALID_TARGET",
                duration_ms,
                "invalid semantic path"
            );
            return Err(io_error_data("invalid semantic path format".to_string()));
        };

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

        let duration_ms = start.elapsed().as_millis();

        // In Milestone 1, dependency resolution via LSP is not yet implemented.
        // Return the symbol scope with degraded: true.
        tracing::info!(
            tool = "read_with_deep_context",
            semantic_path = %params.semantic_path,
            tree_sitter_ms,
            duration_ms,
            degraded = true,
            degraded_reason = "no_lsp",
            engines_used = ?["tree-sitter"],
            "read_with_deep_context: complete (degraded)"
        );

        Ok(Json(ReadWithDeepContextResponse {
            content: scope.content,
            start_line: scope.start_line,
            end_line: scope.end_line,
            version_hash: scope.version_hash.to_string(),
            language: scope.language,
            dependencies: vec![],
            degraded: Some(true),
            degraded_reason: Some("no_lsp".to_owned()),
        }))
    }

    /// Core logic for the `analyze_impact` tool.
    ///
    /// Returns callers (incoming) and callees (outgoing) for the target symbol.
    /// Degrades gracefully to empty results when no LSP is configured.
    ///
    /// In Milestone 1 this always returns empty degraded results. Future milestones
    /// will wire the LSP call hierarchy query.
    // Intentionally `async`: Milestone 2 will add `await` when the LSP call hierarchy
    // is wired. Keeping `async` now avoids a breaking change to the call site in server.rs.
    #[expect(
        clippy::unused_async,
        reason = "Milestone 2 will add `await` when the LSP call hierarchy is wired."
    )]
    pub(crate) async fn analyze_impact_impl(
        &self,
        params: AnalyzeImpactParams,
    ) -> Result<Json<AnalyzeImpactResponse>, ErrorData> {
        let start = std::time::Instant::now();

        tracing::info!(
            tool = "analyze_impact",
            semantic_path = %params.semantic_path,
            max_depth = params.max_depth,
            "analyze_impact: start"
        );

        // Parse and validate the semantic path
        let Some(semantic_path) = SemanticPath::parse(&params.semantic_path) else {
            let duration_ms = start.elapsed().as_millis();
            tracing::warn!(
                tool = "analyze_impact",
                error_code = "INVALID_TARGET",
                duration_ms,
                "invalid semantic path"
            );
            return Err(io_error_data("invalid semantic path format".to_string()));
        };

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

        let duration_ms = start.elapsed().as_millis();

        // Milestone 1: LSP call hierarchy not yet implemented.
        // Return empty degraded results immediately (no I/O needed).
        tracing::info!(
            tool = "analyze_impact",
            semantic_path = %params.semantic_path,
            duration_ms,
            degraded = true,
            degraded_reason = "no_lsp",
            engines_used = ?["none"],
            "analyze_impact: complete (degraded)"
        );

        Ok(Json(AnalyzeImpactResponse {
            incoming: vec![],
            outgoing: vec![],
            depth_reached: 0,
            files_referenced: 0,
            degraded: Some(true),
            degraded_reason: Some("no_lsp".to_owned()),
        }))
    }
}

// ── Helper: build an ImpactReference (used when LSP is available in future) ─

#[expect(
    dead_code,
    reason = "Will be used when LSP call hierarchy is wired in Milestone 2."
)]
fn build_impact_reference(
    semantic_path: String,
    file: String,
    line: usize,
    snippet: String,
    version_hash: String,
) -> ImpactReference {
    ImpactReference {
        semantic_path,
        file,
        line,
        snippet,
        version_hash,
    }
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
    use pathfinder_common::types::{SymbolScope, VersionHash, WorkspaceRoot};
    use pathfinder_lsp::{DefinitionLocation, MockLawyer};
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::mock::MockSurgeon;
    use std::sync::Arc;
    use tempfile::tempdir;

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
            version_hash: VersionHash::compute(b"fn login() { }"),
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
        let val = result.expect("should succeed").0;

        assert_eq!(val.file, "src/auth.rs");
        assert_eq!(val.line, 42);
        assert_eq!(val.preview, "pub fn login() -> bool {");
        assert!(val.degraded.is_none());
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
        let surgeon = Arc::new(MockSurgeon::new());
        let lawyer = Arc::new(MockLawyer::default());
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = GetDefinitionParams {
            semantic_path: String::new(), // empty is truly invalid
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
    async fn test_read_with_deep_context_returns_scope_with_degraded() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = ReadWithDeepContextParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };
        let result = server.read_with_deep_context_impl(params).await;
        let val = result.expect("should succeed").0;

        assert_eq!(val.content, "fn login() { }");
        assert_eq!(val.degraded, Some(true));
        assert_eq!(val.degraded_reason.as_deref(), Some("no_lsp"));
        assert!(val.dependencies.is_empty());
    }

    // ── analyze_impact ────────────────────────────────────────────────

    #[tokio::test]
    async fn test_analyze_impact_returns_empty_degraded() {
        let surgeon = Arc::new(MockSurgeon::new());
        let lawyer = Arc::new(MockLawyer::default());
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = AnalyzeImpactParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 2,
        };
        let result = server.analyze_impact_impl(params).await;
        let val = result.expect("should succeed").0;

        assert!(val.incoming.is_empty());
        assert!(val.outgoing.is_empty());
        assert_eq!(val.degraded, Some(true));
        assert_eq!(val.degraded_reason.as_deref(), Some("no_lsp"));
    }
}
