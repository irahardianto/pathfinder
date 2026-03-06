//! Pathfinder MCP Server — tool registration and dispatch.
//!
//! Implements `rmcp::ServerHandler` with all 16 Pathfinder tools.
//! Epic 2 implements `search_codebase` via `RipgrepScout`.
//! Remaining tools are stubs until their respective epics are built.
//!
//! # Module Layout
//! - [`helpers`] — error conversion, stub builder, language detection
//! - [`types`] — all parameter and response structs + default functions

mod helpers;
pub mod types;

use helpers::{io_error_data, language_from_path, pathfinder_to_error_data, stub_response};
#[allow(clippy::wildcard_imports)]
use types::*;

use pathfinder_common::config::PathfinderConfig;
use pathfinder_common::sandbox::Sandbox;
use pathfinder_common::types::{FilterMode, WorkspaceRoot};
use pathfinder_search::{RipgrepScout, Scout, SearchParams};
use pathfinder_treesitter::{Surgeon, TreeSitterSurgeon};

use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{ErrorData, Implementation, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ServerHandler};

use pathfinder_common::error::PathfinderError;
use pathfinder_common::types::{SemanticPath, VersionHash};
use std::path::Path;
use std::sync::Arc;
use tokio::fs as tfs;
use tokio::io::AsyncWriteExt as _;

/// The main Pathfinder MCP server.
///
/// Holds shared workspace state and dispatches MCP tool calls.
#[derive(Clone)]
pub struct PathfinderServer {
    workspace_root: Arc<WorkspaceRoot>,
    #[allow(dead_code)]
    config: Arc<PathfinderConfig>,
    #[allow(dead_code)]
    sandbox: Arc<Sandbox>,
    scout: Arc<dyn Scout>,
    surgeon: Arc<dyn Surgeon>,
    tool_router: ToolRouter<Self>,
}

impl PathfinderServer {
    /// Create a new Pathfinder server backed by the real Ripgrep scout and Tree-sitter surgeon.
    #[must_use]
    pub fn new(workspace_root: WorkspaceRoot, config: PathfinderConfig) -> Self {
        let sandbox = Sandbox::new(workspace_root.path(), &config.sandbox);
        Self::with_engines(
            workspace_root,
            config,
            sandbox,
            Arc::new(RipgrepScout::new()),
            Arc::new(TreeSitterSurgeon::new(100)), // Cache capacity of 100 files
        )
    }

    /// Create a server with injected engines (for testing).
    #[must_use]
    pub fn with_engines(
        workspace_root: WorkspaceRoot,
        config: PathfinderConfig,
        sandbox: Sandbox,
        scout: Arc<dyn Scout>,
        surgeon: Arc<dyn Surgeon>,
    ) -> Self {
        Self {
            workspace_root: Arc::new(workspace_root),
            config: Arc::new(config),
            sandbox: Arc::new(sandbox),
            scout,
            surgeon,
            tool_router: Self::tool_router(),
        }
    }
}

// ── Tool Router (defines all 16 tools) ──────────────────────────────

