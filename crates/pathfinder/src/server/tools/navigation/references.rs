//! `find_all_references` tool implementation.
//!
//! Finds all usages of a symbol across the codebase using LSP
//! `textDocument/references` with optional `textDocument/implementation`.
//! Supports pagination and degrades gracefully when LSP is unavailable.

use crate::server::helpers::{
    format_degraded_notice, millis_to_u64, parse_semantic_path, pathfinder_to_error_data,
    require_symbol_target, serialize_metadata,
};
use crate::server::PathfinderServer;
use pathfinder_common::types::DegradedReason;
use pathfinder_lsp::LspError;
use rmcp::model::ErrorData;

impl PathfinderServer {
    /// Find all references to a symbol across the entire codebase.
    ///
    /// Uses the LSP `textDocument/references` capability to find all usages of
    /// a given symbol. Unlike `analyze_impact`, this returns all references
    /// including those not in the call hierarchy (e.g., field accesses, imports).
    #[allow(clippy::too_many_lines)]
    #[tracing::instrument(skip(self, params))]
    pub(crate) async fn find_all_references_impl(
        &self,
        params: crate::server::types::FindAllReferencesParams,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        let start = std::time::Instant::now();

        tracing::info!(
            tool = "find_all_references",
            semantic_path = %params.semantic_path,
            "find_all_references: start"
        );

        let semantic_path = parse_semantic_path(&params.semantic_path)?;
        require_symbol_target(&semantic_path, &params.semantic_path)?;

        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            let duration_ms = start.elapsed().as_millis();
            tracing::warn!(
                tool = "find_all_references",
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
                tool = "find_all_references",
                path = %abs_file.display(),
                "file not found"
            );
            return Err(pathfinder_to_error_data(&err));
        }

        let ts_start = std::time::Instant::now();
        let symbol_scope = self
            .read_symbol_scope_enriched(&semantic_path, &params.semantic_path)
            .await?;
        let tree_sitter_ms = ts_start.elapsed().as_millis();

        let file_path = self.workspace_root.path().join(&semantic_path.file_path);
        let file_content = match tokio::fs::read_to_string(&file_path).await {
            Ok(content) => content,
            Err(e) => {
                tracing::warn!(
                    tool = "find_all_references",
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
                    tool = "find_all_references",
                    semantic_path = %semantic_path,
                    error = %e,
                    "open_document failed — LSP queries may return degraded results"
                );
                None
            }
        };

        let lsp_start = std::time::Instant::now();
        let lsp_result = self
            .lawyer
            .references(
                self.workspace_root.path(),
                &semantic_path.file_path,
                u32::try_from(symbol_scope.start_line + 1).unwrap_or(1),
                u32::try_from(symbol_scope.name_column + 1).unwrap_or(1),
            )
            .await;

        let implementations_result = self
            .lawyer
            .goto_implementation(
                self.workspace_root.path(),
                &semantic_path.file_path,
                u32::try_from(symbol_scope.start_line + 1).unwrap_or(1),
                u32::try_from(symbol_scope.name_column + 1).unwrap_or(1),
            )
            .await;
        let lsp_ms = lsp_start.elapsed().as_millis();

        let duration_ms = start.elapsed().as_millis();

