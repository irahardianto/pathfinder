//! `locate` tool handler (definition mode) and grep-based fallback strategies.

use crate::server::helpers::{
    format_degraded_notice, millis_to_u64, parse_semantic_path, pathfinder_to_error_data,
    require_symbol_target, serialize_metadata, treesitter_error_to_error_data,
};
use crate::server::types::{GetDefinitionResponse, LocateParams};
use crate::server::PathfinderServer;
use pathfinder_common::error::PathfinderError;
use pathfinder_common::types::DegradedReason;
use pathfinder_lsp::LspError;
use pathfinder_treesitter::symbols::did_you_mean;
use rmcp::model::ErrorData;

use super::{candidate_definition_pattern, definition_patterns, GrepFallbackBudget};

struct NotReadyArgs {
    elapsed_secs: u32,
    retry_after_secs: u32,
    eta_secs: Option<u32>,
    phase: String,
    retry_attempt: u32,
}

impl PathfinderServer {
    /// Applies a context-specific [`DegradedReason`] override to a grep-fallback
    /// response, preserving strategy-specific reasons (e.g.
    /// `GrepFallbackImplScoped`) already set by [`Self::fallback_definition_grep`],
    /// and stamps the final `duration_ms`.
    fn apply_fallback_reason(
        def: &mut GetDefinitionResponse,
        reason: DegradedReason,
        start: std::time::Instant,
        force_degraded: bool,
    ) {
        if !matches!(
            def.degraded_reason,
            Some(DegradedReason::GrepFallbackImplScoped)
        ) {
            def.degraded_reason = Some(reason);
        }
        if force_degraded {
            def.degraded = true;
        }
        def.duration_ms = Some(millis_to_u64(start.elapsed().as_millis()));
    }

    /// Handles the `Ok(None)` LSP outcome: falls back to grep, then to a
    /// `SymbolNotFound` error with "did you mean" suggestions.
    async fn resolve_lsp_definition_none(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        semantic_path_str: String,
        _symbol_scope: &pathfinder_common::types::SymbolScope,
        start: std::time::Instant,
        tree_sitter_ms: u128,
        lsp_ms: u128,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        let duration_ms = start.elapsed().as_millis();

        tracing::info!(
            tool = "get_definition",
            semantic_path = %semantic_path_str,
            tree_sitter_ms,
            lsp_ms,
            duration_ms,
            "get_definition: no definition found via LSP — attempting grep-based fallback"
        );

        if let Some(mut def) = self.fallback_definition_grep(semantic_path).await {
            Self::apply_fallback_reason(
                &mut def,
                DegradedReason::LspWarmupGrepFallback,
                start,
                false,
            );
            let duration_ms = def.duration_ms.unwrap_or(0);
            tracing::info!(
                tool = "get_definition",
                file = %def.file,
                line = def.line,
                duration_ms,
                degraded = true,
                degraded_reason = "lsp_warmup_grep_fallback",
                engines_used = ?["tree-sitter", "lsp", "ripgrep"],
                "get_definition: degraded complete (grep fallback after LSP None)"
            );
            return Ok(Self::get_def_to_call_result(&def));
        }

        tracing::info!(
            tool = "get_definition",
            semantic_path = %semantic_path_str,
            tree_sitter_ms,
            lsp_ms,
            duration_ms,
            "get_definition: no definition found (LSP None, grep empty)"
        );
        let did_you_mean_suggestions = self.compute_did_you_mean(semantic_path).await;

        let retry_after = if self.lawyer.is_warm_start_complete() {
            None
        } else {
            Some(10u32)
        };

        Err(pathfinder_to_error_data(&PathfinderError::SymbolNotFound {
            semantic_path: semantic_path_str,
            did_you_mean: did_you_mean_suggestions,
            retry_after_seconds: retry_after,
        }))
    }