#[tool_router]
impl PathfinderServer {
    #[tool(
        name = "search_codebase",
        description = "Search the codebase for a text pattern. Returns matching lines with surrounding context. Each match includes an 'enclosing_semantic_path' (the AST symbol containing the match) and 'version_hash' (for immediate editing without a separate read). Use path_glob to narrow the search scope."
    )]
    async fn search_codebase(
        &self,
        Parameters(params): Parameters<SearchCodebaseParams>,
    ) -> Result<Json<SearchCodebaseResponse>, ErrorData> {
        let start = std::time::Instant::now();

        tracing::info!(
            tool = "search_codebase",
            query = %params.query,
            is_regex = params.is_regex,
            path_glob = %params.path_glob,
            filter_mode = ?params.filter_mode,
            "search_codebase: start"
        );

        // Note: filter_mode requires Tree-sitter (Epic 3).
        // In Epic 2 we set `degraded: true` if a filtered mode was requested.
        let degraded = params.filter_mode != FilterMode::All;

        let search_params = SearchParams {
            workspace_root: self.workspace_root.path().to_path_buf(),
            query: params.query.clone(),
            is_regex: params.is_regex,
            path_glob: params.path_glob.clone(),
            max_results: params.max_results as usize,
            context_lines: params.context_lines as usize,
        };

        match self.scout.search(&search_params).await {
            Ok(result) => {
                let mut enriched_matches = result.matches;

                // Populate enclosing_semantic_path using Surgeon
                for m in &mut enriched_matches {
                    let file_path = Path::new(&m.file);
                    if let Ok(Some(symbol)) = self
                        .surgeon
                        .enclosing_symbol(
                            self.workspace_root.path(),
                            file_path,
                            usize::try_from(m.line).unwrap_or(usize::MAX),
                        )
                        .await
                    {
                        m.enclosing_semantic_path = Some(format!("{}::{}", m.file, symbol));
                    }
                }

                let duration_ms = start.elapsed().as_millis();
                tracing::info!(
                    tool = "search_codebase",
                    total_matches = result.total_matches,
                    returned = enriched_matches.len(),
                    truncated = result.truncated,
                    duration_ms,
                    engines_used = ?["ripgrep", "treesitter"],
                    degraded,
                    "search_codebase: complete"
                );

                let mut response = SearchCodebaseResponse {
                    matches: enriched_matches,
                    total_matches: result.total_matches,
                    truncated: result.truncated,
                    degraded: None,
                    degraded_reason: None,
                };

                if degraded {
                    response.degraded = Some(true);
                    response.degraded_reason = Some(
                        "filter_mode requires Tree-sitter (available in Epic 3); returning unfiltered results"
                            .to_owned(),
                    );
                }

                Ok(Json(response))
            }
            Err(err) => {
                let duration_ms = start.elapsed().as_millis();
                tracing::warn!(
                    tool = "search_codebase",
                    error = %err,
                    error_code = "INTERNAL_ERROR",
                    error_message = %err,
                    duration_ms,
                    engines_used = ?["ripgrep"],
                    "search_codebase: failed"
                );
                Err(io_error_data(err.to_string()))
            }
        }
    }

    #[tool(
        name = "get_repo_map",
        description = "Returns the structural skeleton of the project as an indented tree of classes, functions, and type signatures. IMPORTANT: Each symbol has its full semantic path in a trailing comment. You MUST copy-paste these EXACT paths into read/edit tools. Also returns version_hashes per file for immediate editing."
    )]
    #[allow(clippy::unused_self)]
    async fn get_repo_map(
        &self,
        Parameters(params): Parameters<GetRepoMapParams>,
    ) -> Result<Json<GetRepoMapResponse>, rmcp::model::ErrorData> {
        let start = std::time::Instant::now();
        tracing::info!(tool = "get_repo_map", path = %params.path, "get_repo_map: start");

        let target_path = std::path::Path::new(&params.path);

        // Sandbox check
        if let Err(e) = self.sandbox.check(target_path) {
            tracing::warn!(tool = "get_repo_map", path = %params.path, error = %e, "get_repo_map: access denied");
            return Err(pathfinder_to_error_data(&e));
        }

        let result = match self
            .surgeon
            .generate_skeleton(
                self.workspace_root.path(),
                target_path,
                params.max_tokens,
                params.depth,
                match params.visibility {
                    crate::server::types::Visibility::Public => "public",
                    crate::server::types::Visibility::All => "all",
                },
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let pfe = match e {
                    pathfinder_treesitter::error::SurgeonError::ParseError(reason) => {
                        pathfinder_common::error::PathfinderError::ParseError {
                            path: target_path.to_path_buf(),
                            reason,
                        }
                    }
                    pathfinder_treesitter::error::SurgeonError::UnsupportedLanguage(_) => {
                        pathfinder_common::error::PathfinderError::UnsupportedLanguage {
                            path: target_path.to_path_buf(),
                        }
                    }
                    pathfinder_treesitter::error::SurgeonError::SymbolNotFound { .. } => {
                        pathfinder_common::error::PathfinderError::SymbolNotFound {
                            semantic_path: params.path.clone(),
                            did_you_mean: vec![],
                        }
                    }
                    pathfinder_treesitter::error::SurgeonError::Io(err) => {
                        return Err(crate::server::helpers::io_error_data(err.to_string()));
                    }
                };
                return Err(pathfinder_to_error_data(&pfe));
            }
        };

        tracing::info!(
            tool = "get_repo_map",
            path = %params.path,
            duration_ms = start.elapsed().as_millis(),
            files_scanned = result.files_scanned,
            files_truncated = result.files_truncated,
            engines_used = "treesitter",
            "get_repo_map: complete"
        );

        Ok(Json(GetRepoMapResponse {
            skeleton: result.skeleton,
            tech_stack: result.tech_stack,
            files_scanned: result.files_scanned,
            files_truncated: result.files_truncated,
            files_in_scope: result.files_in_scope,
            coverage_percent: result.coverage_percent,
            version_hashes: result.version_hashes,
            // Visibility filtering is not yet implemented; all symbols are returned.
            // Always signal degraded so agents know the param has no effect.
            visibility_degraded: Some(true),
        }))
    }

    #[tool(
        name = "read_symbol_scope",
        description = "Extract the exact source code of a single symbol (function, class, method) by its semantic path. Returns the code, line range, and version_hash for OCC."
    )]
    async fn read_symbol_scope(
        &self,
        Parameters(params): Parameters<ReadSymbolScopeParams>,
    ) -> Result<Json<ReadSymbolScopeResponse>, ErrorData> {
        let start = std::time::Instant::now();

        tracing::info!(
            tool = "read_symbol_scope",
            semantic_path = %params.semantic_path,
            "read_symbol_scope: start"
        );

        let Some(semantic_path) = SemanticPath::parse(&params.semantic_path) else {
            let duration_ms = start.elapsed().as_millis();
            let e = "invalid semantic path format";
            tracing::warn!(
                tool = "read_symbol_scope",
                error = %e,
                error_code = "INVALID_TARGET",
                error_message = %e,
                duration_ms,
                engines_used = ?(&[] as &[&str]),
                "invalid semantic path"
            );
            return Err(io_error_data(e.to_string()));
        };

        // Sandbox check on the file path
        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            let duration_ms = start.elapsed().as_millis();
            tracing::warn!(
                tool = "read_symbol_scope",
                error = %e,
                error_code = e.error_code(),
                error_message = %e,
                duration_ms,
                engines_used = ?(&[] as &[&str]),
                "sandbox check failed"
            );
            return Err(pathfinder_to_error_data(&e));
        }

        // Delegate to surgeon
        match self
            .surgeon
            .read_symbol_scope(self.workspace_root.path(), &semantic_path)
            .await
        {
            Ok(scope) => {
                let duration_ms = start.elapsed().as_millis();
                tracing::info!(
                    tool = "read_symbol_scope",
                    semantic_path = %params.semantic_path,
                    lines = (scope.end_line - scope.start_line + 1),
                    duration_ms,
                    engines_used = ?["tree-sitter"],
                    "read_symbol_scope: complete"
                );

                Ok(Json(ReadSymbolScopeResponse {
                    content: scope.content,
                    start_line: scope.start_line,
                    end_line: scope.end_line,
                    version_hash: scope.version_hash.to_string(),
                    language: scope.language,
                }))
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis();

                // Convert SurgeonError to PathfinderError if possible, or io_error
                let error_data = match &e {
                    pathfinder_treesitter::SurgeonError::SymbolNotFound {
                        path: _,
                        did_you_mean,
                    } => {
                        let err = PathfinderError::SymbolNotFound {
                            semantic_path: semantic_path.to_string(),
                            did_you_mean: did_you_mean.clone(),
                        };
                        pathfinder_to_error_data(&err)
                    }
                    pathfinder_treesitter::SurgeonError::UnsupportedLanguage(_) => {
                        let err = PathfinderError::UnsupportedLanguage {
                            path: semantic_path.file_path.clone(),
                        };
                        pathfinder_to_error_data(&err)
                    }
                    pathfinder_treesitter::SurgeonError::Io(_) => {
                        let err = PathfinderError::FileNotFound {
                            path: semantic_path.file_path.clone(),
                        };
                        pathfinder_to_error_data(&err)
                    }
                    pathfinder_treesitter::SurgeonError::ParseError(msg) => {
                        io_error_data(format!("Parse error: {msg}"))
                    }
                };

                tracing::warn!(
                    tool = "read_symbol_scope",
                    error = %e,
                    error_message = %e,
                    duration_ms,
                    engines_used = ?["tree-sitter"],
                    "read_symbol_scope: failed"
                );
                Err(error_data)
            }
        }
    }

    #[tool(
        name = "read_with_deep_context",
        description = "Extract a symbol's source code PLUS the signatures of all functions it calls. Use this when you need to understand a function's dependencies before editing it."
    )]
    #[allow(clippy::unused_self)]
    fn read_with_deep_context(
        &self,
        Parameters(_params): Parameters<ReadWithDeepContextParams>,
    ) -> Json<StubResponse> {
        stub_response("read_with_deep_context")
    }

    #[tool(
        name = "get_definition",
        description = "Jump to where a symbol is defined. Provide a semantic path to a reference and get back the definition's file, line, and a code preview."
    )]
    #[allow(clippy::unused_self)]
    fn get_definition(
        &self,
        Parameters(_params): Parameters<GetDefinitionParams>,
    ) -> Json<StubResponse> {
        stub_response("get_definition")
    }

    #[tool(
        name = "analyze_impact",
        description = "Find all callers of a symbol (incoming) and all symbols it calls (outgoing). Use this BEFORE refactoring to understand the blast radius of a change. Returns version_hashes for all referenced files."
    )]
    #[allow(clippy::unused_self)]
    fn analyze_impact(
        &self,
        Parameters(_params): Parameters<AnalyzeImpactParams>,
    ) -> Json<StubResponse> {
        stub_response("analyze_impact")
    }

    #[tool(
        name = "replace_body",
        description = "Replace the internal logic of a block-scoped construct (function, method, class body, impl block), keeping the signature intact. Provide ONLY the body content — DO NOT include the outer braces or function signature. DO NOT wrap your code in markdown code blocks."
    )]
    #[allow(clippy::unused_self)]
    fn replace_body(
        &self,
        Parameters(_params): Parameters<ReplaceBodyParams>,
    ) -> Json<StubResponse> {
        stub_response("replace_body")
    }

    #[tool(
        name = "replace_full",
        description = "Replace an entire declaration including its signature, body, decorators, and doc comments. Provide the COMPLETE replacement — anything you omit (decorators, doc comments) will be removed. DO NOT wrap your code in markdown code blocks."
    )]
    #[allow(clippy::unused_self)]
    fn replace_full(
        &self,
        Parameters(_params): Parameters<ReplaceFullParams>,
    ) -> Json<StubResponse> {
        stub_response("replace_full")
    }

    #[tool(
        name = "insert_before",
        description = "Insert new code BEFORE a target symbol. To insert at the TOP of a file (e.g., adding imports), use a bare file path without '::'. Pathfinder automatically adds one blank line between your code and the target."
    )]
    #[allow(clippy::unused_self)]
    fn insert_before(
        &self,
        Parameters(_params): Parameters<InsertBeforeParams>,
    ) -> Json<StubResponse> {
        stub_response("insert_before")
    }

    #[tool(
        name = "insert_after",
        description = "Insert new code AFTER a target symbol. To append to the BOTTOM of a file (e.g., adding new classes), use a bare file path without '::'. Pathfinder automatically adds one blank line between the target and your code."
    )]
    #[allow(clippy::unused_self)]
    fn insert_after(
        &self,
        Parameters(_params): Parameters<InsertAfterParams>,
    ) -> Json<StubResponse> {
        stub_response("insert_after")
    }

    #[tool(
        name = "delete_symbol",
        description = "Delete a symbol and all its associated decorators, attributes, and doc comments. If the target is a class, the ENTIRE class is deleted. If the target is a method (e.g., 'AuthService.login'), only that method is deleted."
    )]
    #[allow(clippy::unused_self)]
    fn delete_symbol(
        &self,
        Parameters(_params): Parameters<DeleteSymbolParams>,
    ) -> Json<StubResponse> {
        stub_response("delete_symbol")
    }

    #[tool(
        name = "validate_only",
        description = "Dry-run an edit WITHOUT writing to disk. Use this to pre-check risky changes. Returns the same validation results as a real edit. IMPORTANT: new_version_hash will be null because nothing was written. Reuse your original base_version for the real edit."
    )]
    #[allow(clippy::unused_self)]
    fn validate_only(
        &self,
        Parameters(_params): Parameters<ValidateOnlyParams>,
    ) -> Json<StubResponse> {
        stub_response("validate_only")
    }

    #[tool(
        name = "create_file",
        description = "Create a new file with initial content. Parent directories are created automatically. Returns a version_hash for subsequent edits."
    )]
    async fn create_file(
        &self,
        Parameters(params): Parameters<CreateFileParams>,
    ) -> Result<Json<CreateFileResponse>, ErrorData> {
        let start = std::time::Instant::now();
        let relative_path = Path::new(&params.filepath);
        let absolute_path = self.workspace_root.resolve(relative_path);

        tracing::info!(
            tool = "create_file",
            filepath = %params.filepath,
            "create_file: start"
        );

        // 1. Sandbox check
        if let Err(e) = self.sandbox.check(relative_path) {
            let duration_ms = start.elapsed().as_millis();
            let err_data = pathfinder_to_error_data(&e);
            tracing::warn!(
                tool = "create_file",
                error = %e,
                error_code = e.error_code(),
                error_message = %e,
                duration_ms,
                engines_used = ?(&[] as &[&str]),
                "sandbox check failed"
            );
            return Err(err_data);
        }

        // 2. Create parent directories
        if let Some(parent) = absolute_path.parent() {
            if let Err(e) = tfs::create_dir_all(parent).await {
                let duration_ms = start.elapsed().as_millis();
                tracing::warn!(
                    tool = "create_file",
                    error = %e,
                    error_code = "INTERNAL_ERROR",
                    error_message = %e,
                    duration_ms,
                    engines_used = ?(&[] as &[&str]),
                    "failed to create parent directories"
                );
                return Err(io_error_data(format!(
                    "failed to create parent directories: {e}"
                )));
            }
        }

        // 3. Atomically create file via tokio::fs::OpenOptions
        let open_result = tfs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&absolute_path)
            .await;

        match open_result {
            Ok(mut file) => {
                if let Err(e) = file.write_all(params.content.as_bytes()).await {
                    let duration_ms = start.elapsed().as_millis();
                    tracing::warn!(
                        tool = "create_file",
                        error = %e,
                        error_code = "INTERNAL_ERROR",
                        error_message = %e,
                        duration_ms,
                        engines_used = ?(&[] as &[&str]),
                        "failed to write file content"
                    );
                    return Err(io_error_data(format!("failed to write file content: {e}")));
                }

                if let Err(e) = file.flush().await {
                    return Err(io_error_data(format!("failed to flush file: {e}")));
                }
                if let Err(e) = file.sync_all().await {
                    return Err(io_error_data(format!("failed to sync file: {e}")));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let err = PathfinderError::FileAlreadyExists {
                    path: relative_path.to_path_buf(),
                };
                let duration_ms = start.elapsed().as_millis();
                tracing::warn!(
                    tool = "create_file",
                    error = %err,
                    error_code = err.error_code(),
                    error_message = %err,
                    duration_ms,
                    engines_used = ?(&[] as &[&str]),
                    "file already exists"
                );
                return Err(pathfinder_to_error_data(&err));
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis();
                tracing::warn!(
                    tool = "create_file",
                    error = %e,
                    error_code = "INTERNAL_ERROR",
                    error_message = %e,
                    duration_ms,
                    engines_used = ?(&[] as &[&str]),
                    "failed to create file"
                );
                return Err(io_error_data(format!("failed to create file: {e}")));
            }
        }

        let version_hash = VersionHash::compute(params.content.as_bytes());
        let duration_ms = start.elapsed().as_millis();

        tracing::info!(
            tool = "create_file",
            filepath = %params.filepath,
            version_hash = %version_hash.as_str(),
            duration_ms,
            engines_used = ?(&[] as &[&str]),
            "create_file: complete"
        );

        Ok(Json(CreateFileResponse {
            success: true,
            version_hash: version_hash.as_str().to_owned(),
            validation: ValidationResult {
                status: "passed".to_owned(),
                introduced_errors: vec![],
            },
        }))
    }

    #[tool(
        name = "delete_file",
        description = "Delete a file. Requires base_version (OCC) to prevent deleting a file that was modified after you last read it."
    )]
    async fn delete_file(
        &self,
        Parameters(params): Parameters<DeleteFileParams>,
    ) -> Result<Json<DeleteFileResponse>, ErrorData> {
        let start = std::time::Instant::now();
        let relative_path = Path::new(&params.filepath);
        let absolute_path = self.workspace_root.resolve(relative_path);

        tracing::info!(
            tool = "delete_file",
            filepath = %params.filepath,
            "delete_file: start"
        );

        // 1. Sandbox check
        if let Err(e) = self.sandbox.check(relative_path) {
            let duration_ms = start.elapsed().as_millis();
            tracing::warn!(
                tool = "delete_file",
                error = %e,
                error_code = e.error_code(),
                error_message = %e,
                duration_ms,
                engines_used = ?(&[] as &[&str]),
                "sandbox check failed"
            );
            return Err(pathfinder_to_error_data(&e));
        }

        // 2. OCC — read the file, verify base_version matches current hash.
        // Handles NotFound atomically (no TOCTOU race between .exists() and read).
        let current_content = match tfs::read(&absolute_path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let err = PathfinderError::FileNotFound {
                    path: relative_path.to_path_buf(),
                };
                let duration_ms = start.elapsed().as_millis();
                tracing::warn!(
                    tool = "delete_file",
                    error = %err,
                    error_code = err.error_code(),
                    error_message = %err,
                    duration_ms,
                    engines_used = ?(&[] as &[&str]),
                    "file not found"
                );
                return Err(pathfinder_to_error_data(&err));
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis();
                tracing::warn!(
                    tool = "delete_file",
                    error = %e,
                    error_code = "INTERNAL_ERROR",
                    error_message = %e,
                    duration_ms,
                    engines_used = ?(&[] as &[&str]),
                    "failed to read file for OCC"
                );
                return Err(io_error_data(format!("failed to read file: {e}")));
            }
        };
        let current_hash = VersionHash::compute(&current_content);
        if current_hash.as_str() != params.base_version {
            let err = PathfinderError::VersionMismatch {
                path: relative_path.to_path_buf(),
                current_version_hash: current_hash.as_str().to_owned(),
            };
            tracing::warn!(tool = "delete_file", error = %err, "OCC version mismatch");
            return Err(pathfinder_to_error_data(&err));
        }

        // 4. Delete
        if let Err(e) = tfs::remove_file(&absolute_path).await {
            tracing::warn!(tool = "delete_file", error = %e, "failed to delete file");
            return Err(io_error_data(format!("failed to delete file: {e}")));
        }

        let duration_ms = start.elapsed().as_millis();
        tracing::info!(
            tool = "delete_file",
            filepath = %params.filepath,
            duration_ms,
            engines_used = ?(&[] as &[&str]),
            "delete_file: complete"
        );

        Ok(Json(DeleteFileResponse { success: true }))
    }

    #[tool(
        name = "read_file",
        description = "Read raw file content. Use ONLY for configuration files (.env, Dockerfile, YAML, TOML, package.json). For source code, use read_symbol_scope instead. Supports pagination via start_line for large files."
    )]
    async fn read_file(
        &self,
        Parameters(params): Parameters<ReadFileParams>,
    ) -> Result<Json<ReadFileResponse>, ErrorData> {
        let start = std::time::Instant::now();
        let relative_path = Path::new(&params.filepath);
        let absolute_path = self.workspace_root.resolve(relative_path);

        tracing::info!(
            tool = "read_file",
            filepath = %params.filepath,
            start_line = params.start_line,
            max_lines = params.max_lines,
            "read_file: start"
        );

        // 1. Sandbox check
        if let Err(e) = self.sandbox.check(relative_path) {
            let duration_ms = start.elapsed().as_millis();
            let err_data = pathfinder_to_error_data(&e);
            tracing::warn!(
                tool = "read_file",
                error = %e,
                error_code = e.error_code(),
                error_message = %e,
                duration_ms,
                engines_used = ?(&[] as &[&str]),
                "sandbox check failed"
            );
            return Err(err_data);
        }

        // 2. Read file
        let raw_content = match tfs::read_to_string(&absolute_path).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let err = PathfinderError::FileNotFound {
                    path: relative_path.to_path_buf(),
                };
                let duration_ms = start.elapsed().as_millis();
                tracing::warn!(
                    tool = "read_file",
                    error = %err,
                    error_code = err.error_code(),
                    error_message = %err,
                    duration_ms,
                    engines_used = ?(&[] as &[&str]),
                    "file not found"
                );
                return Err(pathfinder_to_error_data(&err));
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis();
                tracing::warn!(
                    tool = "read_file",
                    error = %e,
                    error_code = "INTERNAL_ERROR",
                    error_message = %e,
                    duration_ms,
                    engines_used = ?(&[] as &[&str]),
                    "failed to read file"
                );
                return Err(io_error_data(format!("failed to read file: {e}")));
            }
        };

        let version_hash = VersionHash::compute(raw_content.as_bytes());

        // 3. Paginate — start_line is 1-indexed
        let all_lines: Vec<&str> = raw_content.lines().collect();
        let total_lines = u32::try_from(all_lines.len()).unwrap_or(u32::MAX);
        let start_idx = params.start_line.saturating_sub(1) as usize;
        let end_idx = (start_idx + params.max_lines as usize).min(all_lines.len());
        let page_lines = &all_lines[start_idx..end_idx];
        let lines_returned = u32::try_from(page_lines.len()).unwrap_or(u32::MAX);
        let truncated = end_idx < all_lines.len();
        let content = page_lines.join("\n");

        // 4. Detect language from extension
        let language = language_from_path(relative_path);
        let duration_ms = start.elapsed().as_millis();

        tracing::info!(
            tool = "read_file",
            filepath = %params.filepath,
            total_lines,
            lines_returned,
            truncated,
            duration_ms,
            engines_used = ?(&[] as &[&str]),
            "read_file: complete"
        );

        Ok(Json(ReadFileResponse {
            content,
            start_line: params.start_line,
            lines_returned,
            total_lines,
            truncated,
            version_hash: version_hash.as_str().to_owned(),
            language,
        }))
    }

    #[tool(
        name = "write_file",
        description = "WARNING: This bypasses AST validation and formatting. DO NOT use for source code (TypeScript, Python, Go, Rust). ONLY use for configuration files (.env, .gitignore, Dockerfile, YAML). For source code, use replace_body or replace_full instead. Provide EITHER 'content' for full replacement OR 'replacements' for surgical search-and-replace edits (e.g., {old_text: 'postgres:15', new_text: 'postgres:16'}). Use replacements when changing specific text in large files. Requires base_version (OCC)."
    )]
    async fn write_file(
        &self,
        Parameters(params): Parameters<WriteFileParams>,
    ) -> Result<Json<WriteFileResponse>, ErrorData> {
        let start = std::time::Instant::now();
        let relative_path = Path::new(&params.filepath);
        let absolute_path = self.workspace_root.resolve(relative_path);

        tracing::info!(
            tool = "write_file",
            filepath = %params.filepath,
            mode = if params.content.is_some() { "full_replacement" } else { "search_and_replace" },
            "write_file: start"
        );

        // 1. Validate mutually exclusive modes
        match (&params.content, &params.replacements) {
            (None, None) | (Some(_), Some(_)) => {
                let duration_ms = start.elapsed().as_millis();
                let e = "exactly one of 'content' or 'replacements' must be provided";
                tracing::warn!(
                    tool = "write_file",
                    error = %e,
                    error_code = "INVALID_TARGET",
                    error_message = %e,
                    duration_ms,
                    engines_used = ?(&[] as &[&str]),
                    "invalid arguments"
                );
                return Err(io_error_data(e));
            }
            _ => {}
        }

        // 2. Sandbox check
        if let Err(e) = self.sandbox.check(relative_path) {
            let duration_ms = start.elapsed().as_millis();
            tracing::warn!(
                tool = "write_file",
                error = %e,
                error_code = e.error_code(),
                error_message = %e,
                duration_ms,
                engines_used = ?(&[] as &[&str]),
                "sandbox check failed"
            );
            return Err(pathfinder_to_error_data(&e));
        }

        // 3. Verify file exists and read current content
        let current_content = match tfs::read_to_string(&absolute_path).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let err = PathfinderError::FileNotFound {
                    path: relative_path.to_path_buf(),
                };
                let duration_ms = start.elapsed().as_millis();
                tracing::warn!(
                    tool = "write_file",
                    error = %err,
                    error_code = err.error_code(),
                    error_message = %err,
                    duration_ms,
                    engines_used = ?(&[] as &[&str]),
                    "file not found"
                );
                return Err(pathfinder_to_error_data(&err));
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis();
                tracing::warn!(
                    tool = "write_file",
                    error = %e,
                    error_code = "INTERNAL_ERROR",
                    error_message = %e,
                    duration_ms,
                    engines_used = ?(&[] as &[&str]),
                    "failed to read file"
                );
                return Err(io_error_data(format!("failed to read file: {e}")));
            }
        };

        // 4. OCC check
        let current_hash = VersionHash::compute(current_content.as_bytes());
        if current_hash.as_str() != params.base_version {
            let err = PathfinderError::VersionMismatch {
                path: relative_path.to_path_buf(),
                current_version_hash: current_hash.as_str().to_owned(),
            };
            let duration_ms = start.elapsed().as_millis();
            tracing::warn!(
                tool = "write_file",
                error = %err,
                error_code = err.error_code(),
                error_message = %err,
                duration_ms,
                engines_used = ?(&[] as &[&str]),
                "OCC version mismatch"
            );
            return Err(pathfinder_to_error_data(&err));
        }

        // 5. Compute new content
        let new_content = if let Some(content) = params.content {
            content
        } else {
            // Search-and-replace mode
            // SAFETY: validated above that exactly one of content/replacements is Some.
            let replacements = params.replacements.unwrap_or_default();
            let mut working = current_content.clone();
            for replacement in &replacements {
                let occurrences = working.matches(replacement.old_text.as_str()).count();
                match occurrences {
                    0 => {
                        let err = PathfinderError::MatchNotFound {
                            filepath: relative_path.to_path_buf(),
                        };
                        let duration_ms = start.elapsed().as_millis();
                        tracing::warn!(
                            tool = "write_file",
                            old_text = %replacement.old_text,
                            error = %err,
                            error_code = err.error_code(),
                            error_message = %err,
                            duration_ms,
                            engines_used = ?(&[] as &[&str]),
                            "match not found"
                        );
                        return Err(pathfinder_to_error_data(&err));
                    }
                    1 => {
                        working = working.replacen(&replacement.old_text, &replacement.new_text, 1);
                    }
                    n => {
                        let err = PathfinderError::AmbiguousMatch {
                            filepath: relative_path.to_path_buf(),
                            occurrences: n,
                        };
                        let duration_ms = start.elapsed().as_millis();
                        tracing::warn!(
                            tool = "write_file",
                            old_text = %replacement.old_text,
                            occurrences = n,
                            error = %err,
                            error_code = err.error_code(),
                            error_message = %err,
                            duration_ms,
                            engines_used = ?(&[] as &[&str]),
                            "ambiguous match"
                        );
                        return Err(pathfinder_to_error_data(&err));
                    }
                }
            }
            working
        };

        // 6. TOCTOU late-check: re-read and re-hash immediately before write
        let late_content = match tfs::read(&absolute_path).await {
            Ok(b) => b,
            Err(e) => {
                let duration_ms = start.elapsed().as_millis();
                tracing::warn!(
                    tool = "write_file",
                    error = %e,
                    error_code = "INTERNAL_ERROR",
                    error_message = %e,
                    duration_ms,
                    engines_used = ?(&[] as &[&str]),
                    "TOCTOU re-read failed"
                );
                return Err(io_error_data(format!("TOCTOU re-read failed: {e}")));
            }
        };
        let late_hash = VersionHash::compute(&late_content);
        if late_hash.as_str() != params.base_version {
            let err = PathfinderError::VersionMismatch {
                path: relative_path.to_path_buf(),
                current_version_hash: late_hash.as_str().to_owned(),
            };
            let duration_ms = start.elapsed().as_millis();
            tracing::warn!(
                tool = "write_file",
                error = %err,
                error_code = err.error_code(),
                error_message = %err,
                duration_ms,
                engines_used = ?(&[] as &[&str]),
                "TOCTOU version mismatch on late-check"
            );
            return Err(pathfinder_to_error_data(&err));
        }

        // 7. Write to disk (in-place: preserves inode for HMR/watchers)
        if let Err(e) = tfs::write(&absolute_path, new_content.as_bytes()).await {
            let duration_ms = start.elapsed().as_millis();
            tracing::warn!(
                tool = "write_file",
                error = %e,
                error_code = "INTERNAL_ERROR",
                error_message = %e,
                duration_ms,
                engines_used = ?(&[] as &[&str]),
                "failed to write file"
            );
            return Err(io_error_data(format!("failed to write file: {e}")));
        }

        let new_hash = VersionHash::compute(new_content.as_bytes());
        let duration_ms = start.elapsed().as_millis();

        tracing::info!(
            tool = "write_file",
            filepath = %params.filepath,
            duration_ms,
            engines_used = ?(&[] as &[&str]),
            "write_file: complete"
        );

        Ok(Json(WriteFileResponse {
            success: true,
            new_version_hash: new_hash.as_str().to_owned(),
        }))
    }
}

