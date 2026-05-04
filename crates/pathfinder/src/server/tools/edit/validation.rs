use super::text_edit::build_validation_outcome;
use super::{FinalizeEditParams, ValidationOutcome};
use crate::server::helpers::{io_error_data, pathfinder_to_error_data};
use crate::server::types::{EditResponse, EditValidation};
use pathfinder_common::error::{compute_lines_changed, PathfinderError};
use pathfinder_common::types::{SemanticPath, VersionHash};
use pathfinder_lsp::types::{FileChangeType, FileEvent};
use pathfinder_lsp::LspError;
use rmcp::handler::server::wrapper::Json;
use rmcp::model::ErrorData;
use std::path::Path;

impl crate::server::PathfinderServer {
    /// Run LSP Pull Diagnostics validation on a pending in-memory edit.
    ///
    /// # Flow
    /// 1. Notify LSP of the original file via `didOpen`
    /// 2. Snapshot pre-edit diagnostics via `textDocument/diagnostic`
    /// 3. Notify LSP of the new content via `didChange`
    /// 4. Snapshot post-edit diagnostics
    /// 5. Diff pre vs post, returning introduced/resolved lists
    ///
    /// If `ignore_validation_failures = true`, always returns a non-blocking
    /// `ValidationOutcome` even if new errors are introduced.
    ///
    /// Gracefully degrades to `validation_skipped` on all LSP errors.
    pub(crate) fn lsp_error_to_skip_reason(e: &LspError) -> &'static str {
        match e {
            LspError::NoLspAvailable => "no_lsp",
            LspError::Io(io_err) if io_err.kind() == std::io::ErrorKind::NotFound => {
                "lsp_not_on_path"
            }
            LspError::Io(_) => "lsp_start_failed",
            LspError::ConnectionLost => "lsp_crash",
            LspError::Timeout { .. } => "lsp_timeout",
            LspError::UnsupportedCapability { .. } => "pull_diagnostics_unsupported",
            LspError::Protocol(_) => "lsp_protocol_error",
        }
    }

    /// MT-4: Returns `(machine_reason, recovery_hint)` for a skip response.
    pub(crate) fn lsp_error_to_skip_pair(e: &LspError) -> (&'static str, Option<String>) {
        (Self::lsp_error_to_skip_reason(e), e.recovery_hint())
    }

    /// Helper: Open LSP document and collect pre-edit diagnostics.
    ///
    /// Returns `Err((machine_reason, recovery_hint))` on any failure.
    /// MT-4: The tuple pairs the skip code with an actionable agent hint.
    pub(crate) async fn lsp_open_and_pre_diags(
        &self,
        workspace: &Path,
        relative: &Path,
        original_content: &str,
    ) -> Result<Vec<pathfinder_lsp::types::LspDiagnostic>, (&'static str, Option<String>)> {
        // \u2500\u2500 did_open (original content, version 1) \u2500\u2500
        if let Err(e) = self
            .lawyer
            .did_open(workspace, relative, original_content)
            .await
        {
            let should_log = !matches!(
                &e,
                LspError::NoLspAvailable | LspError::UnsupportedCapability { .. }
            );
            if should_log {
                tracing::warn!(error = %e, "validation: did_open failed");
            }
            return Err(Self::lsp_error_to_skip_pair(&e));
        }

        // \u2500\u2500 pre-edit diagnostics \u2500\u2500
        let mut pre_diags = match self.lawyer.pull_diagnostics(workspace, relative).await {
            Ok(d) => d,
            Err(LspError::UnsupportedCapability { .. }) => {
                // LSP running but doesn't support Pull Diagnostics \u2014 close the document
                let _ = self.lawyer.did_close(workspace, relative).await;
                return Err(("pull_diagnostics_unsupported", None));
            }
            Err(e) => {
                tracing::warn!(error = %e, "validation: pre-edit pull_diagnostics failed");
                let _ = self.lawyer.did_close(workspace, relative).await;
                return Err(Self::lsp_error_to_skip_pair(&e));
            }
        };

        // Attempt to augment with workspace diagnostics
        match self
            .lawyer
            .pull_workspace_diagnostics(workspace, relative)
            .await
        {
            Ok(workspace_diags) => pre_diags.extend(workspace_diags),
            Err(LspError::UnsupportedCapability { .. } | LspError::NoLspAvailable) => {
                // Ignore unsupported capabilities or no LSP and just proceed
            }
            Err(e) => {
                // Timeout or protocol error pulling workspace diagnostics.
                // It shouldn't block validation entirely if single-file passed,
                // but we'll log it for observability.
                tracing::warn!(error = %e, "validation: pre-edit pull_workspace_diagnostics failed, continuing with single-file diags");
            }
        }

        Ok(pre_diags)
    }

    /// Helper: Apply LSP change and collect post-edit diagnostics.
    ///
    /// Returns `Err((machine_reason, recovery_hint))` on any failure.
    /// MT-4: The tuple pairs the skip code with an actionable agent hint.
    pub(crate) async fn lsp_change_and_post_diags(
        &self,
        workspace: &Path,
        relative: &Path,
        new_content: &str,
    ) -> Result<Vec<pathfinder_lsp::types::LspDiagnostic>, (&'static str, Option<String>)> {
        // \u2500\u2500 did_change (new content, version 2) \u2500\u2500
        if let Err(e) = self
            .lawyer
            .did_change(workspace, relative, new_content, 2)
            .await
        {
            tracing::warn!(error = %e, "validation: did_change failed");
            let _ = self.lawyer.did_close(workspace, relative).await;
            return Err(Self::lsp_error_to_skip_pair(&e));
        }

        // \u2500\u2500 post-edit diagnostics \u2500\u2500
        let mut post_diags = match self.lawyer.pull_diagnostics(workspace, relative).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, "validation: post-edit pull_diagnostics failed");
                let _ = self.lawyer.did_close(workspace, relative).await;
                return Err(Self::lsp_error_to_skip_pair(&e));
            }
        };

        match self
            .lawyer
            .pull_workspace_diagnostics(workspace, relative)
            .await
        {
            Ok(workspace_diags) => post_diags.extend(workspace_diags),
            Err(LspError::UnsupportedCapability { .. } | LspError::NoLspAvailable) => {}
            Err(e) => {
                tracing::warn!(error = %e, "validation: post-edit pull_workspace_diagnostics failed, continuing with single-file diags");
            }
        }

        Ok(post_diags)
    }

    /// Helper: Revert LSP state to original and close document (fire-and-forget).
    pub(crate) async fn lsp_revert_and_close(
        &self,
        workspace: &Path,
        relative: &Path,
        original_content: &str,
    ) {
        // ── revert LSP state to original (fire-and-forget) ──
        let _ = self
            .lawyer
            .did_change(workspace, relative, original_content, 3)
            .await;

        // ── close document to free LSP memory ──
        let _ = self.lawyer.did_close(workspace, relative).await;
    }

    /// Run LSP validation on a pending in-memory edit.
    ///
    /// Uses either Pull Diagnostics (LSP 3.17) or Push Diagnostics depending
    /// on what the LSP server supports, determined via `diagnostics_strategy`.
    ///
    /// # Flow
    /// 1. Notify LSP of the original file via `didOpen`
    /// 2. Snapshot pre-edit diagnostics
    /// 3. Notify LSP of the new content via `didChange`
    /// 4. Snapshot post-edit diagnostics
    /// 5. Diff pre vs post, returning introduced/resolved lists
    ///
    /// If `ignore_validation_failures = true`, always returns a non-blocking
    /// `ValidationOutcome` even if new errors are introduced.
    ///
    /// Gracefully degrades to `validation_skipped` on all LSP errors.
    #[allow(clippy::too_many_lines)] // Validation pipeline; splitting it would reduce readability
    pub(crate) async fn run_lsp_validation(
        &self,
        file_path: &Path,
        original_content: &str,
        new_content: &str,
        ignore_validation_failures: bool,
    ) -> ValidationOutcome {
        let relative = file_path;
        let workspace = self.workspace_root.path();

        // MT-4: return_skip now accepts a recovery_action hint.
        // Use `lsp_error_to_skip_pair(&e)` to produce both fields.
        let return_skip = |reason: &str, recovery_action: Option<String>| -> ValidationOutcome {
            let ext = relative.extension().and_then(|e| e.to_str()).unwrap_or("");
            let lang = pathfinder_lsp::client::language_id_for_extension(ext).unwrap_or("unknown");
            tracing::debug!(
                file = %relative.display(),
                skip_reason = reason,
                language = lang,
                "validation skip"
            );
            ValidationOutcome {
                validation: EditValidation::skipped_with_recovery(recovery_action),
                skipped: true,
                skipped_reason: Some(reason.to_owned()),
                should_block: false,
            }
        };

        // Determine diagnostics strategy from capabilities
        //
        // Interpretation:
        // - Some("pull") → use pull diagnostics
        // - Some("push") → skip (not yet implemented, PATCH-002)
        // - Some("none") → skip (no diagnostics support)
        // - None → unknown (lazy start, test mock, etc.) → try pull diagnostics and let it fail naturally
        let ext = relative.extension().and_then(|e| e.to_str()).unwrap_or("");
        let lang = pathfinder_lsp::client::language_id_for_extension(ext);
        let caps = self.lawyer.capability_status().await;
        let diagnostics_strategy =
            lang.and_then(|l| caps.get(l).and_then(|s| s.diagnostics_strategy.clone()));

        match diagnostics_strategy.as_deref() {
            Some("push") => {
                // Push diagnostics: didOpen/didChange → collect → didChange → collect → diff
                // MT-2: Use per-server push collection config.
                // gopls/tsserver need extended grace windows for progressive batches.
                // The ceiling_ms replaces the old hardcoded 15s for known servers.
                let push_cfg = {
                    let server =
                        lang.and_then(|l| caps.get(l).and_then(|s| s.server_name.as_deref()));
                    pathfinder_lsp::client::DetectedCapabilities::push_collection_config_for(server)
                };
                let push_timeout_ms = push_cfg.ceiling_ms;

                // Step 1: Open and collect pre-edit diagnostics
                let pre_diags = match self
                    .lawyer
                    .collect_diagnostics(workspace, relative, original_content, 1, push_timeout_ms)
                    .await
                {
                    Ok(d) => d,
                    Err(e) => {
                        let (reason, hint) = Self::lsp_error_to_skip_pair(&e);
                        tracing::warn!(error = %e, "validation: push pre-diagnostics collection failed");
                        return return_skip(reason, hint);
                    }
                };

                // Step 2: Apply change and collect post-edit diagnostics
                let post_diags = match self
                    .lawyer
                    .collect_diagnostics(workspace, relative, new_content, 2, push_timeout_ms)
                    .await
                {
                    Ok(d) => d,
                    Err(e) => {
                        let (reason, hint) = Self::lsp_error_to_skip_pair(&e);
                        tracing::warn!(error = %e, "validation: push post-diagnostics collection failed");
                        self.lsp_revert_and_close(workspace, relative, original_content)
                            .await;
                        return return_skip(reason, hint);
                    }
                };

                // Step 3: Revert and close
                self.lsp_revert_and_close(workspace, relative, original_content)
                    .await;

                // Step 4: Same diff logic as pull diagnostics
                return build_validation_outcome(
                    &pre_diags,
                    &post_diags,
                    ignore_validation_failures,
                    file_path,
                    "push",
                );
            }
            Some("none") => {
                return return_skip("no_diagnostics_support", None);
            }
            // "pull", unknown values, or None → proceed with pull diagnostics flow
            // (unknown/None lets lazy start and test mocks work as before)
            Some(_) | None => {}
        }

        // Step 1: Open LSP document and collect pre-edit diagnostics
        let pre_diags = match self
            .lsp_open_and_pre_diags(workspace, relative, original_content)
            .await
        {
            Ok(d) => d,
            Err((reason, hint)) => return return_skip(reason, hint),
        };

        // Step 2: Apply change and collect post-edit diagnostics
        let post_diags = match self
            .lsp_change_and_post_diags(workspace, relative, new_content)
            .await
        {
            Ok(d) => d,
            Err((reason, hint)) => {
                // Clean up LSP state before returning
                self.lsp_revert_and_close(workspace, relative, original_content)
                    .await;
                return return_skip(reason, hint);
            }
        };

        // Step 3: Revert LSP state to original and close document
        self.lsp_revert_and_close(workspace, relative, original_content)
            .await;

        // ── diff diagnostics ──────────────────────
        build_validation_outcome(
            &pre_diags,
            &post_diags,
            ignore_validation_failures,
            file_path,
            "pull",
        )
    }

    /// Helper to perform the final TOCTOU check and write the modified file to disk.
    /// Re-reads the file, ensures its current hash still matches `current_hash`,
    /// then writes `new_bytes` to disk in-place.
    pub(crate) async fn flush_edit_with_toctou(
        &self,
        semantic_path: &SemanticPath,
        current_hash: &VersionHash,
        source: &[u8],
        new_bytes: &[u8],
    ) -> Result<VersionHash, ErrorData> {
        let absolute_path = self.workspace_root.resolve(&semantic_path.file_path);

        let disk_bytes = tokio::fs::read(&absolute_path)
            .await
            .map_err(|e| io_error_data(format!("TOCTOU re-read failed: {e}")))?;
        let disk_hash = VersionHash::compute(&disk_bytes);

        if disk_hash != *current_hash {
            let prior_str = String::from_utf8_lossy(source);
            let late_str = String::from_utf8_lossy(&disk_bytes);
            let delta = compute_lines_changed(&prior_str, &late_str);
            let err = PathfinderError::VersionMismatch {
                path: semantic_path.file_path.clone(),
                current_version_hash: disk_hash.as_str().to_owned(),
                lines_changed: Some(delta),
            };
            return Err(pathfinder_to_error_data(&err));
        }

        // Use `tokio::fs::write` for in-place write (preserves inode, avoids
        // rename-swap artifacts that would confuse file watchers).
        tokio::fs::write(&absolute_path, new_bytes)
            .await
            .map_err(|e| io_error_data(format!("write failed: {e}")))?;

        // Broadcast file change to LSP processes
        if let Ok(uri) = url::Url::from_file_path(&absolute_path) {
            let event = FileEvent {
                uri: uri.to_string(),
                change_type: FileChangeType::Changed,
            };
            if let Err(e) = self.lawyer.did_change_watched_files(vec![event]).await {
                tracing::warn!(error = %e, "Failed to broadcast didChangeWatchedFiles on edit");
            }
        }

        // Immediately evict this file from the AST cache so the next read
        // re-parses from disk rather than returning the stale pre-edit AST.
        // Without this, a sub-second write+read pair would still see the old
        // symbol tree, causing SYMBOL_NOT_FOUND for newly inserted symbols.
        self.surgeon.invalidate_cache(&semantic_path.file_path);

        Ok(VersionHash::compute(new_bytes))
    }

    /// Helper function to perform LSP validation, TOCTOU check, and disk write.
    /// This dries up the tail end of the edit tools.
    pub(crate) async fn finalize_edit(
        &self,
        params: FinalizeEditParams<'_>,
    ) -> Result<Json<EditResponse>, ErrorData> {
        let validate_start = std::time::Instant::now();
        let original_str = std::str::from_utf8(params.source);
        let new_str = std::str::from_utf8(&params.new_content);
        let validation_outcome = match (original_str, new_str) {
            (Ok(orig), Ok(new)) => {
                // Both valid UTF-8 - proceed with LSP validation
                self.run_lsp_validation(
                    &params.semantic_path.file_path,
                    orig,
                    new,
                    params.ignore_validation_failures,
                )
                .await
            }
            (Err(_), Ok(_)) => {
                // Original was invalid UTF-8 but new content is valid -
                // this fixes a corrupted file, so we allow it.
                tracing::warn!(
                    filepath = %params.semantic_path.file_path.display(),
                    "original content was invalid UTF-8 but new content is valid - proceeding with edit to fix corruption"
                );
                ValidationOutcome {
                    validation: EditValidation::skipped(),
                    skipped: true,
                    skipped_reason: Some("original_invalid_utf8_fixed".to_owned()),
                    should_block: false,
                }
            }
            (Ok(_), Err(_)) => {
                // Original was valid but new content is invalid UTF-8 -
                // this would introduce corruption, so we block it.
                let err = PathfinderError::IoError {
                    message: "new content contains invalid UTF-8 - cannot write corrupted data"
                        .to_owned(),
                };
                return Err(pathfinder_to_error_data(&err));
            }
            (Err(_), Err(_)) => {
                // Both are invalid UTF-8 - skip validation but allow the edit
                // (the agent is presumably fixing corruption in a controlled way).
                tracing::warn!(
                    filepath = %params.semantic_path.file_path.display(),
                    "both original and new content are invalid UTF-8 - proceeding without validation"
                );
                ValidationOutcome {
                    validation: EditValidation::skipped(),
                    skipped: true,
                    skipped_reason: Some("both_invalid_utf8".to_owned()),
                    should_block: false,
                }
            }
        };
        let validate_ms = validate_start.elapsed().as_millis();

        if validation_outcome.should_block {
            let introduced = validation_outcome.validation.introduced_errors.clone();
            let err = PathfinderError::ValidationFailed {
                count: introduced.len(),
                introduced_errors: introduced,
            };
            return Err(pathfinder_to_error_data(&err));
        }

        let flush_start = std::time::Instant::now();
        let new_hash = self
            .flush_edit_with_toctou(
                params.semantic_path,
                params.original_hash,
                params.source,
                &params.new_content,
            )
            .await?;
        let flush_ms = flush_start.elapsed().as_millis();

        // C6: Compute engines_used based on whether validation was actually performed
        let engines_used = if validation_outcome.skipped {
            vec!["tree-sitter"]
        } else {
            vec!["tree-sitter", "lsp"]
        };

        let duration_ms = params.start_time.elapsed().as_millis();
        tracing::info!(
            tool = params.tool_name,
            semantic_path = %params.raw_semantic_path_str,
            duration_ms,
            resolve_ms = params.resolve_ms,
            validate_ms,
            flush_ms,
            new_version_hash = new_hash.as_str(),
            engines_used = ?engines_used,
            ignore_validation_failures = params.ignore_validation_failures,
            "{}: complete",
            params.tool_name
        );

        Ok(Json(EditResponse {
            success: true,
            new_version_hash: Some(new_hash.short().to_owned()),
            formatted: false,
            validation: validation_outcome.validation,
            validation_skipped: validation_outcome.skipped,
            validation_skipped_reason: validation_outcome.skipped_reason,
            warning: params.warning,
        }))
    }
}