    /// Handles `LspError::NoLspAvailable`: attempts a grep-based fallback,
    /// falling back to the original `NoLspAvailable` error when grep also
    /// finds nothing.
    async fn handle_no_lsp_available(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        language: String,
        start: std::time::Instant,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        // Degraded mode — LSP not available. Use a grep-based heuristic to
        // find a likely definition location. This is not LSP-accurate but
        // gives the agent a starting point without requiring a full
        // `search_codebase` call.
        tracing::info!(
            tool = "get_definition",
            symbol = %semantic_path,
            "get_definition: no LSP — attempting grep-based fallback"
        );

        if let Some(mut def) = self.fallback_definition_grep(semantic_path).await {
            Self::apply_fallback_reason(&mut def, DegradedReason::NoLspGrepFallback, start, false);
            let duration_ms = def.duration_ms.unwrap_or(0);
            tracing::info!(
                tool = "get_definition",
                file = %def.file,
                line = def.line,
                duration_ms,
                degraded = true,
                degraded_reason = "no_lsp_grep_fallback",
                engines_used = ?["tree-sitter", "ripgrep"],
                "get_definition: degraded complete (grep fallback)"
            );
            return Ok(Self::get_def_to_call_result(&def));
        }

        // No grep match either — return the original LSP error
        let duration_ms = start.elapsed().as_millis();
        tracing::info!(
            tool = "get_definition",
            duration_ms,
            degraded = true,
            degraded_reason = "no_lsp",
            engines_used = ?["none"],
            "get_definition: degraded (no LSP, grep fallback also empty)"
        );
        Err(pathfinder_to_error_data(&PathfinderError::NoLspAvailable {
            language,
        }))
    }

    /// Handles `LspError::Timeout`: attempts a grep-based fallback, falling
    /// back to a generic `LspError` when grep also finds nothing.
    async fn handle_lsp_timeout(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        semantic_path_str: String,
        start: std::time::Instant,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        // LSP timed out — attempt grep-based fallback
        tracing::info!(
            tool = "get_definition",
            semantic_path = %semantic_path_str,
            "get_definition: LSP timed out — attempting grep-based fallback"
        );

        if let Some(mut def) = self.fallback_definition_grep(semantic_path).await {
            Self::apply_fallback_reason(
                &mut def,
                DegradedReason::LspTimeoutGrepFallback,
                start,
                false,
            );
            let duration_ms = def.duration_ms.unwrap_or(0);
            tracing::info!(
                tool = "get_definition",
                file = %def.file,
                line = def.line,
                duration_ms,
                degraded = true,
                degraded_reason = "lsp_timeout_grep_fallback",
                engines_used = ?["tree-sitter", "lsp", "ripgrep"],
                "get_definition: degraded complete (grep fallback after timeout)"
            );
            return Ok(Self::get_def_to_call_result(&def));
        }

        tracing::warn!(
            tool = "get_definition",
            semantic_path = %semantic_path_str,
            "get_definition: LSP timed out and grep fallback found no match"
        );
        Err(pathfinder_to_error_data(&PathfinderError::LspError {
            message: "LSP timed out and grep fallback found no match".to_owned(),
        }))
    }

    /// Handles any other `LspError` (connection resets, protocol errors,
    /// etc.): attempts a grep-based fallback before giving up. Prevents agent
    /// stalls when an unexpected LSP failure occurs mid-session.
    async fn handle_lsp_generic_error(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        e: LspError,
        start: std::time::Instant,
        tree_sitter_ms: u128,
        lsp_ms: u128,
        duration_ms: u128,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        tracing::warn!(
            tool = "get_definition",
            error = %e,
            tree_sitter_ms,
            lsp_ms,
            duration_ms,
            engines_used = ?["lsp"],
            "get_definition: LSP error — attempting grep fallback"
        );

        if let Some(mut def) = self.fallback_definition_grep(semantic_path).await {
            // Preserve strategy-specific reasons (e.g., GrepFallbackImplScoped)
            // but override the generic GrepFallbackFileScoped with the
            // context-specific reason from the LSP error path.
            Self::apply_fallback_reason(
                &mut def,
                DegradedReason::LspErrorGrepFallback,
                start,
                true,
            );
            let duration_ms = def.duration_ms.unwrap_or(0);
            tracing::info!(
                tool = "get_definition",
                file = %def.file,
                line = def.line,
                duration_ms,
                degraded = true,
                degraded_reason = "lsp_error_grep_fallback",
                engines_used = ?["tree-sitter", "lsp", "ripgrep"],
                "get_definition: degraded complete (grep fallback after LSP error)"
            );
            return Ok(Self::get_def_to_call_result(&def));
        }

        let duration_ms = start.elapsed().as_millis();
        tracing::warn!(
            tool = "get_definition",
            error = %e,
            duration_ms,
            "get_definition: LSP error and grep fallback found no match"
        );
        Err(pathfinder_to_error_data(&PathfinderError::LspError {
            message: e.to_string(),
        }))
    }

