use super::text_edit::{
    build_body_replacement, is_whitespace_significant_file, normalize_blank_lines,
    strip_orphaned_doc_comment,
};
use super::{FinalizeEditParams, InsertEdge};
use crate::server::helpers::{
    check_occ, check_sandbox_access, io_error_data, parse_semantic_path, require_symbol_target,
};
use crate::server::types::{
    DeleteSymbolParams, EditResponse, EditValidation, InsertAfterParams, InsertBeforeParams,
    ReplaceBodyParams, ReplaceFullParams, ValidateOnlyParams,
};
use pathfinder_common::indent::dedent_then_reindent;
use pathfinder_common::normalize::{normalize_for_body_replace, normalize_for_full_replace};
use pathfinder_common::types::VersionHash;
use rmcp::handler::server::wrapper::Json;
use rmcp::model::ErrorData;
use tracing::instrument;

impl crate::server::PathfinderServer {
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
        check_sandbox_access(
            &self.sandbox,
            &semantic_path.file_path,
            "replace_body",
            &params.semantic_path,
        )?;

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
        check_occ(
            &params.base_version,
            &current_hash,
            semantic_path.file_path.clone(),
        )?;

        // ── Step 5: Normalize new_code ────────────────────────────────
        let normalized = normalize_for_body_replace(&params.new_code);

        // ── Steps 6–7: Indent + splice ──────────────────────────────────
        let indented = dedent_then_reindent(&normalized, body_range.body_indent_column);
        let new_content = build_body_replacement(&source, &body_range, &indented)?;
        let new_bytes = new_content.as_bytes();

        // ── Steps 8–11: Validate → TOCTOU → Write → Respond ────────────
        let resolve_ms = start.elapsed().as_millis();
        self.finalize_edit(FinalizeEditParams {
            tool_name: "replace_body",
            semantic_path: &semantic_path,
            raw_semantic_path_str: &params.semantic_path,
            source: &source,
            original_hash: &current_hash,
            new_content: new_bytes.to_vec(),
            ignore_validation_failures: params.ignore_validation_failures,
            start_time: start,
            resolve_ms,
        })
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

        check_sandbox_access(
            &self.sandbox,
            &semantic_path.file_path,
            "replace_full",
            &params.semantic_path,
        )?;

