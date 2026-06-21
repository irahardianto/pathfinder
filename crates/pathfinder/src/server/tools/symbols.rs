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
use futures::StreamExt as _;
use rmcp::model::{CallToolResult, Content, ErrorData};
use std::fmt::Write as _;

impl PathfinderServer {
    /// Consolidated `inspect` handler.
    #[expect(
        clippy::too_many_lines,
        reason = "Sequential batch dispatch pipeline: formats and routes symbols batch and individual requests"
    )]
    pub(crate) async fn inspect_impl(
        &self,
        params: InspectParams,
    ) -> Result<CallToolResult, ErrorData> {
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
                                    Ok(meta) => InspectResultEntry {
                                        semantic_path: path_clone,
                                        status: "ok".to_string(),
                                        source: meta.content,
                                        start_line: Some(meta.start_line),
                                        end_line: Some(meta.end_line),
                                        language: Some(meta.language),
                                        dependencies: Some(meta.dependencies),
                                        error: None,
                                    },
                                    Err(e) => InspectResultEntry {
                                        semantic_path: path_clone,
                                        status: "error".to_string(),
                                        source: None,
                                        start_line: None,
                                        end_line: None,
                                        language: None,
                                        dependencies: None,
                                        error: Some(format!("failed to deserialize metadata: {e}")),
                                    },
                                }
                            } else {
                                match serde_json::from_value::<ReadSymbolScopeMetadata>(val) {
                                    Ok(meta) => InspectResultEntry {
                                        semantic_path: path_clone,
                                        status: "ok".to_string(),
                                        source: Some(meta.content),
                                        start_line: Some(meta.start_line),
                                        end_line: Some(meta.end_line),
                                        language: Some(meta.language),
                                        dependencies: None,
                                        error: None,
                                    },
                                    Err(e) => InspectResultEntry {
                                        semantic_path: path_clone,
                                        status: "error".to_string(),
                                        source: None,
                                        start_line: None,
                                        end_line: None,
                                        language: None,
                                        dependencies: None,
                                        error: Some(format!("failed to deserialize metadata: {e}")),
                                    },
                                }
                            }
                        }
                        Err(err) => InspectResultEntry {
                            semantic_path: path_clone,
                            status: "error".to_string(),
                            source: None,
                            start_line: None,
                            end_line: None,
                            language: None,
                            dependencies: None,
                            error: Some(err.message.to_string()),
                        },
                    }
                });
            }

            let results: Vec<InspectResultEntry> =
                futures::stream::iter(futures).buffered(4).collect().await;

            let mut succeeded = 0;
            let mut failed = 0;
            for r in &results {
                if r.status == "ok" {
                    succeeded += 1;
                } else {
                    failed += 1;
                }
            }

            let total_duration_ms = millis_to_u64(start.elapsed().as_millis());
            let response = BatchInspectResult {
                results,
                succeeded,
                failed,
                total_duration_ms,
            };

            let mut text_parts = Vec::new();
            for entry in &response.results {
                if entry.status == "ok" {
                    let mut part = format!("--- {} ---", entry.semantic_path);
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

        // Delegate to surgeon
        let ts_start = std::time::Instant::now();
        match self
            .surgeon
            .read_symbol_scope(self.workspace_root.path(), &semantic_path)
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
                    content: scope.content.clone(),
                    start_line: scope.start_line,
                    end_line: scope.end_line,
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
                    error = %e,
                    tree_sitter_ms,
                    duration_ms,
                    engines_used = ?["tree-sitter"],
                    "read_symbol_scope: failed"
                );
                Err(crate::server::helpers::treesitter_error_to_error_data(e))
            }
        }
    }
}

#[cfg(test)]
#[path = "symbols_test.rs"]
mod tests;
