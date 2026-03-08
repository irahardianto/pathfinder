//! AST-aware edit tools — `replace_body`, `replace_full`, `insert_before`, `insert_after`,
//! `delete_symbol`, and `validate_only`.
//!
//! All edit tools share a common pipeline:
//! 1. Parse semantic path
//! 2. Sandbox check
//! 3. Read file → OCC check (`base_version`)
//! 4. Resolve scope via Surgeon
//! 5. Normalize input (`normalize_for_body_replace` / `normalize_for_full_replace`)
//! 6. Indentation pre-pass (dedent → reindent to AST column)
//! 7. Splice normalized code into scope byte range
//! 8. **LSP validation** (`did_open` → `pull_diagnostics` → `did_change` → `pull_diagnostics` → diff)
//! 9. TOCTOU late-check: re-read, re-hash immediately before write
//! 10. `tokio::fs::write` (in-place, preserves inode)
//! 11. Compute and return new `version_hash`

use crate::server::helpers::{io_error_data, pathfinder_to_error_data};
use crate::server::tools::diagnostics::diff_diagnostics;
use crate::server::types::{
    DeleteSymbolParams, EditResponse, EditValidation, InsertAfterParams, InsertBeforeParams,
    ReplaceBodyParams, ReplaceFullParams, ValidateOnlyParams,
};
use crate::server::PathfinderServer;
use pathfinder_common::error::{DiagnosticError, PathfinderError};
use pathfinder_common::indent::dedent_then_reindent;
use pathfinder_common::normalize::{normalize_for_body_replace, normalize_for_full_replace};
use pathfinder_common::types::{SemanticPath, VersionHash};
use pathfinder_lsp::LspError;
use rmcp::handler::server::wrapper::Json;
use rmcp::model::ErrorData;
use std::path::Path;
use tracing::instrument;

/// Result of the LSP validation step.
struct ValidationOutcome {
    validation: EditValidation,
    skipped: Option<bool>,
    skipped_reason: Option<String>,
    /// `true` when new errors were introduced and `ignore_validation_failures = false`.
    /// The caller must NOT write to disk in this case.
    should_block: bool,
}

