use super::text_edit::{
    is_whitespace_significant_file, normalize_blank_lines, strip_orphaned_doc_comment,
};
use super::{FinalizeEditParams, InsertEdge};
use crate::server::helpers::{
    check_occ, check_sandbox_access, io_error_data, parse_semantic_path, require_symbol_target,
};
use crate::server::types::{
    DeleteSymbolParams, EditResponse, InsertAfterParams, InsertBeforeParams, ReplaceBodyParams,
    ReplaceFullParams, ValidateOnlyParams,
};
use pathfinder_common::indent::dedent_then_reindent;
use pathfinder_common::normalize::normalize_for_full_replace;
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

        // ── Step 2: Sandbox check ──────────────────────────────────────
        check_sandbox_access(
            &self.sandbox,
            &semantic_path.file_path,
            "replace_body",
            &params.semantic_path,
        )?;

        let (source, current_hash, new_bytes) = self
            .resolve_edit_content(
                &semantic_path,
                &params.semantic_path,
                "replace_body",
                Some(&params.new_code),
            )
            .await?;

        // ── Step 4: OCC check ─────────────────────────────────────────
        check_occ(
            &params.base_version,
            &current_hash,
            semantic_path.file_path.clone(),
        )?;

        // ── Steps 8–11: Validate → TOCTOU → Write → Respond ────────────
        let resolve_ms = start.elapsed().as_millis();
        self.finalize_edit(FinalizeEditParams {
            tool_name: "replace_body",
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
        let (source, current_hash, new_bytes) = self
            .resolve_edit_content(
                &semantic_path,
                &params.semantic_path,
                "replace_full",
                Some(&params.new_code),
            )
            .await?;

        check_occ(
            &params.base_version,
            &current_hash,
            semantic_path.file_path.clone(),
        )?;

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

        let (source, current_hash, new_bytes) = self
            .resolve_edit_content(
                &semantic_path,
                &params.semantic_path,
                "insert_before",
                Some(&params.new_code),
            )
            .await?;

        check_occ(
            &params.base_version,
            &current_hash,
            semantic_path.file_path.clone(),
        )?;

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

        let (source, current_hash, new_bytes) = self
            .resolve_edit_content(
                &semantic_path,
                &params.semantic_path,
                "insert_after",
                Some(&params.new_code),
            )
            .await?;

        check_occ(
            &params.base_version,
            &current_hash,
            semantic_path.file_path.clone(),
        )?;

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

    /// Core logic for the `insert_into` tool (PRD Epic 5, Story 5.5).
    #[instrument(skip(self, params), fields(semantic_path = %params.semantic_path))]
    pub(crate) async fn insert_into_impl(
        &self,
        params: crate::server::types::InsertIntoParams,
    ) -> Result<Json<EditResponse>, ErrorData> {
        let start = std::time::Instant::now();
        tracing::info!(
            tool = "insert_into",
            semantic_path = %params.semantic_path,
            "insert_into: start"
        );
        let semantic_path = parse_semantic_path(&params.semantic_path)?;
        require_symbol_target(&semantic_path, &params.semantic_path)?;

        check_sandbox_access(
            &self.sandbox,
            &semantic_path.file_path,
            "insert_into",
            &params.semantic_path,
        )?;

        let (source, current_hash, new_bytes) = self
            .resolve_edit_content(
                &semantic_path,
                &params.semantic_path,
                "insert_into",
                Some(&params.new_code),
            )
            .await?;

        check_occ(
            &params.base_version,
            &current_hash,
            semantic_path.file_path.clone(),
        )?;

        let resolve_ms = start.elapsed().as_millis();
        self.finalize_edit(FinalizeEditParams {
            tool_name: "insert_into",
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

        check_sandbox_access(
            &self.sandbox,
            &semantic_path.file_path,
            "delete_symbol",
            &params.semantic_path,
        )?;

        let (source, current_hash, new_bytes) = self
            .resolve_edit_content(&semantic_path, &params.semantic_path, "delete", None)
            .await?;

        // P2-1: Cross-File Reference Warning.
        //
        // Before deleting, check whether the symbol is still referenced elsewhere in the
        // workspace. We use `rg -l -w <name>` as a fast heuristic. False positives are
        // possible (e.g., identically-named symbols in unrelated code), but false negatives
        // are not — which is the safe direction.
        //
        // The agent can bypass this check with `ignore_validation_failures: true`.
        if !params.ignore_validation_failures {
            if let Some(symbol_chain) = &semantic_path.symbol_chain {
                if let Some(symbol) = symbol_chain.segments.last() {
                    let symbol_name = &symbol.name;
                    let workspace_path = self.workspace_root.path().to_string_lossy().to_string();

                    // Resolve the target file to an absolute path so we can exclude it
                    // from the rg results reliably (relative path suffix matching is fragile).
                    let absolute_target = self
                        .workspace_root
                        .path()
                        .join(&semantic_path.file_path)
                        .to_string_lossy()
                        .to_string();

                    let mut cmd = tokio::process::Command::new("rg");
                    cmd.arg("-l")
                        .arg("-w")
                        .arg("--")
                        .arg(symbol_name)
                        .arg(&workspace_path)
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::null());

                    if let Ok(out) = cmd.output().await {
                        if out.status.success() {
                            let stdout = String::from_utf8_lossy(&out.stdout);
                            let mut reference_count = 0u32;

                            for line in stdout.lines() {
                                let line = line.trim();
                                if line.is_empty() {
                                    continue;
                                }
                                // Exclude the file being deleted — its own definition
                                // is not a "cross-file reference".
                                if line != absolute_target {
                                    reference_count += 1;
                                }
                            }

                            if reference_count > 0 {
                                let err =
                                    pathfinder_common::error::PathfinderError::InvalidTarget {
                                        semantic_path: params.semantic_path.clone(),
                                        reason: format!(
                                            "Symbol '{symbol_name}' is still referenced in \
                                         {reference_count} other file(s). Delete or update \
                                         those references first, or pass \
                                         'ignore_validation_failures: true' to force deletion."
                                        ),
                                        edit_index: None,
                                        valid_edit_types: None,
                                    };
                                return Err(crate::server::helpers::pathfinder_to_error_data(&err));
                            }
                        }
                    }
                }
            }
        }

        check_occ(
            &params.base_version,
            &current_hash,
            semantic_path.file_path.clone(),
        )?;

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

        let (source, current_hash, new_bytes) = self
            .resolve_edit_content(
                &semantic_path,
                &params.semantic_path,
                &params.edit_type,
                params.new_code.as_deref(),
            )
            .await?;

        check_occ(
            &params.base_version,
            &current_hash,
            semantic_path.file_path.clone(),
        )?;

        // P1-1: Execute run_lsp_validation using the original source and new_bytes
        let original_str = std::str::from_utf8(&source).unwrap_or("");
        let new_str = std::str::from_utf8(&new_bytes).unwrap_or("");
        let validation_outcome = self
            .run_lsp_validation(&semantic_path.file_path, original_str, new_str, false)
            .await;

        let duration_ms = start.elapsed().as_millis();
        tracing::info!(
            tool = "validate_only",
            semantic_path = %params.semantic_path,
            duration_ms,
            engines_used = ?if validation_outcome.skipped { vec!["tree-sitter"] } else { vec!["tree-sitter", "lsp"] },
            "validate_only: complete"
        );

        Ok(Json(EditResponse {
            success: true,
            new_version_hash: None, // No file written
            formatted: false,
            validation: validation_outcome.validation,
            validation_skipped: validation_outcome.skipped,
            validation_skipped_reason: validation_outcome.skipped_reason,
        }))
    }

    /// Extracted helper to resolve the new source content without writing to disk.
    ///
    /// Dispatches to specialized handlers per edit type to keep cyclomatic complexity low.
    pub(crate) async fn resolve_edit_content(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        raw_semantic_path: &str,
        edit_type: &str,
        new_code: Option<&str>,
    ) -> Result<(std::sync::Arc<[u8]>, VersionHash, Vec<u8>), ErrorData> {
        match edit_type {
            "replace_body" => {
                self.resolve_replace_body(semantic_path, raw_semantic_path, new_code)
                    .await
            }
            "replace_full" => self.resolve_replace_full(semantic_path, new_code).await,
            "insert_before" => {
                self.resolve_insert(semantic_path, new_code, InsertEdge::Before)
                    .await
            }
            "insert_after" => {
                self.resolve_insert(semantic_path, new_code, InsertEdge::After)
                    .await
            }
            "insert_into" => self.resolve_insert_into(semantic_path, new_code).await,
            "delete" => self.resolve_delete(semantic_path, raw_semantic_path).await,
            unknown => Err(Self::unsupported_edit_type_error(
                raw_semantic_path,
                unknown,
            )),
        }
    }

    /// Resolve a `replace_body` edit.
    async fn resolve_replace_body(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        raw_semantic_path: &str,
        new_code: Option<&str>,
    ) -> Result<(std::sync::Arc<[u8]>, VersionHash, Vec<u8>), ErrorData> {
        require_symbol_target(semantic_path, raw_semantic_path)?;
        let new_code = new_code.unwrap_or_default();
        let (body_range, source, current_hash) = self
            .surgeon
            .resolve_body_range(self.workspace_root.path(), semantic_path)
            .await
            .map_err(crate::server::helpers::treesitter_error_to_error_data)?;

        let normalized = pathfinder_common::normalize::normalize_for_body_replace(new_code);
        let indented = pathfinder_common::indent::dedent_then_reindent(
            &normalized,
            body_range.body_indent_column,
        );
        let new_content =
            super::text_edit::build_body_replacement(&source, &body_range, &indented)?;
        Ok((source, current_hash, new_content.as_bytes().to_vec()))
    }

    /// Resolve a `replace_full` edit.
    async fn resolve_replace_full(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        new_code: Option<&str>,
    ) -> Result<(std::sync::Arc<[u8]>, VersionHash, Vec<u8>), ErrorData> {
        let new_code = new_code.unwrap_or_default();
        if semantic_path.is_bare_file() {
            self.resolve_replace_full_bare_file(semantic_path, new_code)
                .await
        } else {
            self.resolve_replace_full_symbol(semantic_path, new_code)
                .await
        }
    }

    /// Bare-file path for `replace_full`: reads file directly, no AST validation.
    async fn resolve_replace_full_bare_file(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        new_code: &str,
    ) -> Result<(std::sync::Arc<[u8]>, VersionHash, Vec<u8>), ErrorData> {
        let absolute_path = self.workspace_root.resolve(&semantic_path.file_path);
        let source = tokio::fs::read(&absolute_path)
            .await
            .map_err(|e| io_error_data(format!("failed to read file: {e}")))?;
        let current_hash = VersionHash::compute(&source);
        let new_bytes = new_code.as_bytes().to_vec();

        if let Ok(new_str) = std::str::from_utf8(&new_bytes) {
            if let Some(lang) =
                pathfinder_treesitter::language::SupportedLanguage::detect(&semantic_path.file_path)
            {
                match pathfinder_treesitter::parser::AstParser::parse_source(
                    &semantic_path.file_path,
                    lang,
                    new_str.as_bytes(),
                ) {
                    Ok(tree) => {
                        if tree.root_node().has_error() {
                            tracing::warn!(
                                file = %semantic_path.file_path.display(),
                                "replace_full: bare file content has parse errors"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "replace_full: tree-sitter error");
                    }
                }
            }
        }
        Ok((std::sync::Arc::from(source), current_hash, new_bytes))
    }

    /// Symbol-targeted path for `replace_full`: uses AST-aware range resolution.
    async fn resolve_replace_full_symbol(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        new_code: &str,
    ) -> Result<(std::sync::Arc<[u8]>, VersionHash, Vec<u8>), ErrorData> {
        let (full_range, source, current_hash) = self
            .surgeon
            .resolve_full_range(self.workspace_root.path(), semantic_path)
            .await
            .map_err(crate::server::helpers::treesitter_error_to_error_data)?;

        let normalized = normalize_for_full_replace(new_code);
        let indented = dedent_then_reindent(&normalized, full_range.indent_column);

        let before = &source[..full_range.start_byte];
        let after = &source[full_range.end_byte..];

        let mut new_bytes = Vec::with_capacity(before.len() + indented.len() + after.len());
        new_bytes.extend_from_slice(before);
        new_bytes.extend_from_slice(indented.as_bytes());
        new_bytes.extend_from_slice(after);

        Ok((source, current_hash, new_bytes))
    }

    /// Resolve an `insert_before` or `insert_after` edit.
    async fn resolve_insert(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        new_code: Option<&str>,
        edge: InsertEdge,
    ) -> Result<(std::sync::Arc<[u8]>, VersionHash, Vec<u8>), ErrorData> {
        match edge {
            InsertEdge::Before => self.resolve_insert_before(semantic_path, new_code).await,
            InsertEdge::After => self.resolve_insert_after(semantic_path, new_code).await,
        }
    }

    /// Resolve an `insert_before` edit.
    async fn resolve_insert_before(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        new_code: Option<&str>,
    ) -> Result<(std::sync::Arc<[u8]>, VersionHash, Vec<u8>), ErrorData> {
        let new_code = new_code.unwrap_or_default();
        let (insert_byte, indent_column, source, current_hash) = self
            .resolve_insert_position(semantic_path, InsertEdge::Before)
            .await?;

        let normalized = normalize_for_full_replace(new_code);
        let indented = dedent_then_reindent(&normalized, indent_column);

        let before = &source[..insert_byte];
        let after = &source[insert_byte..];

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

        Ok((source, current_hash, new_bytes))
    }

    /// Resolve an `insert_after` edit.
    async fn resolve_insert_after(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        new_code: Option<&str>,
    ) -> Result<(std::sync::Arc<[u8]>, VersionHash, Vec<u8>), ErrorData> {
        let new_code = new_code.unwrap_or_default();
        let (insert_byte, indent_column, source, current_hash) = self
            .resolve_insert_position(semantic_path, InsertEdge::After)
            .await?;

        let normalized = normalize_for_full_replace(new_code);
        let indented = dedent_then_reindent(&normalized, indent_column);

        let before = &source[..insert_byte];
        let after = &source[insert_byte..];

        // Doc comments need a blank line before them for idiomatic formatting.
        // Detect if the inserted code starts with a doc comment marker.
        let inserted_starts_doc_comment = indented.bytes().next().is_some_and(|b| b == b'/')
            && (indented.starts_with("///")
                || indented.starts_with("//!")
                || indented.starts_with("/**")
                || indented.starts_with("/*!"));

        let before_sep = if before.ends_with(b"\n\n")
            || after.starts_with(b"\n\n")
            || (before.ends_with(b"\n") && after.starts_with(b"\n"))
        {
            if inserted_starts_doc_comment && !before.ends_with(b"\n\n") {
                "\n" // Add one more newline to create blank line before doc comment
            } else {
                ""
            }
        } else if before.ends_with(b"\n") {
            if inserted_starts_doc_comment {
                "\n\n" // Blank line before doc comment
            } else {
                "\n"
            }
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

        Ok((source, current_hash, new_bytes))
    }

    /// Resolve an `insert_into` edit.
    async fn resolve_insert_into(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        new_code: Option<&str>,
    ) -> Result<(std::sync::Arc<[u8]>, VersionHash, Vec<u8>), ErrorData> {
        let new_code = new_code.unwrap_or_default();
        let (body_end, source, current_hash) = self
            .surgeon
            .resolve_body_end_range(self.workspace_root.path(), semantic_path)
            .await
            .map_err(crate::server::helpers::treesitter_error_to_error_data)?;

        let normalized = normalize_for_full_replace(new_code);
        let indented = dedent_then_reindent(&normalized, body_end.body_indent_column);

        let before = &source[..body_end.insert_byte];
        let after = &source[body_end.insert_byte..];

        // Add blank line separator before inserted code if needed
        let sep = if before.ends_with(b"\n\n") || before.ends_with(b"{\n") {
            ""
        } else {
            "\n"
        };
        let trailing = if indented.ends_with('\n') { "" } else { "\n" };

        let mut new_bytes = Vec::with_capacity(
            before.len() + sep.len() + indented.len() + trailing.len() + after.len(),
        );
        new_bytes.extend_from_slice(before);
        new_bytes.extend_from_slice(sep.as_bytes());
        new_bytes.extend_from_slice(indented.as_bytes());
        new_bytes.extend_from_slice(trailing.as_bytes());
        new_bytes.extend_from_slice(after);

        Ok((source, current_hash, new_bytes))
    }

    /// Resolve a `delete` edit.
    async fn resolve_delete(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        raw_semantic_path: &str,
    ) -> Result<(std::sync::Arc<[u8]>, VersionHash, Vec<u8>), ErrorData> {
        require_symbol_target(semantic_path, raw_semantic_path)?;
        let (full_range, source, current_hash) = self
            .surgeon
            .resolve_full_range(self.workspace_root.path(), semantic_path)
            .await
            .map_err(crate::server::helpers::treesitter_error_to_error_data)?;

        let before_end = strip_orphaned_doc_comment(&source, full_range.start_byte);
        let mut b_end = before_end;
        while b_end > 0 && source[b_end - 1].is_ascii_whitespace() {
            b_end -= 1;
        }

        let mut a_start = full_range.end_byte;
        while a_start < source.len() && source[a_start].is_ascii_whitespace() {
            a_start += 1;
        }

        let before = &source[..b_end];
        let after = &source[a_start..];

        let sep = if before.is_empty() || after.is_empty() {
            b"\n" as &[u8]
        } else {
            b"\n\n"
        };

        let mut new_bytes = Vec::with_capacity(before.len() + sep.len() + after.len());
        new_bytes.extend_from_slice(before);
        new_bytes.extend_from_slice(sep);
        new_bytes.extend_from_slice(after);

        Ok((source, current_hash, new_bytes))
    }

    /// Build an `InvalidTarget` error for an unsupported edit type.
    fn unsupported_edit_type_error(raw_semantic_path: &str, edit_type: &str) -> ErrorData {
        let err = pathfinder_common::error::PathfinderError::InvalidTarget {
            semantic_path: raw_semantic_path.to_owned(),
            reason: format!(
                "unsupported edit type: '{edit_type}'. Must be one of: replace_body, replace_full, insert_before, insert_after, insert_into, delete."
            ),
            edit_index: None,
            valid_edit_types: None,
        };
        crate::server::helpers::pathfinder_to_error_data(&err)
    }
}