    /// Parses/validates `semantic_path_str`, enforces the sandbox and
    /// file-existence checks, and resolves the symbol's position via
    /// Tree-sitter.
    ///
    /// Returns the parsed [`SemanticPath`](pathfinder_common::types::SemanticPath),
    /// its [`SymbolScope`](pathfinder_common::types::SymbolScope), and how
    /// long the Tree-sitter resolution took (in ms).
    async fn locate_symbol_for_definition(
        &self,
        semantic_path_str: &str,
        start: std::time::Instant,
    ) -> Result<
        (
            pathfinder_common::types::SemanticPath,
            pathfinder_common::types::SymbolScope,
            u128,
        ),
        ErrorData,
    > {
        let semantic_path = parse_semantic_path(semantic_path_str)?;
        require_symbol_target(&semantic_path, semantic_path_str)?;

        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            let duration_ms = start.elapsed().as_millis();
            tracing::warn!(
                tool = "get_definition",
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
                tool = "get_definition",
                path = %abs_file.display(),
                "file not found"
            );
            return Err(pathfinder_to_error_data(&err));
        }

        // Resolve the symbol position via Tree-sitter to get line/column
        let ts_start = std::time::Instant::now();
        let symbol_scope = self
            .surgeon
            .read_symbol_scope(self.workspace_root.path(), &semantic_path)
            .await
            .map_err(treesitter_error_to_error_data)?;
        let tree_sitter_ms = ts_start.elapsed().as_millis();