        match lsp_result {
            Ok(locations) => {
                let implementations: Vec<crate::server::types::ReferenceLocation> =
                    match implementations_result {
                        Ok(impls) => impls
                            .into_iter()
                            .map(|def| crate::server::types::ReferenceLocation {
                                file: def.file,
                                line: def.line,
                                column: def.column,
                                snippet: def.preview,
                            })
                            .collect(),
                        Err(e) => {
                            tracing::warn!(
                                tool = "find_all_references",
                                error = %e,
                                "goto_implementation failed — returning references only"
                            );
                            vec![]
                        }
                    };

                let all_files = locations
                    .iter()
                    .map(|l| l.file.as_str())
                    .chain(implementations.iter().map(|i| i.file.as_str()))
                    .collect::<std::collections::HashSet<_>>();
                let files_referenced = all_files.len();

                let references: Vec<crate::server::types::ReferenceLocation> = locations
                    .into_iter()
                    .map(|l| crate::server::types::ReferenceLocation {
                        file: l.file,
                        line: l.line,
                        column: l.column,
                        snippet: l.snippet,
                    })
                    .collect();

                // Dedup references that also appear in implementations by (file, line, column).
                let impl_keys: std::collections::HashSet<(String, u32, u32)> = implementations
                    .iter()
                    .map(|i| (i.file.clone(), i.line, i.column))
                    .collect();
                let references: Vec<crate::server::types::ReferenceLocation> = references
                    .into_iter()
                    .filter(|r| !impl_keys.contains(&(r.file.clone(), r.line, r.column)))
                    .collect();

                // Spec 4.4: Apply pagination to each list separately
                let total_references = references.len() + implementations.len();
                let offset = usize::try_from(params.offset).unwrap_or(0);
                // Item 4: Guard against max_results=0 which causes infinite pagination loops.
                let max_results = usize::try_from(params.max_results).unwrap_or(50).max(1);
                let truncated = total_references > offset.saturating_add(max_results);

                // Paginate implementations first, then references (matches display order)
                let impl_count = implementations.len();
                let ref_count = references.len();

                let (paginated_impls, paginated_refs) = if offset >= impl_count {
                    // Past implementations — paginate references only
                    let ref_offset = offset - impl_count;
                    (
                        Vec::new(),
                        references
                            .into_iter()
                            .skip(ref_offset)
                            .take(max_results)
                            .collect::<Vec<_>>(),
                    )
                } else {
                    // Some or all implementations in range
                    let impl_slice: Vec<_> = implementations
                        .into_iter()
                        .skip(offset)
                        .take(max_results)
                        .collect();
                    let remaining = max_results - impl_slice.len();
                    let ref_slice: Vec<_> = references.into_iter().take(remaining).collect();
                    (impl_slice, ref_slice)
                };

                tracing::info!(
                    tool = "find_all_references",
                    references_count = ref_count,
                    implementations_count = impl_count,
                    files_referenced,
                    tree_sitter_ms,
                    lsp_ms,
                    duration_ms,
                    engines_used = ?["tree-sitter", "lsp"],
                    "find_all_references: complete"
                );

                // Build text output before moving vectors into paginated
                let implementations_text = if paginated_impls.is_empty() {
                    String::new()
                } else {
                    let header =
                        format!("Implementations (extends/implements): {impl_count} found\n");
                    let items: Vec<_> = paginated_impls
                        .iter()
                        .map(|imp| {
                            format!("{}:{}:{}: {}", imp.file, imp.line, imp.column, imp.snippet)
                        })
                        .collect();
                    format!("{}{}\n", header, items.join("\n"))
                };

                let references_text = if paginated_refs.is_empty() {
                    String::new()
                } else {
                    let header = format!("References: {ref_count} found\n");
                    let items: Vec<_> = paginated_refs
                        .iter()
                        .map(|r| format!("{}:{}:{}: {}", r.file, r.line, r.column, r.snippet))
                        .collect();
                    format!("{}{}", header, items.join("\n"))
                };

                let paginated_len = paginated_impls.len() + paginated_refs.len();
                let mut paginated = Vec::with_capacity(paginated_len);
                paginated.extend(paginated_impls);
                paginated.extend(paginated_refs);

                let pagination_note = if truncated {
                    format!(
                        "\n[showing {} of {} total — use offset={} for next page]\n",
                        paginated_len,
                        total_references,
                        offset.saturating_add(max_results),
                    )
                } else {
                    String::new()
                };

                let summary = if impl_count > 0 && ref_count > 0 {
                    format!(
                        "Found {ref_count} references + {impl_count} implementations across {files_referenced} files.\n\n"
                    )
                } else if impl_count > 0 {
                    format!(
                        "Found {impl_count} implementations across {files_referenced} files.\n\n"
                    )
                } else if ref_count > 0 {
                    format!("Found {ref_count} references across {files_referenced} files.\n\n")
                } else {
                    "LSP confirmed: zero references or implementations for this symbol.\n"
                        .to_string()
                };

                let metadata = crate::server::types::FindAllReferencesMetadata {
                    references: Some(paginated),
                    total_references: Some(total_references),
                    truncated,
                    files_referenced,
                    degraded: false,
                    degraded_reason: None,
                    actionable_guidance: None,
                    lsp_readiness: Some("ready".to_owned()),
                    warm_start_in_progress: Some(false),
                    duration_ms: Some(millis_to_u64(duration_ms)),
                    resolution_strategy: Some("lsp_references".to_owned()),
                };

                let mut result =
                    rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(
                        format!("{summary}{implementations_text}{references_text}{pagination_note}\n[completed in {duration_ms}ms]"),
                    )]);
                result.structured_content = serialize_metadata(&metadata);
                Ok(result)
            }
            Err(LspError::NoLspAvailable) => {
                tracing::info!(
                    tool = "find_all_references",
                    semantic_path = %params.semantic_path,
                    tree_sitter_ms,
                    lsp_ms,
                    duration_ms,
                    "find_all_references: no LSP — degraded"
                );

                let metadata = crate::server::types::FindAllReferencesMetadata {
                    references: None,
                    total_references: None,
                    truncated: false,
                    files_referenced: 0,
                    degraded: true,
                    degraded_reason: Some(DegradedReason::NoLsp),
                    actionable_guidance: Some(DegradedReason::NoLsp.guidance()),
                    lsp_readiness: Some("unavailable".to_owned()),
                    warm_start_in_progress: None,
                    duration_ms: Some(millis_to_u64(duration_ms)),
                    resolution_strategy: Some("treesitter_fallback".to_owned()),
                };

                let mut result =
                    rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(
                        format!(
                            "{}\nReferences unknown. Use search_codebase to manually find usages of `{}`\n[completed in {duration_ms}ms]",
                            format_degraded_notice(&DegradedReason::NoLsp),
                            params.semantic_path
                        ),
                    )]);
                result.structured_content = serialize_metadata(&metadata);
                Ok(result)
            }
            Err(e) => {
                tracing::warn!(
                    tool = "find_all_references",
                    error = %e,
                    tree_sitter_ms,
                    lsp_ms,
                    duration_ms,
                    "find_all_references: LSP error"
                );

                let degraded_reason = match &e {
                    LspError::Timeout { .. } => DegradedReason::LspTimeoutGrepFallback,
                    LspError::Protocol(_) | LspError::ConnectionLost => {
                        DegradedReason::LspErrorGrepFallback
                    }
                    _ => DegradedReason::LspErrorGrepFallback,
                };

                let is_timeout = matches!(&e, LspError::Timeout { .. });
                let lsp_readiness = if is_timeout {
                    "warming_up"
                } else {
                    "unavailable"
                };
                let warm_start_in_progress = if is_timeout { Some(true) } else { None };

                let metadata = crate::server::types::FindAllReferencesMetadata {
                    references: None,
                    total_references: None,
                    truncated: false,
                    files_referenced: 0,
                    degraded: true,
                    degraded_reason: Some(degraded_reason),
                    actionable_guidance: Some(degraded_reason.guidance()),
                    lsp_readiness: Some(lsp_readiness.to_owned()),
                    warm_start_in_progress,
                    duration_ms: Some(millis_to_u64(duration_ms)),
                    resolution_strategy: Some("treesitter_fallback".to_owned()),
                };

                let mut result =
                    rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(
                        format!(
                            "{}\nReferences unknown. Use search_codebase to manually find usages of `{}`\n[completed in {duration_ms}ms]",
                            format_degraded_notice(&degraded_reason),
                            params.semantic_path
                        ),
                    )]);
                result.structured_content = serialize_metadata(&metadata);
                Ok(result)
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::super::test_helpers::{make_scope, make_server_with_lawyer, make_temp_workspace};
    use super::*;
    use crate::server::PathfinderServer;
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::{DegradedReason, WorkspaceRoot};
    use pathfinder_lsp::types::ReferenceLocation;
    use pathfinder_lsp::{DefinitionLocation, MockLawyer};
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::mock::MockSurgeon;
    use std::sync::Arc;

    // ── find_all_references edge cases ──────────────────────────────────

    #[tokio::test]
    async fn test_find_all_references_lsp_returns_references() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        lawyer.set_references_result(Ok(vec![
            ReferenceLocation {
                file: "src/auth.rs".into(),
                line: 10,
                column: 4,
                snippet: "fn login() {".into(),
            },
            ReferenceLocation {
                file: "src/main.rs".into(),
                line: 20,
                column: 8,
                snippet: "login();".into(),
            },
        ]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        let refs = val.references.unwrap_or_default();
        assert_eq!(refs.len(), 2, "should have 2 references");
        assert!(!val.degraded, "should not be degraded when LSP works");
    }

    #[tokio::test]
    async fn test_find_all_references_respects_max_references() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        // Return 5 references
        let refs: Vec<_> = (0..5)
            .map(|i| ReferenceLocation {
                file: format!("src/file{i}.rs"),
                line: u32::try_from(i + 1).unwrap(),
                column: 1,
                snippet: format!("// reference {i}"),
            })
            .collect();
        lawyer.set_references_result(Ok(refs));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 3, // Limit to 3
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        let refs = val.references.unwrap_or_default();
        assert_eq!(
            refs.len(),
            3,
            "should return exactly max_results=3 references, got {}",
            refs.len()
        );
    }

    // ── find_all_references degraded paths ────────────────────────────────

    #[tokio::test]
    async fn test_find_all_references_degraded_when_no_lsp() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

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

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(val.degraded, "should be degraded when LSP unavailable");
        assert_eq!(val.degraded_reason, Some(DegradedReason::NoLsp));
        assert_eq!(val.lsp_readiness, Some("unavailable".to_owned()));
        assert!(val.references.is_none());
    }

    #[tokio::test]
    async fn test_find_all_references_lsp_error_degraded() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        // Simulate LSP protocol error
        lawyer.set_references_result(Err("protocol error".to_string()));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(val.degraded, "should be degraded on LSP error");
        assert_eq!(
            val.degraded_reason,
            Some(DegradedReason::LspErrorGrepFallback)
        );
        assert_eq!(val.lsp_readiness, Some("unavailable".to_owned()));
        assert!(val.references.is_none());
    }

    #[tokio::test]
    async fn test_find_all_references_connection_lost_degraded() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        // ConnectionLost exercises the dedicated LspError::ConnectionLost branch
        lawyer.set_references_lsp_error(Err(LspError::ConnectionLost));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(val.degraded, "should be degraded on connection lost");
        assert_eq!(
            val.degraded_reason,
            Some(DegradedReason::LspErrorGrepFallback)
        );
        assert_eq!(val.lsp_readiness, Some("unavailable".to_owned()));
        assert!(val.references.is_none());
    }

    // ── find_all_references pagination + implementations ────────────────────

    #[tokio::test]
    async fn test_find_all_references_with_implementations_and_references() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

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
                file: "src/tests.rs".into(),
                line: 5,
                column: 4,
                snippet: "let _ = login();".into(),
            },
        ]));

        // Configure implementations
        lawyer.set_goto_implementation_result(Ok(vec![DefinitionLocation {
            file: "src/auth_impl.rs".into(),
            line: 15,
            column: 4,
            preview: "impl LoginService for AuthService {".into(),
        }]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // Should have 3 total (1 implementation + 2 references)
        assert_eq!(val.total_references, Some(3));
        assert_eq!(val.files_referenced, 3);

        let refs = val.references.unwrap_or_default();
        // First should be implementation, then references
        assert_eq!(refs[0].file, "src/auth_impl.rs");
        assert_eq!(refs[1].file, "src/main.rs");
        assert_eq!(refs[2].file, "src/tests.rs");
    }

    #[tokio::test]
    async fn test_find_all_references_offset_skips_implementations() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        // 2 implementations
        lawyer.set_goto_implementation_result(Ok(vec![
            DefinitionLocation {
                file: "src/auth_impl1.rs".into(),
                line: 10,
                column: 4,
                preview: "impl1".into(),
            },
            DefinitionLocation {
                file: "src/auth_impl2.rs".into(),
                line: 20,
                column: 4,
                preview: "impl2".into(),
            },
        ]));

        // 3 references
        lawyer.set_references_result(Ok(vec![
            ReferenceLocation {
                file: "src/main.rs".into(),
                line: 10,
                column: 8,
                snippet: "login1();".into(),
            },
            ReferenceLocation {
                file: "src/tests.rs".into(),
                line: 5,
                column: 4,
                snippet: "login2();".into(),
            },
            ReferenceLocation {
                file: "src/app.rs".into(),
                line: 15,
                column: 8,
                snippet: "login3();".into(),
            },
        ]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // offset=2 skips both implementations
        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 2,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        let refs = val.references.unwrap_or_default();
        assert_eq!(refs.len(), 3, "should return all 3 references");
        assert_eq!(refs[0].file, "src/main.rs");
        assert_eq!(refs[1].file, "src/tests.rs");
        assert_eq!(refs[2].file, "src/app.rs");
        assert_eq!(val.total_references, Some(5)); // 2 impls + 3 refs
    }

    #[tokio::test]
    async fn test_find_all_references_offset_past_implementations_paginates_references() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        // 1 implementation
        lawyer.set_goto_implementation_result(Ok(vec![DefinitionLocation {
            file: "src/auth_impl.rs".into(),
            line: 10,
            column: 4,
            preview: "impl".into(),
        }]));

        // 5 references
        lawyer.set_references_result(Ok(vec![
            ReferenceLocation {
                file: "src/file1.rs".into(),
                line: 10,
                column: 8,
                snippet: "ref1".into(),
            },
            ReferenceLocation {
                file: "src/file2.rs".into(),
                line: 20,
                column: 8,
                snippet: "ref2".into(),
            },
            ReferenceLocation {
                file: "src/file3.rs".into(),
                line: 30,
                column: 8,
                snippet: "ref3".into(),
            },
            ReferenceLocation {
                file: "src/file4.rs".into(),
                line: 40,
                column: 8,
                snippet: "ref4".into(),
            },
            ReferenceLocation {
                file: "src/file5.rs".into(),
                line: 50,
                column: 8,
                snippet: "ref5".into(),
            },
        ]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // offset=3: skip 1 impl + 2 refs, get next 2 refs
        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 2,
            offset: 3,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        let refs = val.references.unwrap_or_default();
        assert_eq!(refs.len(), 2, "should return 2 references");
        assert_eq!(refs[0].file, "src/file3.rs");
        assert_eq!(refs[1].file, "src/file4.rs");
        assert_eq!(val.total_references, Some(6)); // 1 impl + 5 refs
        assert!(val.truncated, "should be truncated");
    }

    // ── find_all_references edge cases ─────────────────────────────────────

    #[tokio::test]
    async fn test_find_all_references_zero_references_zero_implementations() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        // Empty results
        lawyer.set_references_result(Ok(vec![]));
        lawyer.set_goto_implementation_result(Ok(vec![]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(!val.degraded);
        assert_eq!(val.total_references, Some(0));
        assert_eq!(val.files_referenced, 0);
        assert!(val.references.unwrap_or_default().is_empty());
    }

    #[tokio::test]
    async fn test_find_all_references_rejects_sandbox_denied_path() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // Use path outside workspace (sandbox denies)
        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "/etc/passwd::function".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        assert!(
            result.is_err(),
            "should return error for sandbox denied path"
        );
    }

    // ── goto_implementation Err while references succeeds ────────────

    #[tokio::test]
    async fn test_find_all_references_implementation_error_references_ok() {
        // When goto_implementation returns Err but references succeeds,
        // implementations should be empty vec and references should be present.
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        // References succeed
        lawyer.set_references_result(Ok(vec![ReferenceLocation {
            file: "src/main.rs".into(),
            line: 10,
            column: 8,
            snippet: "login();".into(),
        }]));

        // Implementation fails
        lawyer.set_goto_implementation_result(Err(LspError::Protocol(
            "implementation error".to_string(),
        )));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(!val.degraded);
        // Total = 0 implementations + 1 reference = 1
        assert_eq!(val.total_references, Some(1));
        let refs = val.references.unwrap_or_default();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].file, "src/main.rs");
    }

    // ── Large offset past total results ─────────────────────────────

    #[tokio::test]
    async fn test_find_all_references_large_offset_returns_empty() {
        // offset=100 with only 6 items total should return empty results.
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        // 3 references
        lawyer.set_references_result(Ok(vec![
            ReferenceLocation {
                file: "src/main.rs".into(),
                line: 10,
                column: 8,
                snippet: "login1();".into(),
            },
            ReferenceLocation {
                file: "src/tests.rs".into(),
                line: 5,
                column: 4,
                snippet: "login2();".into(),
            },
            ReferenceLocation {
                file: "src/app.rs".into(),
                line: 15,
                column: 8,
                snippet: "login3();".into(),
            },
        ]));

        // 3 implementations
        lawyer.set_goto_implementation_result(Ok(vec![
            DefinitionLocation {
                file: "src/impl1.rs".into(),
                line: 10,
                column: 4,
                preview: "impl1".into(),
            },
            DefinitionLocation {
                file: "src/impl2.rs".into(),
                line: 20,
                column: 4,
                preview: "impl2".into(),
            },
            DefinitionLocation {
                file: "src/impl3.rs".into(),
                line: 30,
                column: 4,
                preview: "impl3".into(),
            },
        ]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // offset=100 is way past the 6 total items
        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 100,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        let refs = val.references.unwrap_or_default();
        assert!(
            refs.is_empty(),
            "should return empty when offset past total, got {}",
            refs.len()
        );
        assert_eq!(val.total_references, Some(6));
        // When offset is past total, truncated is false (nothing more to show)
        assert!(
            !val.truncated,
            "should NOT be truncated when offset past total"
        );
    }

    // ── Truncation boundary ─────────────────────────────────────────

    #[tokio::test]
    async fn test_find_all_references_truncation_boundary() {
        // Exactly offset + max_results results → truncated should be false.
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        // 2 implementations
        lawyer.set_goto_implementation_result(Ok(vec![
            DefinitionLocation {
                file: "src/impl1.rs".into(),
                line: 10,
                column: 4,
                preview: "impl1".into(),
            },
            DefinitionLocation {
                file: "src/impl2.rs".into(),
                line: 20,
                column: 4,
                preview: "impl2".into(),
            },
        ]));

        // 3 references
        lawyer.set_references_result(Ok(vec![
            ReferenceLocation {
                file: "src/ref1.rs".into(),
                line: 10,
                column: 8,
                snippet: "ref1".into(),
            },
            ReferenceLocation {
                file: "src/ref2.rs".into(),
                line: 20,
                column: 8,
                snippet: "ref2".into(),
            },
            ReferenceLocation {
                file: "src/ref3.rs".into(),
                line: 30,
                column: 8,
                snippet: "ref3".into(),
            },
        ]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // Total = 5, offset=0, max_results=5 → exactly fits → NOT truncated
        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 5,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert_eq!(val.total_references, Some(5));
        let refs = val.references.unwrap_or_default();
        assert_eq!(refs.len(), 5, "should return all 5 items");
        assert!(
            !val.truncated,
            "should NOT be truncated when exactly at boundary"
        );
    }

    // ── Dedup between implementations and references ─────────────────

    #[tokio::test]
    async fn test_find_all_references_deduplicates_impl_and_refs() {
        // When a trait impl also appears in references, it should not appear twice.
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        // Implementation returns a location at file:line
        lawyer.set_goto_implementation_result(Ok(vec![DefinitionLocation {
            file: "src/auth_impl.rs".into(),
            line: 15,
            column: 4,
            preview: "impl LoginService for AuthService {".into(),
        }]));

        // References also includes the same file:line
        lawyer.set_references_result(Ok(vec![
            ReferenceLocation {
                file: "src/auth_impl.rs".into(),
                line: 15, // Same as implementation
                column: 4,
                snippet: "impl LoginService for AuthService {".into(),
            },
            ReferenceLocation {
                file: "src/main.rs".into(),
                line: 10,
                column: 8,
                snippet: "login();".into(),
            },
        ]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // Should have 2 total (1 impl + 1 unique ref), not 3
        assert_eq!(
            val.total_references,
            Some(2),
            "duplicate (file,line) should be deduped"
        );
        let refs = val.references.unwrap_or_default();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].file, "src/auth_impl.rs"); // implementation
        assert_eq!(refs[1].file, "src/main.rs"); // unique reference
    }
}
