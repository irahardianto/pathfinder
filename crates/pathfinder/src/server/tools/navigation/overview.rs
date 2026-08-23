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

pub(super) struct RenderOverviewParams<'a> {
    pub(super) semantic_path: &'a str,
    pub(super) start_line: usize,
    pub(super) end_line: usize,
    pub(super) scope_content: &'a str,
    pub(super) impact: Option<&'a crate::server::types::ImpactSummary>,
    pub(super) references: Option<
        &'a pathfinder_common::types::Evidence<crate::server::types::SymbolOverviewReference>,
    >,
    pub(super) files_referenced: usize,
    pub(super) degraded: bool,
    pub(super) degraded_reason: Option<DegradedReason>,
    pub(super) status: &'a pathfinder_common::types::ToolStatus,
    pub(super) hint: Option<&'a str>,
    pub(super) duration_ms: u128,
}

struct BuildOverviewResponseArgs {
    status: pathfinder_common::types::ToolStatus,
    source: Option<crate::server::types::SymbolSource>,
    impact: Option<crate::server::types::ImpactSummary>,
    references:
        Option<pathfinder_common::types::Evidence<crate::server::types::SymbolOverviewReference>>,
    files_referenced: usize,
    degraded: bool,
    impact_degraded: bool,
    refs_degraded: bool,
    degraded_reason: Option<DegradedReason>,
    lsp_readiness: Option<String>,
    warm_start_in_progress: Option<bool>,
    hint: Option<String>,
}

impl PathfinderServer {
    /// Composite tool: returns source + callers/callees + references in one call.
    ///
    /// Orchestrates `read_symbol_scope` + `find_callers_callees` + `find_all_references`.
    /// Uses depth=2 and capped references for bounded responses.
    fn validate_overview_request(
        &self,
        params: &crate::server::types::TraceParams,
    ) -> Result<pathfinder_common::types::SemanticPath, ErrorData> {
        let semantic_path = parse_semantic_path(&params.semantic_path)?;
        require_symbol_target(&semantic_path, &params.semantic_path)?;

        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            return Err(pathfinder_to_error_data(&e));
        }

        let abs_file = self.workspace_root.path().join(&semantic_path.file_path);
        if !abs_file.exists() {
            let err = pathfinder_common::error::PathfinderError::FileNotFound {
                path: abs_file.clone(),
            };
            return Err(pathfinder_to_error_data(&err));
        }

        if params.max_depth > 2 {
            return Err(crate::server::helpers::invalid_params_error(format!(
                "max_depth exceeds maximum allowed depth of 2 (requested: {}). Hint: For deep dependency traversal beyond depth 2, delegate to subagents or issue follow-up trace calls at each boundary.",
                params.max_depth
            )));
        }

