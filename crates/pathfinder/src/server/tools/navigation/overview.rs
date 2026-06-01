//! Symbol overview tool handler: `symbol_overview`.
//!
//! Composite tool that returns source + callers/callees + references in one call.
//! Orchestrates `read_symbol_scope` + `analyze_impact` + `find_all_references`.

use crate::server::helpers::{
    format_degraded_notice, parse_semantic_path, pathfinder_to_error_data,
    require_symbol_target, serialize_metadata,
};
use crate::server::PathfinderServer;
use pathfinder_common::types::DegradedReason;
use rmcp::model::ErrorData;

impl PathfinderServer {
    /// Composite tool: returns source + callers/callees + references in one call.
    ///
    /// Orchestrates `read_symbol_scope` + `analyze_impact` + `find_all_references`.
    /// Uses depth=2 and capped references for bounded responses.
    #[allow(clippy::too_many_lines)]
    pub(crate) async fn symbol_overview_impl(
        &self,
        params: crate::server::types::SymbolOverviewParams,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        let start = std::time::Instant::now();

        tracing::info!(
            tool = "symbol_overview",
            semantic_path = %params.semantic_path,
            "symbol_overview: start"
        );

        let semantic_path = parse_semantic_path(&params.semantic_path)?;
        require_symbol_target(&semantic_path, &params.semantic_path)?;

        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            return Err(pathfinder_to_error_data(&e));
        }

        // Early file existence check
        let abs_file = self.workspace_root.path().join(&semantic_path.file_path);
        if !abs_file.exists() {
            let err = pathfinder_common::error::PathfinderError::FileNotFound {
                path: abs_file.clone(),
            };
            return Err(pathfinder_to_error_data(&err));
        }

        let scope = self
            .read_symbol_scope_enriched(&semantic_path, &params.semantic_path)
            .await?;

        let source = Some(crate::server::types::SymbolSource {
            content: scope.content.clone(),
            start_line: scope.start_line,
            end_line: scope.end_line,
            language: scope.language.clone(),
        });

