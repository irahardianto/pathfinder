//! `trace` tool handler (overview mode).
//!
//! Composite tool that returns source + callers/callees + references in one call.
//! Orchestrates `read_symbol_scope_enriched` + `find_callers_callees_impl` + `find_all_references_impl`.

use crate::server::helpers::{
    format_degraded_notice, parse_semantic_path, pathfinder_to_error_data, require_symbol_target,
    serialize_metadata,
};
use crate::server::PathfinderServer;
use pathfinder_common::types::DegradedReason;
use rmcp::model::ErrorData;

impl PathfinderServer {
    /// Composite tool: returns source + callers/callees + references in one call.
    ///
    /// Orchestrates `read_symbol_scope` + `find_callers_callees` + `find_all_references`.
    /// Uses depth=2 and capped references for bounded responses.
    #[allow(clippy::too_many_lines)]
    pub(crate) async fn symbol_overview_impl(
        &self,
        params: crate::server::types::TraceParams,
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

        let impact_params = crate::server::types::TraceParams {
            semantic_path: params.semantic_path.clone(),
            scope: crate::server::types::TraceScope::Callers,
            max_depth: 2,
            max_references: params.max_references,
            offset: 0,
        };

        let impact_result = self.find_callers_callees_impl(impact_params).await;

        let (impact, impact_degraded, impact_reason) = match impact_result {
            Ok(result) => {
                let raw = result.structured_content.unwrap_or_default();
                let meta: crate::server::types::FindCallersCalleesMetadata =
                    serde_json::from_value(raw).unwrap_or_else(|e| {
                        debug_assert!(false, "find_callers_callees metadata deserialization failed: {e}");
                        tracing::warn!(
                            error = %e,
                            "symbol_overview: find_callers_callees metadata deserialization failed — using degraded default"
                        );
                        // Item 11: Use degraded=true to avoid hiding the bug from consumers.
                        crate::server::types::FindCallersCalleesMetadata {
                            degraded: true,
                            degraded_reason: Some(DegradedReason::LspErrorGrepFallback),
                            ..Default::default()
                        }
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
            Err(e) => {
                tracing::warn!(
                    tool = "symbol_overview",
                    error = %e,
                    "find_callers_callees_impl failed — impact will be unavailable"
                );
                (None, true, Some(DegradedReason::LspErrorGrepFallback))
            }
        };

        let refs_params = crate::server::types::TraceParams {
            semantic_path: params.semantic_path.clone(),
            scope: crate::server::types::TraceScope::References,
            max_depth: 0,
            max_references: params.max_references,
            offset: 0,
        };

        let refs_result = self.find_all_references_impl(refs_params).await;

        let (references, refs_degraded, refs_reason, files_referenced, _refs_warm_start) =
            match refs_result {
                Ok(result) => {
                    let raw = result.structured_content.unwrap_or_default();
                    let meta: crate::server::types::FindAllReferencesMetadata =
                    serde_json::from_value(raw).unwrap_or_else(|e| {
                        debug_assert!(false, "find_all_references metadata deserialization failed: {e}");
                        tracing::warn!(
                            error = %e,
                            "symbol_overview: find_all_references metadata deserialization failed — using degraded default"
                        );
                        // Item 11: Use degraded=true to avoid hiding the bug from consumers.
                        crate::server::types::FindAllReferencesMetadata {
                            degraded: true,
                            degraded_reason: Some(DegradedReason::LspErrorGrepFallback),
                            ..Default::default()
                        }
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
                Err(e) => {
                    tracing::warn!(
                        tool = "symbol_overview",
                        error = %e,
                        "find_all_references_impl failed — references will be unavailable"
                    );
                    (
                        None,
                        true,
                        Some(DegradedReason::LspErrorGrepFallback),
                        0,
                        None,
                    )
                }
            };

        let duration_ms = start.elapsed().as_millis();

        let (degraded, degraded_reason, lsp_readiness, warm_start_in_progress) =
            Self::resolve_degraded_reason(
                impact_degraded,
                impact_reason,
                refs_degraded,
                refs_reason,
            );

        let response = crate::server::types::SymbolOverviewResponse {
            source,
            impact: impact.clone(),
            references: references.clone(),
            files_referenced,
            degraded,
            impact_degraded,
            references_degraded: refs_degraded,
            degraded_reason,
            actionable_guidance: degraded_reason.as_ref().map(DegradedReason::guidance),
            lsp_readiness,
            warm_start_in_progress,
        };

        let text = Self::render_overview_text(
            &params.semantic_path,
            scope.start_line,
            scope.end_line,
            &scope.content,
            impact.as_ref(),
            references.as_deref(),
            files_referenced,
            degraded,
            degraded_reason,
            duration_ms,
        );

        let mut result =
            rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(text)]);
        result.structured_content = serialize_metadata(&response);
        Ok(result)
    }

    pub(super) fn resolve_degraded_reason(
        impact_degraded: bool,
        impact_reason: Option<DegradedReason>,
        refs_degraded: bool,
        refs_reason: Option<DegradedReason>,
    ) -> (bool, Option<DegradedReason>, Option<String>, Option<bool>) {
        let degraded = impact_degraded || refs_degraded;
        let is_warming = |r: &Option<DegradedReason>| {
            matches!(
                r,
                Some(
                    DegradedReason::LspWarmupEmptyUnverified
                        | DegradedReason::LspWarmupGrepFallback
                        | DegradedReason::LspTimeoutGrepFallback
                )
            )
        };
        let degraded_reason = if is_warming(&impact_reason) || is_warming(&refs_reason) {
            if is_warming(&impact_reason) {
                impact_reason
            } else {
                refs_reason
            }
        } else if impact_degraded {
            impact_reason
        } else if refs_degraded {
            refs_reason
        } else {
            None
        };

        let lsp_readiness = if degraded {
            match degraded_reason {
                Some(
                    DegradedReason::LspWarmupEmptyUnverified
                    | DegradedReason::LspWarmupGrepFallback
                    | DegradedReason::LspTimeoutGrepFallback,
                ) => Some("warming_up".to_owned()),
                _ => Some("unavailable".to_owned()),
            }
        } else {
            Some("ready".to_owned())
        };

        let warm_start_in_progress = match lsp_readiness.as_deref() {
            Some("warming_up") => Some(true),
            Some("ready") => Some(false),
            _ => None,
        };

        (
            degraded,
            degraded_reason,
            lsp_readiness,
            warm_start_in_progress,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_overview_text(
        semantic_path: &str,
        start_line: usize,
        end_line: usize,
        scope_content: &str,
        impact: Option<&crate::server::types::ImpactSummary>,
        references: Option<&[crate::server::types::SymbolOverviewReference]>,
        files_referenced: usize,
        degraded: bool,
        degraded_reason: Option<DegradedReason>,
        duration_ms: u128,
    ) -> String {
        let line_count = end_line - start_line + 1;
        let mut source_block = format!("SYMBOL: {semantic_path} ({line_count} lines)\n");
        if line_count <= 10 {
            source_block.push_str("```\n");
            source_block.push_str(scope_content);
            if !scope_content.ends_with('\n') {
                source_block.push('\n');
            }
            source_block.push_str("```\n");
        }

        let impact_block = if let Some(imp) = impact {
            let inc = imp.incoming.as_ref().map_or(0, Vec::len);
            let out = imp.outgoing.as_ref().map_or(0, Vec::len);
            let deg = if imp.degraded { " (degraded)" } else { "" };
            format!("CALLERS: {inc} direct{deg}\nCALLEES: {out}{deg}\n")
        } else {
            "CALLERS: unavailable\nCALLEES: unavailable\n".to_owned()
        };

        let refs_block = if let Some(refs) = references {
            let total = refs.len();
            format!("REFERENCES: {total} total across {files_referenced} files\n")
        } else {
            "REFERENCES: unavailable\n".to_owned()
        };

        let degraded_block = if degraded {
            let notice = degraded_reason.map_or_else(
                || "DEGRADED (unknown)".to_owned(),
                |r| format_degraded_notice(&r),
            );
            format!("{notice}\n")
        } else {
            "DEGRADED: no (LSP-backed, authoritative)\n".to_owned()
        };

        let extra = if degraded { "\n" } else { "" };
        format!(
            "{source_block}{impact_block}{refs_block}{degraded_block}{extra}[completed in {duration_ms}ms]"
        )
    }
}

#[cfg(test)]
#[path = "overview_test.rs"]
mod tests;
