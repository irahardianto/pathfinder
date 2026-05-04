use super::text_edit::resolve_text_edit;
use super::{FinalizeEditParams, ResolvedEdit};
use crate::server::helpers::{
    check_occ, check_sandbox_access, io_error_data, pathfinder_to_error_data,
};
use crate::server::types::EditResponse;
use pathfinder_common::error::PathfinderError;
use pathfinder_common::indent::dedent_then_reindent;
use pathfinder_common::normalize::{normalize_for_body_replace, normalize_for_full_replace};
use pathfinder_common::types::{SemanticPath, VersionHash};
use rmcp::handler::server::wrapper::Json;
use rmcp::model::ErrorData;
use std::path::{Path, PathBuf};
use tracing::instrument;

impl crate::server::PathfinderServer {
    /// Validate OCC and read file content for batch edits.
    pub(crate) async fn validate_batch_occ(
        &self,
        absolute_path: &Path,
        base_version: &str,
        filepath_str: &str,
    ) -> Result<(Vec<u8>, VersionHash), ErrorData> {
        let source = tokio::fs::read(absolute_path)
            .await
            .map_err(|e| io_error_data(format!("failed to read file: {e}")))?;
        let current_hash = VersionHash::compute(&source);

        check_occ(base_version, &current_hash, PathBuf::from(filepath_str))?;

        Ok((source, current_hash))
    }

