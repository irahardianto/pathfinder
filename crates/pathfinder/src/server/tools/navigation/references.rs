//! `find_all_references` tool implementation.
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
        params: &crate::server::types::FindAllReferencesParams,
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

        let search_params = crate::server::types::SearchCodebaseParams {
            query,
            is_regex: true,
            path_glob: "**/*".to_string(),
            filter_mode: pathfinder_common::types::FilterMode::CodeOnly,
            max_results: params.max_results,
            context_lines: 0,
            known_files: vec![],
            group_by_file: false,
            exclude_glob: String::new(),
            offset: params.offset,
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
        params: crate::server::types::FindAllReferencesParams,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        let start = std::time::Instant::now();

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
                        let max_results = usize::try_from(params.max_results).unwrap_or(50).max(1);

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
                }

                // Spec 4.4: Apply pagination to each list separately
                let total_references = references.len() + implementations.len();
                let offset = usize::try_from(params.offset).unwrap_or(0);
                // Item 4: Guard against max_results=0 which causes infinite pagination loops.
                let max_results = usize::try_from(params.max_results).unwrap_or(50).max(1);
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
                        "References unknown. Use search_codebase to manually find usages of `{}`\n",
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
                        "References unknown. Use search_codebase to manually find usages of `{}`\n",
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
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::super::test_helpers::{make_scope, make_server_with_lawyer, make_temp_workspace};
    use super::*;
    use crate::server::PathfinderServer;
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::{DegradedReason, WorkspaceRoot};
    use pathfinder_lsp::types::ReferenceLocation;
    use pathfinder_lsp::{DefinitionLocation, MockLawyer};
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::mock::MockSurgeon;
    use std::sync::Arc;

    // ── find_all_references edge cases ──────────────────────────────────

    #[tokio::test]
    async fn test_find_all_references_lsp_returns_references() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        lawyer.set_references_result(Ok(vec![
            ReferenceLocation {
                file: "src/auth.rs".into(),
                line: 10,
                column: 4,
                snippet: "fn login() {".into(),
            },
            ReferenceLocation {
                file: "src/main.rs".into(),
                line: 20,
                column: 8,
                snippet: "login();".into(),
            },
        ]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        let refs = val.references.unwrap_or_default();
        assert_eq!(refs.len(), 2, "should have 2 references");
        assert!(!val.degraded, "should not be degraded when LSP works");
    }

    #[tokio::test]
    async fn test_find_all_references_respects_max_references() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        // Return 5 references
        let refs: Vec<_> = (0..5)
            .map(|i| ReferenceLocation {
                file: format!("src/file{i}.rs"),
                line: u32::try_from(i + 1).unwrap(),
                column: 1,
                snippet: format!("// reference {i}"),
            })
            .collect();
        lawyer.set_references_result(Ok(refs));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 3, // Limit to 3
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        let refs = val.references.unwrap_or_default();
        assert_eq!(
            refs.len(),
            3,
            "should return exactly max_results=3 references, got {}",
            refs.len()
        );
    }

    // ── find_all_references degraded paths ────────────────────────────────

    #[tokio::test]
    async fn test_find_all_references_degraded_when_no_lsp() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        // Use NoOpLawyer to simulate LSP unavailable
        let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);
        let ws_dir = make_temp_workspace();
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);
        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            surgeon,
            lawyer,
        );

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(val.degraded, "should be degraded when LSP unavailable");
        assert_eq!(val.degraded_reason, Some(DegradedReason::NoLsp));
        assert_eq!(val.lsp_readiness, Some("unavailable".to_owned()));
        assert!(val.references.is_none());
        // GAP 5: verify resolution_strategy confirms the path taken
        assert_eq!(
            val.resolution_strategy,
            Some("treesitter_fallback".to_owned())
        );
    }

    #[tokio::test]
    async fn test_find_all_references_lsp_error_degraded() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        // Simulate LSP protocol error
        lawyer.set_references_result(Err("protocol error".to_string()));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(val.degraded, "should be degraded on LSP error");
        assert_eq!(
            val.degraded_reason,
            Some(DegradedReason::LspErrorGrepFallback)
        );
        assert_eq!(val.lsp_readiness, Some("unavailable".to_owned()));
        assert!(val.references.is_none());
        // GAP 5: verify resolution_strategy confirms the path taken
        assert_eq!(
            val.resolution_strategy,
            Some("treesitter_fallback".to_owned())
        );
    }

    #[tokio::test]
    async fn test_find_all_references_connection_lost_degraded() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        // ConnectionLost exercises the dedicated LspError::ConnectionLost branch
        lawyer.set_references_lsp_error(Err(LspError::ConnectionLost));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(val.degraded, "should be degraded on connection lost");
        assert_eq!(
            val.degraded_reason,
            Some(DegradedReason::LspErrorGrepFallback)
        );
        assert_eq!(val.lsp_readiness, Some("unavailable".to_owned()));
        assert!(val.references.is_none());
        // GAP 5: verify resolution_strategy confirms the path taken
        assert_eq!(
            val.resolution_strategy,
            Some("treesitter_fallback".to_owned())
        );
    }

    // ── find_all_references pagination + implementations ────────────────────

    #[tokio::test]
    async fn test_find_all_references_with_implementations_and_references() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        // Configure references
        lawyer.set_references_result(Ok(vec![
            ReferenceLocation {
                file: "src/main.rs".into(),
                line: 10,
                column: 8,
                snippet: "login();".into(),
            },
            ReferenceLocation {
                file: "src/tests.rs".into(),
                line: 5,
                column: 4,
                snippet: "let _ = login();".into(),
            },
        ]));

        // Configure implementations
        lawyer.set_goto_implementation_result(Ok(vec![DefinitionLocation {
            file: "src/auth_impl.rs".into(),
            line: 15,
            column: 4,
            preview: "impl LoginService for AuthService {".into(),
        }]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // Should have 3 total (1 implementation + 2 references)
        assert_eq!(val.total_references, Some(3));
        assert_eq!(val.files_referenced, 3);

        let refs = val.references.unwrap_or_default();
        // First should be implementation, then references
        assert_eq!(refs[0].file, "src/auth_impl.rs");
        assert_eq!(refs[1].file, "src/main.rs");
        assert_eq!(refs[2].file, "src/tests.rs");
    }

    #[tokio::test]
    async fn test_find_all_references_offset_skips_implementations() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        // 2 implementations
        lawyer.set_goto_implementation_result(Ok(vec![
            DefinitionLocation {
                file: "src/auth_impl1.rs".into(),
                line: 10,
                column: 4,
                preview: "impl1".into(),
            },
            DefinitionLocation {
                file: "src/auth_impl2.rs".into(),
                line: 20,
                column: 4,
                preview: "impl2".into(),
            },
        ]));

        // 3 references
        lawyer.set_references_result(Ok(vec![
            ReferenceLocation {
                file: "src/main.rs".into(),
                line: 10,
                column: 8,
                snippet: "login1();".into(),
            },
            ReferenceLocation {
                file: "src/tests.rs".into(),
                line: 5,
                column: 4,
                snippet: "login2();".into(),
            },
            ReferenceLocation {
                file: "src/app.rs".into(),
                line: 15,
                column: 8,
                snippet: "login3();".into(),
            },
        ]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // offset=2 skips both implementations
        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 2,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        let refs = val.references.unwrap_or_default();
        assert_eq!(refs.len(), 3, "should return all 3 references");
        assert_eq!(refs[0].file, "src/main.rs");
        assert_eq!(refs[1].file, "src/tests.rs");
        assert_eq!(refs[2].file, "src/app.rs");
        assert_eq!(val.total_references, Some(5)); // 2 impls + 3 refs
    }

    #[tokio::test]
    async fn test_find_all_references_offset_past_implementations_paginates_references() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        // 1 implementation
        lawyer.set_goto_implementation_result(Ok(vec![DefinitionLocation {
            file: "src/auth_impl.rs".into(),
            line: 10,
            column: 4,
            preview: "impl".into(),
        }]));

        // 5 references
        lawyer.set_references_result(Ok(vec![
            ReferenceLocation {
                file: "src/file1.rs".into(),
                line: 10,
                column: 8,
                snippet: "ref1".into(),
            },
            ReferenceLocation {
                file: "src/file2.rs".into(),
                line: 20,
                column: 8,
                snippet: "ref2".into(),
            },
            ReferenceLocation {
                file: "src/file3.rs".into(),
                line: 30,
                column: 8,
                snippet: "ref3".into(),
            },
            ReferenceLocation {
                file: "src/file4.rs".into(),
                line: 40,
                column: 8,
                snippet: "ref4".into(),
            },
            ReferenceLocation {
                file: "src/file5.rs".into(),
                line: 50,
                column: 8,
                snippet: "ref5".into(),
            },
        ]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // offset=3: skip 1 impl + 2 refs, get next 2 refs
        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 2,
            offset: 3,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        let refs = val.references.unwrap_or_default();
        assert_eq!(refs.len(), 2, "should return 2 references");
        assert_eq!(refs[0].file, "src/file3.rs");
        assert_eq!(refs[1].file, "src/file4.rs");
        assert_eq!(val.total_references, Some(6)); // 1 impl + 5 refs
        assert!(val.truncated, "should be truncated");
    }

    // ── find_all_references edge cases ─────────────────────────────────────

    #[tokio::test]
    async fn test_find_all_references_zero_references_zero_implementations() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        // Empty results
        lawyer.set_references_result(Ok(vec![]));
        lawyer.set_goto_implementation_result(Ok(vec![]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(!val.degraded);
        assert_eq!(val.total_references, Some(0));
        assert_eq!(val.files_referenced, 0);
        assert!(val.references.unwrap_or_default().is_empty());
    }

    #[tokio::test]
    async fn test_find_all_references_rejects_sandbox_denied_path() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // Use path outside workspace (sandbox denies)
        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "/etc/passwd::function".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        assert!(
            result.is_err(),
            "should return error for sandbox denied path"
        );
    }

    // ── goto_implementation Err while references succeeds ────────────

    #[tokio::test]
    async fn test_find_all_references_implementation_error_references_ok() {
        // When goto_implementation returns Err but references succeeds,
        // implementations should be empty vec and references should be present.
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        // References succeed
        lawyer.set_references_result(Ok(vec![ReferenceLocation {
            file: "src/main.rs".into(),
            line: 10,
            column: 8,
            snippet: "login();".into(),
        }]));

        // Implementation fails
        lawyer.set_goto_implementation_result(Err(LspError::Protocol(
            "implementation error".to_string(),
        )));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(!val.degraded);
        // Total = 0 implementations + 1 reference = 1
        assert_eq!(val.total_references, Some(1));
        let refs = val.references.unwrap_or_default();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].file, "src/main.rs");
    }

    // ── Large offset past total results ─────────────────────────────

    #[tokio::test]
    async fn test_find_all_references_large_offset_returns_empty() {
        // offset=100 with only 6 items total should return empty results.
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        // 3 references
        lawyer.set_references_result(Ok(vec![
            ReferenceLocation {
                file: "src/main.rs".into(),
                line: 10,
                column: 8,
                snippet: "login1();".into(),
            },
            ReferenceLocation {
                file: "src/tests.rs".into(),
                line: 5,
                column: 4,
                snippet: "login2();".into(),
            },
            ReferenceLocation {
                file: "src/app.rs".into(),
                line: 15,
                column: 8,
                snippet: "login3();".into(),
            },
        ]));

        // 3 implementations
        lawyer.set_goto_implementation_result(Ok(vec![
            DefinitionLocation {
                file: "src/impl1.rs".into(),
                line: 10,
                column: 4,
                preview: "impl1".into(),
            },
            DefinitionLocation {
                file: "src/impl2.rs".into(),
                line: 20,
                column: 4,
                preview: "impl2".into(),
            },
            DefinitionLocation {
                file: "src/impl3.rs".into(),
                line: 30,
                column: 4,
                preview: "impl3".into(),
            },
        ]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // offset=100 is way past the 6 total items
        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 100,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        let refs = val.references.unwrap_or_default();
        assert!(
            refs.is_empty(),
            "should return empty when offset past total, got {}",
            refs.len()
        );
        assert_eq!(val.total_references, Some(6));
        // When offset is past total, truncated is false (nothing more to show)
        assert!(
            !val.truncated,
            "should NOT be truncated when offset past total"
        );
    }

    // ── Truncation boundary ─────────────────────────────────────────

    #[tokio::test]
    async fn test_find_all_references_truncation_boundary() {
        // Exactly offset + max_results results → truncated should be false.
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        // 2 implementations
        lawyer.set_goto_implementation_result(Ok(vec![
            DefinitionLocation {
                file: "src/impl1.rs".into(),
                line: 10,
                column: 4,
                preview: "impl1".into(),
            },
            DefinitionLocation {
                file: "src/impl2.rs".into(),
                line: 20,
                column: 4,
                preview: "impl2".into(),
            },
        ]));

        // 3 references
        lawyer.set_references_result(Ok(vec![
            ReferenceLocation {
                file: "src/ref1.rs".into(),
                line: 10,
                column: 8,
                snippet: "ref1".into(),
            },
            ReferenceLocation {
                file: "src/ref2.rs".into(),
                line: 20,
                column: 8,
                snippet: "ref2".into(),
            },
            ReferenceLocation {
                file: "src/ref3.rs".into(),
                line: 30,
                column: 8,
                snippet: "ref3".into(),
            },
        ]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        // Total = 5, offset=0, max_results=5 → exactly fits → NOT truncated
        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 5,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert_eq!(val.total_references, Some(5));
        let refs = val.references.unwrap_or_default();
        assert_eq!(refs.len(), 5, "should return all 5 items");
        assert!(
            !val.truncated,
            "should NOT be truncated when exactly at boundary"
        );
    }

    // ── Dedup between implementations and references ─────────────────

    #[tokio::test]
    async fn test_find_all_references_deduplicates_impl_and_refs() {
        // When a trait impl also appears in references, it should not appear twice.
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        // Implementation returns a location at file:line
        lawyer.set_goto_implementation_result(Ok(vec![DefinitionLocation {
            file: "src/auth_impl.rs".into(),
            line: 15,
            column: 4,
            preview: "impl LoginService for AuthService {".into(),
        }]));

        // References also includes the same file:line
        lawyer.set_references_result(Ok(vec![
            ReferenceLocation {
                file: "src/auth_impl.rs".into(),
                line: 15, // Same as implementation
                column: 4,
                snippet: "impl LoginService for AuthService {".into(),
            },
            ReferenceLocation {
                file: "src/main.rs".into(),
                line: 10,
                column: 8,
                snippet: "login();".into(),
            },
        ]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // Should have 2 total (1 impl + 1 unique ref), not 3
        assert_eq!(
            val.total_references,
            Some(2),
            "duplicate (file,line) should be deduped"
        );
        let refs = val.references.unwrap_or_default();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].file, "src/auth_impl.rs"); // implementation
        assert_eq!(refs[1].file, "src/main.rs"); // unique reference
    }

    // ── DELIVERABLE B: Grep fallback tests ──────────────────────────────────

    #[tokio::test]
    async fn test_find_all_references_grep_fallback_returns_results_with_mock_scout() {
        // DELIVERABLE B: When LSP is unavailable but search_codebase_impl returns matches,
        // find_all_references should use grep fallback and return:
        // - references: Some(Vec) (not None)
        // - degraded_reason: NoLspGrepFallback (not NoLsp)
        // - resolution_strategy: "grep_file_scoped"
        // - definition site matches should be filtered out
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        // SPEC 008: search_codebase_impl calls enclosing_symbol_detail for each match
        // We have 2 matches, so push 2 results
        surgeon
            .enclosing_symbol_detail_results
            .lock()
            .unwrap()
            .extend([Ok(None), Ok(None)]);

        let ws_dir = make_temp_workspace();
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        // Create the definition file
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/auth.rs"),
            "fn login() -> bool { true }",
        )
        .unwrap();

        let scout = Arc::new(MockScout::default());
        // MockScout returns 2 matches:
        // 1. src/auth.rs - the definition file (has "fn login" which matches def pattern -> FILTERED OUT)
        // 2. src/main.rs - a different file calling login (-> KEPT as reference)
        scout.set_result(Ok(pathfinder_search::SearchResult {
            matches: vec![
                pathfinder_search::SearchMatch {
                    file: "src/auth.rs".to_string(), // DEFINITION FILE - will be excluded
                    line: 1,
                    column: 4,
                    content: "fn login() -> bool { true }".to_string(), // Matches Rust fn pattern
                    context_before: vec![],
                    context_after: vec![],
                    enclosing_semantic_path: None,
                    is_definition: None,
                    version_hash: "sha256:a".to_string(),
                    known: Some(false),
                },
                pathfinder_search::SearchMatch {
                    file: "src/main.rs".to_string(), // REFERENCE FILE - will be kept
                    line: 10,
                    column: 8,
                    content: "let _ = login();".to_string(), // Doesn't match fn pattern
                    context_before: vec![],
                    context_after: vec![],
                    enclosing_semantic_path: None,
                    is_definition: None,
                    version_hash: "sha256:b".to_string(),
                    known: Some(false),
                },
            ],
            total_matches: 2,
            truncated: false,
            files_searched: 2,
            files_in_scope: 2,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }));

        // Use NoOpLawyer to simulate LSP unavailable (forces grep fallback path)
        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            scout,
            surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // DELIVERABLE B assertions:
        assert!(val.degraded, "should still be degraded (grep is heuristic)");

        // Key: degraded_reason should be the GrepFallback variant, not NoLsp
        assert_eq!(
            val.degraded_reason,
            Some(DegradedReason::NoLspGrepFallback),
            "degraded_reason should be NoLspGrepFallback when grep returns results"
        );

        // Key: references should be Some, not None
        assert!(
            val.references.is_some(),
            "references should be Some when grep fallback finds results"
        );

        let refs = val.references.unwrap();
        assert_eq!(
            refs.len(),
            1,
            "should have exactly 1 reference (definition file excluded)"
        );

        // The remaining reference should be from main.rs, not auth.rs
        assert_eq!(refs[0].file, "src/main.rs");
        assert_eq!(refs[0].line, 10);

        // Verify metadata fields
        assert_eq!(val.files_referenced, 1);
        assert_eq!(val.total_references, Some(1));
        assert_eq!(val.resolution_strategy, Some("grep_file_scoped".to_owned()));
        assert_eq!(val.lsp_readiness, Some("unavailable".to_owned()));
    }

    #[tokio::test]
    async fn test_find_all_references_grep_fallback_no_results_stays_none() {
        // When grep fallback finds no results (or search fails):
        // - references should stay None
        // - degraded_reason should stay NoLsp (not GrepFallback variant)
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        // 1 match -> need 1 enclosing_symbol_detail result
        surgeon
            .enclosing_symbol_detail_results
            .lock()
            .unwrap()
            .push(Ok(None));

        let ws_dir = make_temp_workspace();
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/auth.rs"),
            "fn login() -> bool { true }",
        )
        .unwrap();

        let scout = Arc::new(MockScout::default());
        // MockScout returns a match that's ONLY the definition itself
        // - same file as definition
        // - matches the definition pattern ("fn login...")
        // After filtering, there are 0 references left
        scout.set_result(Ok(pathfinder_search::SearchResult {
            matches: vec![pathfinder_search::SearchMatch {
                file: "src/auth.rs".to_string(), // Definition file
                line: 1,
                column: 4,
                content: "fn login() -> bool { true }".to_string(), // Matches def pattern
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:a".to_string(),
                known: Some(false),
            }],
            total_matches: 1,
            truncated: false,
            files_searched: 1,
            files_in_scope: 1,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }));

        // Use NoOpLawyer to force grep fallback path
        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            scout,
            surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // When grep fallback has no valid references after filtering,
        // we should fall back to the original behavior:
        assert!(val.degraded);
        assert_eq!(
            val.degraded_reason,
            Some(DegradedReason::NoLsp),
            "should be NoLsp when grep finds no valid references"
        );
        assert!(
            val.references.is_none(),
            "references should be None when grep finds no valid refs"
        );
        assert_eq!(val.files_referenced, 0);
        assert!(val.total_references.is_none());
    }

    #[tokio::test]
    async fn test_find_all_references_lsp_error_uses_grep_fallback() {
        // DELIVERABLE B: Grep fallback should also work on LspError paths:
        // - Timeout
        // - Protocol error
        // - ConnectionLost
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        // 1 match -> need 1 enclosing_symbol_detail result
        surgeon
            .enclosing_symbol_detail_results
            .lock()
            .unwrap()
            .push(Ok(None));

        let ws_dir = make_temp_workspace();
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/auth.rs"),
            "fn login() -> bool { true }",
        )
        .unwrap();

        let scout = Arc::new(MockScout::default());
        // MockScout returns a reference from a different file
        scout.set_result(Ok(pathfinder_search::SearchResult {
            matches: vec![pathfinder_search::SearchMatch {
                file: "src/main.rs".to_string(), // NOT the definition file
                line: 10,
                column: 8,
                content: "login();".to_string(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:test".to_string(),
                known: Some(false),
            }],
            total_matches: 1,
            truncated: false,
            files_searched: 1,
            files_in_scope: 1,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }));

        // Use MockLawyer that returns ConnectionLost error (not just NoOpLawyer)
        // This tests the Err(e) error path, not just NoLspAvailable
        let lawyer = Arc::new(MockLawyer::default());
        lawyer.set_references_lsp_error(Err(LspError::ConnectionLost));

        let server =
            PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(val.degraded);

        // ConnectionLost should map to LspErrorGrepFallback
        assert_eq!(
            val.degraded_reason,
            Some(DegradedReason::LspErrorGrepFallback)
        );

        // Grep fallback should provide results
        assert!(val.references.is_some());
        let refs = val.references.unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].file, "src/main.rs");
    }

    #[tokio::test]
    async fn test_find_all_references_lsp_timeout_uses_grep_fallback() {
        // Finding 2: Test LspError::Timeout path with grep fallback results.
        // Timeout produces distinct metadata from ConnectionLost:
        // - lsp_readiness: "warming_up" (not "unavailable")
        // - warm_start_in_progress: Some(true) (not None)
        // - degraded_reason: LspTimeoutGrepFallback (not LspErrorGrepFallback)
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        // 1 match -> need 1 enclosing_symbol_detail result
        surgeon
            .enclosing_symbol_detail_results
            .lock()
            .unwrap()
            .push(Ok(None));

        let ws_dir = make_temp_workspace();
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/auth.rs"),
            "fn login() -> bool { true }",
        )
        .unwrap();

        let scout = Arc::new(MockScout::default());
        scout.set_result(Ok(pathfinder_search::SearchResult {
            matches: vec![pathfinder_search::SearchMatch {
                file: "src/main.rs".to_string(),
                line: 10,
                column: 8,
                content: "login();".to_string(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:test".to_string(),
                known: Some(false),
            }],
            total_matches: 1,
            truncated: false,
            files_searched: 1,
            files_in_scope: 1,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }));

        // Use MockLawyer that returns Timeout error (not ConnectionLost)
        let lawyer = Arc::new(MockLawyer::default());
        lawyer.set_references_lsp_error(Err(LspError::Timeout {
            operation: "references".to_string(),
            timeout_ms: 5000,
        }));

        let server =
            PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(val.degraded);

        // Timeout-specific assertions (Finding 2: distinct from ConnectionLost)
        assert_eq!(
            val.degraded_reason,
            Some(DegradedReason::LspTimeoutGrepFallback)
        );
        assert_eq!(val.lsp_readiness, Some("warming_up".to_owned()));
        assert_eq!(val.warm_start_in_progress, Some(true));
        assert_eq!(val.resolution_strategy, Some("grep_file_scoped".to_owned()));

        // Grep fallback should provide results
        assert!(val.references.is_some());
        let refs = val.references.unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].file, "src/main.rs");
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // Intentionally testing multiple file type filtering
    async fn test_find_all_references_grep_filters_non_source_files() {
        // Grep fallback should only return matches from actual source files,
        // not from docs (.md), configs (.json, .toml, .yaml), etc.
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        // 4 matches -> 4 enclosing_symbol_detail results
        surgeon
            .enclosing_symbol_detail_results
            .lock()
            .unwrap()
            .extend([Ok(None), Ok(None), Ok(None), Ok(None)]);

        let ws_dir = make_temp_workspace();
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/auth.rs"),
            "fn login() -> bool { true }",
        )
        .unwrap();

        let scout = Arc::new(MockScout::default());
        // Return mix of source and non-source files
        scout.set_result(Ok(pathfinder_search::SearchResult {
            matches: vec![
                // Rust source - KEPT
                pathfinder_search::SearchMatch {
                    file: "src/main.rs".to_string(),
                    line: 10,
                    column: 8,
                    content: "login();".to_string(),
                    context_before: vec![],
                    context_after: vec![],
                    enclosing_semantic_path: None,
                    is_definition: None,
                    version_hash: "sha256:a".to_string(),
                    known: Some(false),
                },
                // TypeScript source - KEPT
                pathfinder_search::SearchMatch {
                    file: "web/auth.ts".to_string(),
                    line: 5,
                    column: 4,
                    content: "import { login }".to_string(),
                    context_before: vec![],
                    context_after: vec![],
                    enclosing_semantic_path: None,
                    is_definition: None,
                    version_hash: "sha256:b".to_string(),
                    known: Some(false),
                },
                // Markdown doc - FILTERED OUT
                pathfinder_search::SearchMatch {
                    file: "docs/README.md".to_string(),
                    line: 20,
                    column: 1,
                    content: "call login()".to_string(),
                    context_before: vec![],
                    context_after: vec![],
                    enclosing_semantic_path: None,
                    is_definition: None,
                    version_hash: "sha256:c".to_string(),
                    known: Some(false),
                },
                // JSON config - FILTERED OUT
                pathfinder_search::SearchMatch {
                    file: "config.json".to_string(),
                    line: 3,
                    column: 1,
                    content: "\"login\": true".to_string(),
                    context_before: vec![],
                    context_after: vec![],
                    enclosing_semantic_path: None,
                    is_definition: None,
                    version_hash: "sha256:d".to_string(),
                    known: Some(false),
                },
            ],
            total_matches: 4,
            truncated: false,
            files_searched: 4,
            files_in_scope: 4,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }));

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            scout,
            surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(val.degraded);
        assert_eq!(val.degraded_reason, Some(DegradedReason::NoLspGrepFallback));
        assert!(val.references.is_some());

        let refs = val.references.unwrap();
        assert_eq!(
            refs.len(),
            2,
            "should filter out non-source files (.md, .json)"
        );

        let files: std::collections::HashSet<_> = refs.iter().map(|r| r.file.as_str()).collect();
        assert!(files.contains("src/main.rs"));
        assert!(files.contains("web/auth.ts"));
        assert!(!files.contains("docs/README.md"));
        assert!(!files.contains("config.json"));
    }

    #[tokio::test]
    async fn test_find_all_references_grep_fallback_unsupported_ext_uses_line_number() {
        // GAP 6 + BUG 1 test:
        // For unsupported extensions (.vue, .mjs, .cjs, .pyi):
        // - definition_patterns returns catch-all \b{name}\b which matches EVERYTHING
        // - OLD BUG: all same-file references were incorrectly excluded
        // - FIX: line-number matching is primary; catch-all regex is skipped
        //
        // This test uses a .vue file (unsupported extension) and verifies:
        // 1. Same-file definition line is excluded (via line-number)
        // 2. Same-file different-line reference is KEPT (not excluded by catch-all)

        let surgeon = Arc::new(MockSurgeon::new());

        // Custom SymbolScope for a Vue component
        // start_line = 4 (0-indexed) -> definition_line_1indexed = 5
        let vue_scope = pathfinder_common::types::SymbolScope {
            content: "<script setup>const useAuth = () => {}</script>".to_owned(),
            start_line: 4, // 0-indexed, means line 5 in 1-indexed
            end_line: 4,
            name_column: 20, // column of 'u' in useAuth
            language: "vue".to_owned(),
        };
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(vue_scope));

        // 2 matches -> 2 enclosing_symbol_detail results
        surgeon
            .enclosing_symbol_detail_results
            .lock()
            .unwrap()
            .extend([Ok(None), Ok(None)]);

        let ws_dir = tempfile::tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        // Create a .vue file for testing
        let components_dir = ws_dir.path().join("src/components");
        std::fs::create_dir_all(&components_dir).unwrap();
        std::fs::write(
            components_dir.join("Auth.vue"),
            "<script setup>\nconst useAuth = () => {}\n</script>\n<template>\n  <div @click=\"useAuth()\">Login</div>\n</template>",
        ).unwrap();

        let scout = Arc::new(MockScout::default());
        // Return 2 matches in the same .vue file:
        // - Line 5: definition site ("const useAuth = ...") -> should be EXCLUDED via line-number check
        // - Line 8: reference site ("useAuth()") -> should be KEPT (different line from definition)
        scout.set_result(Ok(pathfinder_search::SearchResult {
            matches: vec![
                // Match at line 5 (1-indexed) = definition_scope.start_line + 1 = 4 + 1 = 5
                // This is the DEFINITION SITE -> should be EXCLUDED
                pathfinder_search::SearchMatch {
                    file: "src/components/Auth.vue".to_string(),
                    line: 5, // matches definition line
                    column: 20,
                    content: "const useAuth = () => {}".to_string(),
                    context_before: vec![],
                    context_after: vec![],
                    enclosing_semantic_path: None,
                    is_definition: None,
                    version_hash: "sha256:a".to_string(),
                    known: Some(false),
                },
                // Match at line 8 (1-indexed) = different line
                // This is a SAME-FILE REFERENCE -> should be KEPT (BUG 1 would have excluded it)
                pathfinder_search::SearchMatch {
                    file: "src/components/Auth.vue".to_string(),
                    line: 8, // DIFFERENT line from definition
                    column: 15,
                    content: "<div @click=\"useAuth()\">Login</div>".to_string(),
                    context_before: vec![],
                    context_after: vec![],
                    enclosing_semantic_path: None,
                    is_definition: None,
                    version_hash: "sha256:b".to_string(),
                    known: Some(false),
                },
            ],
            total_matches: 2,
            truncated: false,
            files_searched: 1,
            files_in_scope: 1,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }));

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            scout,
            surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/components/Auth.vue::useAuth".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(val.degraded);
        assert_eq!(val.degraded_reason, Some(DegradedReason::NoLspGrepFallback));
        assert_eq!(val.resolution_strategy, Some("grep_file_scoped".to_owned()));

        // KEY ASSERTION for BUG 1 fix:
        // Before fix: catch-all \b{useAuth}\b would match BOTH lines,
        //             excluding ALL same-file references -> 0 results
        // After fix: line 5 matches definition line -> excluded
        //            line 8 is different -> KEPT
        //            So we should have 1 result
        assert!(
            val.references.is_some(),
            "should have references - same-file different-line refs should be kept"
        );

        let refs = val.references.unwrap();
        assert_eq!(
            refs.len(),
            1,
            "BUG 1: expected exactly 1 reference (def site excluded, same-file diff-line ref kept)"
        );

        assert_eq!(refs[0].file, "src/components/Auth.vue");
        assert_eq!(
            refs[0].line, 8,
            "should be the reference at line 8, not the definition at line 5"
        );
    }

    // ── BATCH-04 Remaining Coverage Tests for references.rs ─────────────────────

    #[tokio::test]
    async fn test_grep_references_fallback_regex_compilation_warning() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let definition_path = std::path::Path::new("src/main.invalid_regex");
        let definition_scope = super::super::test_helpers::make_scope();
        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/main.invalid_regex::main".to_string(),
            max_results: 100,
            offset: 0,
        };

        let res = server
            .grep_references_fallback("test_symbol", definition_path, &definition_scope, &params)
            .await;
        assert!(res.is_none());
    }

    #[tokio::test]
    async fn test_grep_references_fallback_overflows() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
        let scout = Arc::new(pathfinder_search::MockScout::default());

        let ws_dir = crate::server::tools::navigation::test_helpers::make_temp_workspace();
        let ws = pathfinder_common::types::WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = pathfinder_common::config::PathfinderConfig::default();
        let sandbox = pathfinder_common::sandbox::Sandbox::new(ws.path(), &config.sandbox);
        let server = crate::server::PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            scout.clone(),
            surgeon.clone(),
            lawyer,
        );

        scout.set_result(Ok(pathfinder_search::SearchResult {
            matches: vec![pathfinder_search::SearchMatch {
                file: "src/main.rs".to_string(),
                line: u64::MAX,
                column: u64::MAX,
                content: "fn main() {}".to_string(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "hash".to_string(),
                known: None,
            }],
            total_matches: 1,
            truncated: false,
            files_searched: 1,
            files_in_scope: 1,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }));

        // search_codebase_impl will call enclosing_symbol_detail, queue a result for it.
        surgeon
            .enclosing_symbol_detail_results
            .lock()
            .unwrap()
            .push(Ok(None));

        let definition_path = std::path::Path::new("src/main.rs");
        let definition_scope = super::super::test_helpers::make_scope();
        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/main.rs::main".to_string(),
            max_results: 100,
            offset: 0,
        };

        let fallback_res = server
            .grep_references_fallback("test_symbol", definition_path, &definition_scope, &params)
            .await;

        assert!(fallback_res.is_some());
        let (refs, files_count) = fallback_res.unwrap();
        assert_eq!(files_count, 1);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].line, 1);
        assert_eq!(refs[0].column, 1);
    }

    #[tokio::test]
    async fn test_find_all_references_file_read_failure_and_open_doc_failure() {
        let surgeon = Arc::new(MockSurgeon::default());
        let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());

        lawyer
            .did_open_error
            .lock()
            .unwrap()
            .replace(pathfinder_lsp::LspError::ConnectionLost);

        let ws_dir = crate::server::tools::navigation::test_helpers::make_temp_workspace();
        let ws = pathfinder_common::types::WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = pathfinder_common::config::PathfinderConfig::default();
        let sandbox = pathfinder_common::sandbox::Sandbox::new(ws.path(), &config.sandbox);
        let server = crate::server::PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            std::sync::Arc::new(pathfinder_search::MockScout::default()),
            surgeon.clone(),
            lawyer.clone(),
        );

        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(super::super::test_helpers::make_scope()));

        let file_path = ws_dir.path().join("src/main.rs");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o000)).unwrap();
        }

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/main.rs::main".to_string(),
            max_results: 100,
            offset: 0,
        };

        lawyer.references_result.lock().unwrap().replace(Ok(vec![
            pathfinder_lsp::types::ReferenceLocation {
                file: "src/user.rs".to_string(),
                line: 2,
                column: 1,
                snippet: "use main;".to_string(),
            },
        ]));

        lawyer
            .goto_implementation_result
            .lock()
            .unwrap()
            .replace(Err(pathfinder_lsp::LspError::ConnectionLost));

        let call_res = server.find_all_references_impl(params).await;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o755));
        }

        let call_res_unwrapped = call_res.expect("should succeed despite warnings");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res_unwrapped.structured_content.unwrap()).unwrap();

        assert!(!val.degraded);
        assert_eq!(val.references.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_find_all_references_zero_results_but_resolvable_definition_triggers_grep_fallback(
    ) {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        surgeon
            .enclosing_symbol_detail_results
            .lock()
            .unwrap()
            .push(Ok(None));

        let ws_dir = make_temp_workspace();
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/auth.rs"),
            "fn login() -> bool { true }",
        )
        .unwrap();

        let scout = Arc::new(MockScout::default());
        scout.set_result(Ok(pathfinder_search::SearchResult {
            matches: vec![pathfinder_search::SearchMatch {
                file: "src/main.rs".to_string(),
                line: 10,
                column: 8,
                content: "login();".to_string(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:test".to_string(),
                known: Some(false),
            }],
            total_matches: 1,
            truncated: false,
            files_searched: 1,
            files_in_scope: 1,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }));

        let lawyer = Arc::new(MockLawyer::default());
        // Empty references/implementations results
        lawyer.set_references_result(Ok(vec![]));
        lawyer.set_goto_implementation_result(Ok(vec![]));

        // Resolvable definition probe
        lawyer.set_goto_definition_result(Ok(Some(DefinitionLocation {
            file: "src/auth.rs".into(),
            line: 1,
            column: 4,
            preview: "fn login() -> bool { true }".into(),
        })));

        let server =
            PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

        let params = crate::server::types::FindAllReferencesParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_results: 50,
            offset: 0,
        };
        let result = server.find_all_references_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindAllReferencesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(val.degraded);
        assert_eq!(
            val.degraded_reason,
            Some(DegradedReason::LspWarmupGrepFallback)
        );
        let refs = val.references.unwrap_or_default();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].file, "src/main.rs");
    }
}