impl PathfinderServer {
    /// Core logic for the `replace_body` tool (PRD Epic 5, Story 5.3).
    ///
    /// Replaces the **body** of a block-scoped symbol (function, method, class)
    /// in place on disk, using the OCC `base_version` to guard against races.
    ///
    /// # Pipeline (PRD §3.4)
    /// 1. Validate semantic path
    /// 2. Sandbox check
    /// 3. Resolve body range + version hash via Surgeon
    /// 4. OCC: compare `base_version` to current file hash
    /// 5. Normalize `new_code` (fence strip, brace-leniency, CRLF)
    /// 6. Indentation pre-pass (dedent → reindent)
    /// 7. Splice into body byte range
    /// 8. TOCTOU late-check (re-read + re-hash)
    /// 9. Write to disk
    /// 10. Return new version hash
    #[instrument(skip(self, params), fields(semantic_path = %params.semantic_path))]
    #[expect(
        clippy::too_many_lines,
        reason = "Linear 10-step edit pipeline: parse → sandbox → resolve → normalize → indent → splice → validate → TOCTOU → write → respond. Each step is distinct and cannot be collapsed further without obscuring intent."
    )]
    pub(crate) async fn replace_body_impl(
        &self,
        params: ReplaceBodyParams,
    ) -> Result<Json<EditResponse>, ErrorData> {
        let start = std::time::Instant::now();
        tracing::info!(
            tool = "replace_body",
            semantic_path = %params.semantic_path,
            "replace_body: start"
        );

        // ── Step 1: Parse semantic path ────────────────────────────────
        let Some(semantic_path) = SemanticPath::parse(&params.semantic_path) else {
            return Err(io_error_data(format!(
                "invalid semantic path: {}",
                params.semantic_path
            )));
        };

        // replace_body requires a symbol chain, not just a bare file
        if semantic_path.is_bare_file() {
            let err = PathfinderError::InvalidTarget {
                semantic_path: params.semantic_path.clone(),
                reason: "replace_body requires a symbol path (e.g., src/auth.ts::Login). \
                         Use write_file to replace file content."
                    .to_owned(),
            };
            return Err(pathfinder_to_error_data(&err));
        }

        // ── Step 2: Sandbox check ──────────────────────────────────────
        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            tracing::warn!(
                tool = "replace_body",
                semantic_path = %params.semantic_path,
                error = %e,
                "replace_body: access denied"
            );
            return Err(pathfinder_to_error_data(&e));
        }

        // ── Step 3: Resolve body range + read source ─────────────────
        // The Surgeon reads the file, parses the AST, and returns the
        // (open_brace, close_brace, indent_column) triple plus the raw source
        // and the current version hash for OCC.
        let (body_range, source, current_hash) = match self
            .surgeon
            .resolve_body_range(self.workspace_root.path(), &semantic_path)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Err(crate::server::helpers::treesitter_error_to_error_data(e));
            }
        };

        // ── Step 4: OCC check ─────────────────────────────────────────
        let claimed = VersionHash::from_raw(params.base_version.clone());
        if claimed != current_hash {
            let err = PathfinderError::VersionMismatch {
                path: semantic_path.file_path.clone(),
                current_version_hash: current_hash.as_str().to_owned(),
            };
            return Err(pathfinder_to_error_data(&err));
        }

        // ── Step 5: Normalize new_code ────────────────────────────────
        let normalized = normalize_for_body_replace(&params.new_code);

        // ── Step 6: Indentation pre-pass ──────────────────────────────
        // The body content (between braces) should be indented relative to
        // the measured body_indent_column, avoiding hardcoded 4-space delta.
        let body_indent_column = body_range.body_indent_column;
        let indented = dedent_then_reindent(&normalized, body_indent_column);

        // ── Step 7: Splice into body byte range ───────────────────────
        // We know the source bytes. We check if the body is wrapped in braces.
        // For Go/Rust/TS, Tree-sitter includes `{` and `}` in the block range.
        let is_brace_block = if body_range.end_byte > body_range.start_byte {
            source.get(body_range.start_byte) == Some(&b'{')
                && source.get(body_range.end_byte.saturating_sub(1)) == Some(&b'}')
        } else {
            false
        };

        let (before, after) = if is_brace_block {
            // Include `{` in before and `}` in after
            (
                &source[..=body_range.start_byte],
                &source[body_range.end_byte.saturating_sub(1)..],
            )
        } else {
            // E.g., Python: replace exactly the byte range
            // We trim trailing whitespace from `before` to avoid double indentation
            let mut before_slice = &source[..body_range.start_byte];
            while before_slice.last() == Some(&b' ') || before_slice.last() == Some(&b'\t') {
                before_slice = &before_slice[..before_slice.len() - 1];
            }
            (before_slice, &source[body_range.end_byte..])
        };

        // Build the new file content
        let new_content = if is_brace_block {
            if indented.trim().is_empty() {
                // Empty body: `{}`
                [
                    std::str::from_utf8(before)
                        .map_err(|e| io_error_data(format!("source is not valid UTF-8: {e}")))?,
                    std::str::from_utf8(after)
                        .map_err(|e| io_error_data(format!("source is not valid UTF-8: {e}")))?,
                ]
                .concat()
            } else {
                let closing_indent = " ".repeat(body_range.indent_column);
                [
                    std::str::from_utf8(before)
                        .map_err(|e| io_error_data(format!("source is not valid UTF-8: {e}")))?,
                    "\n",
                    &indented,
                    "\n",
                    &closing_indent,
                    std::str::from_utf8(after)
                        .map_err(|e| io_error_data(format!("source is not valid UTF-8: {e}")))?,
                ]
                .concat()
            }
        } else {
            // Python
            [
                std::str::from_utf8(before)
                    .map_err(|e| io_error_data(format!("source is not valid UTF-8: {e}")))?,
                &indented,
                std::str::from_utf8(after)
                    .map_err(|e| io_error_data(format!("source is not valid UTF-8: {e}")))?,
            ]
            .concat()
        };

        let new_bytes = new_content.as_bytes();

        // ── Step 8: LSP validation (in-memory, before disk write) ────────
        let original_str = std::str::from_utf8(&source)
            .map_err(|e| io_error_data(format!("source is not valid UTF-8: {e}")));
        let validation_outcome = match original_str {
            Ok(orig) => {
                self.run_lsp_validation(
                    &semantic_path.file_path,
                    orig,
                    &new_content,
                    params.ignore_validation_failures,
                )
                .await
            }
            Err(_) => ValidationOutcome {
                validation: EditValidation::skipped(),
                skipped: Some(true),
                skipped_reason: Some("utf8_error".to_owned()),
                should_block: false,
            },
        };

        if validation_outcome.should_block {
            let introduced = validation_outcome.validation.introduced_errors.clone();
            let err = PathfinderError::ValidationFailed {
                count: introduced.len(),
                introduced_errors: introduced,
            };
            return Err(pathfinder_to_error_data(&err));
        }

        // ── Step 9 & 10: TOCTOU late-check & Write ────────────────────────
        let new_hash = self
            .flush_edit_with_toctou(&semantic_path, &current_hash, new_bytes)
            .await?;

        let duration_ms = start.elapsed().as_millis();
        tracing::info!(
            tool = "replace_body",
            semantic_path = %params.semantic_path,
            duration_ms,
            new_version_hash = new_hash.as_str(),
            engines_used = ?["tree-sitter"],
            "replace_body: complete"
        );

        Ok(Json(EditResponse {
            success: true,
            new_version_hash: Some(new_hash.as_str().to_owned()),
            formatted: false,
            validation: validation_outcome.validation,
            validation_skipped: validation_outcome.skipped,
            validation_skipped_reason: validation_outcome.skipped_reason,
        }))
    }

    /// Core logic for the `replace_full` tool (PRD Epic 5, Story 5.4).
    ///
    /// Replaces the entire declaration of a symbol (including decorators/docs)
    /// in place on disk, using OCC `base_version` to guard against races.
    #[instrument(skip(self, params), fields(semantic_path = %params.semantic_path))]
    #[expect(
        clippy::too_many_lines,
        reason = "Linear 10-step edit pipeline, same structure as replace_body_impl. Splitting into sub-functions would add indirection without reducing cognitive load."
    )]
    pub(crate) async fn replace_full_impl(
        &self,
        params: ReplaceFullParams,
    ) -> Result<Json<EditResponse>, ErrorData> {
        let start = std::time::Instant::now();
        tracing::info!(
            tool = "replace_full",
            semantic_path = %params.semantic_path,
            "replace_full: start"
        );

        let Some(semantic_path) = SemanticPath::parse(&params.semantic_path) else {
            return Err(io_error_data(format!(
                "invalid semantic path: {}",
                params.semantic_path
            )));
        };

        if semantic_path.is_bare_file() {
            let err = PathfinderError::InvalidTarget {
                semantic_path: params.semantic_path.clone(),
                reason: "replace_full requires a symbol path (e.g., src/auth.ts::Login). \
                         Use write_file to replace file content."
                    .to_owned(),
            };
            return Err(pathfinder_to_error_data(&err));
        }

        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            return Err(pathfinder_to_error_data(&e));
        }

        let (full_range, source, current_hash) = match self
            .surgeon
            .resolve_full_range(self.workspace_root.path(), &semantic_path)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Err(crate::server::helpers::treesitter_error_to_error_data(e));
            }
        };

        let claimed = VersionHash::from_raw(params.base_version.clone());
        if claimed != current_hash {
            let err = PathfinderError::VersionMismatch {
                path: semantic_path.file_path.clone(),
                current_version_hash: current_hash.as_str().to_owned(),
            };
            return Err(pathfinder_to_error_data(&err));
        }

        // Normalize and indent the new code
        let normalized = normalize_for_full_replace(&params.new_code);
        let indented = dedent_then_reindent(&normalized, full_range.indent_column);

        let before = &source[..full_range.start_byte];
        let after = &source[full_range.end_byte..];

        let mut new_bytes = Vec::with_capacity(before.len() + indented.len() + after.len());
        new_bytes.extend_from_slice(before);
        new_bytes.extend_from_slice(indented.as_bytes());
        new_bytes.extend_from_slice(after);

        let original_str = std::str::from_utf8(&source)
            .map_err(|e| io_error_data(format!("source is not valid UTF-8: {e}")));
        let new_content_str = std::str::from_utf8(&new_bytes)
            .map_err(|e| io_error_data(format!("new content is not valid UTF-8: {e}")));
        let validation_outcome = match (original_str, new_content_str) {
            (Ok(orig), Ok(new)) => {
                self.run_lsp_validation(
                    &semantic_path.file_path,
                    orig,
                    new,
                    params.ignore_validation_failures,
                )
                .await
            }
            _ => ValidationOutcome {
                validation: EditValidation::skipped(),
                skipped: Some(true),
                skipped_reason: Some("utf8_error".to_owned()),
                should_block: false,
            },
        };

        if validation_outcome.should_block {
            let introduced = validation_outcome.validation.introduced_errors.clone();
            let err = PathfinderError::ValidationFailed {
                count: introduced.len(),
                introduced_errors: introduced,
            };
            return Err(pathfinder_to_error_data(&err));
        }

        let new_hash = self
            .flush_edit_with_toctou(&semantic_path, &current_hash, &new_bytes)
            .await?;

        let duration_ms = start.elapsed().as_millis();
        tracing::info!(
            tool = "replace_full",
            semantic_path = %params.semantic_path,
            duration_ms,
            new_version_hash = new_hash.as_str(),
            engines_used = ?["tree-sitter"],
            "replace_full: complete"
        );

        Ok(Json(EditResponse {
            success: true,
            new_version_hash: Some(new_hash.as_str().to_owned()),
            formatted: false,
            validation: validation_outcome.validation,
            validation_skipped: validation_outcome.skipped,
            validation_skipped_reason: validation_outcome.skipped_reason,
        }))
    }

    /// Core logic for the `insert_before` tool (PRD Epic 5, Story 5.5).
    #[instrument(skip(self, params), fields(semantic_path = %params.semantic_path))]
    #[expect(
        clippy::too_many_lines,
        reason = "Edit pipeline with BOF/symbol branch at the resolution step, followed by normalization, validation and write. Structurally identical to replace_body; splitting further would obscure the symmetric design."
    )]
    pub(crate) async fn insert_before_impl(
        &self,
        params: InsertBeforeParams,
    ) -> Result<Json<EditResponse>, ErrorData> {
        let start = std::time::Instant::now();
        let Some(semantic_path) = SemanticPath::parse(&params.semantic_path) else {
            return Err(io_error_data("invalid semantic path"));
        };

        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            return Err(pathfinder_to_error_data(&e));
        }

        let (insert_byte, indent_column, source, current_hash) = if semantic_path.is_bare_file() {
            let absolute_path = self.workspace_root.resolve(&semantic_path.file_path);
            let bytes = tokio::fs::read(&absolute_path)
                .await
                .map_err(|e| io_error_data(format!("failed to read file: {e}")))?;
            let hash = VersionHash::compute(&bytes);
            (0, 0, bytes, hash)
        } else {
            let (symbol_range, source, hash) = match self
                .surgeon
                .resolve_symbol_range(self.workspace_root.path(), &semantic_path)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    return Err(crate::server::helpers::treesitter_error_to_error_data(e));
                }
            };
            (
                symbol_range.start_byte,
                symbol_range.indent_column,
                source,
                hash,
            )
        };

        let claimed = VersionHash::from_raw(params.base_version.clone());
        if claimed != current_hash {
            let err = PathfinderError::VersionMismatch {
                path: semantic_path.file_path.clone(),
                current_version_hash: current_hash.as_str().to_owned(),
            };
            return Err(pathfinder_to_error_data(&err));
        }

        let normalized = normalize_for_full_replace(&params.new_code);
        let indented = dedent_then_reindent(&normalized, indent_column);

        // Splice: insert at insert_byte with a double newline separator
        let before = &source[..insert_byte];
        let after = &source[insert_byte..];

        // Use a heuristic to avoid too many newlines if `after` already starts with them
        let sep = if after.starts_with(b"\n\n") {
            ""
        } else if after.starts_with(b"\n") {
            "\n"
        } else {
            "\n\n"
        };

        // Also ensure the indented part has trailing newline if it doesn't
        let trailing = if indented.ends_with('\n') { "" } else { "\n" };

        let mut new_bytes = Vec::with_capacity(
            before.len() + indented.len() + sep.len() + trailing.len() + after.len(),
        );
        new_bytes.extend_from_slice(before);
        new_bytes.extend_from_slice(indented.as_bytes());
        new_bytes.extend_from_slice(trailing.as_bytes());
        new_bytes.extend_from_slice(sep.as_bytes());
        new_bytes.extend_from_slice(after);

        let original_str = std::str::from_utf8(&source)
            .map_err(|e| io_error_data(format!("source is not valid UTF-8: {e}")));
        let new_str = std::str::from_utf8(&new_bytes)
            .map_err(|e| io_error_data(format!("new content is not valid UTF-8: {e}")));
        let validation_outcome = match (original_str, new_str) {
            (Ok(orig), Ok(new)) => {
                self.run_lsp_validation(
                    &semantic_path.file_path,
                    orig,
                    new,
                    params.ignore_validation_failures,
                )
                .await
            }
            _ => ValidationOutcome {
                validation: EditValidation::skipped(),
                skipped: Some(true),
                skipped_reason: Some("utf8_error".to_owned()),
                should_block: false,
            },
        };

        if validation_outcome.should_block {
            let introduced = validation_outcome.validation.introduced_errors.clone();
            let err = PathfinderError::ValidationFailed {
                count: introduced.len(),
                introduced_errors: introduced,
            };
            return Err(pathfinder_to_error_data(&err));
        }

        let new_hash = self
            .flush_edit_with_toctou(&semantic_path, &current_hash, &new_bytes)
            .await?;

        let duration_ms = start.elapsed().as_millis();
        tracing::info!(
            tool = "insert_before",
            semantic_path = %params.semantic_path,
            duration_ms,
            new_version_hash = new_hash.as_str(),
            engines_used = ?["tree-sitter"],
            "insert_before: complete"
        );

        Ok(Json(EditResponse {
            success: true,
            new_version_hash: Some(new_hash.as_str().to_owned()),
            formatted: false,
            validation: validation_outcome.validation,
            validation_skipped: validation_outcome.skipped,
            validation_skipped_reason: validation_outcome.skipped_reason,
        }))
    }

    /// Core logic for the `insert_after` tool (PRD Epic 5, Story 5.5).
    #[instrument(skip(self, params), fields(semantic_path = %params.semantic_path))]
    #[expect(
        clippy::too_many_lines,
        reason = "EOF/symbol branch at resolution step, mirroring insert_before_impl. Linear pipeline with equivalent structure; splitting would duplicate boilerplate without clarity gain."
    )]
    pub(crate) async fn insert_after_impl(
        &self,
        params: InsertAfterParams,
    ) -> Result<Json<EditResponse>, ErrorData> {
        let start = std::time::Instant::now();
        let Some(semantic_path) = SemanticPath::parse(&params.semantic_path) else {
            return Err(io_error_data("invalid semantic path"));
        };

        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            return Err(pathfinder_to_error_data(&e));
        }

        let (insert_byte, indent_column, source, current_hash) = if semantic_path.is_bare_file() {
            let absolute_path = self.workspace_root.resolve(&semantic_path.file_path);
            let bytes = tokio::fs::read(&absolute_path)
                .await
                .map_err(|e| io_error_data(format!("failed to read file: {e}")))?;
            let hash = VersionHash::compute(&bytes);
            (bytes.len(), 0, bytes, hash)
        } else {
            let (symbol_range, source, hash) = match self
                .surgeon
                .resolve_symbol_range(self.workspace_root.path(), &semantic_path)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    return Err(crate::server::helpers::treesitter_error_to_error_data(e));
                }
            };
            (
                symbol_range.end_byte,
                symbol_range.indent_column,
                source,
                hash,
            )
        };

        let claimed = VersionHash::from_raw(params.base_version.clone());
        if claimed != current_hash {
            let err = PathfinderError::VersionMismatch {
                path: semantic_path.file_path.clone(),
                current_version_hash: current_hash.as_str().to_owned(),
            };
            return Err(pathfinder_to_error_data(&err));
        }

        let normalized = normalize_for_full_replace(&params.new_code);
        let indented = dedent_then_reindent(&normalized, indent_column);

        let before = &source[..insert_byte];
        let after = &source[insert_byte..];

        let before_sep = if before.ends_with(b"\n\n") {
            ""
        } else if before.ends_with(b"\n") {
            "\n"
        } else {
            "\n\n"
        };
        let after_sep = if indented.ends_with('\n') { "" } else { "\n" };

        let mut new_bytes = Vec::with_capacity(
            before.len() + before_sep.len() + indented.len() + after_sep.len() + after.len(),
        );
        new_bytes.extend_from_slice(before);
        new_bytes.extend_from_slice(before_sep.as_bytes());
        new_bytes.extend_from_slice(indented.as_bytes());
        new_bytes.extend_from_slice(after_sep.as_bytes());
        new_bytes.extend_from_slice(after);

        let original_str = std::str::from_utf8(&source)
            .map_err(|e| io_error_data(format!("source is not valid UTF-8: {e}")));
        let new_str = std::str::from_utf8(&new_bytes)
            .map_err(|e| io_error_data(format!("new content is not valid UTF-8: {e}")));
        let validation_outcome = match (original_str, new_str) {
            (Ok(orig), Ok(new)) => {
                self.run_lsp_validation(
                    &semantic_path.file_path,
                    orig,
                    new,
                    params.ignore_validation_failures,
                )
                .await
            }
            _ => ValidationOutcome {
                validation: EditValidation::skipped(),
                skipped: Some(true),
                skipped_reason: Some("utf8_error".to_owned()),
                should_block: false,
            },
        };

        if validation_outcome.should_block {
            let introduced = validation_outcome.validation.introduced_errors.clone();
            let err = PathfinderError::ValidationFailed {
                count: introduced.len(),
                introduced_errors: introduced,
            };
            return Err(pathfinder_to_error_data(&err));
        }

        let new_hash = self
            .flush_edit_with_toctou(&semantic_path, &current_hash, &new_bytes)
            .await?;

        let duration_ms = start.elapsed().as_millis();
        tracing::info!(
            tool = "insert_after",
            semantic_path = %params.semantic_path,
            duration_ms,
            new_version_hash = new_hash.as_str(),
            engines_used = ?["tree-sitter"],
            "insert_after: complete"
        );

        Ok(Json(EditResponse {
            success: true,
            new_version_hash: Some(new_hash.as_str().to_owned()),
            formatted: false,
            validation: validation_outcome.validation,
            validation_skipped: validation_outcome.skipped,
            validation_skipped_reason: validation_outcome.skipped_reason,
        }))
    }

    /// Core logic for the `delete_symbol` tool (PRD Epic 5, Story 5.6).
    #[instrument(skip(self, params), fields(semantic_path = %params.semantic_path))]
    #[expect(
        clippy::too_many_lines,
        reason = "Deletion pipeline: symbol-only guard, resolve, OCC, blank-line collapse, validation, TOCTOU, write. Each of the 8 steps is distinct; no repeated structure to extract."
    )]
    pub(crate) async fn delete_symbol_impl(
        &self,
        params: DeleteSymbolParams,
    ) -> Result<Json<EditResponse>, ErrorData> {
        let start = std::time::Instant::now();
        let Some(semantic_path) = SemanticPath::parse(&params.semantic_path) else {
            return Err(io_error_data("invalid semantic path"));
        };

        if semantic_path.is_bare_file() {
            let err = PathfinderError::InvalidTarget {
                semantic_path: params.semantic_path.clone(),
                reason: "delete_symbol requires a symbol path (e.g., src/auth.ts::Login)."
                    .to_owned(),
            };
            return Err(pathfinder_to_error_data(&err));
        }

        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            return Err(pathfinder_to_error_data(&e));
        }

        let (full_range, source, current_hash) = match self
            .surgeon
            .resolve_full_range(self.workspace_root.path(), &semantic_path)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Err(crate::server::helpers::treesitter_error_to_error_data(e));
            }
        };

        let claimed = VersionHash::from_raw(params.base_version.clone());
        if claimed != current_hash {
            let err = PathfinderError::VersionMismatch {
                path: semantic_path.file_path.clone(),
                current_version_hash: current_hash.as_str().to_owned(),
            };
            return Err(pathfinder_to_error_data(&err));
        }

        // Collapse whitespace: If deleting a symbol leaves more than one consecutive blank line, collapse it.
        // Or simply: strip the symbol, then normalise the gap.
        let before_end = full_range.start_byte;
        let after_start = full_range.end_byte;

        // Trim trailing whitespace (except newlines if we want, but trimming all is safer)
        let mut b_end = before_end;
        while b_end > 0 && source[b_end - 1].is_ascii_whitespace() {
            b_end -= 1;
        }

        let mut a_start = after_start;
        while a_start < source.len() && source[a_start].is_ascii_whitespace() {
            a_start += 1;
        }

        let before = &source[..b_end];
        let after = &source[a_start..];

        // Insert exactly two newlines (one blank line) if neither is empty
        let sep = if before.is_empty() || after.is_empty() {
            b"\n" as &[u8]
        } else {
            b"\n\n"
        };

        let mut new_bytes = Vec::with_capacity(before.len() + sep.len() + after.len());
        new_bytes.extend_from_slice(before);
        new_bytes.extend_from_slice(sep);
        new_bytes.extend_from_slice(after);

        let original_str = std::str::from_utf8(&source)
            .map_err(|e| io_error_data(format!("source is not valid UTF-8: {e}")));
        let new_str = std::str::from_utf8(&new_bytes)
            .map_err(|e| io_error_data(format!("new content is not valid UTF-8: {e}")));
        let validation_outcome = match (original_str, new_str) {
            (Ok(orig), Ok(new)) => {
                self.run_lsp_validation(
                    &semantic_path.file_path,
                    orig,
                    new,
                    params.ignore_validation_failures,
                )
                .await
            }
            _ => ValidationOutcome {
                validation: EditValidation::skipped(),
                skipped: Some(true),
                skipped_reason: Some("utf8_error".to_owned()),
                should_block: false,
            },
        };

        if validation_outcome.should_block {
            let introduced = validation_outcome.validation.introduced_errors.clone();
            let err = PathfinderError::ValidationFailed {
                count: introduced.len(),
                introduced_errors: introduced,
            };
            return Err(pathfinder_to_error_data(&err));
        }

        let new_hash = self
            .flush_edit_with_toctou(&semantic_path, &current_hash, &new_bytes)
            .await?;

        let duration_ms = start.elapsed().as_millis();
        tracing::info!(
            tool = "delete_symbol",
            semantic_path = %params.semantic_path,
            duration_ms,
            new_version_hash = new_hash.as_str(),
            engines_used = ?["tree-sitter"],
            "delete_symbol: complete"
        );

        Ok(Json(EditResponse {
            success: true,
            new_version_hash: Some(new_hash.as_str().to_owned()),
            formatted: false,
            validation: validation_outcome.validation,
            validation_skipped: validation_outcome.skipped,
            validation_skipped_reason: validation_outcome.skipped_reason,
        }))
    }

    /// Core logic for the `validate_only` tool (PRD Epic 5, Story 5.7).
    ///
    /// Dry-runs an edit operation WITHOUT writing to disk. Uses the same pipeline
    /// for resolution, normalization, and OCC checking, but skips the TOCTOU check
    /// and disk write. Always returns `new_version_hash: None`.
    #[instrument(skip(self, params), fields(semantic_path = %params.semantic_path, edit_type = %params.edit_type))]
    pub(crate) async fn validate_only_impl(
        &self,
        params: ValidateOnlyParams,
    ) -> Result<Json<EditResponse>, ErrorData> {
        let start = std::time::Instant::now();
        tracing::info!(
            tool = "validate_only",
            semantic_path = %params.semantic_path,
            edit_type = %params.edit_type,
            "validate_only: start"
        );

        let Some(semantic_path) = SemanticPath::parse(&params.semantic_path) else {
            return Err(io_error_data("invalid semantic path"));
        };

        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            return Err(pathfinder_to_error_data(&e));
        }

        // Resolve the current version hash for the target path+type and OCC-check it.
        let current_hash = self
            .resolve_version_hash_for_edit_type(
                &semantic_path,
                &params.semantic_path,
                params.edit_type.as_str(),
            )
            .await?;

        let claimed = VersionHash::from_raw(params.base_version.clone());
        if claimed != current_hash {
            let err = PathfinderError::VersionMismatch {
                path: semantic_path.file_path.clone(),
                current_version_hash: current_hash.as_str().to_owned(),
            };
            return Err(pathfinder_to_error_data(&err));
        }

        // validate_only: no disk write, so we skip actual LSP validation here.
        // The OCC + Sandbox check is the primary purpose of this tool.
        // A future enhancement could perform read-only LSP diagnostics.

        let duration_ms = start.elapsed().as_millis();
        tracing::info!(
            tool = "validate_only",
            semantic_path = %params.semantic_path,
            duration_ms,
            engines_used = ?["tree-sitter"],
            "validate_only: complete"
        );

        Ok(Json(EditResponse {
            success: true,
            new_version_hash: None, // No file written
            formatted: false,
            validation: EditValidation::skipped(),
            validation_skipped: Some(true),
            validation_skipped_reason: Some("validate_only_no_write".to_owned()),
        }))
    }

    /// Resolve the current on-disk `VersionHash` for the path targeted by a
    /// `validate_only` call.
    ///
    /// Each edit type uses a different Surgeon method to locate the symbol,
    /// so the resolution path differs. This helper centralises that dispatch
    /// and returns the hash without performing the OCC comparison — that remains
    /// the caller's responsibility.
    async fn resolve_version_hash_for_edit_type(
        &self,
        semantic_path: &SemanticPath,
        raw_path: &str,
        edit_type: &str,
    ) -> Result<VersionHash, ErrorData> {
        use crate::server::helpers::treesitter_error_to_error_data;

        match edit_type {
            "replace_body" => {
                if semantic_path.is_bare_file() {
                    return Err(pathfinder_to_error_data(&PathfinderError::InvalidTarget {
                        semantic_path: raw_path.to_owned(),
                        reason: "replace_body requires a symbol path".to_owned(),
                    }));
                }
                let (_, _, hash) = self
                    .surgeon
                    .resolve_body_range(self.workspace_root.path(), semantic_path)
                    .await
                    .map_err(treesitter_error_to_error_data)?;
                Ok(hash)
            }
            "replace_full" => {
                if semantic_path.is_bare_file() {
                    return Err(pathfinder_to_error_data(&PathfinderError::InvalidTarget {
                        semantic_path: raw_path.to_owned(),
                        reason: "replace_full requires a symbol path".to_owned(),
                    }));
                }
                let (_, _, hash) = self
                    .surgeon
                    .resolve_full_range(self.workspace_root.path(), semantic_path)
                    .await
                    .map_err(treesitter_error_to_error_data)?;
                Ok(hash)
            }
            "insert_before" | "insert_after" => {
                if semantic_path.is_bare_file() {
                    let absolute_path = self.workspace_root.resolve(&semantic_path.file_path);
                    let bytes = tokio::fs::read(&absolute_path)
                        .await
                        .map_err(|e| io_error_data(format!("failed to read file: {e}")))?;
                    Ok(VersionHash::compute(&bytes))
                } else {
                    let (_, _, hash) = self
                        .surgeon
                        .resolve_symbol_range(self.workspace_root.path(), semantic_path)
                        .await
                        .map_err(treesitter_error_to_error_data)?;
                    Ok(hash)
                }
            }
            "delete" => {
                if semantic_path.is_bare_file() {
                    return Err(pathfinder_to_error_data(&PathfinderError::InvalidTarget {
                        semantic_path: raw_path.to_owned(),
                        reason: "delete requires a symbol path".to_owned(),
                    }));
                }
                let (_, _, hash) = self
                    .surgeon
                    .resolve_full_range(self.workspace_root.path(), semantic_path)
                    .await
                    .map_err(treesitter_error_to_error_data)?;
                Ok(hash)
            }
            unknown => Err(io_error_data(format!("unknown edit_type: {unknown}"))),
        }
    }

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
    #[expect(
        clippy::too_many_lines,
        reason = "Sequential LSP validation pipeline with 5 distinct failure modes (no_lsp, unsupported, pre-diag timeout, post-diag timeout, blocking failure), each requiring its own ValidationOutcome. No repeated structure to extract without obscuring the error taxonomy."
    )]
    async fn run_lsp_validation(
        &self,
        file_path: &Path,
        original_content: &str,
        new_content: &str,
        ignore_validation_failures: bool,
    ) -> ValidationOutcome {
        // version 1 = original, version 2 = post-edit
        let relative = file_path;
        let workspace = self.workspace_root.path();

        // ── did_open (original content, version 1) ──
        if let Err(e) = self
            .lawyer
            .did_open(workspace, relative, original_content)
            .await
        {
            let (skipped_reason, should_log) = match &e {
                LspError::NoLspAvailable => ("no_lsp", false),
                LspError::UnsupportedCapability { .. } => ("unsupported", false),
                _ => ("lsp_error", true),
            };
            if should_log {
                tracing::warn!(error = %e, "validation: did_open failed");
            }
            return ValidationOutcome {
                validation: EditValidation::skipped(),
                skipped: Some(true),
                skipped_reason: Some(skipped_reason.to_owned()),
                should_block: false,
            };
        }

        // ── pre-edit diagnostics ──
        let pre_diags = match self.lawyer.pull_diagnostics(workspace, relative).await {
            Ok(d) => d,
            Err(LspError::UnsupportedCapability { .. }) => {
                // LSP running but doesn't support Pull Diagnostics
                return ValidationOutcome {
                    validation: EditValidation::skipped(),
                    skipped: Some(true),
                    skipped_reason: Some("pull_diagnostics_unsupported".to_owned()),
                    should_block: false,
                };
            }
            Err(e) => {
                tracing::warn!(error = %e, "validation: pre-edit pull_diagnostics failed");
                return ValidationOutcome {
                    validation: EditValidation::skipped(),
                    skipped: Some(true),
                    skipped_reason: Some("diagnostic_timeout".to_owned()),
                    should_block: false,
                };
            }
        };

        // ── did_change (new content, version 2) ──
        if let Err(e) = self
            .lawyer
            .did_change(workspace, relative, new_content, 2)
            .await
        {
            tracing::warn!(error = %e, "validation: did_change failed");
            return ValidationOutcome {
                validation: EditValidation::skipped(),
                skipped: Some(true),
                skipped_reason: Some("lsp_error".to_owned()),
                should_block: false,
            };
        }

        // ── post-edit diagnostics ──
        let post_diags = match self.lawyer.pull_diagnostics(workspace, relative).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, "validation: post-edit pull_diagnostics failed");
                return ValidationOutcome {
                    validation: EditValidation::skipped(),
                    skipped: Some(true),
                    skipped_reason: Some("diagnostic_timeout".to_owned()),
                    should_block: false,
                };
            }
        };

        // ── revert LSP state to original (fire-and-forget) ──
        let _ = self
            .lawyer
            .did_change(workspace, relative, original_content, 3)
            .await;

        // ── diff diagnostics ──────────────────────
        let diff = diff_diagnostics(&pre_diags, &post_diags);
        let has_new_errors = diff.has_new_errors();

        let introduced: Vec<DiagnosticError> = diff
            .introduced
            .iter()
            .map(|d| DiagnosticError {
                severity: d.severity as u8,
                code: d.code.clone().unwrap_or_default(),
                message: d.message.clone(),
                file: d.file.clone(),
            })
            .collect();

        let resolved: Vec<DiagnosticError> = diff
            .resolved
            .iter()
            .map(|d| DiagnosticError {
                severity: d.severity as u8,
                code: d.code.clone().unwrap_or_default(),
                message: d.message.clone(),
                file: d.file.clone(),
            })
            .collect();

        if has_new_errors && !ignore_validation_failures {
            ValidationOutcome {
                validation: EditValidation {
                    status: "failed".to_owned(),
                    introduced_errors: introduced,
                    resolved_errors: resolved,
                },
                skipped: None,
                skipped_reason: None,
                should_block: true,
            }
        } else {
            ValidationOutcome {
                validation: EditValidation {
                    status: "passed".to_owned(),
                    introduced_errors: introduced,
                    resolved_errors: resolved,
                },
                skipped: None,
                skipped_reason: None,
                should_block: false,
            }
        }
    }

    /// Helper to perform the final TOCTOU check and write the modified file to disk.
    /// Re-reads the file, ensures its current hash still matches `current_hash`,
    /// then writes `new_bytes` to disk in-place.
    async fn flush_edit_with_toctou(
        &self,
        semantic_path: &SemanticPath,
        current_hash: &VersionHash,
        new_bytes: &[u8],
    ) -> Result<VersionHash, ErrorData> {
        let absolute_path = self.workspace_root.resolve(&semantic_path.file_path);

        let disk_bytes = tokio::fs::read(&absolute_path)
            .await
            .map_err(|e| io_error_data(format!("TOCTOU re-read failed: {e}")))?;
        let disk_hash = VersionHash::compute(&disk_bytes);

        if disk_hash != *current_hash {
            let err = PathfinderError::VersionMismatch {
                path: semantic_path.file_path.clone(),
                current_version_hash: disk_hash.as_str().to_owned(),
            };
            return Err(pathfinder_to_error_data(&err));
        }

        // Use `tokio::fs::write` for in-place write (preserves inode, avoids
        // rename-swap artifacts that would confuse file watchers).
        tokio::fs::write(&absolute_path, new_bytes)
            .await
            .map_err(|e| io_error_data(format!("write failed: {e}")))?;

        Ok(VersionHash::compute(new_bytes))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::{VersionHash, WorkspaceRoot};
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::mock::MockSurgeon;
    use pathfinder_treesitter::surgeon::BodyRange;
    use rmcp::handler::server::wrapper::Parameters;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn make_server_dyn(
        ws_dir: &tempfile::TempDir,
        surgeon: Arc<dyn pathfinder_treesitter::surgeon::Surgeon>,
    ) -> PathfinderServer {
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);
        PathfinderServer::with_engines(ws, config, sandbox, Arc::new(MockScout::default()), surgeon)
    }

    fn make_server(ws_dir: &tempfile::TempDir, mock_surgeon: MockSurgeon) -> PathfinderServer {
        make_server_dyn(ws_dir, Arc::new(mock_surgeon))
    }

    fn make_body_range(open: usize, close: usize, indent: usize, body_indent: usize) -> BodyRange {
        BodyRange {
            start_byte: open,
            end_byte: close,
            indent_column: indent,
            body_indent_column: body_indent,
        }
    }

    // ── replace_body_success ─────────────────────────────────────────

    #[tokio::test]
    async fn test_replace_body_success() {
        let ws_dir = tempdir().expect("temp dir");

        // Write a simple Go file
        let src = "func Login() {\n    // old body\n}\n";
        let filepath = "src/auth.go";
        let abs = ws_dir.path().join(filepath);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, src).unwrap();

        let src_bytes = src.as_bytes();
        let hash = VersionHash::compute(src_bytes);

        // Locate braces: `{` is at position 13, `}` is at position 31 (inclusive), so length is 32.
        // Tree-sitter is exclusive of end_byte, so it should be close + 1.
        let open = src.find('{').unwrap();
        let close = src.rfind('}').unwrap() + 1;

        let mock_surgeon = MockSurgeon::new();
        mock_surgeon
            .resolve_body_range_results
            .lock()
            .unwrap()
            .push(Ok((
                make_body_range(open, close, 0, 4),
                src_bytes.to_vec(),
                hash.clone(),
            )));

        let server = make_server(&ws_dir, mock_surgeon);

        let params = ReplaceBodyParams {
            semantic_path: format!("{filepath}::Login"),
            base_version: hash.as_str().to_owned(),
            new_code: "    return nil".to_owned(),
            ignore_validation_failures: false,
        };

        let result = server
            .replace_body(Parameters(params))
            .await
            .expect("should succeed");
        let resp = result.0;

        assert!(resp.success);
        assert!(resp.new_version_hash.is_some());
        assert_eq!(resp.validation.status, "skipped");
        assert_eq!(resp.validation_skipped, Some(true));

        // Verify the file was actually written
        let written = std::fs::read_to_string(&abs).unwrap();
        assert!(written.contains("return nil"), "written: {written}");
        assert!(!written.contains("old body"), "written: {written}");
    }

    // ── replace_body_version_mismatch ────────────────────────────────

    #[tokio::test]
    async fn test_replace_body_version_mismatch() {
        let ws_dir = tempdir().expect("temp dir");

        let src = "func Login() {\n    // body\n}\n";
        let filepath = "src/auth.go";
        let abs = ws_dir.path().join(filepath);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, src).unwrap();

        let src_bytes = src.as_bytes();
        let real_hash = VersionHash::compute(src_bytes);
        let stale_hash = "sha256:stale000".to_owned();

        let open = src.find('{').unwrap();
        let close = src.rfind('}').unwrap() + 1;

        let mock_surgeon = MockSurgeon::new();
        mock_surgeon
            .resolve_body_range_results
            .lock()
            .unwrap()
            .push(Ok((
                make_body_range(open, close, 0, 4),
                src_bytes.to_vec(),
                real_hash,
            )));

        let server = make_server(&ws_dir, mock_surgeon);

        let params = ReplaceBodyParams {
            semantic_path: format!("{filepath}::Login"),
            base_version: stale_hash,
            new_code: "return nil".to_owned(),
            ignore_validation_failures: false,
        };

        let result = server.replace_body(Parameters(params)).await;
        let Err(err) = result else {
            panic!("expected VERSION_MISMATCH error");
        };

        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "VERSION_MISMATCH", "got: {err:?}");
    }

    // ── replace_body_symbol_not_found ────────────────────────────────

    #[tokio::test]
    async fn test_replace_body_symbol_not_found() {
        let ws_dir = tempdir().expect("temp dir");

        let mock_surgeon = MockSurgeon::new();
        mock_surgeon
            .resolve_body_range_results
            .lock()
            .unwrap()
            .push(Err(pathfinder_treesitter::SurgeonError::SymbolNotFound {
                path: "src/auth.go::Lgon".to_owned(),
                did_you_mean: vec!["Login".to_owned()],
            }));

        let server = make_server(&ws_dir, mock_surgeon);

        let params = ReplaceBodyParams {
            semantic_path: "src/auth.go::Lgon".to_owned(),
            base_version: "sha256:any".to_owned(),
            new_code: "return nil".to_owned(),
            ignore_validation_failures: false,
        };

        let result = server.replace_body(Parameters(params)).await;
        let Err(err) = result else {
            panic!("expected SYMBOL_NOT_FOUND error");
        };

        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "SYMBOL_NOT_FOUND", "got: {err:?}");
    }

    // ── replace_body_access_denied ────────────────────────────────────

    #[tokio::test]
    async fn test_replace_body_access_denied() {
        let ws_dir = tempdir().expect("temp dir");
        let server = make_server(&ws_dir, MockSurgeon::new());

        let params = ReplaceBodyParams {
            semantic_path: ".git/config::Login".to_owned(),
            base_version: "sha256:any".to_owned(),
            new_code: "body".to_owned(),
            ignore_validation_failures: false,
        };

        let result = server.replace_body(Parameters(params)).await;
        let Err(err) = result else {
            panic!("expected ACCESS_DENIED error");
        };

        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "ACCESS_DENIED", "got: {err:?}");
    }

    // ── replace_body_brace_leniency ───────────────────────────────────

    #[tokio::test]
    async fn test_replace_body_brace_leniency() {
        // LLM wraps code in braces — should be auto-stripped
        let ws_dir = tempdir().expect("temp dir");

        let src = "func Login() {\n    // old\n}\n";
        let filepath = "src/auth.go";
        let abs = ws_dir.path().join(filepath);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, src).unwrap();

        let src_bytes = src.as_bytes();
        let hash = VersionHash::compute(src_bytes);

        let open = src.find('{').unwrap();
        let close = src.rfind('}').unwrap() + 1;

        let mock_surgeon = MockSurgeon::new();
        mock_surgeon
            .resolve_body_range_results
            .lock()
            .unwrap()
            .push(Ok((
                make_body_range(open, close, 0, 4),
                src_bytes.to_vec(),
                hash.clone(),
            )));

        let server = make_server(&ws_dir, mock_surgeon);

        // Pass code wrapped in braces — brace-leniency should strip them
        let params = ReplaceBodyParams {
            semantic_path: format!("{filepath}::Login"),
            base_version: hash.as_str().to_owned(),
            new_code: "{ return nil }".to_owned(),
            ignore_validation_failures: false,
        };

        let result = server
            .replace_body(Parameters(params))
            .await
            .expect("should succeed despite outer braces");
        assert!(result.0.success);

        let written = std::fs::read_to_string(&abs).unwrap();
        // Should NOT contain `{{ return nil }}` — braces should have been stripped
        assert!(!written.contains("{ return nil }"), "written: {written}");
        assert!(written.contains("return nil"), "written: {written}");
    }

    // ── replace_body_bare_file_rejected ──────────────────────────────

    #[tokio::test]
    async fn test_replace_body_bare_file_rejected() {
        let ws_dir = tempdir().expect("temp dir");
        let server = make_server(&ws_dir, MockSurgeon::new());

        let params = ReplaceBodyParams {
            semantic_path: "src/auth.go".to_owned(), // no :: symbol
            base_version: "sha256:any".to_owned(),
            new_code: "body".to_owned(),
            ignore_validation_failures: false,
        };

        let result = server.replace_body(Parameters(params)).await;
        let Err(err) = result else {
            panic!("expected INVALID_TARGET error");
        };

        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "INVALID_TARGET", "got: {err:?}");
    }

    // ── Integration Tests with Real TreeSitterSurgeon ───────────────────

    #[tokio::test]
    async fn test_replace_body_real_parser_go() {
        use pathfinder_treesitter::treesitter_surgeon::TreeSitterSurgeon;
        let ws_dir = tempdir().expect("temp dir");

        let src = "package main\n\nfunc Login() {\n    // old body\n}\n";
        let filepath = "src/auth.go";
        let abs = ws_dir.path().join(filepath);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, src).unwrap();

        let hash = VersionHash::compute(src.as_bytes());

        let real_surgeon = Arc::new(TreeSitterSurgeon::new(10));
        let server = make_server_dyn(&ws_dir, real_surgeon);

        let params = ReplaceBodyParams {
            semantic_path: format!("{filepath}::Login"),
            base_version: hash.as_str().to_owned(),
            new_code: "    return nil".to_owned(),
            ignore_validation_failures: false,
        };

        let result = server
            .replace_body(Parameters(params))
            .await
            .expect("should succeed");
        assert!(result.0.success);

        let written = std::fs::read_to_string(&abs).unwrap();
        assert!(written.contains("return nil"), "written: {written}");
        assert!(!written.contains("old body"), "written: {written}");
        // Make sure braces are preserved
        assert!(written.contains("func Login() {\n"), "written: {written}");
        assert!(written.ends_with("}\n"), "written: {written}");
    }

    #[tokio::test]
    async fn test_replace_body_real_parser_python() {
        use pathfinder_treesitter::treesitter_surgeon::TreeSitterSurgeon;
        let ws_dir = tempdir().expect("temp dir");

        let src = "def login():\n    # old body\n    pass\n";
        let filepath = "src/auth.py";
        let abs = ws_dir.path().join(filepath);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, src).unwrap();

        let hash = VersionHash::compute(src.as_bytes());

        let real_surgeon = Arc::new(TreeSitterSurgeon::new(10));
        let server = make_server_dyn(&ws_dir, real_surgeon);

        let params = ReplaceBodyParams {
            semantic_path: format!("{filepath}::login"),
            base_version: hash.as_str().to_owned(),
            new_code: "    return None".to_owned(),
            ignore_validation_failures: false,
        };

        let result = server
            .replace_body(Parameters(params))
            .await
            .expect("should succeed");
        assert!(result.0.success);

        let written = std::fs::read_to_string(&abs).unwrap();

        let expected = "def login():\n    # old body\n    return None\n";
        assert_eq!(written, expected);
    }

    // ── Integration Tests for New Tools ─────────────────────────────────────

    #[tokio::test]
    async fn test_replace_full_real_parser_go() {
        use pathfinder_treesitter::treesitter_surgeon::TreeSitterSurgeon;
        let ws_dir = tempdir().expect("temp dir");

        let src = "package main\n\n// DOC\nfunc Login() {\n    // old body\n}\n\nfunc Other() {}";
        let filepath = "src/auth.go";
        let abs = ws_dir.path().join(filepath);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, src).unwrap();

        let hash = VersionHash::compute(src.as_bytes());

        let real_surgeon = Arc::new(TreeSitterSurgeon::new(10));
        let server = make_server_dyn(&ws_dir, real_surgeon);

        let params = ReplaceFullParams {
            semantic_path: format!("{filepath}::Login"),
            base_version: hash.as_str().to_owned(),
            new_code: "func NewLogin() {\n    return nil\n}".to_owned(),
            ignore_validation_failures: false,
        };

        let result = server
            .replace_full(Parameters(params))
            .await
            .expect("should succeed");
        assert!(result.0.success);

        let written = std::fs::read_to_string(&abs).unwrap();
        assert!(written.contains("func NewLogin"));
        assert!(!written.contains("func Login"));
        assert!(
            !written.contains("// DOC"),
            "Doc comment should be replaced"
        );
    }

    #[tokio::test]
    async fn test_insert_before_bare_file() {
        let ws_dir = tempdir().expect("temp dir");

        let src = "package main\n";
        let filepath = "src/main.go";
        let abs = ws_dir.path().join(filepath);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, src).unwrap();

        let hash = VersionHash::compute(src.as_bytes());

        let server = make_server(&ws_dir, MockSurgeon::new());

        let params = InsertBeforeParams {
            semantic_path: filepath.to_owned(), // BOF
            base_version: hash.as_str().to_owned(),
            new_code: "// License\n".to_owned(),
            ignore_validation_failures: false,
        };

        let result = server
            .insert_before(Parameters(params))
            .await
            .expect("should succeed");
        assert!(result.0.success);

        let written = std::fs::read_to_string(&abs).unwrap();
        assert!(written.starts_with("// License\n"));
        assert!(written.contains("package main"));
    }

    #[tokio::test]
    async fn test_insert_after_bare_file() {
        let ws_dir = tempdir().expect("temp dir");

        let src = "package main\n";
        let filepath = "src/main.go";
        let abs = ws_dir.path().join(filepath);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, src).unwrap();

        let hash = VersionHash::compute(src.as_bytes());

        let server = make_server(&ws_dir, MockSurgeon::new());

        let params = InsertAfterParams {
            semantic_path: filepath.to_owned(), // EOF
            base_version: hash.as_str().to_owned(),
            new_code: "func append() {}".to_owned(),
            ignore_validation_failures: false,
        };

        let result = server
            .insert_after(Parameters(params))
            .await
            .expect("should succeed");
        assert!(result.0.success);

        let written = std::fs::read_to_string(&abs).unwrap();
        assert!(written.contains("package main\n\nfunc append() {}"));
    }

    #[tokio::test]
    async fn test_delete_symbol_real_parser_go() {
        use pathfinder_treesitter::treesitter_surgeon::TreeSitterSurgeon;
        let ws_dir = tempdir().expect("temp dir");

        let src = "package main\n\n// DOC\nfunc Login() {\n    // body\n}\n\nfunc Next() {}";
        let filepath = "src/auth.go";
        let abs = ws_dir.path().join(filepath);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, src).unwrap();

        let hash = VersionHash::compute(src.as_bytes());

        let real_surgeon = Arc::new(TreeSitterSurgeon::new(10));
        let server = make_server_dyn(&ws_dir, real_surgeon);

        let params = DeleteSymbolParams {
            semantic_path: format!("{filepath}::Login"),
            base_version: hash.as_str().to_owned(),
            ignore_validation_failures: false,
        };

        let result = server
            .delete_symbol(Parameters(params))
            .await
            .expect("should succeed");
        assert!(result.0.success);

        let written = std::fs::read_to_string(&abs).unwrap();
        assert!(!written.contains("Login"));
        assert!(!written.contains("// DOC"));
        assert_eq!(written, "package main\n\nfunc Next() {}");
    }

    // ── validate_only tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_validate_only_replace_body() {
        let ws_dir = tempdir().expect("temp dir");
        let src = "func Login() {\n    // old body\n}\n";
        let filepath = "src/auth.go";
        let abs = ws_dir.path().join(filepath);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, src).unwrap();

        let src_bytes = src.as_bytes();
        let hash = VersionHash::compute(src_bytes);

        let open = src.find('{').unwrap();
        let close = src.rfind('}').unwrap() + 1;

        let mock_surgeon = MockSurgeon::new();
        mock_surgeon
            .resolve_body_range_results
            .lock()
            .unwrap()
            .push(Ok((
                make_body_range(open, close, 0, 4),
                src_bytes.to_vec(),
                hash.clone(),
            )));

        let server = make_server(&ws_dir, mock_surgeon);

        let params = ValidateOnlyParams {
            semantic_path: format!("{filepath}::Login"),
            edit_type: "replace_body".to_string(),
            new_code: Some("    return nil".to_string()),
            base_version: hash.as_str().to_owned(),
        };

        let result = server
            .validate_only(Parameters(params))
            .await
            .expect("should succeed");
        let resp = result.0;

        assert!(resp.success);
        assert!(resp.new_version_hash.is_none());
        assert_eq!(resp.validation.status, "skipped");
        assert_eq!(resp.validation_skipped, Some(true));

        // Verify the file was NOT written
        let written = std::fs::read_to_string(&abs).unwrap();
        assert!(
            !written.contains("return nil"),
            "File should not be modified"
        );
        assert!(
            written.contains("old body"),
            "File should retain original content"
        );
    }

    #[tokio::test]
    async fn test_validate_only_version_mismatch() {
        let ws_dir = tempdir().expect("temp dir");
        let src = "func Login() {\n    // old body\n}\n";
        let filepath = "src/auth.go";
        let abs = ws_dir.path().join(filepath);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, src).unwrap();

        let src_bytes = src.as_bytes();
        let real_hash = VersionHash::compute(src_bytes);
        let stale_hash = "sha256:stale000".to_owned();

        let open = src.find('{').unwrap();
        let close = src.rfind('}').unwrap() + 1;

        let mock_surgeon = MockSurgeon::new();
        mock_surgeon
            .resolve_body_range_results
            .lock()
            .unwrap()
            .push(Ok((
                make_body_range(open, close, 0, 4),
                src_bytes.to_vec(),
                real_hash,
            )));

        let server = make_server(&ws_dir, mock_surgeon);

        let params = ValidateOnlyParams {
            semantic_path: format!("{filepath}::Login"),
            edit_type: "replace_body".to_string(),
            new_code: Some("return nil".to_string()),
            base_version: stale_hash,
        };

        let result = server.validate_only(Parameters(params)).await;
        let Err(err) = result else {
            panic!("expected VERSION_MISMATCH error");
        };

        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "VERSION_MISMATCH");
    }

    #[tokio::test]
    async fn test_validate_only_invalid_edit_type() {
        let ws_dir = tempdir().expect("temp dir");
        let server = make_server(&ws_dir, MockSurgeon::new());

        let params = ValidateOnlyParams {
            semantic_path: "src/auth.go::Login".to_string(),
            edit_type: "foo_bar".to_string(),
            new_code: Some("return nil".to_string()),
            base_version: "sha256:any".to_owned(),
        };

        let result = server.validate_only(Parameters(params)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_validate_only_delete() {
        let ws_dir = tempdir().expect("temp dir");
        let src = "func Login() {}";
        let filepath = "src/auth.go";
        let abs = ws_dir.path().join(filepath);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, src).unwrap();

        let src_bytes = src.as_bytes();
        let hash = VersionHash::compute(src_bytes);

        let mock_surgeon = MockSurgeon::new();
        mock_surgeon
            .resolve_full_range_results
            .lock()
            .unwrap()
            .push(Ok((
                pathfinder_treesitter::surgeon::FullRange {
                    start_byte: 0,
                    end_byte: src_bytes.len(),
                    indent_column: 0,
                },
                src_bytes.to_vec(),
                hash.clone(),
            )));

        let server = make_server(&ws_dir, mock_surgeon);

        let params = ValidateOnlyParams {
            semantic_path: format!("{filepath}::Login"),
            edit_type: "delete".to_string(),
            new_code: None,
            base_version: hash.as_str().to_owned(),
        };

        let result = server
            .validate_only(Parameters(params))
            .await
            .expect("should succeed");
        assert!(result.0.success);
        assert!(result.0.new_version_hash.is_none());
    }

    #[tokio::test]
    async fn test_validate_only_real_parser_go() {
        use pathfinder_treesitter::treesitter_surgeon::TreeSitterSurgeon;
        let ws_dir = tempdir().expect("temp dir");

        let src = "package main\n\nfunc Login() {\n    // old body\n}\n";
        let filepath = "src/auth.go";
        let abs = ws_dir.path().join(filepath);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, src).unwrap();

        let hash = VersionHash::compute(src.as_bytes());

        let real_surgeon = Arc::new(TreeSitterSurgeon::new(10));
        let server = make_server_dyn(&ws_dir, real_surgeon);

        let params = ValidateOnlyParams {
            semantic_path: format!("{filepath}::Login"),
            edit_type: "replace_full".to_string(),
            new_code: Some("func NewLogin() {}".to_string()),
            base_version: hash.as_str().to_owned(),
        };

        let result = server
            .validate_only(Parameters(params))
            .await
            .expect("should succeed");
        assert!(result.0.success);
        assert!(result.0.new_version_hash.is_none());

        // Ensure disk untouched
        let written = std::fs::read_to_string(&abs).unwrap();
        assert!(written.contains("func Login() {"));
    }
}
