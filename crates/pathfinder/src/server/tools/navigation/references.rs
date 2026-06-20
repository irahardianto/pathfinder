//! `trace` tool handler (references mode).
//!
//! Finds all usages of a symbol across the codebase using LSP
//! `textDocument/references` with optional `textDocument/implementation`.
//! Supports pagination and degrades gracefully when LSP is unavailable.

use crate::server::helpers::{
    format_degraded_notice, millis_to_u64, parse_semantic_path, pathfinder_to_error_data,
    require_symbol_target, serialize_metadata,
};
use crate::server::PathfinderServer;
use pathfinder_common::types::DegradedReason;
use pathfinder_lsp::LspError;
use rmcp::model::ErrorData;

impl PathfinderServer {
    /// DELIVERABLE B: Grep-based reference fallback for `find_all_references`.
    ///
    /// When LSP is unavailable, times out, or returns error, use ripgrep via
    /// `search_codebase_impl` to find symbol references as a heuristic fallback.
    ///
    /// Filters (per spec B2):
    /// - Only source files (via `is_source_file`)
    /// - Excludes the definition site using line-number matching: if a same-file match
    ///   is on the same line as `definition_scope.start_line`, exclude it.
    /// - As a secondary safeguard: if line numbers don't match but content matches
    ///   a definition pattern for that language -> also exclude.
    ///
    /// Pagination (per spec B2):
    /// - Passes `max_results` and `offset` directly to `search_codebase_impl`
    ///
    /// Returns `Some((references, files_referenced))` if matches found after filtering,
    /// `None` if no results or search failed.
    async fn grep_references_fallback(
        &self,
        symbol_name: &str,
        definition_path: &std::path::Path,
        definition_scope: &pathfinder_common::types::SymbolScope,
        params: &crate::server::types::TraceParams,
    ) -> Option<(Vec<crate::server::types::ReferenceLocation>, usize)> {
        let query = format!(r"\b{}\b", regex::escape(symbol_name));

        // Get file extension for definition_patterns (used as secondary filter only)
        let def_ext = definition_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        // Get language-aware definition patterns (secondary fallback filter)
        // Primary filter = line-number matching
        let def_patterns = super::definition_patterns(def_ext, symbol_name);

        // Check if we got ONLY the catch-all pattern \b{name}\b. If so, don't use regex filtering.
        // The catch-all matches EVERY search result (since query is \b{name}\b), which would
        // incorrectly exclude ALL same-file different-line references. This fixes BUG 1.
        //
        // Note: definition_patterns internally does regex::escape(symbol_name), so we must do
        // the same here for the string comparison to work correctly with symbols containing
        // regex-special characters like +, *, $, etc.
        let escaped_name = regex::escape(symbol_name);
        let catch_all_pattern = format!(r"\b{escaped_name}\b");
        let has_real_definition_patterns =
            !(def_patterns.len() == 1 && def_patterns[0] == catch_all_pattern);

        let def_res: Result<Vec<regex::Regex>, _> = if has_real_definition_patterns {
            def_patterns.iter().map(|p| regex::Regex::new(p)).collect()
        } else {
            // Skip regex filtering entirely for unknown languages - rely solely on line-number matching
            Ok(Vec::new())
        };

        let def_res = match def_res {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    tool = "grep_references_fallback",
                    symbol = %symbol_name,
                    error = %e,
                    "definition pattern compilation failed — proceeding with line-number-only filtering"
                );
                Vec::new()
            }
        };

        // definition_scope.start_line is 0-indexed, convert to 1-indexed for comparison with search results
        let definition_line_1indexed = (definition_scope.start_line + 1) as u64;

        let search_params = crate::server::types::SearchParams {
            query,
            mode: crate::server::types::SearchMode::Regex,
            path_glob: "**/*".to_string(),
            max_results: params.max_references,
            context_lines: 0,
            known_files: vec![],
            offset: params.offset,
            kind: None,
            ..Default::default()
        };

        let result = match self.search_codebase_impl(search_params).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    tool = "grep_references_fallback",
                    symbol = %symbol_name,
                    error = %e,
                    "search_codebase_impl failed during grep fallback"
                );
                return None;
            }
        };

        if result.0.matches.is_empty() {
            return None;
        }

        let mut files_referenced = std::collections::HashSet::new();

        let references: Vec<crate::server::types::ReferenceLocation> = result
            .0
            .matches
            .into_iter()
            .filter(|m| {
                // Filter 1: must be a source file
                if !super::is_source_file(&m.file) {
                    return false;
                }

                let m_path = std::path::Path::new(&m.file);

                // Filter 2: if different file from definition, it's a reference -> KEEP
                if m_path != definition_path {
                    return true;
                }

                // Filter 3: same file - PRIMARY exclusion via line-number matching
                // If match is on the exact definition line -> EXCLUDE
                if m.line == definition_line_1indexed {
                    return false;
                }

                // Filter 4: same file, different line - SECONDARY check via definition patterns
                // Only exclude if line doesn't match but content looks like a definition
                // This is a safeguard; most cases caught by line-number check above
                if def_res.iter().any(|re| re.is_match(&m.content)) {
                    return false;
                }

                // Same file, different line, not a definition pattern -> KEEP
                true
            })
            .map(|m| {
                files_referenced.insert(m.file.clone());

                // Safe u64 -> u32 conversion with logging on overflow
                let line = match u32::try_from(m.line) {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::warn!(
                            tool = "grep_references_fallback",
                            file = %m.file,
                            line_u64 = %m.line,
                            error = %e,
                            "line number overflow u64->u32 — using line 1 as fallback"
                        );
                        1
                    }
                };

                let column = match u32::try_from(m.column) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(
                            tool = "grep_references_fallback",
                            file = %m.file,
                            column_u64 = %m.column,
                            error = %e,
                            "column number overflow u64->u32 — using column 1 as fallback"
                        );
                        1
                    }
                };

                crate::server::types::ReferenceLocation {
                    file: m.file,
                    line,
                    column,
                    snippet: m.content,
                }
            })
            .collect();

        if references.is_empty() {
            None
        } else {
            Some((references, files_referenced.len()))
        }
    }

    /// Find all references to a symbol across the entire codebase.
    ///
    /// Uses the LSP `textDocument/references` capability to find all usages of
    /// a given symbol. Unlike `find_callers_callees`, this returns all references
    /// including those not in the call hierarchy (e.g., field accesses, imports).
    #[allow(clippy::too_many_lines)]
    #[tracing::instrument(skip(self, params))]
    pub(crate) async fn find_all_references_impl(
        &self,
        params: crate::server::types::TraceParams,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        let start = std::time::Instant::now();

        // Clamp max_references to [1, 500] to bound grep fallback and pagination.
        let params = crate::server::types::TraceParams {
            max_references: params.max_references.clamp(1, 500),
            ..params
        };

        tracing::info!(
            tool = "find_all_references",
            semantic_path = %params.semantic_path,
            "find_all_references: start"
        );

        let semantic_path = parse_semantic_path(&params.semantic_path)?;
        require_symbol_target(&semantic_path, &params.semantic_path)?;

        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            let duration_ms = start.elapsed().as_millis();
            tracing::warn!(
                tool = "find_all_references",
                error_code = e.error_code(),
                duration_ms,
                "sandbox check failed"
            );
            return Err(pathfinder_to_error_data(&e));
        }

        // Early file existence check — avoid tree-sitter parse on nonexistent files
        let abs_file = self.workspace_root.path().join(&semantic_path.file_path);
        if !abs_file.exists() {
            let err = pathfinder_common::error::PathfinderError::FileNotFound {
                path: abs_file.clone(),
            };
            tracing::warn!(
                tool = "find_all_references",
                path = %abs_file.display(),
                "file not found"
            );
            return Err(pathfinder_to_error_data(&err));
        }

        let ts_start = std::time::Instant::now();
        let symbol_scope = self
            .read_symbol_scope_enriched(&semantic_path, &params.semantic_path)
            .await?;
        let tree_sitter_ms = ts_start.elapsed().as_millis();

        let file_path = self.workspace_root.path().join(&semantic_path.file_path);
        let file_content = match tokio::fs::read_to_string(&file_path).await {
            Ok(content) => content,
            Err(e) => {
                tracing::warn!(
                    tool = "find_all_references",
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
                    tool = "find_all_references",
                    semantic_path = %semantic_path,
                    error = %e,
                    "open_document failed — LSP queries may return degraded results"
                );
                None
            }
        };

        let lsp_start = std::time::Instant::now();
        let lsp_result = self
            .lawyer
            .references(
                self.workspace_root.path(),
                &semantic_path.file_path,
                u32::try_from(symbol_scope.start_line + 1).unwrap_or(1),
                u32::try_from(symbol_scope.name_column + 1).unwrap_or(1),
            )
            .await;

        let implementations_result = self
            .lawyer
            .goto_implementation(
                self.workspace_root.path(),
                &semantic_path.file_path,
                u32::try_from(symbol_scope.start_line + 1).unwrap_or(1),
                u32::try_from(symbol_scope.name_column + 1).unwrap_or(1),
            )
            .await;
        let lsp_ms = lsp_start.elapsed().as_millis();

        let duration_ms = start.elapsed().as_millis();

        match lsp_result {
            Ok(locations) => {
                let implementations: Vec<crate::server::types::ReferenceLocation> =
                    match implementations_result {
                        Ok(impls) => impls
                            .into_iter()
                            .map(|def| crate::server::types::ReferenceLocation {
                                file: def.file,
                                line: def.line,
                                column: def.column,
                                snippet: def.preview,
                            })
                            .collect(),
                        Err(e) => {
                            tracing::warn!(
                                tool = "find_all_references",
                                error = %e,
                                "goto_implementation failed — returning references only"
                            );
                            vec![]
                        }
                    };

                let all_files = locations
                    .iter()
                    .map(|l| l.file.as_str())
                    .chain(implementations.iter().map(|i| i.file.as_str()))
                    .collect::<std::collections::HashSet<_>>();
                let files_referenced = all_files.len();

                let references: Vec<crate::server::types::ReferenceLocation> = locations
                    .into_iter()
                    .map(|l| crate::server::types::ReferenceLocation {
                        file: l.file,
                        line: l.line,
                        column: l.column,
                        snippet: l.snippet,
                    })
                    .collect();

                // Dedup references that also appear in implementations by (file, line, column).
                let impl_keys: std::collections::HashSet<(String, u32, u32)> = implementations
                    .iter()
                    .map(|i| (i.file.clone(), i.line, i.column))
                    .collect();
                let references: Vec<crate::server::types::ReferenceLocation> = references
                    .into_iter()
                    .filter(|r| !impl_keys.contains(&(r.file.clone(), r.line, r.column)))
                    .collect();

                if references.is_empty() && implementations.is_empty() {
                    let probe = self
                        .lawyer
                        .goto_definition(
                            self.workspace_root.path(),
                            &semantic_path.file_path,
                            u32::try_from(symbol_scope.start_line + 1).unwrap_or(1),
                            u32::try_from(symbol_scope.name_column + 1).unwrap_or(1),
                        )
                        .await;

                    if matches!(probe, Ok(Some(_))) {
                        let symbol_name =
                            super::last_symbol_name(&semantic_path).unwrap_or_default();
                        let grep_result = if symbol_name.is_empty() {
                            None
                        } else {
                            self.grep_references_fallback(
                                &symbol_name,
                                &semantic_path.file_path,
                                &symbol_scope,
                                &params,
                            )
                            .await
                        };

                        let offset = usize::try_from(params.offset).unwrap_or(0);
                        let max_results =
                            usize::try_from(params.max_references).unwrap_or(50).max(1);

                        let (paginated_refs, files_referenced, total_references) =
                            if let Some((refs, file_count)) = grep_result {
                                let ref_count = refs.len();
                                let paginated = refs
                                    .into_iter()
                                    .skip(offset)
                                    .take(max_results)
                                    .collect::<Vec<_>>();
                                (paginated, file_count, ref_count)
                            } else {
                                (Vec::new(), 0, 0)
                            };

                        let truncated = total_references > offset.saturating_add(max_results);
                        let paginated_len = paginated_refs.len();

                        let summary = if total_references > 0 {
                            format!("Found {total_references} references across {files_referenced} files (grep fallback).\n\n")
                        } else {
                            "LSP confirmed: zero references or implementations for this symbol.\n"
                                .to_string()
                        };

                        let references_text = if paginated_refs.is_empty() {
                            String::new()
                        } else {
                            let header = format!("References: {total_references} found\n");
                            let items: Vec<_> = paginated_refs
                                .iter()
                                .map(|r| {
                                    format!("{}:{}:{}: {}", r.file, r.line, r.column, r.snippet)
                                })
                                .collect();
                            format!("{}{}", header, items.join("\n"))
                        };

                        let pagination_note = if truncated {
                            format!(
                                "\n[showing {} of {} total — use offset={} for next page]\n",
                                paginated_len,
                                total_references,
                                offset.saturating_add(max_results),
                            )
                        } else {
                            String::new()
                        };

                        let metadata = crate::server::types::FindAllReferencesMetadata {
                            references: Some(paginated_refs),
                            total_references: Some(total_references),
                            truncated,
                            files_referenced,
                            degraded: true,
                            degraded_reason: Some(DegradedReason::LspWarmupGrepFallback),
                            actionable_guidance: Some(
                                DegradedReason::LspWarmupGrepFallback.guidance(),
                            ),
                            lsp_readiness: Some("warming_up".to_owned()),
                            warm_start_in_progress: Some(true),
                            duration_ms: Some(millis_to_u64(duration_ms)),
                            resolution_strategy: Some("grep_file_scoped".to_owned()),
                            hint: None,
                        };

                        let mut result =
                            rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(
                                format!(
                                    "{}\n{}{}{}\n[completed in {duration_ms}ms]",
                                    format_degraded_notice(&DegradedReason::LspWarmupGrepFallback),
                                    summary,
                                    references_text,
                                    pagination_note
                                ),
                            )]);
                        result.structured_content = serialize_metadata(&metadata);
                        return Ok(result);
                    }

                    // probe returned Ok(None) or Err — LSP either has no definition for this
                    // symbol (could be a built-in/external) or is still warming up.
                    //
                    // If warmup is incomplete, 0 references is unverified — the LSP hasn't
                    // finished indexing and may be missing real usages. Report degraded to
                    // prevent agents from concluding the symbol is unused.
                    if !self.lawyer.is_warm_start_complete() {
                        tracing::info!(
                            tool = "find_all_references",
                            semantic_path = %params.semantic_path,
                            "find_all_references: LSP returned 0 refs and warmup incomplete — \
                             reporting as unverified to avoid false confidence"
                        );

                        let symbol_name =
                            super::last_symbol_name(&semantic_path).unwrap_or_default();
                        let grep_result = if symbol_name.is_empty() {
                            None
                        } else {
                            self.grep_references_fallback(
                                &symbol_name,
                                &semantic_path.file_path,
                                &symbol_scope,
                                &params,
                            )
                            .await
                        };

                        let offset = usize::try_from(params.offset).unwrap_or(0);
                        let max_results =
                            usize::try_from(params.max_references).unwrap_or(50).max(1);

                        let (paginated_refs, files_referenced, total_references) =
                            if let Some((refs, file_count)) = grep_result {
                                let ref_count = refs.len();
                                let paginated = refs
                                    .into_iter()
                                    .skip(offset)
                                    .take(max_results)
                                    .collect::<Vec<_>>();
                                (paginated, file_count, ref_count)
                            } else {
                                (Vec::new(), 0, 0)
                            };

                        let truncated = total_references > offset.saturating_add(max_results);
                        let paginated_len = paginated_refs.len();

                        let references_text = if paginated_refs.is_empty() {
                            String::new()
                        } else {
                            let header = format!("References: {total_references} found\n");
                            let items: Vec<_> = paginated_refs
                                .iter()
                                .map(|r| {
                                    format!("{}:{}:{}: {}", r.file, r.line, r.column, r.snippet)
                                })
                                .collect();
                            format!("{}{}", header, items.join("\n"))
                        };

                        let pagination_note = if truncated {
                            format!(
                                "\n[showing {} of {} total — use offset={} for next page]\n",
                                paginated_len,
                                total_references,
                                offset.saturating_add(max_results),
                            )
                        } else {
                            String::new()
                        };

                        let summary = if total_references > 0 {
                            format!("Found {total_references} references across {files_referenced} files (grep fallback, LSP still warming up).\n\n")
                        } else {
                            "LSP result unverified: zero references returned but LSP is still indexing. \
                             Use search to find usages, or retry after lsp_health reports ready.\n"
                                .to_string()
                        };

                        let degraded_reason = DegradedReason::LspWarmupEmptyUnverified;
                        let metadata = crate::server::types::FindAllReferencesMetadata {
                            references: Some(paginated_refs),
                            total_references: Some(total_references),
                            truncated,
                            files_referenced,
                            degraded: true,
                            degraded_reason: Some(degraded_reason),
                            actionable_guidance: Some(degraded_reason.guidance()),
                            lsp_readiness: Some("warming_up".to_owned()),
                            warm_start_in_progress: Some(true),
                            duration_ms: Some(millis_to_u64(duration_ms)),
                            resolution_strategy: Some("lsp_unverified_warmup".to_owned()),
                            hint: None,
                        };

                        let mut result =
                            rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(
                                format!(
                                    "{}\n{}{}{}\n[completed in {duration_ms}ms]",
                                    format_degraded_notice(&degraded_reason),
                                    summary,
                                    references_text,
                                    pagination_note
                                ),
                            )]);
                        result.structured_content = serialize_metadata(&metadata);
                        return Ok(result);
                    }
                }

                // Spec 4.4: Apply pagination to each list separately
                let total_references = references.len() + implementations.len();
                let offset = usize::try_from(params.offset).unwrap_or(0);
                // Item 4: Guard against max_results=0 which causes infinite pagination loops.
                let max_results = usize::try_from(params.max_references).unwrap_or(50).max(1);
                let truncated = total_references > offset.saturating_add(max_results);

                // Paginate implementations first, then references (matches display order)
                let impl_count = implementations.len();
                let ref_count = references.len();

                let (paginated_impls, paginated_refs) = if offset >= impl_count {
                    // Past implementations — paginate references only
                    let ref_offset = offset - impl_count;
                    (
                        Vec::new(),
                        references
                            .into_iter()
                            .skip(ref_offset)
                            .take(max_results)
                            .collect::<Vec<_>>(),
                    )
                } else {
                    // Some or all implementations in range
                    let impl_slice: Vec<_> = implementations
                        .into_iter()
                        .skip(offset)
                        .take(max_results)
                        .collect();
                    let remaining = max_results - impl_slice.len();
                    let ref_slice: Vec<_> = references.into_iter().take(remaining).collect();
                    (impl_slice, ref_slice)
                };

                tracing::info!(
                    tool = "find_all_references",
                    references_count = ref_count,
                    implementations_count = impl_count,
                    files_referenced,
                    tree_sitter_ms,
                    lsp_ms,
                    duration_ms,
                    engines_used = ?["tree-sitter", "lsp"],
                    "find_all_references: complete"
                );

                // Build text output before moving vectors into paginated
                let implementations_text = if paginated_impls.is_empty() {
                    String::new()
                } else {
                    let header =
                        format!("Implementations (extends/implements): {impl_count} found\n");
                    let items: Vec<_> = paginated_impls
                        .iter()
                        .map(|imp| {
                            format!("{}:{}:{}: {}", imp.file, imp.line, imp.column, imp.snippet)
                        })
                        .collect();
                    format!("{}{}\n", header, items.join("\n"))
                };

                let references_text = if paginated_refs.is_empty() {
                    String::new()
                } else {
                    let header = format!("References: {ref_count} found\n");
                    let items: Vec<_> = paginated_refs
                        .iter()
                        .map(|r| format!("{}:{}:{}: {}", r.file, r.line, r.column, r.snippet))
                        .collect();
                    format!("{}{}", header, items.join("\n"))
                };

                let paginated_len = paginated_impls.len() + paginated_refs.len();
                let mut paginated = Vec::with_capacity(paginated_len);
                paginated.extend(paginated_impls);
                paginated.extend(paginated_refs);

                let pagination_note = if truncated {
                    format!(
                        "\n[showing {} of {} total — use offset={} for next page]\n",
                        paginated_len,
                        total_references,
                        offset.saturating_add(max_results),
                    )
                } else {
                    String::new()
                };

                let summary = if impl_count > 0 && ref_count > 0 {
                    format!(
                        "Found {ref_count} references + {impl_count} implementations across {files_referenced} files.\n\n"
                    )
                } else if impl_count > 0 {
                    format!(
                        "Found {impl_count} implementations across {files_referenced} files.\n\n"
                    )
                } else if ref_count > 0 {
                    format!("Found {ref_count} references across {files_referenced} files.\n\n")
                } else {
                    "LSP confirmed: zero references or implementations for this symbol.\n"
                        .to_string()
                };

                // P2-7: Hint for non-degraded zero-reference results.
                let hint = if total_references == 0 {
                    Some(
                        "LSP confirmed zero references. This symbol may be unused, \
                         an entry point, or only referenced via dynamic dispatch/reflection."
                            .to_owned(),
                    )
                } else {
                    None
                };

                let metadata = crate::server::types::FindAllReferencesMetadata {
                    references: Some(paginated),
                    total_references: Some(total_references),
                    truncated,
                    files_referenced,
                    degraded: false,
                    degraded_reason: None,
                    actionable_guidance: None,
                    lsp_readiness: Some("ready".to_owned()),
                    warm_start_in_progress: Some(false),
                    duration_ms: Some(millis_to_u64(duration_ms)),
                    resolution_strategy: Some("lsp_references".to_owned()),
                    hint,
                };

                let mut result =
                    rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(
                        format!("{summary}{implementations_text}{references_text}{pagination_note}\n[completed in {duration_ms}ms]"),
                    )]);
                result.structured_content = serialize_metadata(&metadata);
                Ok(result)
            }
            Err(LspError::NoLspAvailable) => {
                tracing::info!(
                    tool = "find_all_references",
                    semantic_path = %params.semantic_path,
                    tree_sitter_ms,
                    lsp_ms,
                    duration_ms,
                    "find_all_references: no LSP — attempting grep fallback"
                );

                // DELIVERABLE B: Attempt grep-based reference fallback
                let symbol_name = super::last_symbol_name(&semantic_path).unwrap_or_default();
                let grep_result = if symbol_name.is_empty() {
                    None
                } else {
                    self.grep_references_fallback(
                        &symbol_name,
                        &semantic_path.file_path,
                        &symbol_scope,
                        &params,
                    )
                    .await
                };

                // Determine final state based on whether grep fallback succeeded
                let (
                    references,
                    total_references,
                    files_referenced,
                    degraded_reason,
                    resolution_strategy,
                    text_body,
                ) = if let Some((refs, file_count)) = grep_result {
                    tracing::info!(
                        tool = "find_all_references",
                        references_found = refs.len(),
                        "grep fallback found references"
                    );
                    let ref_count = refs.len();
                    let items: Vec<_> = refs
                        .iter()
                        .map(|r| format!("{}:{}:{}: {}", r.file, r.line, r.column, r.snippet))
                        .collect();
                    let text = format!(
                            "Grep fallback: found {} references across {} files (heuristic only).\n\nReferences: {}\n{}\n",
                            ref_count, file_count, ref_count, items.join("\n")
                        );
                    (
                        Some(refs),
                        Some(ref_count),
                        file_count,
                        DegradedReason::NoLspGrepFallback,
                        "grep_file_scoped",
                        text,
                    )
                } else {
                    // Fallback unsuccessful - keep original behavior
                    let text = format!(
                        "References unknown. Use search to manually find usages of `{}`\n",
                        params.semantic_path
                    );
                    (
                        None,
                        None,
                        0,
                        DegradedReason::NoLsp,
                        "treesitter_fallback",
                        text,
                    )
                };

                let metadata = crate::server::types::FindAllReferencesMetadata {
                    references,
                    total_references,
                    truncated: false,
                    files_referenced,
                    degraded: true,
                    degraded_reason: Some(degraded_reason),
                    actionable_guidance: Some(degraded_reason.guidance()),
                    lsp_readiness: Some("unavailable".to_owned()),
                    warm_start_in_progress: None,
                    duration_ms: Some(millis_to_u64(duration_ms)),
                    resolution_strategy: Some(resolution_strategy.to_owned()),
                    hint: None,
                };

                let mut result =
                    rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(
                        format!(
                            "{}\n{}[completed in {duration_ms}ms]",
                            format_degraded_notice(&degraded_reason),
                            text_body
                        ),
                    )]);
                result.structured_content = serialize_metadata(&metadata);
                Ok(result)
            }
            Err(e) => {
                tracing::warn!(
                    tool = "find_all_references",
                    error = %e,
                    tree_sitter_ms,
                    lsp_ms,
                    duration_ms,
                    "find_all_references: LSP error — attempting grep fallback"
                );

                let is_timeout = matches!(&e, LspError::Timeout { .. });
                let lsp_readiness = if is_timeout {
                    "warming_up"
                } else {
                    "unavailable"
                };
                let warm_start_in_progress = if is_timeout { Some(true) } else { None };

                // DELIVERABLE B: Attempt grep-based reference fallback
                let symbol_name = super::last_symbol_name(&semantic_path).unwrap_or_default();
                let grep_result = if symbol_name.is_empty() {
                    None
                } else {
                    self.grep_references_fallback(
                        &symbol_name,
                        &semantic_path.file_path,
                        &symbol_scope,
                        &params,
                    )
                    .await
                };

                let default_degraded_reason = if is_timeout {
                    DegradedReason::LspTimeoutGrepFallback
                } else {
                    DegradedReason::LspErrorGrepFallback
                };

                // Determine final state based on whether grep fallback succeeded
                let (
                    references,
                    total_references,
                    files_referenced,
                    degraded_reason,
                    resolution_strategy,
                    text_body,
                ) = if let Some((refs, file_count)) = grep_result {
                    tracing::info!(
                        tool = "find_all_references",
                        references_found = refs.len(),
                        "grep fallback found references after LSP error"
                    );
                    let ref_count = refs.len();
                    let items: Vec<_> = refs
                        .iter()
                        .map(|r| format!("{}:{}:{}: {}", r.file, r.line, r.column, r.snippet))
                        .collect();
                    let text = format!(
                        "Grep fallback: found {} references across {} files (heuristic only).\n\nReferences: {}\n{}\n",
                        ref_count, file_count, ref_count, items.join("\n")
                    );
                    (
                        Some(refs),
                        Some(ref_count),
                        file_count,
                        default_degraded_reason,
                        "grep_file_scoped",
                        text,
                    )
                } else {
                    // Fallback unsuccessful - keep original behavior
                    let text = format!(
                        "References unknown. Use search to manually find usages of `{}`\n",
                        params.semantic_path
                    );
                    (
                        None,
                        None,
                        0,
                        default_degraded_reason,
                        "treesitter_fallback",
                        text,
                    )
                };

                let metadata = crate::server::types::FindAllReferencesMetadata {
                    references,
                    total_references,
                    truncated: false,
                    files_referenced,
                    degraded: true,
                    degraded_reason: Some(degraded_reason),
                    actionable_guidance: Some(degraded_reason.guidance()),
                    lsp_readiness: Some(lsp_readiness.to_owned()),
                    warm_start_in_progress,
                    duration_ms: Some(millis_to_u64(duration_ms)),
                    resolution_strategy: Some(resolution_strategy.to_owned()),
                    hint: None,
                };

                let mut result =
                    rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(
                        format!(
                            "{}\n{}[completed in {duration_ms}ms]",
                            format_degraded_notice(&degraded_reason),
                            text_body
                        ),
                    )]);
                result.structured_content = serialize_metadata(&metadata);
                Ok(result)
            }
        }
    }
}

#[cfg(test)]
#[path = "references_test.rs"]
mod tests;
