//! `inspect` tool (symbol scope mode) — AST-based symbol extraction via Tree-sitter.

use crate::server::helpers::{
    invalid_params_error, millis_to_u64, parse_semantic_path, pathfinder_to_error_data,
    require_symbol_target, serialize_metadata,
};
use crate::server::types::{
    BatchInspectResult, InspectParams, InspectResultEntry, ReadSymbolScopeMetadata,
    ReadWithDeepContextMetadata,
};
use crate::server::PathfinderServer;
use rmcp::model::{CallToolResult, Content, ErrorData};
use std::fmt::Write as _;

impl PathfinderServer {
    /// Consolidated `inspect` handler.
    #[expect(
        clippy::too_many_lines,
        reason = "Sequential batch dispatch pipeline: formats and routes symbols batch and individual requests"
    )]
    #[allow(clippy::missing_errors_doc)]
    pub async fn inspect_impl(&self, params: InspectParams) -> Result<CallToolResult, ErrorData> {
        if params.semantic_paths.is_some() && params.semantic_path.is_some() {
            return Err(invalid_params_error(
                "provide either `semantic_path` (single) or `semantic_paths` (batch), not both",
            ));
        }

        if params.semantic_paths.is_none() && params.semantic_path.is_none() {
            return Err(invalid_params_error(
                "provide either `semantic_path` (single) or `semantic_paths` (batch)",
            ));
        }

        if let Some(ref paths) = params.semantic_paths {
            if paths.is_empty() {
                return Err(invalid_params_error("`semantic_paths` must not be empty"));
            }
            if paths.len() > 10 {
                return Err(invalid_params_error(
                    "`semantic_paths` must contain at most 10 paths",
                ));
            }

            let start = std::time::Instant::now();
            let mut futures = Vec::new();
            for path in paths {
                let server = self.clone();
                let single_params = InspectParams {
                    semantic_path: Some(path.clone()),
                    semantic_paths: None,
                    include_dependencies: params.include_dependencies,
                    max_dependencies: params.max_dependencies,
                    include_imports: params.include_imports,
                    retry_attempt: 0,
                };
                futures.push(async move {
                    let path_clone = path.clone();
                    let res = server.inspect_impl_single(single_params).await;
                    match res {
                        Ok(call_res) => {
                            let val = call_res
                                .structured_content
                                .unwrap_or(serde_json::Value::Null);
                            if params.include_dependencies {
                                match serde_json::from_value::<ReadWithDeepContextMetadata>(val) {
                                    Ok(meta) => {
                                        let (deps_vec, confidence) = match meta.dependencies {
                                            Some(ev) => (Some(ev.items), ev.confidence),
                                            None => (
                                                None,
                                                if meta.degraded {
                                                    pathfinder_common::types::Confidence::Heuristic
                                                } else {
                                                    pathfinder_common::types::Confidence::Verified
                                                },
                                            ),
                                        };
                                        InspectResultEntry {
                                            semantic_path: path_clone,
                                            status: "ok".to_string(),
                                            confidence,
                                            source: meta.content,
                                            start_line: Some(meta.start_line),
                                            end_line: Some(meta.end_line),
                                            language: Some(meta.language),
                                            dependencies: deps_vec,
                                            readiness: meta.lsp_readiness,
                                            degraded_reason: meta.degraded_reason,
                                            error: None,
                                        }
                                    }
                                    Err(e) => InspectResultEntry {
                                        semantic_path: path_clone,
                                        status: "error".to_string(),
                                        confidence: pathfinder_common::types::Confidence::Heuristic,
                                        source: None,
                                        start_line: None,
                                        end_line: None,
                                        language: None,
                                        dependencies: None,
                                        readiness: None,
                                        degraded_reason: None,
                                        error: Some(format!("failed to deserialize metadata: {e}")),
                                    },
                                }
                            } else {
                                match serde_json::from_value::<ReadSymbolScopeMetadata>(val) {
                                    Ok(meta) => InspectResultEntry {
                                        semantic_path: path_clone,
                                        status: "ok".to_string(),
                                        confidence: pathfinder_common::types::Confidence::Verified,
                                        source: Some(meta.content),
                                        start_line: Some(meta.start_line),
                                        end_line: Some(meta.end_line),
                                        language: Some(meta.language),
                                        dependencies: None,
                                        readiness: Some("ready".to_string()),
                                        degraded_reason: None,
                                        error: None,
                                    },
                                    Err(e) => InspectResultEntry {
                                        semantic_path: path_clone,
                                        status: "error".to_string(),
                                        confidence: pathfinder_common::types::Confidence::Heuristic,
                                        source: None,
                                        start_line: None,
                                        end_line: None,
                                        language: None,
                                        dependencies: None,
                                        readiness: None,
                                        degraded_reason: None,
                                        error: Some(format!("failed to deserialize metadata: {e}")),
                                    },
                                }
                            }
                        }
                        Err(err) => InspectResultEntry {
                            semantic_path: path_clone,
                            status: "error".to_string(),
                            confidence: pathfinder_common::types::Confidence::Heuristic,
                            source: None,
                            start_line: None,
                            end_line: None,
                            language: None,
                            dependencies: None,
                            readiness: None,
                            degraded_reason: None,
                            error: Some(err.message.to_string()),
                        },
                    }
                });
            }

            // Process entries sequentially to guarantee deterministic ordering.
            // Each entry performs heavy I/O (tree-sitter + optional LSP), and
            // the max batch size is 10, so sequential execution has negligible
            // latency impact.
            let mut results = Vec::with_capacity(futures.len());
            for fut in futures {
                results.push(fut.await);
            }

            let mut succeeded = 0;
            let mut failed = 0;
            let mut heuristic_count = 0;
            for r in &results {
                if r.status == "ok" {
                    succeeded += 1;
                    if r.confidence == pathfinder_common::types::Confidence::Heuristic {
                        heuristic_count += 1;
                    }
                } else {
                    failed += 1;
                }
            }

            let status = if failed > 0 || heuristic_count > 0 {
                pathfinder_common::types::ToolStatus::Degraded {
                    reason: if heuristic_count > 0 {
                        "heuristic_results_present".to_string()
                    } else {
                        "errors_present".to_string()
                    },
                    retry_after_secs: None,
                }
            } else {
                pathfinder_common::types::ToolStatus::Ok
            };

            let total_duration_ms = millis_to_u64(start.elapsed().as_millis());
            let response = BatchInspectResult {
                status: status.clone(),
                results,
                succeeded,
                failed,
                heuristic_count,
                total_duration_ms,
            };

            let mut text_parts = Vec::new();
            if let Some(header) = crate::server::helpers::format_tool_status_header(&status) {
                text_parts.push(header);
            }

            for entry in &response.results {
                if entry.status == "ok" {
                    let heur_tag =
                        if entry.confidence == pathfinder_common::types::Confidence::Heuristic {
                            " [HEURISTIC]"
                        } else {
                            ""
                        };
                    let mut part = format!("--- {}{heur_tag} ---", entry.semantic_path);
                    if let Some(ref deps) = entry.dependencies {
                        if !deps.is_empty() {
                            let dep_lines: Vec<String> = deps
                                .iter()
                                .map(|d| format!("  {} ({}:L{})", d.signature, d.file, d.line))
                                .collect();
                            let dep_len = deps.len();
                            let dep_joined = dep_lines.join("\n");
                            let _ = write!(part, "\n{dep_len} dependencies loaded\n{dep_joined}");
                        }
                    }
                    if let Some(ref source) = entry.source {
                        let _ = write!(part, "\n\n{source}");
                    }
                    text_parts.push(part);
                } else {
                    text_parts.push(format!(
                        "--- {} (error: {}) ---",
                        entry.semantic_path,
                        entry.error.as_deref().unwrap_or("unknown error")
                    ));
                }
            }
            text_parts.push(format!(
                "[completed in {}ms, {}/{} symbols inspected]",
                total_duration_ms,
                succeeded,
                succeeded + failed
            ));

            let mut call_result =
                CallToolResult::success(vec![rmcp::model::Content::text(text_parts.join("\n"))]);
            call_result.structured_content = serialize_metadata(&response);
            Ok(call_result)
        } else {
            self.inspect_impl_single(params).await
        }
    }

    /// Single `inspect` implementation helper.
    pub(crate) async fn inspect_impl_single(
        &self,
        params: InspectParams,
    ) -> Result<CallToolResult, ErrorData> {
        if params.include_dependencies {
            if params.max_dependencies == 0 {
                return Err(invalid_params_error("`max_dependencies` must be >= 1"));
            }
            if params.max_dependencies > 500 {
                return Err(invalid_params_error("`max_dependencies` must be <= 500"));
            }
            self.read_with_deep_context_impl(params).await
        } else {
            self.read_symbol_scope_impl(params).await
        }
    }

    /// Core logic for the `read_symbol_scope` tool.
    ///
    /// Parses the semantic path, performs a sandbox check, then delegates
    /// to the `Surgeon` to extract the AST-located symbol scope.
    #[tracing::instrument(skip(self, params), fields(semantic_path = ?params.semantic_path))]
    pub(crate) async fn read_symbol_scope_impl(
        &self,
        params: InspectParams,
    ) -> Result<CallToolResult, ErrorData> {
        let start = std::time::Instant::now();

        let semantic_path_str = params
            .semantic_path
            .as_deref()
            .ok_or_else(|| invalid_params_error("`semantic_path` must be provided"))?;

        tracing::info!(tool = "read_symbol_scope", "read_symbol_scope: start");

        let semantic_path = parse_semantic_path(semantic_path_str)?;

        // read_symbol_scope requires a symbol chain, not just a bare file
        require_symbol_target(&semantic_path, semantic_path_str)?;

        // Sandbox check on the file path
        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            tracing::warn!(tool = "read_symbol_scope", error = %e, "sandbox check failed");
            return Err(pathfinder_to_error_data(&e));
        }

        // Early file existence check — avoid tree-sitter parse on nonexistent files
        let abs_file = self.workspace_root.path().join(&semantic_path.file_path);
        if !abs_file.exists() {
            let err = pathfinder_common::error::PathfinderError::FileNotFound {
                path: abs_file.clone(),
            };
            tracing::warn!(
                tool = "read_symbol_scope",
                path = %abs_file.display(),
                "file not found"
            );
            return Err(pathfinder_to_error_data(&err));
        }

        // Delegate to surgeon with did-you-mean and separator enrichment
        let ts_start = std::time::Instant::now();
        match self
            .read_symbol_scope_enriched(&semantic_path, semantic_path_str)
            .await
        {
            Ok(scope) => {
                let tree_sitter_ms = ts_start.elapsed().as_millis();
                let duration_ms = start.elapsed().as_millis();
                tracing::info!(
                    tool = "read_symbol_scope",
                    lines = (scope.end_line - scope.start_line + 1),
                    tree_sitter_ms,
                    duration_ms,
                    engines_used = ?["tree-sitter"],
                    "read_symbol_scope: complete"
                );

                let metadata = crate::server::types::ReadSymbolScopeMetadata {
                    status: pathfinder_common::types::ToolStatus::Ok,
                    content: scope.content.clone(),
                    start_line: scope.start_line + 1,
                    end_line: scope.end_line + 1,
                    language: scope.language,
                    duration_ms: Some(millis_to_u64(duration_ms)),
                };

                let text = format!("{}\n[completed in {duration_ms}ms]", scope.content);
                let mut result = CallToolResult::success(vec![Content::text(text)]);
                result.structured_content = serialize_metadata(&metadata);

                Ok(result)
            }
            Err(e) => {
                let tree_sitter_ms = ts_start.elapsed().as_millis();
                let duration_ms = start.elapsed().as_millis();
                tracing::warn!(
                    tool = "read_symbol_scope",
                    error = %e.message,
                    tree_sitter_ms,
                    duration_ms,
                    engines_used = ?["tree-sitter"],
                    "read_symbol_scope: failed"
                );
                Err(e)
            }
        }
    }
}

#[cfg(test)]
#[path = "inspect_test.rs"]
mod tests;