        // C2: Bare-file replace_full bypasses AST validation.
        //
        // DESIGN DECISION:
        // When a user targets an entire file (bare path), we skip tree-sitter parsing
        // and LSP validation to allow full-file replacements (e.g., config file edits,
        // code generation, or file-wide refactors). This is intentional flexibility.
        //
        // SECURITY IMPLICATIONS:
        // - No AST validation means malformed code could be written
        // - LSP validation is also skipped for bare files
        // - Caller assumes responsibility for content validity
        // - OCC still prevents race conditions
        //
        // MITIGATION:
        // We perform an optional post-write tree-sitter parse check that logs a warning
        // (but does NOT block the write) to catch obvious syntax errors early.
        let (source, current_hash, new_bytes) = if semantic_path.is_bare_file() {
            let absolute_path = self.workspace_root.resolve(&semantic_path.file_path);
            let source = tokio::fs::read(&absolute_path)
                .await
                .map_err(|e| io_error_data(format!("failed to read file: {e}")))?;
            let current_hash = VersionHash::compute(&source);

            check_occ(
                &params.base_version,
                &current_hash,
                semantic_path.file_path.clone(),
            )?;

            // For bare file substitution, insert exactly as provided
            let new_bytes = params.new_code.as_bytes().to_vec();

            // C2: Optional tree-sitter parse check (logs warning but does not block)
            // This catches obvious syntax errors without preventing the write
            if let Ok(new_str) = std::str::from_utf8(&new_bytes) {
                if let Some(lang) = pathfinder_treesitter::language::SupportedLanguage::detect(
                    &semantic_path.file_path,
                ) {
                    match pathfinder_treesitter::parser::AstParser::parse_source(
                        &semantic_path.file_path,
                        lang,
                        new_str.as_bytes(),
                    ) {
                        Ok(tree) => {
                            if tree.root_node().has_error() {
                                tracing::warn!(
                                    file = %semantic_path.file_path.display(),
                                    "replace_full: bare file content has parse errors (tree-sitter reported ERROR nodes)"
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                file = %semantic_path.file_path.display(),
                                error = %e,
                                "replace_full: bare file content failed tree-sitter parse check - syntax errors likely"
                            );
                        }
                    }
                }
            } else {
                tracing::warn!(
                    file = %semantic_path.file_path.display(),
                    "replace_full: bare file content is not valid UTF-8, skipping parse check"
                );
            }

            (std::sync::Arc::from(source), current_hash, new_bytes)
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

            check_occ(
                &params.base_version,
                &current_hash,
                semantic_path.file_path.clone(),
            )?;

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
        self.finalize_edit(FinalizeEditParams {
            tool_name: "replace_full",
            semantic_path: &semantic_path,
            raw_semantic_path_str: &params.semantic_path,
            source: &source,
            original_hash: &current_hash,
            new_content: new_bytes,
            ignore_validation_failures: params.ignore_validation_failures,
            start_time: start,
            resolve_ms,
        })
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

        check_sandbox_access(
            &self.sandbox,
            &semantic_path.file_path,
            "insert_before",
            &params.semantic_path,
        )?;

        let (insert_byte, indent_column, source, current_hash) = self
            .resolve_insert_position(&semantic_path, InsertEdge::Before)
            .await?;

        check_occ(
            &params.base_version,
            &current_hash,
            semantic_path.file_path.clone(),
        )?;

        let normalized = normalize_for_full_replace(&params.new_code);
        let indented = dedent_then_reindent(&normalized, indent_column);

        // Splice: insert at insert_byte with a double newline separator
        let before = &source[..insert_byte];
        let after = &source[insert_byte..];

        // C9: Prevent double blank lines by checking both sides.
        // No separator when the boundary already provides sufficient whitespace:
        // - before ends with double newline, or
        // - after starts with double newline, or
        // - before ends with newline AND after starts with newline
        // Otherwise use a single newline if after already has one, or double newline.
        let sep = if before.ends_with(b"\n\n")
            || after.starts_with(b"\n\n")
            || (before.ends_with(b"\n") && after.starts_with(b"\n"))
        {
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

        if !is_whitespace_significant_file(std::path::Path::new(&semantic_path.file_path)) {
            new_bytes = normalize_blank_lines(&new_bytes);
        }

        let resolve_ms = start.elapsed().as_millis();
        self.finalize_edit(FinalizeEditParams {
            tool_name: "insert_before",
            semantic_path: &semantic_path,
            raw_semantic_path_str: &params.semantic_path,
            source: &source,
            original_hash: &current_hash,
            new_content: new_bytes,
            ignore_validation_failures: params.ignore_validation_failures,
            start_time: start,
            resolve_ms,
        })
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

        check_sandbox_access(
            &self.sandbox,
            &semantic_path.file_path,
            "insert_after",
            &params.semantic_path,
        )?;

        let (insert_byte, indent_column, source, current_hash) = self
            .resolve_insert_position(&semantic_path, InsertEdge::After)
            .await?;

        check_occ(
            &params.base_version,
            &current_hash,
            semantic_path.file_path.clone(),
        )?;

        let normalized = normalize_for_full_replace(&params.new_code);
        let indented = dedent_then_reindent(&normalized, indent_column);

        let before = &source[..insert_byte];
        let after = &source[insert_byte..];

        // C9: Prevent double blank lines by checking both sides.
        // No separator when the boundary already provides sufficient whitespace:
        // - before ends with double newline, or
        // - after starts with double newline, or
        // - before ends with newline AND after starts with newline
        // Otherwise use a single newline if before has one, or double newline.
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

        let mut new_bytes = Vec::with_capacity(
            before.len() + before_sep.len() + indented.len() + after_sep.len() + after.len(),
        );
        new_bytes.extend_from_slice(before);
        new_bytes.extend_from_slice(before_sep.as_bytes());
        new_bytes.extend_from_slice(indented.as_bytes());
        new_bytes.extend_from_slice(after_sep.as_bytes());
        new_bytes.extend_from_slice(after);

        if !is_whitespace_significant_file(std::path::Path::new(&semantic_path.file_path)) {
            new_bytes = normalize_blank_lines(&new_bytes);
        }

        let resolve_ms = start.elapsed().as_millis();
        self.finalize_edit(FinalizeEditParams {
            tool_name: "insert_after",
            semantic_path: &semantic_path,
            raw_semantic_path_str: &params.semantic_path,
            source: &source,
            original_hash: &current_hash,
            new_content: new_bytes,
            ignore_validation_failures: params.ignore_validation_failures,
            start_time: start,
            resolve_ms,
        })
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
    pub(crate) async fn resolve_insert_position(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        edge: InsertEdge,
    ) -> Result<(usize, usize, std::sync::Arc<[u8]>, VersionHash), ErrorData> {
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
            return Ok((offset, 0, std::sync::Arc::from(bytes), hash));
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

        check_sandbox_access(
            &self.sandbox,
            &semantic_path.file_path,
            "delete_symbol",
            &params.semantic_path,
        )?;

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

        check_occ(
            &params.base_version,
            &current_hash,
            semantic_path.file_path.clone(),
        )?;

        // Collapse whitespace: If deleting a symbol leaves more than one consecutive blank line, collapse it.
        // Or simply: strip the symbol, then normalise the gap.
        let before_end = full_range.start_byte;
        let after_start = full_range.end_byte;

        // Post-pass: strip any orphaned doc-comment fragment on the line immediately
        // preceding the symbol.
        let before_end = strip_orphaned_doc_comment(&source, before_end);

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
        self.finalize_edit(FinalizeEditParams {
            tool_name: "delete_symbol",
            semantic_path: &semantic_path,
            raw_semantic_path_str: &params.semantic_path,
            source: &source,
            original_hash: &current_hash,
            new_content: new_bytes,
            ignore_validation_failures: params.ignore_validation_failures,
            start_time: start,
            resolve_ms,
        })
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

        check_sandbox_access(
            &self.sandbox,
            &semantic_path.file_path,
            "validate_only",
            &params.semantic_path,
        )?;

        // Resolve the current version hash for the target path+type and OCC-check it.
        let current_hash = self
            .resolve_version_hash_for_edit_type(
                &semantic_path,
                &params.semantic_path,
                params.edit_type.as_str(),
            )
            .await?;

        check_occ(
            &params.base_version,
            &current_hash,
            semantic_path.file_path.clone(),
        )?;

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
            // B5: Improved skip reason explaining why LSP validation is skipped
            validation_skipped_reason: Some(
                "validate_only mode: LSP validation requires writing to disk, which is not performed in validate-only mode. Tree-sitter structural validation was performed.".to_owned()
            ),
        }))
    }
}
