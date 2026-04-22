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

use crate::server::helpers::{io_error_data, parse_semantic_path, pathfinder_to_error_data, require_symbol_target};
use crate::server::tools::diagnostics::diff_diagnostics;
use crate::server::types::{
    DeleteSymbolParams, EditResponse, EditValidation, InsertAfterParams, InsertBeforeParams,
    ReplaceBodyParams, ReplaceFullParams, ValidateOnlyParams,
};
use crate::server::PathfinderServer;
use pathfinder_common::error::{compute_lines_changed, DiagnosticError, PathfinderError};
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
    skipped: bool,
    skipped_reason: Option<String>,
    /// `true` when new errors were introduced and `ignore_validation_failures = false`.
    /// The caller must NOT write to disk in this case.
    should_block: bool,
}

/// Selects which end of a resolved symbol range is used as the insertion point.
///
/// Passed to [`PathfinderServer::resolve_insert_position`] to distinguish
/// `insert_before` (start of symbol) from `insert_after` (end of symbol).
enum InsertEdge {
    /// Insert at `symbol_range.start_byte` (before the symbol) or file offset 0.
    Before,
    /// Insert at `symbol_range.end_byte` (after the symbol) or end-of-file.
    After,
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
        let semantic_path = parse_semantic_path(&params.semantic_path)?;
        require_symbol_target(&semantic_path, &params.semantic_path)?;

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
                lines_changed: None,
            };
            return Err(pathfinder_to_error_data(&err));
        }

        // ── Step 5: Normalize new_code ────────────────────────────────
        let normalized = normalize_for_body_replace(&params.new_code);

        // ── Steps 6–7: Indent + splice ──────────────────────────────────
        let indented = dedent_then_reindent(&normalized, body_range.body_indent_column);
        let new_content = build_body_replacement(&source, &body_range, &indented)?;
        let new_bytes = new_content.as_bytes();

        // ── Steps 8–11: Validate → TOCTOU → Write → Respond ────────────
        let resolve_ms = start.elapsed().as_millis();
        self.finalize_edit(
            "replace_body",
            &semantic_path,
            &params.semantic_path,
            &source,
            new_bytes,
            &current_hash,
            params.ignore_validation_failures,
            start,
            resolve_ms,
        )
        .await
    }

    /// Core logic for the `replace_full` tool (PRD Epic 5, Story 5.4).
    ///
    /// Replaces the entire declaration of a symbol (including decorators/docs)
    /// in place on disk, using OCC `base_version` to guard against races.
    #[instrument(skip(self, params), fields(semantic_path = %params.semantic_path))]
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

        let semantic_path = parse_semantic_path(&params.semantic_path)?;

        let (source, current_hash, new_bytes) = if semantic_path.is_bare_file() {
            let absolute_path = self.workspace_root.resolve(&semantic_path.file_path);
            let source = tokio::fs::read(&absolute_path)
                .await
                .map_err(|e| io_error_data(format!("failed to read file: {e}")))?;
            let current_hash = VersionHash::compute(&source);

            let claimed = VersionHash::from_raw(params.base_version.clone());
            if claimed != current_hash {
                let err = PathfinderError::VersionMismatch {
                    path: semantic_path.file_path.clone(),
                    current_version_hash: current_hash.as_str().to_owned(),
                    lines_changed: None,
                };
                return Err(pathfinder_to_error_data(&err));
            }

            // For bare file substitution, insert exactly as provided
            (source, current_hash, params.new_code.as_bytes().to_vec())
        } else {
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
                    lines_changed: None,
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

            (source, current_hash, new_bytes)
        };

        let resolve_ms = start.elapsed().as_millis();
        self.finalize_edit(
            "replace_full",
            &semantic_path,
            &params.semantic_path,
            &source,
            &new_bytes,
            &current_hash,
            params.ignore_validation_failures,
            start,
            resolve_ms,
        )
        .await
    }


    /// Core logic for the `insert_before` tool (PRD Epic 5, Story 5.5).
    #[instrument(skip(self, params), fields(semantic_path = %params.semantic_path))]
    pub(crate) async fn insert_before_impl(
        &self,
        params: InsertBeforeParams,
    ) -> Result<Json<EditResponse>, ErrorData> {
        let start = std::time::Instant::now();
        tracing::info!(
            tool = "insert_before",
            semantic_path = %params.semantic_path,
            "insert_before: start"
        );
        let semantic_path = parse_semantic_path(&params.semantic_path)?;

        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            tracing::warn!(
                tool = "insert_before",
                semantic_path = %params.semantic_path,
                error = %e,
                "insert_before: access denied"
            );
            return Err(pathfinder_to_error_data(&e));
        }

        let (insert_byte, indent_column, source, current_hash) = self
            .resolve_insert_position(&semantic_path, InsertEdge::Before)
            .await?;

        let claimed = VersionHash::from_raw(params.base_version.clone());
        if claimed != current_hash {
            let err = PathfinderError::VersionMismatch {
                path: semantic_path.file_path.clone(),
                current_version_hash: current_hash.as_str().to_owned(),
                lines_changed: None,
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

        let resolve_ms = start.elapsed().as_millis();
        self.finalize_edit(
            "insert_before",
            &semantic_path,
            &params.semantic_path,
            &source,
            &new_bytes,
            &current_hash,
            params.ignore_validation_failures,
            start,
            resolve_ms,
        )
        .await
    }

    /// Core logic for the `insert_after` tool (PRD Epic 5, Story 5.5).
    #[instrument(skip(self, params), fields(semantic_path = %params.semantic_path))]
    pub(crate) async fn insert_after_impl(
        &self,
        params: InsertAfterParams,
    ) -> Result<Json<EditResponse>, ErrorData> {
        let start = std::time::Instant::now();
        tracing::info!(
            tool = "insert_after",
            semantic_path = %params.semantic_path,
            "insert_after: start"
        );
        let semantic_path = parse_semantic_path(&params.semantic_path)?;

        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            tracing::warn!(
                tool = "insert_after",
                semantic_path = %params.semantic_path,
                error = %e,
                "insert_after: access denied"
            );
            return Err(pathfinder_to_error_data(&e));
        }

        let (insert_byte, indent_column, source, current_hash) = self
            .resolve_insert_position(&semantic_path, InsertEdge::After)
            .await?;

        let claimed = VersionHash::from_raw(params.base_version.clone());
        if claimed != current_hash {
            let err = PathfinderError::VersionMismatch {
                path: semantic_path.file_path.clone(),
                current_version_hash: current_hash.as_str().to_owned(),
                lines_changed: None,
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

        let resolve_ms = start.elapsed().as_millis();
        self.finalize_edit(
            "insert_after",
            &semantic_path,
            &params.semantic_path,
            &source,
            &new_bytes,
            &current_hash,
            params.ignore_validation_failures,
            start,
            resolve_ms,
        )
        .await
    }

    /// Resolve the byte offset, indentation column, file source, and current version hash
    /// for an insertion operation.
    ///
    /// - `InsertEdge::Before` → byte offset = `symbol_range.start_byte` (or 0 for bare files)
    /// - `InsertEdge::After`  → byte offset = `symbol_range.end_byte`   (or EOF for bare files)
    ///
    /// Extracted to eliminate the ~30-line duplicated file-read / symbol-range logic that
    /// previously existed in both `insert_before_impl` and `insert_after_impl`.
    async fn resolve_insert_position(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        edge: InsertEdge,
    ) -> Result<(usize, usize, Vec<u8>, VersionHash), ErrorData> {
        if semantic_path.is_bare_file() {
            let absolute_path = self.workspace_root.resolve(&semantic_path.file_path);
            let bytes = tokio::fs::read(&absolute_path)
                .await
                .map_err(|e| io_error_data(format!("failed to read file: {e}")))?;
            let hash = VersionHash::compute(&bytes);
            let offset = match edge {
                InsertEdge::Before => 0,
                InsertEdge::After => bytes.len(),
            };
            return Ok((offset, 0, bytes, hash));
        }

        let (symbol_range, source, hash) = self
            .surgeon
            .resolve_symbol_range(self.workspace_root.path(), semantic_path)
            .await
            .map_err(crate::server::helpers::treesitter_error_to_error_data)?;

        let insert_byte = match edge {
            InsertEdge::Before => symbol_range.start_byte,
            InsertEdge::After => symbol_range.end_byte,
        };

        Ok((insert_byte, symbol_range.indent_column, source, hash))
    }

    /// Core logic for the `delete_symbol` tool (PRD Epic 5, Story 5.6).
    #[instrument(skip(self, params), fields(semantic_path = %params.semantic_path))]
    pub(crate) async fn delete_symbol_impl(
        &self,
        params: DeleteSymbolParams,
    ) -> Result<Json<EditResponse>, ErrorData> {
        let start = std::time::Instant::now();
        tracing::info!(
            tool = "delete_symbol",
            semantic_path = %params.semantic_path,
            "delete_symbol: start"
        );

        let semantic_path = parse_semantic_path(&params.semantic_path)?;
        require_symbol_target(&semantic_path, &params.semantic_path)?;

        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            tracing::warn!(
                tool = "delete_symbol",
                semantic_path = %params.semantic_path,
                error = %e,
                "delete_symbol: access denied"
            );
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
                lines_changed: None,
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

        let resolve_ms = start.elapsed().as_millis();
        self.finalize_edit(
            "delete_symbol",
            &semantic_path,
            &params.semantic_path,
            &source,
            &new_bytes,
            &current_hash,
            params.ignore_validation_failures,
            start,
            resolve_ms,
        )
        .await
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

        let semantic_path = parse_semantic_path(&params.semantic_path)?;

        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            tracing::warn!(
                tool = "validate_only",
                semantic_path = %params.semantic_path,
                error = %e,
                "validate_only: access denied"
            );
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
                lines_changed: None,
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
            validation_skipped: true,
            validation_skipped_reason: Some("validate_only_no_write".to_owned()),
        }))
    }

    /// Core logic for the `replace_batch` tool (PRD Epic 5).
    ///
    /// Executes multiple edits on the same file atomically. Edits are resolved,
    /// sorted backwards by byte offset, and spliced together. This avoids OCC
    /// mismatches from chains of edits.
    #[instrument(skip(self, params), fields(filepath = %params.filepath))]
    #[allow(clippy::too_many_lines)]
    pub(crate) async fn replace_batch_impl(
        &self,
        params: crate::server::types::ReplaceBatchParams,
    ) -> Result<Json<EditResponse>, ErrorData> {
        let start = std::time::Instant::now();
        tracing::info!(
            tool = "replace_batch",
            filepath = %params.filepath,
            "replace_batch: start"
        );

        let file_path = Path::new(&params.filepath);
        if let Err(e) = self.sandbox.check(file_path) {
            return Err(pathfinder_to_error_data(&e));
        }

        let absolute_path = self.workspace_root.resolve(file_path);
        let source = tokio::fs::read(&absolute_path)
            .await
            .map_err(|e| io_error_data(format!("failed to read file: {e}")))?;
        let current_hash = VersionHash::compute(&source);

        let claimed = VersionHash::from_raw(params.base_version.clone());
        if claimed != current_hash {
            let err = PathfinderError::VersionMismatch {
                path: std::path::PathBuf::from(&params.filepath),
                current_version_hash: current_hash.as_str().to_owned(),
                lines_changed: None,
            };
            return Err(pathfinder_to_error_data(&err));
        }

        struct ResolvedEdit {
            start_byte: usize,
            end_byte: usize,
            replacement: Vec<u8>,
        }
        let mut resolved_edits = Vec::new();

        for (edit_index, edit) in params.edits.iter().enumerate() {
            // ── Branch A: Text-range targeting ─────────────────────────────────────
            if let Some(ref old_text) = edit.old_text {
                let Some(context_line) = edit.context_line else {
                    let err = PathfinderError::InvalidTarget {
                        semantic_path: format!("edit[{edit_index}]"),
                        reason: "`context_line` is required when `old_text` is set".to_owned(),
                        edit_index: Some(edit_index),
                        valid_edit_types: None,
                    };
                    return Err(pathfinder_to_error_data(&err));
                };
                let replacement = edit.replacement_text.as_deref().unwrap_or("");
                let free = resolve_text_edit(
                    &source,
                    old_text.as_str(),
                    context_line,
                    replacement,
                    edit.normalize_whitespace,
                    file_path,
                )
                .map_err(|e| pathfinder_to_error_data(&e))?;
                resolved_edits.push(ResolvedEdit {
                    start_byte: free.start_byte,
                    end_byte: free.end_byte,
                    replacement: free.replacement,
                });
                continue;
            }

            // ── Branch B: Semantic targeting (existing) ───────────────────────────
            let Some(semantic_path) = SemanticPath::parse(&edit.semantic_path) else {
                let err = PathfinderError::InvalidSemanticPath {
                    input: edit.semantic_path.clone(),
                    issue: "Semantic path is malformed or missing '::' separator.".to_owned(),
                };
                return Err(pathfinder_to_error_data(&err));
            };

            match edit.edit_type.as_str() {
                "replace_body" => {
                    let (body_range, _, _hash) = self
                        .surgeon
                        .resolve_body_range(self.workspace_root.path(), &semantic_path)
                        .await
                        .map_err(crate::server::helpers::treesitter_error_to_error_data)?;

                    let new_code = edit.new_code.as_deref().unwrap_or_default();
                    let normalized = normalize_for_body_replace(new_code);
                    let indented = dedent_then_reindent(&normalized, body_range.body_indent_column);

                    let is_brace_block = if body_range.end_byte > body_range.start_byte {
                        source.get(body_range.start_byte) == Some(&b'{')
                            && source.get(body_range.end_byte.saturating_sub(1)) == Some(&b'}')
                    } else {
                        false
                    };

                    if is_brace_block {
                        let inner_start = body_range.start_byte + 1;
                        let inner_end = body_range.end_byte.saturating_sub(1);
                        let replacement = if indented.trim().is_empty() {
                            Vec::new()
                        } else {
                            let closing_indent = " ".repeat(body_range.indent_column);
                            format!("\n{indented}\n{closing_indent}").into_bytes()
                        };
                        resolved_edits.push(ResolvedEdit {
                            start_byte: inner_start,
                            end_byte: inner_end,
                            replacement,
                        });
                    } else {
                        let mut end = body_range.start_byte;
                        while end > 0 && (source[end - 1] == b' ' || source[end - 1] == b'\t') {
                            end -= 1;
                        }
                        resolved_edits.push(ResolvedEdit {
                            start_byte: end,
                            end_byte: body_range.end_byte,
                            replacement: format!("\n{indented}").into_bytes(),
                        });
                    }
                }
                "replace_full" => {
                    let new_code = edit.new_code.as_deref().unwrap_or_default();
                    if semantic_path.is_bare_file() {
                        resolved_edits.push(ResolvedEdit {
                            start_byte: 0,
                            end_byte: source.len(),
                            replacement: new_code.as_bytes().to_vec(),
                        });
                    } else {
                        let (full_range, _, _) = self
                            .surgeon
                            .resolve_full_range(self.workspace_root.path(), &semantic_path)
                            .await
                            .map_err(crate::server::helpers::treesitter_error_to_error_data)?;

                        let normalized = normalize_for_full_replace(new_code);
                        let indented = dedent_then_reindent(&normalized, full_range.indent_column);

                        resolved_edits.push(ResolvedEdit {
                            start_byte: full_range.start_byte,
                            end_byte: full_range.end_byte,
                            replacement: indented.into_bytes(),
                        });
                    }
                }
                "insert_before" => {
                    let (insert_byte, indent_column) = if semantic_path.is_bare_file() {
                        (0, 0)
                    } else {
                        let (symbol_range, _, _) = self
                            .surgeon
                            .resolve_symbol_range(self.workspace_root.path(), &semantic_path)
                            .await
                            .map_err(crate::server::helpers::treesitter_error_to_error_data)?;
                        (symbol_range.start_byte, symbol_range.indent_column)
                    };

                    let new_code = edit.new_code.as_deref().unwrap_or_default();
                    let normalized = normalize_for_full_replace(new_code);
                    let indented = dedent_then_reindent(&normalized, indent_column);

                    let trailing = if indented.ends_with('\n') { "" } else { "\n" };
                    let after = &source[insert_byte..];
                    let sep = if after.starts_with(b"\n\n") {
                        ""
                    } else if after.starts_with(b"\n") {
                        "\n"
                    } else {
                        "\n\n"
                    };

                    resolved_edits.push(ResolvedEdit {
                        start_byte: insert_byte,
                        end_byte: insert_byte,
                        replacement: format!("{indented}{trailing}{sep}").into_bytes(),
                    });
                }
                "insert_after" => {
                    let (insert_byte, indent_column) = if semantic_path.is_bare_file() {
                        (source.len(), 0)
                    } else {
                        let (symbol_range, _, _) = self
                            .surgeon
                            .resolve_symbol_range(self.workspace_root.path(), &semantic_path)
                            .await
                            .map_err(crate::server::helpers::treesitter_error_to_error_data)?;
                        (symbol_range.end_byte, symbol_range.indent_column)
                    };

                    let new_code = edit.new_code.as_deref().unwrap_or_default();
                    let normalized = normalize_for_full_replace(new_code);
                    let indented = dedent_then_reindent(&normalized, indent_column);

                    let before = &source[..insert_byte];
                    let before_sep = if before.ends_with(b"\n\n") {
                        ""
                    } else if before.ends_with(b"\n") {
                        "\n"
                    } else {
                        "\n\n"
                    };
                    let after_sep = if indented.ends_with('\n') { "" } else { "\n" };

                    resolved_edits.push(ResolvedEdit {
                        start_byte: insert_byte,
                        end_byte: insert_byte,
                        replacement: format!("{before_sep}{indented}{after_sep}").into_bytes(),
                    });
                }
                "delete" => {
                    let (full_range, _, _) = self
                        .surgeon
                        .resolve_full_range(self.workspace_root.path(), &semantic_path)
                        .await
                        .map_err(crate::server::helpers::treesitter_error_to_error_data)?;

                    let mut b_end = full_range.start_byte;
                    while b_end > 0 && source[b_end - 1].is_ascii_whitespace() {
                        b_end -= 1;
                    }

                    let mut a_start = full_range.end_byte;
                    while a_start < source.len() && source[a_start].is_ascii_whitespace() {
                        a_start += 1;
                    }

                    let sep = if b_end == 0 || a_start == source.len() {
                        b"\n" as &[u8]
                    } else {
                        b"\n\n"
                    };

                    resolved_edits.push(ResolvedEdit {
                        start_byte: b_end,
                        end_byte: a_start,
                        replacement: sep.to_vec(),
                    });
                }
                _ => {
                    let err = pathfinder_common::error::PathfinderError::InvalidTarget {
                        semantic_path: edit.semantic_path.clone(),
                        reason: format!(
                            "edit_type is required for semantic targeting. Got: '{}' (empty).",
                            edit.edit_type
                        ),
                        edit_index: Some(edit_index),
                        valid_edit_types: Some(vec![
                            "replace_body".to_string(),
                            "replace_full".to_string(),
                            "insert_before".to_string(),
                            "insert_after".to_string(),
                            "delete".to_string(),
                        ]),
                    };
                    return Err(pathfinder_to_error_data(&err));
                }
            }
        }

        // Sort edits backwards to prevent shifted byte offsets
        resolved_edits.sort_by_key(|e| std::cmp::Reverse(e.start_byte));

        // Ensure no overlapping edits
        for i in 1..resolved_edits.len() {
            let prev = &resolved_edits[i - 1]; // This is later in the file
            let curr = &resolved_edits[i]; // This is earlier in the file
            if curr.end_byte > prev.start_byte {
                return Err(io_error_data("overlapping edits in replace_batch"));
            }
        }

        let mut new_bytes = source.clone();
        for edit in resolved_edits {
            new_bytes.splice(edit.start_byte..edit.end_byte, edit.replacement.into_iter());
        }

        let resolve_ms = start.elapsed().as_millis();
        let dummy_path = SemanticPath::parse(&params.filepath).unwrap_or_else(|| SemanticPath {
            file_path: file_path.to_path_buf(),
            symbol_chain: None,
        });

        self.finalize_edit(
            "replace_batch",
            &dummy_path,
            &params.filepath,
            &source,
            &new_bytes,
            &current_hash,
            params.ignore_validation_failures,
            start,
            resolve_ms,
        )
        .await
    }

    /// Read file and compute its version hash (for bare-file edit types).
    async fn hash_file_content(&self, semantic_path: &SemanticPath) -> Result<VersionHash, ErrorData> {
        let absolute_path = self.workspace_root.resolve(&semantic_path.file_path);
        let bytes = tokio::fs::read(&absolute_path)
            .await
            .map_err(|e| io_error_data(format!("failed to read file: {e}")))?;
        Ok(VersionHash::compute(&bytes))
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
                require_symbol_target(semantic_path, raw_path)?;
                let (_, _, hash) = self
                    .surgeon
                    .resolve_body_range(self.workspace_root.path(), semantic_path)
                    .await
                    .map_err(treesitter_error_to_error_data)?;
                Ok(hash)
            }
            "replace_full" => {
                if semantic_path.is_bare_file() {
                    self.hash_file_content(semantic_path).await
                } else {
                    let (_, _, hash) = self
                        .surgeon
                        .resolve_full_range(self.workspace_root.path(), semantic_path)
                        .await
                        .map_err(treesitter_error_to_error_data)?;
                    Ok(hash)
                }
            }
            "delete" => {
                require_symbol_target(semantic_path, raw_path)?;
                let (_, _, hash) = self
                    .surgeon
                    .resolve_full_range(self.workspace_root.path(), semantic_path)
                    .await
                    .map_err(treesitter_error_to_error_data)?;
                Ok(hash)
            }
            "insert_before" | "insert_after" => {
                if semantic_path.is_bare_file() {
                    self.hash_file_content(semantic_path).await
                } else {
                    let (_, _, hash) = self
                        .surgeon
                        .resolve_symbol_range(self.workspace_root.path(), semantic_path)
                        .await
                        .map_err(treesitter_error_to_error_data)?;
                    Ok(hash)
                }
            }
            unknown => {
                let err = pathfinder_common::error::PathfinderError::InvalidTarget {
                    semantic_path: raw_path.to_owned(),
                    reason: format!(
                        "unsupported edit type: '{unknown}'. Must be one of: replace_body, replace_full, insert_before, insert_after, delete."
                    ),
                    edit_index: None,
                    valid_edit_types: None,
                };
                Err(pathfinder_to_error_data(&err))
            }
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
    fn lsp_error_to_skip_reason(e: &LspError) -> &'static str {
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

    #[allow(
        clippy::too_many_lines,
        reason = "The LSP validation pipeline is intentionally a single sequential flow: \
                  open → pre-diags → change → post-diags → close → diff. \
                  Splitting it would obscure the linear state machine and scatter did_close call sites."
    )]
    async fn run_lsp_validation(
        &self,
        file_path: &Path,
        original_content: &str,
        new_content: &str,
        ignore_validation_failures: bool,
    ) -> ValidationOutcome {
        // version 1 = original, version 2 = post-edit
        // version 1 = original, version 2 = post-edit
        let relative = file_path;
        let workspace = self.workspace_root.path();

        let return_skip = |reason: &str| -> ValidationOutcome {
            let ext = relative.extension().and_then(|e| e.to_str()).unwrap_or("");
            let lang = pathfinder_lsp::client::language_id_for_extension(ext).unwrap_or("unknown");
            tracing::debug!(
                file = %relative.display(),
                skip_reason = reason,
                language = lang,
                "validation skip"
            );
            ValidationOutcome {
                validation: EditValidation::skipped(),
                skipped: true,
                skipped_reason: Some(reason.to_owned()),
                should_block: false,
            }
        };

        // ── did_open (original content, version 1) ──
        if let Err(e) = self
            .lawyer
            .did_open(workspace, relative, original_content)
            .await
        {
            let skipped_reason = Self::lsp_error_to_skip_reason(&e);
            let should_log = !matches!(
                &e,
                LspError::NoLspAvailable | LspError::UnsupportedCapability { .. }
            );

            if should_log {
                tracing::warn!(error = %e, "validation: did_open failed");
            }
            return return_skip(skipped_reason);
        }

        // ── pre-edit diagnostics ──
        let mut pre_diags = match self.lawyer.pull_diagnostics(workspace, relative).await {
            Ok(d) => d,
            Err(LspError::UnsupportedCapability { .. }) => {
                // LSP running but doesn't support Pull Diagnostics — close the document
                let _ = self.lawyer.did_close(workspace, relative).await;
                return return_skip("pull_diagnostics_unsupported");
            }
            Err(e) => {
                let skipped_reason = Self::lsp_error_to_skip_reason(&e);
                tracing::warn!(error = %e, "validation: pre-edit pull_diagnostics failed");
                let _ = self.lawyer.did_close(workspace, relative).await;
                return return_skip(skipped_reason);
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

        // ── did_change (new content, version 2) ──
        if let Err(e) = self
            .lawyer
            .did_change(workspace, relative, new_content, 2)
            .await
        {
            let skipped_reason = Self::lsp_error_to_skip_reason(&e);
            tracing::warn!(error = %e, "validation: did_change failed");
            let _ = self.lawyer.did_close(workspace, relative).await;
            return return_skip(skipped_reason);
        }

        // ── post-edit diagnostics ──
        let mut post_diags = match self.lawyer.pull_diagnostics(workspace, relative).await {
            Ok(d) => d,
            Err(e) => {
                let skipped_reason = Self::lsp_error_to_skip_reason(&e);
                tracing::warn!(error = %e, "validation: post-edit pull_diagnostics failed");
                let _ = self.lawyer.did_close(workspace, relative).await;
                return return_skip(skipped_reason);
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

        // ── revert LSP state to original (fire-and-forget) ──
        let _ = self
            .lawyer
            .did_change(workspace, relative, original_content, 3)
            .await;

        // ── close document to free LSP memory ──
        let _ = self.lawyer.did_close(workspace, relative).await;

        // ── diff diagnostics ──────────────────────
        build_validation_outcome(&pre_diags, &post_diags, ignore_validation_failures)
    }

    /// Helper to perform the final TOCTOU check and write the modified file to disk.
    /// Re-reads the file, ensures its current hash still matches `current_hash`,
    /// then writes `new_bytes` to disk in-place.
    async fn flush_edit_with_toctou(
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

        Ok(VersionHash::compute(new_bytes))
    }

    /// Helper function to perform LSP validation, TOCTOU check, and disk write.
    /// This dries up the tail end of the edit tools.
    #[expect(
        clippy::too_many_arguments,
        reason = "Helper function to dry up edit tool validation and response tails."
    )]
    async fn finalize_edit(
        &self,
        tool_name: &'static str,
        semantic_path: &SemanticPath,
        raw_semantic_path_str: &str,
        source: &[u8],
        new_bytes: &[u8],
        current_hash: &VersionHash,
        ignore_validation_failures: bool,
        start_time: std::time::Instant,
        resolve_ms: u128,
    ) -> Result<Json<EditResponse>, ErrorData> {
        let validate_start = std::time::Instant::now();
        let original_str = std::str::from_utf8(source);
        let new_str = std::str::from_utf8(new_bytes);
        let validation_outcome = match (original_str, new_str) {
            (Ok(orig), Ok(new)) => {
                self.run_lsp_validation(
                    &semantic_path.file_path,
                    orig,
                    new,
                    ignore_validation_failures,
                )
                .await
            }
            _ => ValidationOutcome {
                validation: EditValidation::skipped(),
                skipped: true,
                skipped_reason: Some("utf8_error".to_owned()),
                should_block: false,
            },
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
            .flush_edit_with_toctou(semantic_path, current_hash, source, new_bytes)
            .await?;
        let flush_ms = flush_start.elapsed().as_millis();

        let duration_ms = start_time.elapsed().as_millis();
        tracing::info!(
            tool = tool_name,
            semantic_path = %raw_semantic_path_str,
            duration_ms,
            resolve_ms,
            validate_ms,
            flush_ms,
            new_version_hash = new_hash.as_str(),
            engines_used = ?["tree-sitter"],
            "{tool_name}: complete"
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
}

// ── Extracted helpers ──────────────────────────────────────────────────────────

/// Resolve a text-range edit (E3.1) to a concrete byte span in `source`.
///
/// # Algorithm
///
/// 1. Collect byte offsets for all line starts (0-indexed lines).
/// 2. Compute a search window: lines `(context_line - 1) ± 25` (clamped to file bounds).
/// 3. Extract the UTF-8 text for that window.
/// 4. Search for `old_text` within the window, optionally collapsing `\s+` → `' '`
///    when `normalize_whitespace` is `true`.
/// 5. Map the within-window match offset back to absolute byte positions in `source`.
///
/// Returns a [`ResolvedEdit`] whose `replacement` is `new_text.as_bytes()`.
/// Returns [`PathfinderError::TextNotFound`] if no match is found.
///
/// # Errors
/// - [`PathfinderError::TextNotFound`] — `old_text` not present in the ±25-line window.
/// - Propagated UTF-8 errors if source is not valid UTF-8 (returns an opaque I/O error).
#[allow(clippy::too_many_lines)]
fn resolve_text_edit(
    source: &[u8],
    old_text: &str,
    context_line: u32,
    new_text: &str,
    normalize_whitespace: bool,
    filepath: &std::path::Path,
) -> Result<ResolvedEditFree, pathfinder_common::error::PathfinderError> {
    // Convert source to UTF-8 — required for line-wise text operations.
    let source_str = std::str::from_utf8(source).map_err(|e| {
        pathfinder_common::error::PathfinderError::IoError {
            message: format!("source file is not valid UTF-8: {e}"),
        }
    })?;

    // Build a list of byte offsets for the start of every line (0-indexed).
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(
            source_str
                .char_indices()
                .filter(|(_, c)| *c == '\n')
                .map(|(i, _)| i + 1),
        )
        .collect();

    let total_lines = line_starts.len();

    // context_line is 1-indexed; convert to 0-indexed.
    let center = context_line.saturating_sub(1) as usize;
    let window_start_line = center.saturating_sub(25);
    let window_end_line = (center + 25).min(total_lines.saturating_sub(1));

    // Byte range [window_byte_start, window_byte_end) covering the search window.
    let window_byte_start = line_starts[window_start_line];
    let window_byte_end = if window_end_line + 1 < total_lines {
        line_starts[window_end_line + 1]
    } else {
        source_str.len()
    };

    let window_text = &source_str[window_byte_start..window_byte_end];

    // Perform the match, with optional whitespace normalisation.
    let match_offset_in_window: usize = if normalize_whitespace {
        // Collapse all runs of whitespace to a single space for both needle and haystack.
        let normalise = |s: &str| {
            let mut out = String::with_capacity(s.len());
            let mut prev_ws = false;
            for ch in s.chars() {
                if ch.is_ascii_whitespace() {
                    if !prev_ws {
                        out.push(' ');
                    }
                    prev_ws = true;
                } else {
                    out.push(ch);
                    prev_ws = false;
                }
            }
            out
        };
        let normalised_window = normalise(window_text);
        let normalised_needle = normalise(old_text);

        // After normalisation the char-level offsets no longer map 1:1 to the
        // original bytes. We need to find the match in the *normalised* window
        // and then walk back to find the corresponding byte span in the original.
        let norm_match_start = normalised_window
            .find(normalised_needle.as_str())
            .ok_or_else(|| pathfinder_common::error::PathfinderError::TextNotFound {
                filepath: filepath.to_path_buf(),
                old_text: old_text.to_owned(),
                context_line,
                actual_content: Some(window_text.to_owned()),
            })?;
        let norm_match_end = norm_match_start + normalised_needle.len();

        // Re-walk the window to find the original byte span that corresponds to
        // the normalised match range [norm_match_start, norm_match_end).
        let mut orig_start: Option<usize> = None;
        let mut orig_end: Option<usize> = None;
        let mut norm_pos = 0usize;
        let mut prev_ws2 = false;

        for (orig_i, ch) in window_text.char_indices() {
            let was_prev_ws = prev_ws2;
            let ch_is_ws = ch.is_ascii_whitespace();

            let norm_char_start = norm_pos;
            if ch_is_ws {
                if !was_prev_ws {
                    norm_pos += 1; // the space we emitted
                }
                prev_ws2 = true;
            } else {
                norm_pos += ch.len_utf8();
                prev_ws2 = false;
            }

            if orig_start.is_none() && norm_char_start == norm_match_start {
                orig_start = Some(orig_i);
            }
            if orig_end.is_none() && norm_pos >= norm_match_end {
                // The original span ends after this character.
                orig_end = Some(orig_i + ch.len_utf8());
                break;
            }
        }

        // If the match reached end-of-window.
        if orig_end.is_none() && norm_pos >= norm_match_end {
            orig_end = Some(window_text.len());
        }

        match (orig_start, orig_end) {
            (Some(s), Some(e)) => {
                return Ok(ResolvedEditFree {
                    start_byte: window_byte_start + s,
                    end_byte: window_byte_start + e,
                    replacement: new_text.as_bytes().to_vec(),
                });
            }
            _ => {
                return Err(pathfinder_common::error::PathfinderError::TextNotFound {
                    filepath: filepath.to_path_buf(),
                    old_text: old_text.to_owned(),
                    context_line,
                    actual_content: Some(window_text.to_owned()),
                });
            }
        }
    } else {
        // Try exact match first
        let exact_match = window_text.find(old_text);

        if exact_match.is_none() && !normalize_whitespace {
            // Exact match failed and we haven't tried fuzzy matching yet.
            // Retry with whitespace normalization as a fallback.
            tracing::info!(
                filepath = %filepath.display(),
                context_line,
                old_text_len = old_text.len(),
                "text_edit: exact match failed, trying whitespace-normalized fuzzy fallback"
            );

            // Recursively call with normalize_whitespace=true.
            // Note: This recursion is bounded (max 1 level) because we only recurse
            // when normalize_whitespace is false, and the recursive call sets it to true.
            return resolve_text_edit(source, old_text, context_line, new_text, true, filepath);
        }

        exact_match.ok_or_else(|| pathfinder_common::error::PathfinderError::TextNotFound {
            filepath: filepath.to_path_buf(),
            old_text: old_text.to_owned(),
            context_line,
            actual_content: Some(window_text.to_owned()),
        })?
    };

    let abs_start = window_byte_start + match_offset_in_window;
    let abs_end = abs_start + old_text.len();

    Ok(ResolvedEditFree {
        start_byte: abs_start,
        end_byte: abs_end,
        replacement: new_text.as_bytes().to_vec(),
    })
}

/// Mirror of the local `ResolvedEdit` struct used by free functions outside
/// `replace_batch_impl`. Carries the same payload — a byte span and its replacement.
///
/// Named `ResolvedEditFree` to avoid a name clash with the local struct inside
/// `replace_batch_impl`.
#[derive(Debug)]
struct ResolvedEditFree {
    start_byte: usize,
    end_byte: usize,
    replacement: Vec<u8>,
}

impl From<ResolvedEditFree> for (usize, usize, Vec<u8>) {
    fn from(r: ResolvedEditFree) -> Self {
        (r.start_byte, r.end_byte, r.replacement)
    }
}

/// Splice `indented` code into `source` at the given `body_range`.
///
/// Handles two cases:
/// - **Brace-enclosed blocks** (Go/Rust/TS): keeps `{` and `}`, inserts body
///   between them with proper indentation for the closing brace.
/// - **Non-brace blocks** (Python): replaces only the byte range, trimming
///   trailing whitespace before the insertion point to avoid double indentation.
fn build_body_replacement(
    source: &[u8],
    body_range: &pathfinder_treesitter::surgeon::BodyRange,
    indented: &str,
) -> Result<String, ErrorData> {
    let is_brace_block = if body_range.end_byte > body_range.start_byte {
        source.get(body_range.start_byte) == Some(&b'{')
            && source.get(body_range.end_byte.saturating_sub(1)) == Some(&b'}')
    } else {
        false
    };

    let utf8_err =
        |e: std::str::Utf8Error| io_error_data(format!("source is not valid UTF-8: {e}"));

    if is_brace_block {
        let before = std::str::from_utf8(&source[..=body_range.start_byte]).map_err(utf8_err)?;
        let after = std::str::from_utf8(&source[body_range.end_byte.saturating_sub(1)..])
            .map_err(utf8_err)?;

        if indented.trim().is_empty() {
            Ok([before, after].concat())
        } else {
            let closing_indent = " ".repeat(body_range.indent_column);
            Ok([before, "\n", indented, "\n", &closing_indent, after].concat())
        }
    } else {
        // Non-brace block (e.g., Python): trim trailing whitespace from `before`.
        let mut end = body_range.start_byte;
        while end > 0 && (source[end - 1] == b' ' || source[end - 1] == b'\t') {
            end -= 1;
        }
        let before = std::str::from_utf8(&source[..end]).map_err(utf8_err)?;
        let after = std::str::from_utf8(&source[body_range.end_byte..]).map_err(utf8_err)?;
        Ok([before, indented, after].concat())
    }
}

/// Convert pre/post diagnostic lists into a `ValidationOutcome`.
///
/// Pure function: diffs the diagnostics, maps them to `DiagnosticError`,
/// and decides whether the edit should be blocked.
fn build_validation_outcome(
    pre_diags: &[pathfinder_lsp::types::LspDiagnostic],
    post_diags: &[pathfinder_lsp::types::LspDiagnostic],
    ignore_validation_failures: bool,
) -> ValidationOutcome {
    let diff = diff_diagnostics(pre_diags, post_diags);
    let has_new_errors = diff.has_new_errors();

    let to_diag_error = |d: &pathfinder_lsp::types::LspDiagnostic| DiagnosticError {
        severity: d.severity as u8,
        code: d.code.clone().unwrap_or_default(),
        message: d.message.clone(),
        file: d.file.clone(),
    };

    let introduced: Vec<DiagnosticError> = diff.introduced.iter().map(to_diag_error).collect();
    let resolved: Vec<DiagnosticError> = diff.resolved.iter().map(to_diag_error).collect();

    let should_block = has_new_errors && !ignore_validation_failures;
    let status = if should_block { "failed" } else { "passed" };

    ValidationOutcome {
        validation: EditValidation {
            status: status.to_owned(),
            introduced_errors: introduced,
            resolved_errors: resolved,
        },
        skipped: false,
        skipped_reason: None,
        should_block,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::{VersionHash, WorkspaceRoot};
    use pathfinder_lsp::types::{DefinitionLocation, LspDiagnostic};
    use pathfinder_lsp::{Lawyer, LspError};
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::mock::MockSurgeon;
    use pathfinder_treesitter::surgeon::BodyRange;
    use rmcp::handler::server::wrapper::Parameters;
    use std::path::Path;
    use std::sync::Arc;
    use tempfile::tempdir;

    /// Minimal `Lawyer` that always returns `LspError::UnsupportedCapability` from
    /// `pull_diagnostics`. Used to exercise the `pull_diagnostics_unsupported` branch
    /// in `run_lsp_validation` (the `MockLawyer`'s queue can only inject `Protocol`
    /// errors, not `UnsupportedCapability`).
    struct UnsupportedDiagLawyer;

    #[async_trait::async_trait]
    impl Lawyer for UnsupportedDiagLawyer {
        async fn goto_definition(
            &self,
            _workspace_root: &Path,
            _file_path: &Path,
            _line: u32,
            _column: u32,
        ) -> Result<Option<DefinitionLocation>, LspError> {
            Ok(None)
        }
        async fn call_hierarchy_prepare(
            &self,
            _workspace_root: &std::path::Path,
            _file_path: &std::path::Path,
            _line: u32,
            _column: u32,
        ) -> Result<Vec<pathfinder_lsp::types::CallHierarchyItem>, LspError> {
            Err(LspError::NoLspAvailable)
        }

        async fn call_hierarchy_incoming(
            &self,
            _workspace_root: &std::path::Path,
            _item: &pathfinder_lsp::types::CallHierarchyItem,
        ) -> Result<Vec<pathfinder_lsp::types::CallHierarchyCall>, LspError> {
            Err(LspError::NoLspAvailable)
        }

        async fn call_hierarchy_outgoing(
            &self,
            _workspace_root: &std::path::Path,
            _item: &pathfinder_lsp::types::CallHierarchyItem,
        ) -> Result<Vec<pathfinder_lsp::types::CallHierarchyCall>, LspError> {
            Err(LspError::NoLspAvailable)
        }

        async fn did_open(
            &self,
            _workspace_root: &Path,
            _file_path: &Path,
            _content: &str,
        ) -> Result<(), LspError> {
            Ok(())
        }
        async fn did_change(
            &self,
            _workspace_root: &Path,
            _file_path: &Path,
            _content: &str,
            _version: i32,
        ) -> Result<(), LspError> {
            Ok(())
        }
        async fn did_close(
            &self,
            _workspace_root: &Path,
            _file_path: &Path,
        ) -> Result<(), LspError> {
            Ok(())
        }
        async fn pull_diagnostics(
            &self,
            _workspace_root: &Path,
            _file_path: &Path,
        ) -> Result<Vec<LspDiagnostic>, LspError> {
            Err(LspError::UnsupportedCapability {
                capability: "diagnosticProvider".into(),
            })
        }
        async fn pull_workspace_diagnostics(
            &self,
            _workspace_root: &Path,
            _file_path: &Path,
        ) -> Result<Vec<LspDiagnostic>, LspError> {
            Err(LspError::UnsupportedCapability {
                capability: "diagnosticProvider".into(),
            })
        }
        async fn range_formatting(
            &self,
            _workspace_root: &Path,
            _file_path: &Path,
            _start_line: u32,
            _end_line: u32,
        ) -> Result<Option<String>, LspError> {
            Ok(None)
        }

        async fn capability_status(
            &self,
        ) -> std::collections::HashMap<String, pathfinder_lsp::types::LspLanguageStatus> {
            std::collections::HashMap::new()
        }
    }

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
        assert!(resp.validation_skipped);

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
            panic!("expected INVALID_SEMANTIC_PATH error");
        };

        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "INVALID_SEMANTIC_PATH", "got: {err:?}");
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
        assert!(resp.validation_skipped);

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

    // ── run_lsp_validation tests ────────────────────────────────────────────
    //
    // `run_lsp_validation` is `async fn` on `PathfinderServer` (private), so we
    // drive it indirectly via `replace_full_impl`, which calls it in the happy
    // path after the body splice.  All tests inject a `MockLawyer` via
    // `with_all_engines` and configure the desired lawyer behaviour before the
    // call.

    /// Build a `PathfinderServer` with an injected `MockLawyer` and a
    /// `MockSurgeon` that has one `resolve_full_range` result ready.
    ///
    /// The caller writes the source file; the surgeon is pre-configured to
    /// return `full_range` covering the full file so `replace_full_impl` reaches
    /// `run_lsp_validation`.
    fn make_server_with_lawyer(
        ws_dir: &tempfile::TempDir,
        mock_surgeon: MockSurgeon,
        mock_lawyer: pathfinder_lsp::MockLawyer,
    ) -> PathfinderServer {
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);
        PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            Arc::new(mock_surgeon),
            Arc::new(mock_lawyer),
        )
    }

    /// Helper: write a tiny Go source file and build a `MockSurgeon` whose
    /// `resolve_full_range` returns a range covering the whole file.
    fn setup_full_replace_fixture(
        ws_dir: &tempfile::TempDir,
        filepath: &str,
        src: &str,
    ) -> (MockSurgeon, VersionHash) {
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

        (mock_surgeon, hash)
    }

    // ── no_lsp: did_open returns NoLspAvailable → validation skipped ────

    #[tokio::test]
    async fn test_run_lsp_validation_no_lsp() {
        let ws_dir = tempdir().expect("temp dir");
        let filepath = "src/auth.go";
        let src = "func Login() {}";
        let (mock_surgeon, hash) = setup_full_replace_fixture(&ws_dir, filepath, src);

        let mock_lawyer = pathfinder_lsp::MockLawyer::default();
        mock_lawyer.set_did_open_error(pathfinder_lsp::LspError::NoLspAvailable);

        let server = make_server_with_lawyer(&ws_dir, mock_surgeon, mock_lawyer);

        let params = ReplaceFullParams {
            semantic_path: format!("{filepath}::Login"),
            base_version: hash.as_str().to_owned(),
            new_code: "func Login() { return }\n".to_owned(),
            ignore_validation_failures: false,
        };
        let result = server
            .replace_full(Parameters(params))
            .await
            .expect("should succeed — no_lsp gracefully degrades");
        let resp = result.0;

        assert!(resp.success);
        assert_eq!(resp.validation.status, "skipped");
        assert!(resp.validation_skipped);
        assert_eq!(resp.validation_skipped_reason.as_deref(), Some("no_lsp"));
    }

    // ── unsupported: did_open returns UnsupportedCapability → skipped ───

    #[tokio::test]
    async fn test_run_lsp_validation_unsupported() {
        let ws_dir = tempdir().expect("temp dir");
        let filepath = "src/auth.go";
        let src = "func Login() {}";
        let (mock_surgeon, hash) = setup_full_replace_fixture(&ws_dir, filepath, src);

        let mock_lawyer = pathfinder_lsp::MockLawyer::default();
        mock_lawyer.set_did_open_error(pathfinder_lsp::LspError::UnsupportedCapability {
            capability: "textDocument/diagnostic".to_owned(),
        });

        let server = make_server_with_lawyer(&ws_dir, mock_surgeon, mock_lawyer);

        let params = ReplaceFullParams {
            semantic_path: format!("{filepath}::Login"),
            base_version: hash.as_str().to_owned(),
            new_code: "func Login() { return }\n".to_owned(),
            ignore_validation_failures: false,
        };
        let result = server
            .replace_full(Parameters(params))
            .await
            .expect("should succeed — unsupported gracefully degrades");
        let resp = result.0;

        assert_eq!(resp.validation.status, "skipped");
        assert!(resp.validation_skipped);
        assert_eq!(
            resp.validation_skipped_reason.as_deref(),
            Some("pull_diagnostics_unsupported")
        );
    }

    // ── pre_diag_timeout: first pull_diagnostics errors → skipped ───────

    #[tokio::test]
    async fn test_run_lsp_validation_pre_diag_timeout() {
        let ws_dir = tempdir().expect("temp dir");
        let filepath = "src/auth.go";
        let src = "func Login() {}";
        let (mock_surgeon, hash) = setup_full_replace_fixture(&ws_dir, filepath, src);

        let mock_lawyer = pathfinder_lsp::MockLawyer::default();
        // did_open succeeds (default); first pull_diagnostics returns a protocol
        // error — any error that is not UnsupportedCapability maps to
        // "diagnostic_timeout" in run_lsp_validation.
        mock_lawyer.push_pull_diagnostics_result(Err("LSP timed out".to_owned()));

        let server = make_server_with_lawyer(&ws_dir, mock_surgeon, mock_lawyer);

        let params = ReplaceFullParams {
            semantic_path: format!("{filepath}::Login"),
            base_version: hash.as_str().to_owned(),
            new_code: "func Login() { return }\n".to_owned(),
            ignore_validation_failures: false,
        };
        let result = server
            .replace_full(Parameters(params))
            .await
            .expect("should succeed — pre-diag timeout gracefully degrades");
        let resp = result.0;

        assert!(resp.success);
        assert_eq!(resp.validation.status, "skipped");
        assert!(resp.validation_skipped);
        assert_eq!(
            resp.validation_skipped_reason.as_deref(),
            Some("lsp_protocol_error")
        );
    }

    // ── pre_diag_unsupported: first pull_diagnostics → UnsupportedCapability
    //    → skipped with "pull_diagnostics_unsupported" reason ────────────────

    #[tokio::test]
    async fn test_run_lsp_validation_pull_diagnostics_unsupported() {
        let ws_dir = tempdir().expect("temp dir");
        let filepath = "src/auth.go";
        let src = "func Login() {}";
        // `mock_surgeon` is used in the first call but we need a fresh surgeon
        // for the second server construction; discard the first fixture result.
        let (_mock_surgeon, hash) = setup_full_replace_fixture(&ws_dir, filepath, src);

        // UnsupportedDiagLawyer always returns UnsupportedCapability from
        // pull_diagnostics, exercising the "pull_diagnostics_unsupported" branch.
        let (mock_surgeon_2, _) = setup_full_replace_fixture(&ws_dir, filepath, src);
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);
        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            Arc::new(mock_surgeon_2),
            Arc::new(UnsupportedDiagLawyer),
        );

        let params = ReplaceFullParams {
            semantic_path: format!("{filepath}::Login"),
            base_version: hash.as_str().to_owned(),
            new_code: "func Login() { return }\n".to_owned(),
            ignore_validation_failures: false,
        };
        let result = server
            .replace_full(Parameters(params))
            .await
            .expect("should succeed — pull_diagnostics_unsupported degrades");
        let resp = result.0;

        assert_eq!(resp.validation.status, "skipped");
        assert_eq!(
            resp.validation_skipped_reason.as_deref(),
            Some("pull_diagnostics_unsupported")
        );
    }

    // ── post_diag_timeout: second pull_diagnostics errors → skipped ──────

    #[tokio::test]
    async fn test_run_lsp_validation_post_diag_timeout() {
        let ws_dir = tempdir().expect("temp dir");
        let filepath = "src/auth.go";
        let src = "func Login() {}";
        let (mock_surgeon, hash) = setup_full_replace_fixture(&ws_dir, filepath, src);

        let mock_lawyer = pathfinder_lsp::MockLawyer::default();
        // Pre-edit pull_diagnostics succeeds with empty diags.
        mock_lawyer.push_pull_diagnostics_result(Ok(vec![]));
        // Post-edit pull_diagnostics errors (e.g. timeout).
        mock_lawyer.push_pull_diagnostics_result(Err("timeout after 10s".to_owned()));

        let server = make_server_with_lawyer(&ws_dir, mock_surgeon, mock_lawyer);

        let params = ReplaceFullParams {
            semantic_path: format!("{filepath}::Login"),
            base_version: hash.as_str().to_owned(),
            new_code: "func Login() { return }\n".to_owned(),
            ignore_validation_failures: false,
        };
        let result = server
            .replace_full(Parameters(params))
            .await
            .expect("should succeed — post-diag timeout gracefully degrades");
        let resp = result.0;

        assert!(resp.success);
        assert_eq!(resp.validation.status, "skipped");
        assert!(resp.validation_skipped);
        assert_eq!(
            resp.validation_skipped_reason.as_deref(),
            Some("lsp_protocol_error")
        );
    }

    // ── blocking: new errors introduced + ignore_validation_failures=false ─

    #[tokio::test]
    async fn test_run_lsp_validation_blocking() {
        use pathfinder_lsp::types::{LspDiagnostic, LspDiagnosticSeverity};

        let ws_dir = tempdir().expect("temp dir");
        let filepath = "src/auth.go";
        let src = "func Login() {}";
        let (mock_surgeon, hash) = setup_full_replace_fixture(&ws_dir, filepath, src);

        let mock_lawyer = pathfinder_lsp::MockLawyer::default();
        // Pre-edit: no errors.
        mock_lawyer.push_pull_diagnostics_result(Ok(vec![]));
        // Post-edit: one new error introduced.
        mock_lawyer.push_pull_diagnostics_result(Ok(vec![LspDiagnostic {
            severity: LspDiagnosticSeverity::Error,
            code: Some("E001".into()),
            message: "undefined: Foo".into(),
            file: filepath.to_owned(),
            start_line: 1,
            end_line: 1,
        }]));

        let server = make_server_with_lawyer(&ws_dir, mock_surgeon, mock_lawyer);

        // ignore_validation_failures = false → should block
        let params = ReplaceFullParams {
            semantic_path: format!("{filepath}::Login"),
            base_version: hash.as_str().to_owned(),
            new_code: "func Login() { Foo() }\n".to_owned(),
            ignore_validation_failures: false,
        };
        let result = server.replace_full(Parameters(params)).await;

        let Err(err) = result else {
            panic!("expected VALIDATION_FAILED error when new errors are introduced");
        };
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "VALIDATION_FAILED", "got: {err:?}");
        // Confirm the introduced error is surfaced (nested under details.introduced_errors
        // because pathfinder_to_error_data serializes ErrorResponse which has a `details` field)
        let introduced = err
            .data
            .as_ref()
            .and_then(|d| d.get("details"))
            .and_then(|d| d.get("introduced_errors"))
            .and_then(|v| v.as_array())
            .map_or(0, Vec::len);
        assert_eq!(
            introduced, 1,
            "one new error should appear in introduced_errors"
        );
    }

    // ── workspace blocking: new errors in other files block the edit ────────

    #[tokio::test]
    async fn test_run_lsp_validation_workspace_blocking() {
        use pathfinder_lsp::types::{LspDiagnostic, LspDiagnosticSeverity};

        let ws_dir = tempdir().expect("temp dir");
        let filepath = "src/auth.go";
        let src = "func Login() {}";
        let (mock_surgeon, hash) = setup_full_replace_fixture(&ws_dir, filepath, src);

        let mock_lawyer = pathfinder_lsp::MockLawyer::default();
        // Pre-edit diagnostics (file + workspace)
        mock_lawyer.push_pull_diagnostics_result(Ok(vec![]));
        mock_lawyer.push_pull_workspace_diagnostics_result(Ok(vec![]));

        // Post-edit diagnostics (no errors in single file, but 1 error in workspace)
        mock_lawyer.push_pull_diagnostics_result(Ok(vec![]));
        mock_lawyer.push_pull_workspace_diagnostics_result(Ok(vec![LspDiagnostic {
            severity: LspDiagnosticSeverity::Error,
            code: Some("E002".into()),
            message: "cannot call Login with 1 argument".into(),
            file: "src/main.go".to_owned(), // Different file!
            start_line: 5,
            end_line: 5,
        }]));

        let server = make_server_with_lawyer(&ws_dir, mock_surgeon, mock_lawyer);

        // ignore_validation_failures = false → should block
        let params = ReplaceFullParams {
            semantic_path: format!("{filepath}::Login"),
            base_version: hash.as_str().to_owned(),
            new_code: "func Login(a string) { }\n".to_owned(), // changed signature
            ignore_validation_failures: false,
        };
        let result = server.replace_full(Parameters(params)).await;

        let Err(err) = result else {
            panic!("expected VALIDATION_FAILED error when workspace errors are introduced");
        };
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "VALIDATION_FAILED", "got: {err:?}");

        // Confirm the workspace error is reported
        let introduced = err
            .data
            .as_ref()
            .and_then(|d| d.get("details"))
            .and_then(|d| d.get("introduced_errors"))
            .and_then(|v| v.as_array())
            .expect("should have introduced_errors");
        assert_eq!(
            introduced.len(),
            1,
            "one workspace error should appear in introduced_errors"
        );
        let first_err_file = introduced[0].get("file").and_then(|v| v.as_str()).unwrap();
        assert_eq!(
            first_err_file, "src/main.go",
            "error should be in src/main.go"
        );
    }

    // ── blocking_ignored: new errors + ignore_validation_failures=true → passes

    #[tokio::test]
    async fn test_run_lsp_validation_blocking_ignored() {
        use pathfinder_lsp::types::{LspDiagnostic, LspDiagnosticSeverity};

        let ws_dir = tempdir().expect("temp dir");
        let filepath = "src/auth.go";
        let src = "func Login() {}";
        let (mock_surgeon, hash) = setup_full_replace_fixture(&ws_dir, filepath, src);

        let mock_lawyer = pathfinder_lsp::MockLawyer::default();
        mock_lawyer.push_pull_diagnostics_result(Ok(vec![]));
        mock_lawyer.push_pull_diagnostics_result(Ok(vec![LspDiagnostic {
            severity: LspDiagnosticSeverity::Error,
            code: Some("E001".into()),
            message: "undefined: Foo".into(),
            file: filepath.to_owned(),
            start_line: 1,
            end_line: 1,
        }]));

        let server = make_server_with_lawyer(&ws_dir, mock_surgeon, mock_lawyer);

        // ignore_validation_failures = true → should NOT block, file is written
        let params = ReplaceFullParams {
            semantic_path: format!("{filepath}::Login"),
            base_version: hash.as_str().to_owned(),
            new_code: "func Login() { Foo() }\n".to_owned(),
            ignore_validation_failures: true,
        };
        let _result = server
            .replace_full(Parameters(params))
            .await
            .expect("should succeed when ignore_validation_failures=true");
        let filepath = "src/auth.go";
        let src = "func Login() {}";
        let (mock_surgeon, hash) = setup_full_replace_fixture(&ws_dir, filepath, src);

        let mock_lawyer = pathfinder_lsp::MockLawyer::default();
        // One pre-existing warning (non-error) in both pre and post.
        let existing_warning = LspDiagnostic {
            severity: LspDiagnosticSeverity::Warning,
            code: Some("W001".into()),
            message: "unused import".into(),
            file: filepath.to_owned(),
            start_line: 1,
            end_line: 1,
        };
        mock_lawyer.push_pull_diagnostics_result(Ok(vec![existing_warning.clone()]));
        mock_lawyer.push_pull_diagnostics_result(Ok(vec![existing_warning]));

        let server = make_server_with_lawyer(&ws_dir, mock_surgeon, mock_lawyer);

        let params = ReplaceFullParams {
            semantic_path: format!("{filepath}::Login"),
            base_version: hash.as_str().to_owned(),
            new_code: "func Login() { return }\n".to_owned(),
            ignore_validation_failures: false,
        };
        let result = server
            .replace_full(Parameters(params))
            .await
            .expect("should succeed — no new errors");
        let resp = result.0;

        assert!(resp.success);
        assert_eq!(resp.validation.status, "passed");
        assert!(!resp.validation_skipped);
        assert!(resp.validation.introduced_errors.is_empty());
        assert!(resp.validation.resolved_errors.is_empty());
    }
}