    /// Resolve a single edit from a batch into a concrete byte range.
    pub(crate) async fn resolve_single_batch_edit(
        &self,
        edit: &crate::server::types::BatchEdit,
        edit_index: usize,
        source: &[u8],
        file_path: &Path,
    ) -> Result<ResolvedEdit, ErrorData> {
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
                source,
                old_text.as_str(),
                context_line,
                replacement,
                edit.normalize_whitespace,
                file_path,
            )
            .map_err(|e| pathfinder_to_error_data(&e))?;
            return Ok(ResolvedEdit {
                start_byte: free.start_byte,
                end_byte: free.end_byte,
                replacement: free.replacement,
            });
        }

        // ── Branch B: Semantic targeting ───────────────────────────────────────
        let Some(semantic_path) = SemanticPath::parse(&edit.semantic_path) else {
            let err = PathfinderError::InvalidSemanticPath {
                input: edit.semantic_path.clone(),
                issue: "Semantic path is malformed or missing '::' separator.".to_owned(),
            };
            return Err(pathfinder_to_error_data(&err));
        };

        self.resolve_semantic_batch_edit(&semantic_path, edit, edit_index, source)
            .await
    }

    /// Batch resolver for `replace_body` edits.
    pub(crate) async fn resolve_batch_replace_body(
        &self,
        semantic_path: &SemanticPath,
        new_code: &str,
        source: &[u8],
    ) -> Result<ResolvedEdit, ErrorData> {
        let (body_range, _) = self
            .surgeon
            .resolve_body_range(self.workspace_root.path(), semantic_path)
            .await
            .map_err(crate::server::helpers::treesitter_error_to_error_data)?;

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
            Ok(ResolvedEdit {
                start_byte: inner_start,
                end_byte: inner_end,
                replacement,
            })
        } else {
            let mut end = body_range.start_byte;
            while end > 0 && (source[end - 1] == b' ' || source[end - 1] == b'\t') {
                end -= 1;
            }
            Ok(ResolvedEdit {
                start_byte: end,
                end_byte: body_range.end_byte,
                replacement: format!("\n{indented}").into_bytes(),
            })
        }
    }

    /// Batch resolver for `replace_full` edits.
    pub(crate) async fn resolve_batch_replace_full(
        &self,
        semantic_path: &SemanticPath,
        new_code: &str,
        source: &[u8],
    ) -> Result<ResolvedEdit, ErrorData> {
        if semantic_path.is_bare_file() {
            return Ok(ResolvedEdit {
                start_byte: 0,
                end_byte: source.len(),
                replacement: new_code.as_bytes().to_vec(),
            });
        }

        let (full_range, _) = self
            .surgeon
            .resolve_full_range(self.workspace_root.path(), semantic_path)
            .await
            .map_err(crate::server::helpers::treesitter_error_to_error_data)?;

        let normalized = normalize_for_full_replace(new_code);
        let indented = dedent_then_reindent(&normalized, full_range.indent_column);

        Ok(ResolvedEdit {
            start_byte: full_range.start_byte,
            end_byte: full_range.end_byte,
            replacement: indented.into_bytes(),
        })
    }

    /// Batch resolver for `insert_before` edits.
    pub(crate) async fn resolve_batch_insert_before(
        &self,
        semantic_path: &SemanticPath,
        new_code: &str,
        source: &[u8],
    ) -> Result<ResolvedEdit, ErrorData> {
        let (insert_byte, indent_column) = if semantic_path.is_bare_file() {
            (0, 0)
        } else {
            let (symbol_range, _) = self
                .surgeon
                .resolve_symbol_range(self.workspace_root.path(), semantic_path)
                .await
                .map_err(crate::server::helpers::treesitter_error_to_error_data)?;
            (symbol_range.start_byte, symbol_range.indent_column)
        };

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

        Ok(ResolvedEdit {
            start_byte: insert_byte,
            end_byte: insert_byte,
            replacement: format!("{indented}{trailing}{sep}").into_bytes(),
        })
    }

    /// Batch resolver for `insert_after` edits.
    pub(crate) async fn resolve_batch_insert_after(
        &self,
        semantic_path: &SemanticPath,
        new_code: &str,
        source: &[u8],
    ) -> Result<ResolvedEdit, ErrorData> {
        let (insert_byte, indent_column) = if semantic_path.is_bare_file() {
            (source.len(), 0)
        } else {
            let (symbol_range, _) = self
                .surgeon
                .resolve_symbol_range(self.workspace_root.path(), semantic_path)
                .await
                .map_err(crate::server::helpers::treesitter_error_to_error_data)?;
            (symbol_range.end_byte, symbol_range.indent_column)
        };

        let normalized = normalize_for_full_replace(new_code);
        let indented = dedent_then_reindent(&normalized, indent_column);

        let before = &source[..insert_byte];
        let after = &source[insert_byte..];

        // WP5: Match the standalone insert_after spacing logic: check both `before`
        // AND `after` content so that inserting between two top-level items doesn't
        // produce `}\npub fn` without the required blank line.
        let before_sep = if before.ends_with(b"\n\n")
            || after.starts_with(b"\n\n")
            || (before.ends_with(b"\n") && after.starts_with(b"\n"))
        {
            ""
        } else if before.ends_with(b"\n") {
            "\n"
        } else {
            "\n\n"
        };
        let after_sep = if indented.ends_with('\n') { "" } else { "\n" };

        Ok(ResolvedEdit {
            start_byte: insert_byte,
            end_byte: insert_byte,
            replacement: format!("{before_sep}{indented}{after_sep}").into_bytes(),
        })
    }

    /// Batch resolver for `delete` edits.
    pub(crate) async fn resolve_batch_delete(
        &self,
        semantic_path: &SemanticPath,
        source: &[u8],
    ) -> Result<ResolvedEdit, ErrorData> {
        let (full_range, _) = self
            .surgeon
            .resolve_full_range(self.workspace_root.path(), semantic_path)
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

        Ok(ResolvedEdit {
            start_byte: b_end,
            end_byte: a_start,
            replacement: sep.to_vec(),
        })
    }

    /// Dispatch a semantic batch edit to the per-type resolver.
    pub(crate) async fn resolve_semantic_batch_edit(
        &self,
        semantic_path: &SemanticPath,
        edit: &crate::server::types::BatchEdit,
        edit_index: usize,
        source: &[u8],
    ) -> Result<ResolvedEdit, ErrorData> {
        let new_code = edit.new_code.as_deref().unwrap_or_default();
        match edit.edit_type.as_str() {
            "replace_body" => {
                self.resolve_batch_replace_body(semantic_path, new_code, source)
                    .await
            }
            "replace_full" => {
                self.resolve_batch_replace_full(semantic_path, new_code, source)
                    .await
            }
            "insert_before" => {
                self.resolve_batch_insert_before(semantic_path, new_code, source)
                    .await
            }
            "insert_after" => {
                self.resolve_batch_insert_after(semantic_path, new_code, source)
                    .await
            }
            "delete" => self.resolve_batch_delete(semantic_path, source).await,
            _unknown => {
                let err = PathfinderError::InvalidTarget {
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
                Err(pathfinder_to_error_data(&err))
            }
        }
    }

    /// Apply resolved edits to source content, sorted backwards to prevent offset shifts.
    pub(crate) fn apply_sorted_edits(
        source: &[u8],
        mut resolved_edits: Vec<(usize, String, ResolvedEdit)>,
    ) -> Result<Vec<u8>, ErrorData> {
        // Sort edits backwards to prevent shifted byte offsets
        resolved_edits.sort_by_key(|(_, _, e)| std::cmp::Reverse(e.start_byte));

        // Ensure no overlapping edits
        for i in 1..resolved_edits.len() {
            let (prev_idx, _, prev) = &resolved_edits[i - 1]; // This is later in the file
            let (curr_idx, curr_path, curr) = &resolved_edits[i]; // This is earlier in the file
            if curr.end_byte > prev.start_byte {
                let err = PathfinderError::InvalidTarget {
                    semantic_path: curr_path.clone(),
                    reason: format!(
                        "overlapping edits in replace_batch: edit {curr_idx} overlaps with edit {prev_idx}"
                    ),
                    edit_index: Some(*curr_idx),
                    valid_edit_types: None,
                };
                return Err(pathfinder_to_error_data(&err));
            }
        }

        let mut new_bytes = source.to_vec();
        for (_, _, edit) in resolved_edits {
            new_bytes.splice(edit.start_byte..edit.end_byte, edit.replacement);
        }

        Ok(new_bytes)
    }

    /// WP3: Post-apply structural validation.
    ///
    /// Re-parses both the original source and the edited result with Tree-sitter,
    /// then compares `ERROR` node counts. If the edit introduces new parse errors,
    /// it's likely due to nesting corruption (e.g., adjacent `replace_full` edits
    /// consuming each other's closing braces).
    ///
    /// Returns `Ok(())` if the edit is structurally sound, or an `ErrorData` with
    /// a clear message suggesting sequential edits as a fallback.
    fn verify_no_new_parse_errors(
        original: &[u8],
        edited: &[u8],
        file_path: &Path,
    ) -> Result<(), ErrorData> {
        let original_errors =
            pathfinder_treesitter::language::count_parse_errors(original, file_path);
        let edited_errors = pathfinder_treesitter::language::count_parse_errors(edited, file_path);

        let (Some(orig), Some(edited_count)) = (original_errors, edited_errors) else {
            return Ok(());
        };

        if edited_count > orig {
            let new_errors = edited_count - orig;
            let err = PathfinderError::BatchStructuralCorruption {
                filepath: file_path.display().to_string(),
                original_errors: orig,
                new_errors,
            };
            return Err(pathfinder_to_error_data(&err));
        }

        Ok(())
    }

    /// Core logic for the `replace_batch` tool (PRD Epic 5).
    ///
    /// Executes multiple edits on the same file atomically. Edits are resolved,
    /// sorted backwards by byte offset, and spliced together. This avoids OCC
    /// mismatches from chains of edits.
    #[instrument(skip(self, params), fields(filepath = %params.filepath))]
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
        check_sandbox_access(&self.sandbox, file_path, "replace_batch", &params.filepath)?;

        let absolute_path = self.workspace_root.resolve(file_path);
        let (source, current_hash) = self
            .validate_batch_occ(&absolute_path, &params.base_version, &params.filepath)
            .await?;

        let mut resolved_edits = Vec::new();
        for (edit_index, edit) in params.edits.iter().enumerate() {
            let resolved = self
                .resolve_single_batch_edit(edit, edit_index, &source, file_path)
                .await?;
            let path_or_text = if !edit.semantic_path.is_empty() {
                edit.semantic_path.clone()
            } else if let Some(old_text) = &edit.old_text {
                format!("text match: '{old_text}'")
            } else {
                "unknown".to_string()
            };
            resolved_edits.push((edit_index, path_or_text, resolved));
        }

        let new_bytes = Self::apply_sorted_edits(&source, resolved_edits)?;

        // WP3: Post-apply structural validation — re-parse the result with
        // Tree-sitter and reject if it introduces parse errors that weren't
        // in the original source. This catches nesting corruption from
        // overlapping or adjacent symbol replacements.
        Self::verify_no_new_parse_errors(&source, &new_bytes, file_path)?;

        let resolve_ms = start.elapsed().as_millis();

        // C1: Log when SemanticPath::parse fails and falls back to bare file
        let semantic_path = if let Some(p) = SemanticPath::parse(&params.filepath) {
            p
        } else {
            tracing::warn!(
                filepath = %params.filepath,
                "replace_batch: SemanticPath::parse failed, treating as bare file"
            );
            SemanticPath {
                file_path: file_path.to_path_buf(),
                symbol_chain: None,
            }
        };

        self.finalize_edit(FinalizeEditParams {
            tool_name: "replace_batch",
            semantic_path: &semantic_path,
            raw_semantic_path_str: &params.filepath,
            source: &source,
            original_hash: &current_hash,
            new_content: new_bytes,
            ignore_validation_failures: params.ignore_validation_failures,
            start_time: start,
            resolve_ms,
            warning: None,
        })
        .await
    }
}