// ── ServerHandler trait impl ────────────────────────────────────────

#[tool_handler]
impl ServerHandler for PathfinderServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::default()
            .with_server_info(Implementation::new("pathfinder", env!("CARGO_PKG_VERSION")))
    }
}

// ── Language Detection ──────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use pathfinder_search::{MockScout, SearchMatch, SearchResult};
    use pathfinder_treesitter::mock::MockSurgeon;
    use rmcp::model::ErrorCode;
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_get_repo_map_success() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let mock_surgeon = MockSurgeon::new();
        mock_surgeon
            .generate_skeleton_results
            .lock()
            .unwrap()
            .push(Ok(pathfinder_treesitter::repo_map::RepoMapResult {
                skeleton: "class Mock {}".to_string(),
                tech_stack: vec!["TypeScript".to_string()],
                files_scanned: 1,
                files_truncated: 0,
                files_in_scope: 1,
                coverage_percent: 100,
                version_hashes: std::collections::HashMap::new(),
            }));

        let server = PathfinderServer::with_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            Arc::new(mock_surgeon),
        );

        let params = GetRepoMapParams {
            path: ".".to_string(),
            max_tokens: 1000,
            depth: 3,
            visibility: crate::server::types::Visibility::Public,
            include_imports: crate::server::types::IncludeImports::None,
        };

        let result = server.get_repo_map(Parameters(params)).await;
        assert!(result.is_ok());
        let response = result.unwrap().0;
        assert_eq!(response.skeleton, "class Mock {}");
        assert_eq!(response.files_scanned, 1);
        assert_eq!(response.coverage_percent, 100);
        // Visibility filtering is not implemented; always degraded
        assert_eq!(response.visibility_degraded, Some(true));
    }

    #[tokio::test]
    async fn test_get_repo_map_visibility_degraded() {
        // Even when visibility = All, the response should always be visibility_degraded: Some(true)
        // because the feature is not yet implemented.
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let mock_surgeon = MockSurgeon::new();
        mock_surgeon
            .generate_skeleton_results
            .lock()
            .unwrap()
            .push(Ok(pathfinder_treesitter::repo_map::RepoMapResult {
                skeleton: String::new(),
                tech_stack: vec![],
                files_scanned: 0,
                files_truncated: 0,
                files_in_scope: 0,
                coverage_percent: 100,
                version_hashes: std::collections::HashMap::new(),
            }));

        let server = PathfinderServer::with_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            Arc::new(mock_surgeon),
        );

        let params = GetRepoMapParams {
            visibility: crate::server::types::Visibility::All,
            ..Default::default()
        };
        let result = server
            .get_repo_map(Parameters(params))
            .await
            .expect("should succeed");
        assert_eq!(
            result.0.visibility_degraded,
            Some(true),
            "visibility_degraded must be Some(true) regardless of requested visibility"
        );
    }

    #[tokio::test]
    async fn test_get_repo_map_access_denied() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let mock_surgeon = MockSurgeon::new();
        let server = PathfinderServer::with_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            Arc::new(mock_surgeon),
        );

        let params = GetRepoMapParams {
            path: ".env".to_string(), // Sandbox should deny this
            ..Default::default()
        };

        let Err(err) = server.get_repo_map(Parameters(params)).await else {
            panic!("Expected ACCESS_DENIED error");
        };
        assert_eq!(err.code, ErrorCode::INTERNAL_ERROR);
    }

    #[tokio::test]
    async fn test_create_file_success_and_already_exists() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);
        let mock_scout = MockScout::default();
        let server = PathfinderServer::with_engines(
            ws,
            config,
            sandbox,
            Arc::new(mock_scout),
            Arc::new(MockSurgeon::new()),
        );

        let filepath = "src/new_file.ts";
        let content = "console.log('hello');";
        let params = CreateFileParams {
            filepath: filepath.to_owned(),
            content: content.to_owned(),
        };

        // 1. First creation should succeed
        let result = server.create_file(Parameters(params.clone())).await;
        assert!(result.is_ok(), "Expected success, got {:#?}", result.err());
        let val = result.expect("create_file should succeed").0;
        assert!(val.success);
        assert_eq!(val.validation.status, "passed");

        let expected_hash = VersionHash::compute(content.as_bytes());
        assert_eq!(val.version_hash, expected_hash.as_str());

        // Verify file is on disk
        let absolute_path = ws_dir.path().join(filepath);
        assert!(absolute_path.exists());
        let read_content = fs::read_to_string(&absolute_path).expect("read file");
        assert_eq!(read_content, content);

        // 2. Second creation should fail (FILE_ALREADY_EXISTS)
        let result2 = server.create_file(Parameters(params)).await;
        assert!(result2.is_err());
        if let Err(err) = result2 {
            let code = err
                .data
                .as_ref()
                .and_then(|d| d.get("error"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            assert_eq!(code, "FILE_ALREADY_EXISTS", "got data: {:?}", err.data);
        } else {
            panic!("Expected error mapping to FILE_ALREADY_EXISTS");
        }

        // 3. Attempt to create file in a denied location
        let deny_params = CreateFileParams {
            filepath: ".git/objects/some_file".to_owned(),
            content: "payload".to_owned(),
        };
        let result3 = server.create_file(Parameters(deny_params)).await;
        assert!(result3.is_err());
        if let Err(err) = result3 {
            let code = err
                .data
                .as_ref()
                .and_then(|d| d.get("error"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            assert_eq!(code, "ACCESS_DENIED", "got data: {:?}", err.data);
        } else {
            panic!("Expected error mapping to ACCESS_DENIED");
        }
    }

    #[tokio::test]
    async fn test_search_codebase_routes_to_scout_and_handles_success() {
        let ws = WorkspaceRoot::new(std::env::temp_dir()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let mock_scout = MockScout::default();
        mock_scout.set_result(Ok(SearchResult {
            matches: vec![SearchMatch {
                file: "src/main.rs".to_owned(),
                line: 10,
                column: 5,
                content: "test_query()".to_owned(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                version_hash: "sha256:123".to_owned(),
            }],
            total_matches: 1,
            truncated: false,
        }));

        let mock_surgeon = Arc::new(MockSurgeon::new());
        mock_surgeon
            .enclosing_symbol_results
            .lock()
            .unwrap()
            .push(Ok(Some("test_query_func".to_owned())));

        let server = PathfinderServer::with_engines(
            ws,
            config,
            sandbox,
            Arc::new(mock_scout.clone()),
            mock_surgeon.clone(),
        );
        let params = SearchCodebaseParams {
            query: "test_query".to_owned(),
            is_regex: true,
            ..Default::default()
        };

        let result = server.search_codebase(Parameters(params)).await;
        // Json(val) gives us val.0
        let val = result.expect("search_codebase should succeed").0;

        assert_eq!(val.total_matches, 1);
        assert!(!val.truncated);
        let matches = val.matches;
        assert_eq!(matches[0].file, "src/main.rs");
        assert_eq!(matches[0].content, "test_query()");
        assert_eq!(
            matches[0].enclosing_semantic_path.as_deref(),
            Some("src/main.rs::test_query_func")
        );

        let calls = mock_scout.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].query, "test_query");
        assert!(calls[0].is_regex);

        let surgeon_calls = mock_surgeon.enclosing_symbol_calls.lock().unwrap();
        assert_eq!(surgeon_calls.len(), 1);
        assert_eq!(surgeon_calls[0].1, std::path::PathBuf::from("src/main.rs"));
        assert_eq!(surgeon_calls[0].2, 10);
    }

    #[tokio::test]
    async fn test_search_codebase_handles_scout_error() {
        let ws = WorkspaceRoot::new(std::env::temp_dir()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let mock_scout = MockScout::default();
        mock_scout.set_result(Err("simulated engine error".to_owned()));

        let server = PathfinderServer::with_engines(
            ws,
            config,
            sandbox,
            Arc::new(mock_scout),
            Arc::new(MockSurgeon::new()),
        );
        let params = SearchCodebaseParams::default();

        let result = server.search_codebase(Parameters(params)).await;

        let err = result
            .err()
            .expect("search_codebase should return error on scout failure");
        assert_eq!(err.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(err.message, "search engine error: simulated engine error");
    }

    // ── delete_file tests ────────────────────────────────────────────

    #[tokio::test]
    async fn test_delete_file_success_and_occ_failure() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);
        let server = PathfinderServer::with_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            Arc::new(MockSurgeon::new()),
        );

        // Create a file to delete
        let filepath = "to_delete.txt";
        let content = "goodbye";
        let abs = ws_dir.path().join(filepath);
        fs::write(&abs, content).expect("write");
        let hash = VersionHash::compute(content.as_bytes());

        // Happy path
        let result = server
            .delete_file(Parameters(DeleteFileParams {
                filepath: filepath.to_owned(),
                base_version: hash.as_str().to_owned(),
            }))
            .await;
        assert!(result.is_ok(), "Expected success, got {:?}", result.err());
        assert!(!abs.exists(), "File should be gone");

        // FILE_NOT_FOUND — file is already deleted, now handled via tfs::read NotFound (no pre-check race)
        let result2 = server
            .delete_file(Parameters(DeleteFileParams {
                filepath: filepath.to_owned(),
                base_version: hash.as_str().to_owned(),
            }))
            .await;
        assert!(result2.is_err());
        let Err(err) = result2 else {
            panic!("expected error")
        };
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "FILE_NOT_FOUND", "got: {err:?}");

        // VERSION_MISMATCH — recreate file, pass wrong hash
        fs::write(&abs, content).expect("write");
        let result3 = server
            .delete_file(Parameters(DeleteFileParams {
                filepath: filepath.to_owned(),
                base_version: "sha256:wrong".to_owned(),
            }))
            .await;
        assert!(result3.is_err());
        let Err(err) = result3 else {
            panic!("expected error")
        };
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "VERSION_MISMATCH", "got: {err:?}");

        // ACCESS_DENIED — sandbox-protected path
        let result4 = server
            .delete_file(Parameters(DeleteFileParams {
                filepath: ".git/objects/x".to_owned(),
                base_version: "sha256:any".to_owned(),
            }))
            .await;
        assert!(result4.is_err());
        let Err(err) = result4 else {
            panic!("expected error")
        };
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "ACCESS_DENIED", "got: {err:?}");
    }

    // ── read_file tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_read_file_pagination() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);
        let server = PathfinderServer::with_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            Arc::new(MockSurgeon::new()),
        );

        // Write a 10-line file
        let filepath = "config.yaml";
        let lines: Vec<String> = (1..=10).map(|i| format!("line{i}: value")).collect();
        let content = lines.join("\n");
        fs::write(ws_dir.path().join(filepath), &content).expect("write");

        // Full read
        let result = server
            .read_file(Parameters(ReadFileParams {
                filepath: filepath.to_owned(),
                start_line: 1,
                max_lines: 500,
            }))
            .await
            .expect("should succeed");
        let val = result.0;
        assert_eq!(val.total_lines, 10);
        assert_eq!(val.lines_returned, 10);
        assert!(!val.truncated);
        assert_eq!(val.language, "yaml");

        // Paginated read — lines 3-5
        let result2 = server
            .read_file(Parameters(ReadFileParams {
                filepath: filepath.to_owned(),
                start_line: 3,
                max_lines: 3,
            }))
            .await
            .expect("should succeed");
        let val2 = result2.0;
        assert_eq!(val2.start_line, 3);
        assert_eq!(val2.lines_returned, 3);
        assert!(val2.truncated);
        assert!(val2.content.contains("line3"));
        assert!(val2.content.contains("line5"));
        assert!(!val2.content.contains("line6"));

        // FILE_NOT_FOUND
        let result3 = server
            .read_file(Parameters(ReadFileParams {
                filepath: "nonexistent.yaml".to_owned(),
                start_line: 1,
                max_lines: 500,
            }))
            .await;
        assert!(result3.is_err());
        let Err(err) = result3 else {
            panic!("expected error")
        };
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "FILE_NOT_FOUND", "got: {err:?}");
    }

    // ── read_symbol_scope tests ─────────────────────────────────────

    #[tokio::test]
    async fn test_read_symbol_scope_routes_to_surgeon_and_handles_success() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);
        let mock_surgeon = Arc::new(MockSurgeon::new());

        let content = "func Login() {}";
        let expected_scope = pathfinder_common::types::SymbolScope {
            content: content.to_owned(),
            start_line: 5,
            end_line: 7,
            version_hash: VersionHash::compute(content.as_bytes()),
            language: "go".to_owned(),
        };
        mock_surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(expected_scope.clone()));

        let server = PathfinderServer::with_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            mock_surgeon.clone(),
        );

        let params = ReadSymbolScopeParams {
            semantic_path: "src/auth.go::Login".to_owned(),
        };

        let result = server.read_symbol_scope(Parameters(params)).await;
        let val = result.expect("should succeed").0;

        assert_eq!(val.content, expected_scope.content);
        assert_eq!(val.start_line, expected_scope.start_line);
        assert_eq!(val.end_line, expected_scope.end_line);
        assert_eq!(val.version_hash, expected_scope.version_hash.as_str());
        assert_eq!(val.language, expected_scope.language);

        let calls = mock_surgeon.read_symbol_scope_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
    }

    #[tokio::test]
    async fn test_read_symbol_scope_handles_surgeon_error() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);
        let mock_surgeon = Arc::new(MockSurgeon::new());

        mock_surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Err(pathfinder_treesitter::SurgeonError::SymbolNotFound {
                path: "src/auth.go::Login".to_owned(),
                did_you_mean: vec!["Logout".to_owned()],
            }));

        let server = PathfinderServer::with_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            mock_surgeon,
        );

        let params = ReadSymbolScopeParams {
            semantic_path: "src/auth.go::Login".to_owned(),
        };

        let Err(err) = server.read_symbol_scope(Parameters(params)).await else {
            panic!("Expected failed response");
        };

        assert_eq!(err.code, ErrorCode::INTERNAL_ERROR); // All PathfinderErrors are mapped to INTERNAL_ERROR in helpers.rs
        let code = err
            .data
            .as_ref()
            .unwrap()
            .get("error")
            .unwrap()
            .as_str()
            .unwrap();
        assert_eq!(code, "SYMBOL_NOT_FOUND");
    }

    // ── write_file tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_write_file_full_replacement() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);
        let server = PathfinderServer::with_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            Arc::new(MockSurgeon::new()),
        );

        let filepath = "config.toml";
        let original = "[server]\nport = 8080";
        let abs = ws_dir.path().join(filepath);
        fs::write(&abs, original).expect("write");
        let hash = VersionHash::compute(original.as_bytes());

        // Happy path — full replacement
        let replacement = "[server]\nport = 9090";
        let result = server
            .write_file(Parameters(WriteFileParams {
                filepath: filepath.to_owned(),
                base_version: hash.as_str().to_owned(),
                content: Some(replacement.to_owned()),
                replacements: None,
            }))
            .await
            .expect("should succeed");
        let val = result.0;
        assert!(val.success);
        let on_disk = fs::read_to_string(&abs).expect("read");
        assert_eq!(on_disk, replacement);
        let new_hash = VersionHash::compute(replacement.as_bytes());
        assert_eq!(val.new_version_hash, new_hash.as_str());

        // VERSION_MISMATCH — use old hash
        let result2 = server
            .write_file(Parameters(WriteFileParams {
                filepath: filepath.to_owned(),
                base_version: hash.as_str().to_owned(), // stale
                content: Some("something else".to_owned()),
                replacements: None,
            }))
            .await;
        assert!(result2.is_err());
        let Err(err) = result2 else {
            panic!("expected error")
        };
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "VERSION_MISMATCH", "got: {err:?}");
    }

    #[tokio::test]
    async fn test_write_file_search_and_replace() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);
        let server = PathfinderServer::with_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            Arc::new(MockSurgeon::new()),
        );

        let filepath = "docker-compose.yml";
        let original = "image: postgres:15\nports:\n  - 5432:5432";
        let abs = ws_dir.path().join(filepath);
        fs::write(&abs, original).expect("write");
        let hash = VersionHash::compute(original.as_bytes());

        // Happy path — single match
        let result = server
            .write_file(Parameters(WriteFileParams {
                filepath: filepath.to_owned(),
                base_version: hash.as_str().to_owned(),
                content: None,
                replacements: Some(vec![Replacement {
                    old_text: "postgres:15".to_owned(),
                    new_text: "postgres:16-alpine".to_owned(),
                }]),
            }))
            .await
            .expect("should succeed");
        assert!(result.0.success);
        let on_disk = fs::read_to_string(&abs).expect("read");
        assert!(on_disk.contains("postgres:16-alpine"));
        let new_hash_val = result.0.new_version_hash;

        // MATCH_NOT_FOUND — old text no longer exists
        let result2 = server
            .write_file(Parameters(WriteFileParams {
                filepath: filepath.to_owned(),
                base_version: new_hash_val.clone(),
                content: None,
                replacements: Some(vec![Replacement {
                    old_text: "postgres:15".to_owned(), // already replaced
                    new_text: "postgres:17".to_owned(),
                }]),
            }))
            .await;
        assert!(result2.is_err());
        let Err(err) = result2 else {
            panic!("expected error")
        };
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "MATCH_NOT_FOUND", "got: {err:?}");

        // AMBIGUOUS_MATCH — inject a file where old_text appears twice
        let ambiguous = "tag: v1\ntag: v1";
        fs::write(&abs, ambiguous).expect("write");
        let ambig_hash = VersionHash::compute(ambiguous.as_bytes());
        let result3 = server
            .write_file(Parameters(WriteFileParams {
                filepath: filepath.to_owned(),
                base_version: ambig_hash.as_str().to_owned(),
                content: None,
                replacements: Some(vec![Replacement {
                    old_text: "tag: v1".to_owned(),
                    new_text: "tag: v2".to_owned(),
                }]),
            }))
            .await;
        assert!(result3.is_err());
        let Err(err) = result3 else {
            panic!("expected error")
        };
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "AMBIGUOUS_MATCH", "got: {err:?}");
    }
}
