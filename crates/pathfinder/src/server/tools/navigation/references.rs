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
        budget: &super::GrepFallbackBudget,
    ) -> Option<(Vec<crate::server::types::ReferenceLocation>, usize)> {
        // RSP-A10: this fallback previously had no wall-clock bound at all --
        // every other grep-fallback caller in this tool surface
        // (impact.rs/definition.rs/deep_context.rs) already shares a
        // `GrepFallbackBudget`. `acquire_search()` returns `None` once the
        // shared 5s deadline or 8-search cap is exhausted, in which case
        // this fallback correctly reports "found nothing" rather than
        // running an unbounded workspace walk.
        let deadline = budget.acquire_search()?;
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

        let search_params = pathfinder_search::SearchParams {
            workspace_root: self.workspace_root.path().to_path_buf(),
            query,
            is_regex: true,
            path_glob: "**/*".to_string(),
            max_results: params.max_references as usize,
            context_lines: 0,
            exclude_glob: vec![],
            offset: params.offset as usize,
            deadline: Some(deadline),
        };

        let result = match self
            .grep_candidates(
                &search_params,
                pathfinder_common::types::FilterMode::CodeOnly,
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    tool = "grep_references_fallback",
                    symbol = %symbol_name,
                    error = %e,
                    "grep_candidates failed during grep fallback"
                );
                return None;
            }
        };

        if result.is_empty() {
            return None;
        }

        let mut files_referenced = std::collections::HashSet::new();

        let references: Vec<crate::server::types::ReferenceLocation> = result
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
                    confidence: pathfinder_common::types::Confidence::Heuristic,
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

        if params.max_depth > 2 {
            return Err(crate::server::helpers::invalid_params_error(format!(
                "max_depth exceeds maximum allowed depth of 2 (requested: {}). Hint: For deep dependency traversal beyond depth 2, delegate to subagents or issue follow-up trace calls at each boundary.",
                params.max_depth
            )));
        }

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

        let ext = semantic_path
            .file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let language_id = pathfinder_lsp::language_id_for_extension(ext).unwrap_or("unknown");
        let readiness = self
            .lawyer
            .ensure_ready(language_id, std::time::Duration::from_secs(5))
            .await
            .unwrap_or(pathfinder_lsp::types::Readiness::Unavailable {
                language: language_id.to_string(),
                install_hint: format!("No LSP configured for {language_id}"),
                alternative: "Built-in fallback tools".to_string(),
            });

        if let pathfinder_lsp::types::Readiness::NotReady {
            elapsed_secs,
            retry_after_secs,
            eta_secs,
            phase,
        } = readiness
        {
            let symbol_name = super::last_symbol_name(&semantic_path).unwrap_or_default();
            let grep_result = if symbol_name.is_empty() {
                None
            } else {
                self.grep_references_fallback(
                    &symbol_name,
                    &semantic_path.file_path,
                    &symbol_scope,
                    &params,
                    &super::GrepFallbackBudget::new(),
                )
                .await
            };

            let offset = usize::try_from(params.offset).unwrap_or(0);
            let max_results = usize::try_from(params.max_references).unwrap_or(50).max(1);

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
            let next_offset = if truncated {
                Some(u32::try_from(offset.saturating_add(max_results)).unwrap_or(u32::MAX))
            } else {
                None
            };
            let duration_ms = start.elapsed().as_millis();

            let status = pathfinder_common::types::ToolStatus::NotReady {
                elapsed_secs,
                retry_after_secs,
                eta_secs,
                phase: phase.clone(),
                attempt: params.retry_attempt,
                max_attempts: 3,
            };

            let metadata = crate::server::types::FindAllReferencesMetadata {
                status: status.clone(),
                references: None,
                heuristic_candidates: if paginated_refs.is_empty() {
                    None
                } else {
                    Some(paginated_refs)
                },
                total_references: Some(total_references),
                truncated,
                next_offset,
                files_referenced,
                degraded: true,
                degraded_reason: Some(DegradedReason::LspWarmupEmptyUnverified),
                actionable_guidance: Some(DegradedReason::LspWarmupEmptyUnverified.guidance()),
                lsp_readiness: Some("warming_up".to_owned()),
                warm_start_in_progress: Some(true),
                duration_ms: Some(millis_to_u64(duration_ms)),
                resolution_strategy: Some("not_ready".to_owned()),
                hint: None,
            };

            let mut text_parts = Vec::new();
            if let Some(header) = crate::server::helpers::format_tool_status_header(&status) {
                text_parts.push(header);
            }
            text_parts.push(format!(
                "LSP is still warming up (phase: {phase}). Use host grep or retry later.\n[completed in {duration_ms}ms]"
            ));

            let mut result =
                rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(
                    text_parts.join("\n"),
                )]);
            result.structured_content = serialize_metadata(&metadata);
            return Ok(result);
        }

        let (position_line, position_char) = self
            .compute_lsp_position(
                &semantic_path.file_path,
                symbol_scope.name_line,
                symbol_scope.name_column,
                language_id,
            )
            .await;

        let lsp_start = std::time::Instant::now();
        let lsp_result = self
            .lawyer
            .references(
                self.workspace_root.path(),
                &semantic_path.file_path,
                position_line,
                position_char,
            )
            .await;

        let implementations_result = self
            .lawyer
            .goto_implementation(
                self.workspace_root.path(),
                &semantic_path.file_path,
                position_line,
                position_char,
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
                                snippet: String::new(),
                                confidence: pathfinder_common::types::Confidence::Verified,
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
                        snippet: String::new(),
                        confidence: pathfinder_common::types::Confidence::Verified,
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

                // Spec 4.4: Apply pagination to each list separately
                let total_references = references.len() + implementations.len();
                let offset = usize::try_from(params.offset).unwrap_or(0);
                // Item 4: Guard against max_results=0 which causes infinite pagination loops.
                let max_results = usize::try_from(params.max_references).unwrap_or(50).max(1);
                let truncated = total_references > offset.saturating_add(max_results);
                let next_offset = if truncated {
                    Some(u32::try_from(offset.saturating_add(max_results)).unwrap_or(u32::MAX))
                } else {
                    None
                };

                // Paginate implementations first, then references (matches display order)
                let impl_count = implementations.len();
                let ref_count = references.len();

                // Build resolution block before moving implementations
                let resolution_block = if implementations.is_empty() {
                    String::new()
                } else {
                    let sym_name = super::last_symbol_name(&semantic_path).unwrap_or_default();
                    let queried = if let Some(parent) = &symbol_scope.parent_name {
                        if sym_name == *parent {
                            parent.clone()
                        } else {
                            format!("{parent}.{sym_name}")
                        }
                    } else {
                        sym_name.clone()
                    };
                    let declared = format!(
                        "{}:{}",
                        semantic_path.file_path.display(),
                        symbol_scope.name_line + 1
                    );
                    let mut res_lines = vec![
                        "resolution:".to_string(),
                        format!("  queried:  {queried}"),
                        format!("  declared: {declared}"),
                    ];
                    for imp in &implementations {
                        res_lines.push(format!(
                            "  impl:     {}:{}   <- the runtime type",
                            imp.file, imp.line
                        ));
                    }
                    res_lines.push(
                        "  note:     name-matching tools commonly return the declaration site here"
                            .to_string(),
                    );
                    format!("{}\n\n", res_lines.join("\n"))
                };

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

                let implementations_text = if paginated_impls.is_empty() {
                    String::new()
                } else {
                    let header = format!(
                        "Implementations (extends/implements): {impl_count} found (verified)\n"
                    );
                    let items: Vec<_> = paginated_impls
                        .iter()
                        .map(|imp| format!("{}:{}", imp.file, imp.line))
                        .collect();
                    format!("{}{}\n", header, items.join("\n"))
                };

                let references_text = if paginated_refs.is_empty() {
                    if total_references == 0 {
                        "References: 0 (verified zero)\n".to_string()
                    } else {
                        String::new()
                    }
                } else {
                    let header = format!("References: {ref_count} found (verified)\n");
                    let items: Vec<_> = paginated_refs
                        .iter()
                        .map(|r| {
                            if r.confidence == pathfinder_common::types::Confidence::Heuristic
                                && !r.snippet.is_empty()
                            {
                                format!("{}:{}: {}", r.file, r.line, r.snippet)
                            } else {
                                format!("{}:{}", r.file, r.line)
                            }
                        })
                        .collect();
                    format!("{}{}", header, items.join("\n"))
                };

                let paginated_len = paginated_impls.len() + paginated_refs.len();
                let mut paginated = Vec::with_capacity(paginated_len);
                paginated.extend(paginated_impls);
                paginated.extend(paginated_refs);

                let pagination_note = if let Some(next) = next_offset {
                    format!(
                        "\nShowing {paginated_len} of {total_references} total. Reissue with offset={next} for the rest.\n"
                    )
                } else {
                    String::new()
                };

                let summary = if impl_count > 0 && ref_count > 0 {
                    format!(
                        "Found {ref_count} references + {impl_count} implementations across {files_referenced} files (verified).\n\n"
                    )
                } else if impl_count > 0 {
                    format!(
                        "Found {impl_count} implementations across {files_referenced} files (verified).\n\n"
                    )
                } else if ref_count > 0 {
                    format!("Found {ref_count} references across {files_referenced} files (verified).\n\n")
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

                let status = pathfinder_common::types::ToolStatus::Ok;
                let evidence = Some(pathfinder_common::types::Evidence::verified(paginated));

                crate::server::helpers::validate_evidence(&status, evidence.as_ref());

                let metadata = crate::server::types::FindAllReferencesMetadata {
                    status: status.clone(),
                    references: evidence,
                    heuristic_candidates: None,
                    total_references: Some(total_references),
                    truncated,
                    next_offset,
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

                let mut text_parts = Vec::new();
                if let Some(header) = crate::server::helpers::format_tool_status_header(&status) {
                    text_parts.push(header);
                    text_parts.push(String::new());
                }
                let hint_str = if let Some(h) = &metadata.hint {
                    format!("\nHINT: {h}\n")
                } else {
                    String::new()
                };
                text_parts.push(format!("{resolution_block}{summary}{implementations_text}{references_text}{pagination_note}{hint_str}\n[completed in {duration_ms}ms]"));

                let mut result =
                    rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(
                        text_parts.join("\n"),
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
                        &super::GrepFallbackBudget::new(),
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
                        .map(|r| {
                            format!(
                                "{}:{}:{}: {} [HEURISTIC]",
                                r.file, r.line, r.column, r.snippet
                            )
                        })
                        .collect();
                    let text = format!(
                            "Grep fallback: found {} references across {} files (heuristic only).\n\nReferences: {}\n{}\n",
                            ref_count, file_count, ref_count, items.join("\n")
                        );
                    (
                        Some(pathfinder_common::types::Evidence::heuristic(refs)),
                        Some(ref_count),
                        file_count,
                        DegradedReason::NoLspGrepFallback,
                        "grep_file_scoped",
                        text,
                    )
                } else {
                    // Fallback unsuccessful - keep original behavior
                    let text = format!(
                        "References: UNKNOWN (not verified — do NOT treat as zero). Use your host's grep tool to manually find usages of `{}`\n",
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

                let status = pathfinder_common::types::ToolStatus::Degraded {
                    reason: degraded_reason.to_string(),
                    retry_after_secs: None,
                };

                crate::server::helpers::validate_evidence(&status, references.as_ref());

                let metadata = crate::server::types::FindAllReferencesMetadata {
                    status: status.clone(),
                    references: references.clone(),
                    heuristic_candidates: references.as_ref().map(|ev| ev.items.clone()),
                    total_references,
                    truncated: false,
                    next_offset: None,
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

                let mut text_parts = Vec::new();
                if let Some(header) = crate::server::helpers::format_tool_status_header(&status) {
                    text_parts.push(header);
                } else {
                    text_parts.push(format_degraded_notice(&degraded_reason));
                }
                text_parts.push(format!("{text_body}[completed in {duration_ms}ms]"));

                let mut result =
                    rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(
                        text_parts.join("\n"),
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
                        &super::GrepFallbackBudget::new(),
                    )
                    .await
                };

                let default_degraded_reason = if is_timeout {
                    DegradedReason::LspTimeoutGrepFallback
                } else {
                    DegradedReason::LspErrorGrepFallback
                };

                let status = if is_timeout {
                    pathfinder_common::types::ToolStatus::NotReady {
                        elapsed_secs: u32::try_from(duration_ms / 1000).unwrap_or(0),
                        retry_after_secs: 15,
                        eta_secs: None,
                        phase: "indexing".to_string(),
                        attempt: params.retry_attempt,
                        max_attempts: 3,
                    }
                } else {
                    pathfinder_common::types::ToolStatus::Degraded {
                        reason: default_degraded_reason.to_string(),
                        retry_after_secs: None,
                    }
                };
                let is_not_ready = matches!(
                    status,
                    pathfinder_common::types::ToolStatus::NotReady { .. }
                );

                // Determine final state based on whether grep fallback succeeded.
                //
                // Contract 1 Decision 3.4: under `NotReady`, heuristic candidates
                // must NEVER populate the primary `references` field — only
                // `heuristic_candidates`. That rule does not apply to `Degraded`
                // (Decision 5's table explicitly permits Heuristic-confidence
                // data in the primary field there), so the primary/candidate
                // split below is a function of `is_not_ready`, not of whether
                // grep succeeded.
                let (
                    grep_evidence,
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
                    let text = if is_not_ready {
                        format!(
                            "No verified results are returned because an empty result here \
                             would be indistinguishable from a verified zero. {ref_count} \
                             unverified heuristic candidate(s) across {file_count} file(s) are \
                             available in `heuristic_candidates` — do NOT treat these as the \
                             answer; they may be false positives (same-named symbols, comments, \
                             imports). ACTION: wait, then reissue this exact call with \
                             retry_attempt={}.\n",
                            params.retry_attempt + 1
                        )
                    } else {
                        let items: Vec<_> = refs
                            .iter()
                            .map(|r| format!("{}:{}:{}: {}", r.file, r.line, r.column, r.snippet))
                            .collect();
                        format!(
                            "Grep fallback: found {} references across {} files (heuristic only).\n\nReferences: {}\n{}\n",
                            ref_count, file_count, ref_count, items.join("\n")
                        )
                    };
                    (
                        Some(pathfinder_common::types::Evidence::heuristic(refs)),
                        Some(ref_count),
                        file_count,
                        default_degraded_reason,
                        "grep_file_scoped",
                        text,
                    )
                } else {
                    // Fallback unsuccessful - keep original behavior
                    let text = format!(
                        "References: UNKNOWN (not verified — do NOT treat as zero). Use your host's grep tool to manually find usages of `{}`\n",
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

                let (primary_references, heuristic_candidates) = if is_not_ready {
                    (None, grep_evidence.as_ref().map(|ev| ev.items.clone()))
                } else {
                    (grep_evidence, None)
                };

                crate::server::helpers::validate_evidence(&status, primary_references.as_ref());

                let metadata = crate::server::types::FindAllReferencesMetadata {
                    status: status.clone(),
                    references: primary_references,
                    heuristic_candidates,
                    total_references,
                    truncated: false,
                    next_offset: None,
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

                let mut text_parts = Vec::new();
                if let Some(header) = crate::server::helpers::format_tool_status_header(&status) {
                    text_parts.push(header);
                } else {
                    text_parts.push(format_degraded_notice(&degraded_reason));
                }
                text_parts.push(format!("{text_body}[completed in {duration_ms}ms]"));

                let mut result =
                    rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(
                        text_parts.join("\n"),
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