        Ok((semantic_path, symbol_scope, tree_sitter_ms))
    }

    /// Opens `semantic_path`'s file in the LSP's document buffer via an RAII
    /// guard, so `did_close` fires automatically when the guard drops.
    ///
    /// rust-analyzer (and similar servers) require files to be open in their
    /// document buffer to resolve definitions — without this, `goto_definition`
    /// returns null for all navigation. Returns `None` (degraded) when the
    /// file can't be read or the LSP rejects the open.
    async fn open_document_for_definition(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
    ) -> Option<Box<dyn pathfinder_lsp::lawyer::DocumentLease>> {
        let file_path = self.workspace_root.path().join(&semantic_path.file_path);
        let file_content = match tokio::fs::read_to_string(&file_path).await {
            Ok(content) => content,
            Err(e) => {
                tracing::warn!(
                    tool = "get_definition",
                    path = %file_path.display(),
                    error = %e,
                    "file read failed — LSP will receive empty content, goto_definition may return null"
                );
                String::new()
            }
        };
        match self
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
                    tool = "get_definition",
                    semantic_path = %semantic_path,
                    error = %e,
                    "open_document failed — LSP queries may return degraded results"
                );
                None
            }
        }
    }

    /// Core logic for the `get_definition` tool.
    ///
    /// Resolves the semantic path to a file position, queries the LSP for the
    /// definition location, and returns the result.
    ///
    /// **Degraded mode:** Returns a `LSP_REQUIRED` error when no LSP is configured.
    // This function coordinates Tree-sitter (position resolution), LSP (goto_definition),
    // and Ripgrep (degraded fallback). It has multiple outcome paths, each
    // delegated to its own handler:
    // 1. Happy path: LSP returns Some(def) — handled inline
    // 2. Warmup path: LSP returns None → resolve_lsp_definition_none
    // 3. Degraded path: NoLspAvailable → handle_no_lsp_available
    // 4. Timeout path: LspError::Timeout → handle_lsp_timeout
    // 5. Error path: any other LspError → handle_lsp_generic_error
    pub(crate) async fn get_definition_impl(
        &self,
        params: LocateParams,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        let start = std::time::Instant::now();
        let semantic_path_str = params.semantic_path.clone().unwrap_or_default();

        tracing::info!(
            tool = "get_definition",
            semantic_path = %semantic_path_str,
            "get_definition: start"
        );

        let (semantic_path, symbol_scope, tree_sitter_ms) = self
            .locate_symbol_for_definition(&semantic_path_str, start)
            .await?;

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

        match readiness {
            pathfinder_lsp::types::Readiness::Ready(_) => {
                self.handle_ready_definition(
                    &semantic_path,
                    &semantic_path_str,
                    &symbol_scope,
                    start,
                    tree_sitter_ms,
                )
                .await
            }
            pathfinder_lsp::types::Readiness::NotReady {
                elapsed_secs,
                retry_after_secs,
                eta_secs,
                phase,
            } => {
                self.handle_not_ready_definition(
                    &semantic_path,
                    start,
                    NotReadyArgs {
                        retry_attempt: params.retry_attempt,
                        elapsed_secs,
                        retry_after_secs,
                        eta_secs,
                        phase,
                    },
                )
                .await
            }
            pathfinder_lsp::types::Readiness::Unavailable {
                language,
                install_hint,
                alternative,
            } => {
                self.handle_unavailable_definition(
                    &semantic_path,
                    start,
                    language,
                    install_hint,
                    alternative,
                )
                .await
            }
            pathfinder_lsp::types::Readiness::Degraded {
                reason,
                retry_after_secs,
            } => {
                self.handle_degraded_definition(&semantic_path, start, reason, retry_after_secs)
                    .await
            }
        }
    }

    async fn handle_ready_definition(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        semantic_path_str: &str,
        symbol_scope: &pathfinder_common::types::SymbolScope,
        start: std::time::Instant,
        tree_sitter_ms: u128,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        let _doc_guard = self.open_document_for_definition(semantic_path).await;

        let ext = semantic_path
            .file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let language_id = pathfinder_lsp::language_id_for_extension(ext).unwrap_or("unknown");
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
            .goto_definition(
                self.workspace_root.path(),
                &semantic_path.file_path,
                position_line,
                position_char,
            )
            .await;
        let lsp_ms = lsp_start.elapsed().as_millis();
        let duration_ms = start.elapsed().as_millis();

        match lsp_result {
            Ok(Some(def)) => {
                tracing::info!(
                    tool = "get_definition",
                    file = %def.file,
                    definition_line = def.line,
                    tree_sitter_ms,
                    lsp_ms,
                    duration_ms,
                    engines_used = ?["tree-sitter", "lsp"],
                    "get_definition: complete"
                );
                Ok(Self::get_def_to_call_result(&GetDefinitionResponse {
                    status: pathfinder_common::types::ToolStatus::Ok,
                    file: def.file,
                    line: def.line,
                    column: def.column,
                    preview: def.preview,
                    degraded: false,
                    degraded_reason: None,
                    actionable_guidance: None,
                    lsp_readiness: Some("ready".to_owned()),
                    warm_start_in_progress: Some(false),
                    duration_ms: Some(millis_to_u64(duration_ms)),
                    resolution_strategy: Some("lsp".to_owned()),
                }))
            }
            Ok(None) => {
                self.resolve_lsp_definition_none(
                    semantic_path,
                    semantic_path_str.to_owned(),
                    symbol_scope,
                    start,
                    tree_sitter_ms,
                    lsp_ms,
                )
                .await
            }
            Err(LspError::NoLspAvailable) => {
                self.handle_no_lsp_available(semantic_path, symbol_scope.language.clone(), start)
                    .await
            }
            Err(LspError::Timeout { .. }) => {
                self.handle_lsp_timeout(semantic_path, semantic_path_str.to_owned(), start)
                    .await
            }
            Err(e) => {
                self.handle_lsp_generic_error(
                    semantic_path,
                    e,
                    start,
                    tree_sitter_ms,
                    lsp_ms,
                    duration_ms,
                )
                .await
            }
        }
    }

    async fn handle_not_ready_definition(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        start: std::time::Instant,
        args: NotReadyArgs,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        let status = pathfinder_common::types::ToolStatus::NotReady {
            elapsed_secs: args.elapsed_secs,
            retry_after_secs: args.retry_after_secs,
            eta_secs: args.eta_secs,
            phase: args.phase,
            attempt: args.retry_attempt,
            max_attempts: 3,
        };
        if let Some(mut def) = self.fallback_definition_grep(semantic_path).await {
            Self::apply_fallback_reason(
                &mut def,
                DegradedReason::LspWarmupGrepFallback,
                start,
                false,
            );
            def.status = status;
            return Ok(Self::get_def_to_call_result(&def));
        }
        let duration_ms = start.elapsed().as_millis();
        let def = GetDefinitionResponse {
            status,
            file: String::new(),
            line: 0,
            column: 0,
            preview: String::new(),
            degraded: true,
            degraded_reason: Some(DegradedReason::LspWarmupEmptyUnverified),
            actionable_guidance: Some(DegradedReason::LspWarmupEmptyUnverified.guidance()),
            lsp_readiness: Some("warming_up".to_owned()),
            warm_start_in_progress: Some(true),
            duration_ms: Some(millis_to_u64(duration_ms)),
            resolution_strategy: Some("not_ready".to_owned()),
        };
        Ok(Self::get_def_to_call_result(&def))
    }

    async fn handle_unavailable_definition(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        start: std::time::Instant,
        language: String,
        install_hint: String,
        alternative: String,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        let status = pathfinder_common::types::ToolStatus::Unavailable {
            language,
            install_hint,
            alternative,
        };
        if let Some(mut def) = self.fallback_definition_grep(semantic_path).await {
            Self::apply_fallback_reason(&mut def, DegradedReason::NoLspGrepFallback, start, true);
            def.status = pathfinder_common::types::ToolStatus::Degraded {
                reason: "LSP unavailable; using grep fallback".to_string(),
                retry_after_secs: None,
            };
            return Ok(Self::get_def_to_call_result(&def));
        }
        let duration_ms = start.elapsed().as_millis();
        let def = GetDefinitionResponse {
            status,
            file: String::new(),
            line: 0,
            column: 0,
            preview: String::new(),
            degraded: true,
            degraded_reason: Some(DegradedReason::NoLsp),
            actionable_guidance: Some(DegradedReason::NoLsp.guidance()),
            lsp_readiness: Some("unavailable".to_owned()),
            warm_start_in_progress: Some(false),
            duration_ms: Some(millis_to_u64(duration_ms)),
            resolution_strategy: Some("none".to_owned()),
        };
        Ok(Self::get_def_to_call_result(&def))
    }

    async fn handle_degraded_definition(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        start: std::time::Instant,
        reason: String,
        retry_after_secs: Option<u32>,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        let status = pathfinder_common::types::ToolStatus::Degraded {
            reason,
            retry_after_secs,
        };
        if let Some(mut def) = self.fallback_definition_grep(semantic_path).await {
            Self::apply_fallback_reason(
                &mut def,
                DegradedReason::LspErrorGrepFallback,
                start,
                true,
            );
            def.status = status;
            return Ok(Self::get_def_to_call_result(&def));
        }
        let duration_ms = start.elapsed().as_millis();
        let def = GetDefinitionResponse {
            status,
            file: String::new(),
            line: 0,
            column: 0,
            preview: String::new(),
            degraded: true,
            degraded_reason: Some(DegradedReason::LspErrorGrepFallback),
            actionable_guidance: Some(DegradedReason::LspErrorGrepFallback.guidance()),
            lsp_readiness: Some("degraded".to_owned()),
            warm_start_in_progress: Some(false),
            duration_ms: Some(millis_to_u64(duration_ms)),
            resolution_strategy: Some("none".to_owned()),
        };
        Ok(Self::get_def_to_call_result(&def))
    }

    /// Convert a `GetDefinitionResponse` into a `CallToolResult` with a
    /// human-readable text summary and the struct in `structured_content`.
    ///
    /// Mirrors the pattern used by all other tools in the suite.
    fn get_def_to_call_result(def: &GetDefinitionResponse) -> rmcp::model::CallToolResult {
        let mut text_parts = Vec::new();
        if let Some(header) = crate::server::helpers::format_tool_status_header(&def.status) {
            text_parts.push(header);
        } else if def.degraded {
            let notice = def
                .degraded_reason
                .as_ref()
                .map_or_else(|| "DEGRADED (unknown)".to_owned(), format_degraded_notice);
            text_parts.push(notice);
        }

        let loc = if def.column > 0 {
            format!(
                "{}:L{} col:{} — {}",
                def.file,
                def.line,
                def.column,
                if def.preview.is_empty() {
                    "(no preview)"
                } else {
                    &def.preview
                }
            )
        } else {
            format!(
                "{}:L{} — {}",
                def.file,
                def.line,
                if def.preview.is_empty() {
                    "(no preview)"
                } else {
                    &def.preview
                }
            )
        };
        text_parts.push(loc);
        text_parts.push(format!("[completed in {}ms]", def.duration_ms.unwrap_or(0)));

        let text = text_parts.join("\n");
        let mut res = rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(text)]);
        res.structured_content = serialize_metadata(def);
        res
    }

    async fn compute_did_you_mean(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
    ) -> Vec<String> {
        let Some(ref symbol_chain) = semantic_path.symbol_chain else {
            return Vec::new();
        };
        let symbols = self
            .surgeon
            .extract_symbols(self.workspace_root.path(), &semantic_path.file_path)
            .await;
        let Ok(symbols) = symbols else {
            return Vec::new();
        };
        did_you_mean(&symbols, symbol_chain, 3)
    }

    /// Grep-based fallback for definition resolution when LSP is unavailable or warming up.
    ///
    /// Uses a multi-strategy approach with a shared fan-out budget (RSP-A10):
    /// 1. Search the expected file first (if known from the semantic path)
    /// 2. Search for struct-qualified patterns (e.g., `impl Struct` + `fn method`)
    /// 3. Fall back to a global search (excludes test/mock files, returns first match)
    /// 4. Broad symbol search as last resort
    async fn fallback_definition_grep(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
    ) -> Option<GetDefinitionResponse> {
        let symbol_chain = semantic_path.symbol_chain.as_ref()?;
        let symbol_name = symbol_chain.segments.last()?.name.clone();
        let expected_file = &semantic_path.file_path;
        let budget = GrepFallbackBudget::new();

        // Strategy 1: Search the expected file first (highest confidence)
        if let Some(result) = self
            .grep_definition_in_file(symbol_name.clone(), expected_file.clone(), &budget)
            .await
        {
            return Some(result);
        }

        // Strategy 2: For method lookups (impl Struct), search for the impl block
        if symbol_chain.segments.len() >= 2 {
            let parent_name = symbol_chain.segments[symbol_chain.segments.len() - 2]
                .name
                .clone();
            let ext = expected_file
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            let path_glob = format!("**/*.{ext}");
            if let Some(result) = self
                .grep_impl_method(&parent_name, &symbol_name, &path_glob, &budget)
                .await
            {
                return Some(result);
            }
        }

        // Strategy 3: Global search (excludes test/mock files, returns first match)
        if let Some(result) = self
            .grep_definition_global(symbol_name.clone(), &budget)
            .await
        {
            return Some(result);
        }

        None
    }

    /// Search for a definition within a specific file.
    ///
    /// Uses language-aware patterns from `definition_patterns` (SPEC 007).
    async fn grep_definition_in_file(
        &self,
        symbol_name: String,
        file_path: std::path::PathBuf,
        budget: &GrepFallbackBudget,
    ) -> Option<GetDefinitionResponse> {
        // Extract file extension to determine which language patterns to use
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");

        // Get language-specific definition patterns
        let patterns = definition_patterns(ext, &symbol_name);

        // Use the file as a specific path glob. Convert to forward-slash
        // format for ripgrep compatibility across platforms.
        let glob = file_path.to_string_lossy().replace('\\', "/");

        // Try each pattern in sequence until a match is found
        for pattern in patterns {
            let Some(deadline) = budget.acquire_search() else {
                break;
            };

            let search_result = self
                .scout
                .search(&pathfinder_search::SearchParams {
                    workspace_root: self.workspace_root.path().to_path_buf(),
                    query: pattern,
                    is_regex: true,
                    max_results: 5,
                    path_glob: glob.clone(),
                    exclude_glob: Vec::new(),
                    context_lines: 0,
                    offset: 0,
                    deadline: Some(deadline),
                })
                .await;

            if let Ok(result) = search_result {
                if !result.matches.is_empty() {
                    let m = &result.matches[0];
                    return Some(GetDefinitionResponse {
                        status: pathfinder_common::types::ToolStatus::Degraded {
                            reason: DegradedReason::GrepFallbackFileScoped.to_string(),
                            retry_after_secs: None,
                        },
                        file: m.file.clone(),
                        line: u32::try_from(m.line).unwrap_or(u32::MAX),
                        column: u32::try_from(m.column).unwrap_or(1),
                        preview: m.content.clone(),
                        degraded: true,
                        degraded_reason: Some(DegradedReason::GrepFallbackFileScoped),
                        actionable_guidance: Some(
                            DegradedReason::GrepFallbackFileScoped.guidance(),
                        ),
                        lsp_readiness: Some("unavailable".to_owned()),
                        warm_start_in_progress: None,
                        duration_ms: None,
                        resolution_strategy: Some("grep_file".to_owned()),
                    });
                }
            } else if let Err(e) = search_result {
                tracing::warn!(
                    tool = "get_definition",
                    strategy = "grep_definition_in_file",
                    error = %e,
                    "scout.search failed during grep fallback"
                );
            }
        }
        None
    }

    /// Search for a method within an impl block (e.g., `impl Sandbox` containing `fn check`).
    async fn grep_impl_method(
        &self,
        parent_name: &str,
        method_name: &str,
        path_glob: &str,
        budget: &GrepFallbackBudget,
    ) -> Option<GetDefinitionResponse> {
        let deadline = budget.acquire_search()?;

        // First find files containing the impl block
        let parent_escaped = regex::escape(parent_name);
        let impl_pattern = format!(r"impl\s+(?:<[^>]+>\s+)?{parent_escaped}\b");
        let search_result = self
            .scout
            .search(&pathfinder_search::SearchParams {
                workspace_root: self.workspace_root.path().to_path_buf(),
                query: impl_pattern,
                is_regex: true,
                max_results: 10,
                path_glob: path_glob.to_owned(),
                exclude_glob: Vec::new(),
                context_lines: 0,
                offset: 0,
                deadline: Some(deadline),
            })
            .await;

        if let Ok(result) = search_result {
            for m in &result.matches {
                let Some(deadline) = budget.acquire_search() else {
                    break;
                };

                // Now search within this specific file for the method using shared candidate pattern.
                let ext = std::path::Path::new(&m.file)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");
                let method_pattern = candidate_definition_pattern(ext, method_name);
                let file_search = self
                    .scout
                    .search(&pathfinder_search::SearchParams {
                        workspace_root: self.workspace_root.path().to_path_buf(),
                        query: method_pattern,
                        is_regex: true,
                        max_results: 5,
                        path_glob: m.file.clone(),
                        exclude_glob: Vec::new(),
                        context_lines: 0,
                        offset: 0,
                        deadline: Some(deadline),
                    })
                    .await;

                if let Ok(file_result) = file_search {
                    if !file_result.matches.is_empty() {
                        let hit = &file_result.matches[0];
                        return Some(GetDefinitionResponse {
                            status: pathfinder_common::types::ToolStatus::Degraded {
                                reason: DegradedReason::GrepFallbackImplScoped.to_string(),
                                retry_after_secs: None,
                            },
                            file: hit.file.clone(),
                            line: u32::try_from(hit.line).unwrap_or(u32::MAX),
                            column: u32::try_from(hit.column).unwrap_or(1),
                            preview: hit.content.clone(),
                            degraded: true,
                            degraded_reason: Some(DegradedReason::GrepFallbackImplScoped),
                            actionable_guidance: Some(
                                DegradedReason::GrepFallbackImplScoped.guidance(),
                            ),
                            lsp_readiness: Some("unavailable".to_owned()),
                            warm_start_in_progress: None,
                            duration_ms: None,
                            resolution_strategy: Some("grep_impl".to_owned()),
                        });
                    }
                } else if let Err(e) = file_search {
                    tracing::warn!(
                        tool = "get_definition",
                        strategy = "grep_impl_method_file",
                        file = %m.file,
                        error = %e,
                        "scout.search failed during impl-method grep fallback"
                    );
                }
            }
        } else if let Err(e) = search_result {
            tracing::warn!(
                tool = "get_definition",
                strategy = "grep_impl_method",
                error = %e,
                "scout.search failed during impl-block grep fallback"
            );
        }
        None
    }

    /// Search globally across the workspace for a definition when file-scoped search fails.
    ///
    /// Excludes test files and mock implementations to prefer real definitions.
    async fn grep_definition_global(
        &self,
        symbol_name: String,
        budget: &GrepFallbackBudget,
    ) -> Option<GetDefinitionResponse> {
        let deadline = budget.acquire_search()?;

        // RSP-A10: shared with `candidate_definition_pattern`'s own
        // catch-all arm (see `generic_definition_pattern`) instead of a
        // hand-rolled, independently-drifting variant.
        let pattern = super::generic_definition_pattern(&symbol_name);

        let search_result = self
            .scout
            .search(&pathfinder_search::SearchParams {
                workspace_root: self.workspace_root.path().to_path_buf(),
                query: pattern,
                is_regex: true,
                max_results: 10,
                path_glob: "**/*".to_owned(),
                // Exclude test files and mock implementations to prefer real definitions
                exclude_glob: vec!["**/{test,tests,mock}*/**".to_owned()],
                offset: 0,
                context_lines: 0,
                deadline: Some(deadline),
            })
            .await;

        if let Ok(result) = search_result {
            if !result.matches.is_empty() {
                let m = &result.matches[0];
                return Some(GetDefinitionResponse {
                    status: pathfinder_common::types::ToolStatus::Degraded {
                        reason: DegradedReason::GrepFallbackGlobal.to_string(),
                        retry_after_secs: None,
                    },
                    file: m.file.clone(),
                    line: u32::try_from(m.line).unwrap_or(u32::MAX),
                    column: u32::try_from(m.column).unwrap_or(1),
                    preview: m.content.clone(),
                    degraded: true,
                    degraded_reason: Some(DegradedReason::GrepFallbackGlobal),
                    actionable_guidance: Some(DegradedReason::GrepFallbackGlobal.guidance()),
                    lsp_readiness: Some("unavailable".to_owned()),
                    warm_start_in_progress: None,
                    duration_ms: None,
                    resolution_strategy: Some("grep_global".to_owned()),
                });
            }
        } else if let Err(e) = search_result {
            tracing::warn!(
                tool = "get_definition",
                strategy = "grep_definition_global",
                error = %e,
                "scout.search failed during global grep fallback"
            );
        }
        None
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "definition_test.rs"]
mod tests;