        Ok(semantic_path)
    }

    fn compute_overview_status(
        degraded: bool,
        warm_start_in_progress: Option<bool>,
        has_verified: bool,
        duration_ms: u128,
        attempt: u32,
        degraded_reason: Option<&DegradedReason>,
    ) -> pathfinder_common::types::ToolStatus {
        if degraded {
            if warm_start_in_progress == Some(true) && !has_verified {
                pathfinder_common::types::ToolStatus::NotReady {
                    elapsed_secs: u32::try_from(duration_ms / 1000).unwrap_or(0),
                    retry_after_secs: 15,
                    eta_secs: None,
                    phase: "indexing".to_string(),
                    attempt,
                    max_attempts: 3,
                }
            } else {
                pathfinder_common::types::ToolStatus::Degraded {
                    reason: degraded_reason.map_or_else(|| "degraded".into(), ToString::to_string),
                    retry_after_secs: None,
                }
            }
        } else {
            pathfinder_common::types::ToolStatus::Ok
        }
    }

    fn check_has_verified(
        impact: Option<&crate::server::types::ImpactSummary>,
        references: Option<
            &pathfinder_common::types::Evidence<crate::server::types::SymbolOverviewReference>,
        >,
    ) -> bool {
        let has_impact = impact.is_some_and(|imp| {
            imp.incoming
                .as_ref()
                .is_some_and(|e| e.confidence == pathfinder_common::types::Confidence::Verified)
                || imp
                    .outgoing
                    .as_ref()
                    .is_some_and(|e| e.confidence == pathfinder_common::types::Confidence::Verified)
        });
        has_impact
            || references
                .is_some_and(|e| e.confidence == pathfinder_common::types::Confidence::Verified)
    }

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

        let semantic_path = self.validate_overview_request(&params)?;
        let scope = self
            .read_symbol_scope_enriched(&semantic_path, &params.semantic_path)
            .await?;

        let source = Some(crate::server::types::SymbolSource {
            content: scope.content.clone(),
            start_line: scope.start_line,
            end_line: scope.end_line,
            language: scope.language.clone(),
        });

        let _doc_guard = self.open_document_safely(&semantic_path).await;
        let max_refs = if params.max_references == 0 {
            20
        } else {
            params.max_references.clamp(1, 500)
        };

        let (impact, impact_degraded, impact_reason, impact_hint) = self
            .get_impact_summary(&params.semantic_path, max_refs, params.retry_attempt)
            .await;

        let (
            references,
            refs_degraded,
            refs_reason,
            files_referenced,
            _warm_start_in_progress,
            refs_hint,
        ) = self
            .get_references_summary(&params.semantic_path, max_refs, params.retry_attempt)
            .await;

        let duration_ms = start.elapsed().as_millis();

        let (degraded, degraded_reason, lsp_readiness, warm_start_in_progress) =
            Self::resolve_degraded_reason(
                impact_degraded,
                impact_reason,
                refs_degraded,
                refs_reason,
            );

        let has_verified = Self::check_has_verified(impact.as_ref(), references.as_ref());
        let status = Self::compute_overview_status(
            degraded,
            warm_start_in_progress,
            has_verified,
            duration_ms,
            params.retry_attempt,
            degraded_reason.as_ref(),
        );

        let (impact, references) = Self::normalize_overview_evidence(&status, impact, references);

        let hint = impact_hint.or(refs_hint);
        let response = Self::build_overview_response(BuildOverviewResponseArgs {
            status,
            source,
            impact,
            references,
            files_referenced,
            degraded,
            impact_degraded,
            refs_degraded,
            degraded_reason,
            lsp_readiness,
            warm_start_in_progress,
            hint,
        });

        Ok(Self::build_overview_call_result(
            &params,
            &scope,
            &response,
            duration_ms,
        ))
    }

    fn normalize_overview_evidence(
        status: &pathfinder_common::types::ToolStatus,
        impact: Option<crate::server::types::ImpactSummary>,
        references: Option<
            pathfinder_common::types::Evidence<crate::server::types::SymbolOverviewReference>,
        >,
    ) -> (
        Option<crate::server::types::ImpactSummary>,
        Option<pathfinder_common::types::Evidence<crate::server::types::SymbolOverviewReference>>,
    ) {
        let (impact, references) = match status {
            pathfinder_common::types::ToolStatus::NotReady { .. } => (None, None),
            pathfinder_common::types::ToolStatus::Degraded { .. }
            | pathfinder_common::types::ToolStatus::Unavailable { .. } => {
                let imp = impact.map(|mut imp| {
                    if let Some(ev) = imp.incoming {
                        imp.incoming = if ev.items.is_empty() {
                            None
                        } else {
                            Some(pathfinder_common::types::Evidence::heuristic(ev.items))
                        };
                    }
                    if let Some(ev) = imp.outgoing {
                        imp.outgoing = if ev.items.is_empty() {
                            None
                        } else {
                            Some(pathfinder_common::types::Evidence::heuristic(ev.items))
                        };
                    }
                    imp
                });
                let refs = references.and_then(|ev| {
                    if ev.items.is_empty() {
                        None
                    } else {
                        Some(pathfinder_common::types::Evidence::heuristic(ev.items))
                    }
                });
                (imp, refs)
            }
            pathfinder_common::types::ToolStatus::Ok => (impact, references),
        };

        if let Some(imp) = &impact {
            crate::server::helpers::validate_evidence(status, imp.incoming.as_ref());
            crate::server::helpers::validate_evidence(status, imp.outgoing.as_ref());
        }
        crate::server::helpers::validate_evidence(status, references.as_ref());

        (impact, references)
    }

    fn build_overview_response(
        args: BuildOverviewResponseArgs,
    ) -> crate::server::types::SymbolOverviewResponse {
        crate::server::types::SymbolOverviewResponse {
            status: args.status,
            source: args.source,
            impact: args.impact,
            references: args.references,
            heuristic_candidates: None,
            files_referenced: args.files_referenced,
            degraded: args.degraded,
            impact_degraded: args.impact_degraded,
            references_degraded: args.refs_degraded,
            degraded_reason: args.degraded_reason,
            actionable_guidance: args.degraded_reason.as_ref().map(DegradedReason::guidance),
            lsp_readiness: args.lsp_readiness,
            warm_start_in_progress: args.warm_start_in_progress,
            hint: args.hint,
            resolution_strategy: None,
        }
    }

    fn build_overview_call_result(
        params: &crate::server::types::TraceParams,
        scope: &pathfinder_common::types::SymbolScope,
        response: &crate::server::types::SymbolOverviewResponse,
        duration_ms: u128,
    ) -> rmcp::model::CallToolResult {
        let render_params = RenderOverviewParams {
            semantic_path: &params.semantic_path,
            start_line: scope.start_line,
            end_line: scope.end_line,
            scope_content: &scope.content,
            impact: response.impact.as_ref(),
            references: response.references.as_ref(),
            files_referenced: response.files_referenced,
            degraded: response.degraded,
            degraded_reason: response.degraded_reason,
            status: &response.status,
            hint: response.hint.as_deref(),
            duration_ms,
        };
        let text = Self::render_overview_text(&render_params);

        let mut result =
            rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(text)]);
        result.structured_content = serialize_metadata(response);
        result
    }

    async fn open_document_safely(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
    ) -> Option<Box<dyn pathfinder_lsp::lawyer::DocumentLease>> {
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
        match self
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
        }
    }

    async fn get_impact_summary(
        &self,
        semantic_path: &str,
        max_references: u32,
        retry_attempt: u32,
    ) -> (
        Option<crate::server::types::ImpactSummary>,
        bool,
        Option<DegradedReason>,
        Option<String>,
    ) {
        let impact_params = crate::server::types::TraceParams {
            semantic_path: semantic_path.to_owned(),
            scope: crate::server::types::TraceScope::Callers,
            max_depth: 1,
            max_references,
            offset: 0,
            retry_attempt,
        };

        let impact_result = self.find_callers_callees_impl(impact_params).await;

        match impact_result {
            Ok(result) => {
                let raw = result.structured_content.unwrap_or_default();
                let meta: crate::server::types::FindCallersCalleesMetadata =
                    serde_json::from_value(raw).unwrap_or_else(|e| {
                        debug_assert!(false, "find_callers_callees metadata deserialization failed: {e}");
                        tracing::warn!(
                            error = %e,
                            "symbol_overview: find_callers_callees metadata deserialization failed — using degraded default"
                        );
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
                            pathfinder_common::types::Evidence {
                                confidence: incoming.confidence,
                                items: incoming
                                    .items
                                    .into_iter()
                                    .map(|r| crate::server::types::SymbolOverviewImpactEntry {
                                        semantic_path: r.semantic_path,
                                        file: r.file,
                                        line: r.line,
                                        snippet: r.snippet,
                                        direction: r.direction,
                                        confidence: r.confidence,
                                    })
                                    .collect(),
                            }
                        }),
                        outgoing: meta.outgoing.map(|outgoing| {
                            pathfinder_common::types::Evidence {
                                confidence: outgoing.confidence,
                                items: outgoing
                                    .items
                                    .into_iter()
                                    .map(|r| crate::server::types::SymbolOverviewImpactEntry {
                                        semantic_path: r.semantic_path,
                                        file: r.file,
                                        line: r.line,
                                        snippet: r.snippet,
                                        direction: r.direction,
                                        confidence: r.confidence,
                                    })
                                    .collect(),
                            }
                        }),
                        degraded: meta.degraded,
                    })
                };
                (summary, meta.degraded, meta.degraded_reason, meta.hint)
            }
            Err(e) => {
                tracing::warn!(
                    tool = "symbol_overview",
                    error = %e,
                    "find_callers_callees_impl failed — impact will be unavailable"
                );
                (None, true, Some(DegradedReason::LspErrorGrepFallback), None)
            }
        }
    }

    async fn get_references_summary(
        &self,
        semantic_path: &str,
        max_references: u32,
        retry_attempt: u32,
    ) -> (
        Option<pathfinder_common::types::Evidence<crate::server::types::SymbolOverviewReference>>,
        bool,
        Option<DegradedReason>,
        usize,
        Option<bool>,
        Option<String>,
    ) {
        let refs_params = crate::server::types::TraceParams {
            semantic_path: semantic_path.to_owned(),
            scope: crate::server::types::TraceScope::References,
            max_depth: 0,
            max_references,
            offset: 0,
            retry_attempt,
        };

        let refs_result = self.find_all_references_impl(refs_params).await;

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
                        crate::server::types::FindAllReferencesMetadata {
                            degraded: true,
                            degraded_reason: Some(DegradedReason::LspErrorGrepFallback),
                            ..Default::default()
                        }
                    });
                let refs = meta
                    .references
                    .map(|refs| pathfinder_common::types::Evidence {
                        confidence: refs.confidence,
                        items: refs
                            .items
                            .into_iter()
                            .map(|r| crate::server::types::SymbolOverviewReference {
                                file: r.file,
                                line: r.line,
                                column: r.column,
                                snippet: r.snippet,
                                confidence: r.confidence,
                            })
                            .collect(),
                    });
                let warm_start_in_progress = meta.warm_start_in_progress;
                (
                    refs,
                    meta.degraded,
                    meta.degraded_reason,
                    meta.files_referenced,
                    warm_start_in_progress,
                    meta.hint,
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
                    None,
                )
            }
        }
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

    pub(super) fn render_overview_text(params: &RenderOverviewParams<'_>) -> String {
        let line_count = params.end_line - params.start_line + 1;
        let mut source_block = format!("SYMBOL: {} ({line_count} lines)\n", params.semantic_path);
        if line_count <= 10 {
            source_block.push_str("```\n");
            source_block.push_str(params.scope_content);
            if !params.scope_content.ends_with('\n') {
                source_block.push('\n');
            }
            source_block.push_str("```\n");
        }

        let mut status_prefix = String::new();
        if let Some(header) = crate::server::helpers::format_tool_status_header(params.status) {
            status_prefix = format!("{header}\n\n");
        }

        let impact_block = if let Some(imp) = params.impact {
            let inc_str = match &imp.incoming {
                None => "unavailable (not verified)".to_string(),
                Some(ev)
                    if ev.confidence == pathfinder_common::types::Confidence::Verified
                        && ev.items.is_empty() =>
                {
                    "0 direct (verified zero)".to_string()
                }
                Some(ev) if ev.confidence == pathfinder_common::types::Confidence::Verified => {
                    format!("{} direct (verified)", ev.items.len())
                }
                Some(ev) => format!("{} direct (heuristic candidates)", ev.items.len()),
            };
            let out_str = match &imp.outgoing {
                None => "unavailable (not verified)".to_string(),
                Some(ev)
                    if ev.confidence == pathfinder_common::types::Confidence::Verified
                        && ev.items.is_empty() =>
                {
                    "0 (verified zero)".to_string()
                }
                Some(ev) if ev.confidence == pathfinder_common::types::Confidence::Verified => {
                    format!("{} (verified)", ev.items.len())
                }
                Some(ev) => format!("{} (heuristic candidates)", ev.items.len()),
            };
            format!("CALLERS: {inc_str}\nCALLEES: {out_str}\n")
        } else {
            "CALLERS: unavailable (not verified)\nCALLEES: unavailable (not verified)\n".to_owned()
        };

        let refs_block = if let Some(ev) = params.references {
            let total = ev.items.len();
            let conf_label = if ev.confidence == pathfinder_common::types::Confidence::Verified {
                if total == 0 {
                    "(verified zero)"
                } else {
                    "(verified)"
                }
            } else {
                "(heuristic candidates)"
            };
            format!(
                "REFERENCES: {total} total across {} files {conf_label}\n",
                params.files_referenced
            )
        } else {
            "REFERENCES: unavailable (not verified)\n".to_owned()
        };

        let degraded_block =
            if !params.degraded && *params.status == pathfinder_common::types::ToolStatus::Ok {
                "DEGRADED: no (LSP-backed, authoritative)\n".to_owned()
            } else {
                let notice = params
                    .degraded_reason
                    .as_ref()
                    .map_or_else(|| "DEGRADED (unknown)".to_owned(), format_degraded_notice);
                format!("{notice}\n")
            };

        let hint_block = if let Some(h) = params.hint {
            if h.is_empty() {
                String::new()
            } else {
                format!("HINT: {h}\n")
            }
        } else {
            String::new()
        };

        let extra = if params.degraded { "\n" } else { "" };
        format!(
            "{status_prefix}{source_block}{impact_block}{refs_block}{degraded_block}{hint_block}{extra}[completed in {}ms]",
            params.duration_ms
        )
    }
}

#[cfg(test)]
#[path = "overview_test.rs"]
mod tests;