        let file_path = self.workspace_root.path().join(&semantic_path.file_path);
        let file_content = match tokio::fs::read_to_string(&file_path).await {
            Ok(content) => content,
            Err(e) => {
                tracing::warn!(
                    tool = "symbol_overview",
                    path = %file_path.display(),
                    error = %e,
                    "file read failed — LSP will receive empty content"
                );
                String::new()
            }
        };
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
                    tool = "symbol_overview",
                    semantic_path = %semantic_path,
                    error = %e,
                    "open_document failed — LSP queries may return degraded results"
                );
                None
            }
        };

        let impact_params = crate::server::types::AnalyzeImpactParams {
            semantic_path: params.semantic_path.clone(),
            max_depth: 2,
            max_references: params.max_callers_callees,
            project_only: params.project_only,
            include_test_coverage: false,
        };

        let impact_result = self.analyze_impact_impl(impact_params).await;

        let (impact, impact_degraded, impact_reason) = match impact_result {
            Ok(result) => {
                let raw = result.structured_content.unwrap_or_default();
                let meta: crate::server::types::AnalyzeImpactMetadata =
                    serde_json::from_value(raw).unwrap_or_else(|e| {
                        debug_assert!(false, "analyze_impact metadata deserialization failed: {e}");
                        tracing::warn!(
                            error = %e,
                            "symbol_overview: analyze_impact metadata deserialization failed — using default"
                        );
                        crate::server::types::AnalyzeImpactMetadata::default()
                    });
                let summary = if meta.incoming.is_none() && meta.outgoing.is_none() {
                    None
                } else {
                    Some(crate::server::types::ImpactSummary {
                        incoming: meta.incoming.map(|incoming| {
                            incoming
                                .into_iter()
                                .map(|r| crate::server::types::SymbolOverviewImpactEntry {
                                    semantic_path: r.semantic_path,
                                    file: r.file,
                                    line: r.line,
                                    snippet: r.snippet,
                                    direction: r.direction,
                                })
                                .collect()
                        }),
                        outgoing: meta.outgoing.map(|outgoing| {
                            outgoing
                                .into_iter()
                                .map(|r| crate::server::types::SymbolOverviewImpactEntry {
                                    semantic_path: r.semantic_path,
                                    file: r.file,
                                    line: r.line,
                                    snippet: r.snippet,
                                    direction: r.direction,
                                })
                                .collect()
                        }),
                        degraded: meta.degraded,
                    })
                };
                (summary, meta.degraded, meta.degraded_reason)
            }
            Err(_) => (None, true, Some(DegradedReason::LspErrorGrepFallback)),
        };

        let refs_params = crate::server::types::FindAllReferencesParams {
            semantic_path: params.semantic_path.clone(),
            max_results: 50,
            offset: 0,
        };

        let refs_result = self.find_all_references_impl(refs_params).await;

        let (references, refs_degraded, refs_reason, files_referenced, warm_start_in_progress) = match refs_result {
            Ok(result) => {
                let raw = result.structured_content.unwrap_or_default();
                let meta: crate::server::types::FindAllReferencesMetadata =
                    serde_json::from_value(raw).unwrap_or_else(|e| {
                        debug_assert!(false, "find_all_references metadata deserialization failed: {e}");
                        tracing::warn!(
                            error = %e,
                            "symbol_overview: find_all_references metadata deserialization failed — using default"
                        );
                        crate::server::types::FindAllReferencesMetadata::default()
                    });
                let refs = meta.references.map(|refs| {
                    refs.into_iter()
                        .map(|r| crate::server::types::SymbolOverviewReference {
                            file: r.file,
                            line: r.line,
                            column: r.column,
                            snippet: r.snippet,
                        })
                        .collect()
                });
                let warm_start_in_progress = meta.warm_start_in_progress;
                (
                    refs,
                    meta.degraded,
                    meta.degraded_reason,
                    meta.files_referenced,
                    warm_start_in_progress,
                )
            }
            Err(_) => (None, true, Some(DegradedReason::LspErrorGrepFallback), 0, None),
        };

        let duration_ms = start.elapsed().as_millis();

        let degraded = impact_degraded || refs_degraded;
        let degraded_reason = if impact_degraded {
            impact_reason
        } else if refs_degraded {
            refs_reason
        } else {
            None
        };

        let lsp_readiness = if degraded {
            match degraded_reason {
                Some(DegradedReason::NoLsp) => Some("unavailable".to_owned()),
                _ => Some("warming_up".to_owned()),
            }
        } else {
            Some("ready".to_owned())
        };

        let response = crate::server::types::SymbolOverviewResponse {
            source,
            impact: impact.clone(),
            references: references.clone(),
            files_referenced,
            degraded,
            degraded_reason,
            actionable_guidance: degraded_reason.as_ref().map(DegradedReason::guidance),
            lsp_readiness,
            warm_start_in_progress,
        };

        let source_block = format!(
            "SYMBOL: {} ({} lines)\n",
            params.semantic_path,
            scope.end_line - scope.start_line
        );

        let impact_block = if let Some(ref imp) = impact {
            let inc = imp.incoming.as_ref().map_or(0, Vec::len);
            let out = imp.outgoing.as_ref().map_or(0, Vec::len);
            let deg = if imp.degraded { " (degraded)" } else { "" };
            format!("CALLERS: {inc} direct{deg}\nCALLEES: {out}{deg}\n")
        } else {
            "CALLERS: unavailable\nCALLEES: unavailable\n".to_owned()
        };

        let refs_block = if let Some(ref refs) = references {
            let total = refs.len();
            format!("REFERENCES: {total} total across {files_referenced} files\n")
        } else {
            "REFERENCES: unavailable\n".to_owned()
        };

        let degraded_block = if degraded {
            let notice = degraded_reason
                .as_ref()
                .map_or_else(|| "DEGRADED (unknown)".to_owned(), format_degraded_notice);
            format!("{notice}\n")
        } else {
            "DEGRADED: no (LSP-backed, authoritative)\n".to_owned()
        };

        let extra = if degraded { "\n" } else { "" };
        let text = format!(
            "{source_block}{impact_block}{refs_block}{degraded_block}{extra}[completed in {duration_ms}ms]"
        );

        let mut result =
            rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(text)]);
        result.structured_content = serialize_metadata(&response);
        Ok(result)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use super::super::test_helpers::{make_scope, make_server_with_lawyer, make_temp_workspace};
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::{DegradedReason, WorkspaceRoot};
    use pathfinder_lsp::types::{CallHierarchyCall, CallHierarchyItem, ReferenceLocation};
    use pathfinder_lsp::{LspError, MockLawyer};
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::mock::MockSurgeon;
    use std::sync::Arc;

    // ── symbol_overview ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_symbol_overview_aggregates_callers_callees_references() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .extend([Ok(make_scope()), Ok(make_scope()), Ok(make_scope())]);

        let lawyer = Arc::new(MockLawyer::default());

        // Configure analyze_impact
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

        // Configure find_all_references
        lawyer.set_references_result(Ok(vec![
            ReferenceLocation {
                file: "src/main.rs".into(),
                line: 10,
                column: 8,
                snippet: "login();".into(),
            },
            ReferenceLocation {
                file: "src/tests.rs".into(),
                line: 5,
                column: 4,
                snippet: "let _ = login();".into(),
            },
        ]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = crate::server::types::SymbolOverviewParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            project_only: Some(true),
            max_callers_callees: 50,
            max_references: 50,
        };

        let result = server.symbol_overview_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::SymbolOverviewResponse =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // Verify source
        assert!(val.source.is_some());
        let source = val.source.as_ref().unwrap();
        assert_eq!(source.content, "fn login() { }");
        assert_eq!(source.start_line, 9);
        assert_eq!(source.end_line, 9);

        // Verify impact
        assert!(val.impact.is_some());
        let impact = val.impact.as_ref().unwrap();
        assert!(impact.incoming.is_some());
        assert!(impact.outgoing.is_some());
        assert_eq!(impact.incoming.as_ref().unwrap().len(), 1);
        assert_eq!(impact.outgoing.as_ref().unwrap().len(), 1);

        // Verify references
        assert!(val.references.is_some());
        let refs = val.references.as_ref().unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].file, "src/main.rs");
        assert_eq!(refs[1].file, "src/tests.rs");

        // Verify not degraded
        assert!(!val.degraded);
        assert!(val.degraded_reason.is_none());
        assert_eq!(val.lsp_readiness, Some("ready".to_owned()));
    }

    #[tokio::test]
    async fn test_symbol_overview_no_impact_no_references_shows_unavailable() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .extend([Ok(make_scope()), Ok(make_scope()), Ok(make_scope())]);

        let lawyer = Arc::new(MockLawyer::default());

        // Configure empty impact (no items, no errors)
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

        lawyer.push_incoming_call_result(Ok(vec![])); // No incoming
        lawyer.push_outgoing_call_result(Ok(vec![])); // No outgoing

        // Configure empty references
        lawyer.set_references_result(Ok(vec![]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = crate::server::types::SymbolOverviewParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            project_only: Some(true),
            max_callers_callees: 50,
            max_references: 50,
        };

        let result = server.symbol_overview_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::SymbolOverviewResponse =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // Verify impact shows empty arrays (prepare succeeded but BFS found nothing)
        assert!(val.impact.is_some());
        let impact = val.impact.as_ref().unwrap();
        assert!(impact.incoming.is_some());
        assert!(impact.outgoing.is_some());
        assert_eq!(impact.incoming.as_ref().unwrap().len(), 0);
        assert_eq!(impact.outgoing.as_ref().unwrap().len(), 0);

        // Verify references shows 0 files
        assert!(val.references.is_some());
        let refs = val.references.as_ref().unwrap();
        assert_eq!(refs.len(), 0);
        assert_eq!(val.files_referenced, 0);

        // Not degraded, just empty results
        assert!(!val.degraded);
        assert_eq!(val.lsp_readiness, Some("ready".to_owned()));
    }

    #[tokio::test]
    async fn test_symbol_overview_with_references_only() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .extend([Ok(make_scope()), Ok(make_scope()), Ok(make_scope())]);

        let lawyer = Arc::new(MockLawyer::default());

        // Configure references
        lawyer.set_references_result(Ok(vec![
            ReferenceLocation {
                file: "src/main.rs".into(),
                line: 10,
                column: 8,
                snippet: "login();".into(),
            },
            ReferenceLocation {
                file: "src/auth.rs".into(),
                line: 5,
                column: 4,
                snippet: "fn login() {".into(),
            },
        ]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = crate::server::types::SymbolOverviewParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            project_only: Some(true),
            max_callers_callees: 50,
            max_references: 50,
        };

        let result = server.symbol_overview_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::SymbolOverviewResponse =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // Verify references aggregated
        assert!(val.references.is_some());
        let refs = val.references.as_ref().unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(val.files_referenced, 2);
    }

    #[tokio::test]
    async fn test_symbol_overview_degraded_when_lsp_unavailable() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .extend([Ok(make_scope()), Ok(make_scope()), Ok(make_scope())]);

        // Use NoOpLawyer to simulate LSP unavailable
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

        let params = crate::server::types::SymbolOverviewParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            project_only: Some(true),
            max_callers_callees: 50,
            max_references: 50,
        };

        let result = server.symbol_overview_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::SymbolOverviewResponse =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // Verify degraded
        assert!(val.degraded);
        assert_eq!(val.degraded_reason, Some(DegradedReason::NoLsp));
        assert_eq!(val.lsp_readiness, Some("unavailable".to_owned()));

        // Verify impact unavailable
        assert!(val.impact.is_none());

        // Verify references unavailable
        assert!(val.references.is_none());
    }

    #[tokio::test]
    async fn test_symbol_overview_lsp_error_references_degraded() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .extend([Ok(make_scope()), Ok(make_scope()), Ok(make_scope())]);

        let lawyer = Arc::new(MockLawyer::default());

        // Configure LSP error for references - this tests line 3061 Err(_) branch
        // Also configure a valid prepare result so impact is not degraded
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
        lawyer.set_references_lsp_error(Err(LspError::Timeout {
            operation: "references".to_string(),
            timeout_ms: 10000,
        }));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = crate::server::types::SymbolOverviewParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            project_only: Some(true),
            max_callers_callees: 50,
            max_references: 50,
        };

        let result = server.symbol_overview_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::SymbolOverviewResponse =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // Verify degraded on LSP error in find_all_references_impl
        assert!(val.degraded);
        assert_eq!(val.degraded_reason, Some(DegradedReason::LspTimeoutGrepFallback));
        assert_eq!(val.lsp_readiness, Some("warming_up".to_owned()));
        assert_eq!(val.warm_start_in_progress, Some(true));

        // References unavailable due to degradation
        assert!(val.references.is_none());
        assert_eq!(val.files_referenced, 0);
    }

    #[tokio::test]
    async fn test_symbol_overview_bfs_error_logs_warning_continues_with_empty_results() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .extend([Ok(make_scope()), Ok(make_scope()), Ok(make_scope())]);

        let lawyer = Arc::new(MockLawyer::default());

        // Configure LSP error for impact
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
        lawyer.push_incoming_call_result(Err(LspError::Protocol(
            "LSP call hierarchy error".to_string(),
        )));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = crate::server::types::SymbolOverviewParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            project_only: Some(true),
            max_callers_callees: 50,
            max_references: 50,
        };

        let result = server.symbol_overview_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::SymbolOverviewResponse =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // Verify NOT degraded (LSP error in BFS is logged but doesn't set overall degraded flag)
        assert!(!val.degraded);
        assert_eq!(val.degraded_reason, None);
        assert_eq!(val.lsp_readiness, Some("ready".to_owned()));

        // Impact is populated with empty arrays (prepare succeeded)
        assert!(val.impact.is_some());
        let impact = val.impact.as_ref().unwrap();
        assert!(impact.incoming.is_some());
        assert!(impact.outgoing.is_some());
    }

    #[tokio::test]
    async fn test_symbol_overview_partial_degradation_treesitter_fails_refs_ok() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .extend([Ok(make_scope()), Ok(make_scope()), Ok(make_scope())]);

        let lawyer = Arc::new(MockLawyer::default());

        // Configure references to succeed
        lawyer.set_references_result(Ok(vec![ReferenceLocation {
            file: "src/main.rs".into(),
            line: 10,
            column: 8,
            snippet: "login();".into(),
        }]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = crate::server::types::SymbolOverviewParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            project_only: Some(true),
            max_callers_callees: 50,
            max_references: 50,
        };

        let result = server.symbol_overview_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::SymbolOverviewResponse =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // Verify degraded (impact failed due to LSP not providing items)
        assert!(val.degraded);
        // Degraded reason is LspWarmupEmptyUnverified (prepare returned empty, goto_definition returned None)
        assert_eq!(val.degraded_reason, Some(DegradedReason::LspWarmupEmptyUnverified));

        // Impact unavailable due to degradation
        assert!(val.impact.is_none());

        // References available (partial degradation)
        assert!(val.references.is_some());
        let refs = val.references.as_ref().unwrap();
        assert_eq!(refs.len(), 1);
    }

    #[tokio::test]
    async fn test_symbol_overview_rejects_empty_semantic_path() {
        let surgeon = Arc::new(MockSurgeon::new());

        let lawyer = Arc::new(MockLawyer::default());
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = crate::server::types::SymbolOverviewParams {
            semantic_path: "".to_owned(),
            project_only: Some(true),
            max_callers_callees: 50,
            max_references: 50,
        };

        let result = server.symbol_overview_impl(params).await;
        assert!(result.is_err(), "should reject empty semantic path");
    }

    #[tokio::test]
    async fn test_symbol_overview_file_not_found_returns_error() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
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

        let params = crate::server::types::SymbolOverviewParams {
            semantic_path: "nonexistent/path.rs::function".to_owned(),
            project_only: Some(true),
            max_callers_callees: 50,
            max_references: 50,
        };

        let result = server.symbol_overview_impl(params).await;
        assert!(result.is_err(), "should return error for nonexistent file");
    }

    #[tokio::test]
    async fn test_symbol_overview_respects_max_callers_callees_limit() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .extend([Ok(make_scope()), Ok(make_scope()), Ok(make_scope())]);

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

        // Configure 5 incoming calls
        let incoming: Vec<_> = (0..5)
            .map(|i| CallHierarchyCall {
                item: CallHierarchyItem {
                    name: format!("caller{i}"),
                    kind: "function".into(),
                    detail: Some(format!("fn caller{i}()")),
                    file: format!("src/caller{i}.rs"),
                    line: (i + 1) as u32,
                    column: 4,
                    data: None,
                },
                call_sites: vec![(i + 10) as u32],
            })
            .collect();
        lawyer.push_incoming_call_result(Ok(incoming));

        lawyer.push_outgoing_call_result(Ok(vec![]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = crate::server::types::SymbolOverviewParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            project_only: Some(true),
            max_callers_callees: 3, // Limit to 3
            max_references: 50,
        };

        let result = server.symbol_overview_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::SymbolOverviewResponse =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // Verify max_callers_callees limit respected
        assert!(val.impact.is_some());
        let impact = val.impact.as_ref().unwrap();
        assert!(impact.incoming.is_some());
        let incoming = impact.incoming.as_ref().unwrap();
        assert!(
            incoming.len() <= 3,
            "should respect max_callers_callees limit, got {}",
            incoming.len()
        );
    }
}
