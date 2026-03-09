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
    ReadWithDeepContextParams, ReadWithDeepContextResponse,
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
    #[expect(
        clippy::too_many_lines,
        reason = "Sequential pipeline (parse→sandbox→tree-sitter→LSP→fallback branches)."
    )]
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

        let lsp_start = std::time::Instant::now();
        let mut dependencies = Vec::new();
        let mut degraded = true;
        let mut degraded_reason = Some("no_lsp".to_owned());
        let mut engines = vec!["tree-sitter"];

        let lsp_result = self
            .lawyer
            .call_hierarchy_prepare(
                self.workspace_root.path(),
                &semantic_path.file_path,
                u32::try_from(scope.start_line + 1).unwrap_or(1),
                1, // Column 1
            )
            .await;

        match lsp_result {
            Ok(items) if !items.is_empty() => {
                let item = &items[0];
                match self
                    .lawyer
                    .call_hierarchy_outgoing(self.workspace_root.path(), item)
                    .await
                {
                    Ok(outgoing) => {
                        engines.push("lsp");
                        for call in outgoing {
                            let callee = call.item;
                            let signature =
                                callee.detail.clone().unwrap_or_else(|| callee.name.clone());
                            let sp = format!("{}::{}", callee.file, callee.name);
                            dependencies.push(crate::server::types::DeepContextDependency {
                                semantic_path: sp,
                                signature,
                                file: callee.file,
                                line: callee.line as usize,
                            });
                        }
                        degraded = false;
                        degraded_reason = None;
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
            Ok(_) => {
                // Empty prepare result, LSP is available and attempted.
                engines.push("lsp");
                degraded = false;
                degraded_reason = None;
            }
            Err(LspError::NoLspAvailable | LspError::UnsupportedCapability { .. }) => {
                // Keep degraded default
            }
            Err(e) => {
                tracing::warn!(
                    tool = "read_with_deep_context",
                    error = %e,
                    "call_hierarchy_prepare failed"
                );
            }
        }

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

        Ok(Json(ReadWithDeepContextResponse {
            content: scope.content,
            start_line: scope.start_line,
            end_line: scope.end_line,
            version_hash: scope.version_hash.to_string(),
            language: scope.language,
            dependencies,
            degraded: if degraded { Some(true) } else { None },
            degraded_reason,
        }))
    }

    /// Core logic for the `analyze_impact` tool.
    ///
    /// Returns callers (incoming) and callees (outgoing) for the target symbol.
    /// Degrades gracefully to empty results when no LSP is configured.
    #[expect(
        clippy::too_many_lines,
        reason = "Two full BFS loops (incoming/outgoing) make this long but straightforward."
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

        let lsp_start = std::time::Instant::now();
        let mut incoming = Vec::new();
        let mut outgoing = Vec::new();
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
                1, // Column 1
            )
            .await;

        match lsp_result {
            Ok(items) if !items.is_empty() => {
                engines.push("lsp");
                degraded = false;
                degraded_reason = None;

                let initial_item = &items[0];

                // --- INCOMING BFS ---
                let mut queue = std::collections::VecDeque::new();
                queue.push_back((initial_item.clone(), 0));
                let mut seen = std::collections::HashSet::new();
                seen.insert((initial_item.file.clone(), initial_item.line));
                files_referenced.insert(initial_item.file.clone());

                while let Some((item, current_depth)) = queue.pop_front() {
                    max_depth_reached = std::cmp::max(max_depth_reached, current_depth);
                    if current_depth >= params.max_depth {
                        continue;
                    }

                    if let Ok(calls) = self
                        .lawyer
                        .call_hierarchy_incoming(self.workspace_root.path(), &item)
                        .await
                    {
                        for call in calls {
                            let caller = call.item;
                            files_referenced.insert(caller.file.clone());

                            let key = (caller.file.clone(), caller.line);
                            if !seen.contains(&key) {
                                seen.insert(key);
                                queue.push_back((caller.clone(), current_depth + 1));

                                incoming.push(crate::server::types::ImpactReference {
                                    semantic_path: format!("{}::{}", caller.file, caller.name),
                                    file: caller.file.clone(),
                                    line: caller.line as usize,
                                    snippet: caller.detail.unwrap_or_else(|| caller.name.clone()),
                                    version_hash: String::new(), // Populated at higher layer if needed
                                });
                            }
                        }
                    }
                }

                // --- OUTGOING BFS ---
                let mut queue_out = std::collections::VecDeque::new();
                queue_out.push_back((initial_item.clone(), 0));
                let mut seen_out = std::collections::HashSet::new();
                seen_out.insert((initial_item.file.clone(), initial_item.line));

                while let Some((item, current_depth)) = queue_out.pop_front() {
                    max_depth_reached = std::cmp::max(max_depth_reached, current_depth);
                    if current_depth >= params.max_depth {
                        continue;
                    }

                    if let Ok(calls) = self
                        .lawyer
                        .call_hierarchy_outgoing(self.workspace_root.path(), &item)
                        .await
                    {
                        for call in calls {
                            let callee = call.item;
                            files_referenced.insert(callee.file.clone());

                            let key = (callee.file.clone(), callee.line);
                            if !seen_out.contains(&key) {
                                seen_out.insert(key);
                                queue_out.push_back((callee.clone(), current_depth + 1));

                                outgoing.push(crate::server::types::ImpactReference {
                                    semantic_path: format!("{}::{}", callee.file, callee.name),
                                    file: callee.file.clone(),
                                    line: callee.line as usize,
                                    snippet: callee.detail.unwrap_or_else(|| callee.name.clone()),
                                    version_hash: String::new(), // Populated at higher layer if needed
                                });
                            }
                        }
                    }
                }
            }
            Ok(_) => {
                engines.push("lsp");
                degraded = false;
                degraded_reason = None;
            }
            Err(LspError::NoLspAvailable | LspError::UnsupportedCapability { .. }) => {
                // Keep degraded default
            }
            Err(e) => {
                tracing::warn!(
                    tool = "analyze_impact",
                    error = %e,
                    "call_hierarchy_prepare failed"
                );
            }
        }

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

        Ok(Json(AnalyzeImpactResponse {
            incoming,
            outgoing,
            depth_reached: max_depth_reached,
            files_referenced: files_referenced.len(),
            degraded: if degraded { Some(true) } else { None },
            degraded_reason,
        }))
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
    use pathfinder_lsp::types::{CallHierarchyCall, CallHierarchyItem};
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
        let val = result.expect("should succeed").0;

        assert_eq!(val.content, "fn login() { }");
        assert_eq!(val.degraded, Some(true));
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
        let val = result.expect("should succeed").0;

        assert_eq!(val.content, "fn login() { }");
        assert_eq!(val.degraded, None);
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
        let val = result.expect("should succeed").0;

        assert!(val.incoming.is_empty());
        assert!(val.outgoing.is_empty());
        assert_eq!(val.degraded, Some(true));
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
        let val = result.expect("should succeed").0;

        assert_eq!(val.degraded, None);
        assert_eq!(val.degraded_reason, None);
        assert_eq!(val.depth_reached, 1); // BFS pops level 1, updates max_depth_reached, then continues
        assert_eq!(val.files_referenced, 3); // initial + caller + callee
        assert_eq!(val.incoming.len(), 1);
        assert_eq!(val.incoming[0].file, "src/server.rs");
        assert_eq!(val.outgoing.len(), 1);
        assert_eq!(val.outgoing[0].file, "src/token.rs");
    }
}