// ── resolve_text_edit unit tests (E3.1) ────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod text_edit_tests {
    use super::*;
    use std::path::Path;

    fn src(lines: &[&str]) -> Vec<u8> {
        lines.join("\n").into_bytes()
    }

    // ── success cases ───────────────────────────────────────────────────

    #[test]
    fn test_exact_match_replaces_correctly() {
        let source = src(&[
            "line 1",
            "line 2",
            "line 3",
            "line 4",
            "<button>Click me</button>",
            "line 6",
            "line 7",
        ]);
        let r = resolve_text_edit(
            &source,
            "<button>Click me</button>",
            5,
            "<button>Submit</button>",
            false,
            Path::new("app.vue"),
        )
        .expect("exact match should succeed");

        let mut out = source.clone();
        out.splice(r.start_byte..r.end_byte, r.replacement);
        let out_str = String::from_utf8(out).unwrap();
        assert!(
            out_str.contains("<button>Submit</button>"),
            "replacement present: {out_str}"
        );
        assert!(
            !out_str.contains("<button>Click me</button>"),
            "old text gone: {out_str}"
        );
    }

    #[test]
    fn test_window_boundary_at_plus_10_is_included() {
        // Target on line 20, context_line 10 → window = [1, 20].
        let mut lines = vec!["filler"; 25];
        lines[19] = "special text"; // line 20 (1-indexed)
        let source = src(&lines);
        resolve_text_edit(
            &source,
            "special text",
            10,
            "replaced",
            false,
            Path::new("a.rs"),
        )
        .expect("±10 edge should be included");
    }

    #[test]
    fn test_context_line_zero_clamped_safely() {
        let source = src(&["only line"]);
        let r = resolve_text_edit(
            &source,
            "only line",
            0,
            "replaced",
            false,
            Path::new("a.rs"),
        )
        .expect("context_line=0 clamped to center=0 → window ok");
        assert_eq!(&r.replacement, b"replaced");
    }

    #[test]
    fn test_multiline_old_text() {
        let source = src(&[
            "fn foo() {",
            "    let x = 1;",
            "    let y = 2;",
            "}",
            "fn bar() {}",
        ]);
        let old = "    let x = 1;\n    let y = 2;";
        let r = resolve_text_edit(
            &source,
            old,
            2,
            "    let z = 42;",
            false,
            Path::new("lib.rs"),
        )
        .expect("multi-line match should succeed");
        let mut out = source.clone();
        out.splice(r.start_byte..r.end_byte, r.replacement);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("let z = 42;"), "replacement present: {s}");
        assert!(!s.contains("let x = 1;"), "old text removed: {s}");
    }

    // ── normalize_whitespace ────────────────────────────────────────────

    #[test]
    fn test_normalize_whitespace_matches_with_collapsed_spaces() {
        let source = src(&[
            "<div>",
            "  <button   class=\"btn\"   >Click</button>",
            "</div>",
        ]);
        let r = resolve_text_edit(
            &source,
            "<button class=\"btn\" >Click</button>",
            2,
            "<button class=\"btn\">Submit</button>",
            true,
            Path::new("comp.vue"),
        )
        .expect("normalized whitespace should match");
        let mut out = source.clone();
        out.splice(r.start_byte..r.end_byte, r.replacement);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("Submit"), "replacement present: {s}");
    }

    #[test]
    fn test_no_normalize_fails_on_spacing_mismatch() {
        // With fuzzy fallback, spacing mismatches are now handled automatically
        let source = src(&["<button   class=\"btn\">Click</button>"]);
        let r = resolve_text_edit(
            &source,
            "<button class=\"btn\">Click</button>",
            1,
            "<button>Submit</button>",
            false,
            Path::new("a.vue"),
        )
        .expect("fuzzy fallback should handle spacing mismatch");

        let mut out = source.clone();
        out.splice(r.start_byte..r.end_byte, r.replacement);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("Submit"), "replacement present");
        assert!(!s.contains("Click"), "old text removed");
    }

    // ── failure cases ───────────────────────────────────────────────────

    #[test]
    fn test_text_not_in_window_returns_text_not_found() {
        let mut lines = vec!["line"; 60];
        lines[50] = "target text"; // line 51
        let source = src(&lines);
        // Window for context_line=5 covers lines 1–30 (±25); line 51 is outside.
        let err = resolve_text_edit(&source, "target text", 5, "r", false, Path::new("a.rs"))
            .expect_err("out-of-window match should fail");
        let pathfinder_common::error::PathfinderError::TextNotFound { context_line, .. } = err
        else {
            panic!("expected TextNotFound, got: {err:?}");
        };
        assert_eq!(context_line, 5);
    }

    #[test]
    fn test_text_not_found_at_all_returns_error() {
        let source = src(&["hello world"]);
        let err = resolve_text_edit(&source, "not present", 1, "", false, Path::new("f.rs"))
            .expect_err("missing text returns TextNotFound");
        assert!(matches!(
            err,
            pathfinder_common::error::PathfinderError::TextNotFound { .. }
        ));
    }

    // ── Fix 3: Text Edit Improvements ─────────────────────────────────────

    #[test]
    fn test_window_25_lines() {
        // Test that the search window is ±25 lines (not ±10)
        let source = src(&[
            "line 1", "line 2", "line 3", "line 4", "line 5", "line 6", "line 7", "line 8",
            "line 9", "line 10", "line 11", "line 12", "line 13", "line 14", "line 15", "line 16",
            "line 17", "line 18", "line 19", "line 20", "line 21", "line 22", "line 23", "line 24",
            "line 25", "line 26", "line 27", "line 28", "line 29", "line 30",
        ]);

        // context_line=15, target text at line 30 (15+15=30, within ±25 window)
        let r = resolve_text_edit(
            &source,
            "line 30",
            15,
            "replaced",
            false,
            Path::new("test.rs"),
        )
        .expect("match at line 30 (±25 from line 15) should succeed");

        let mut out = source.clone();
        out.splice(r.start_byte..r.end_byte, r.replacement);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("replaced"), "replacement present");
        assert!(!s.contains("line 30"), "old text removed");
    }

    #[test]
    fn test_fuzzy_whitespace_fallback() {
        // Test that whitespace-normalized fuzzy matching is attempted when exact match fails
        let source = src(&[
            "<div>",
            "  <button   class=\"btn\">Click</button>",
            "</div>",
        ]);

        // Exact match should fail (wrong spacing), but fuzzy fallback should succeed
        let r = resolve_text_edit(
            &source,
            "<button class=\"btn\">Click</button>",
            2,
            "<button>Submit</button>",
            false,
            Path::new("comp.vue"),
        )
        .expect("fuzzy whitespace fallback should succeed");

        let mut out = source.clone();
        out.splice(r.start_byte..r.end_byte, r.replacement);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("Submit"), "replacement present");
        assert!(!s.contains("Click"), "old text removed");
    }

    #[test]
    fn test_text_not_found_includes_actual_content() {
        let source = src(&["line 1", "line 2", "line 3"]);

        let err = resolve_text_edit(&source, "not present", 2, "", false, Path::new("f.rs"))
            .expect_err("missing text returns TextNotFound");

        let pathfinder_common::error::PathfinderError::TextNotFound {
            filepath,
            old_text,
            context_line,
            actual_content,
        } = err
        else {
            panic!("expected TextNotFound, got: {err:?}");
        };

        assert_eq!(filepath, Path::new("f.rs"));
        assert_eq!(old_text, "not present");
        assert_eq!(context_line, 2);
        assert!(
            actual_content.is_some(),
            "actual_content should be populated with window text"
        );

        let content = actual_content.unwrap();
        assert!(
            content.contains("line 1") || content.contains("line 2") || content.contains("line 3"),
            "actual_content should contain context from the source file"
        );
    }
}
