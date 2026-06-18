//! `read` tool (batch mode) — batch read multiple files in a single call.

use crate::server::helpers::{
    invalid_params_error, io_error_data, language_from_path, serialize_metadata,
};
use crate::server::types::{FileResult, ReadFilesResponse, ReadParams};
use crate::server::PathfinderServer;
use rmcp::model::{CallToolResult, ErrorData};
use std::path::Path;
use tokio::task::JoinSet;

const READ_FILES_CONCURRENCY: usize = 5;

/// Source file extensions that get AST-based processing via `read_source_file_impl`.
///
/// Matches `SupportedLanguage::detect` in `crates/pathfinder-treesitter/src/language.rs`.
const SOURCE_FILE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "go", "py", "pyi", "vue", "js", "jsx", "mjs", "cjs", "java",
];

/// Check if a file path is a source file (eligible for AST-based processing).
fn is_source_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| SOURCE_FILE_EXTENSIONS.contains(&ext))
}

/// Truncate content to `max_lines_per_file`.
fn truncate_content(content: &str, max_lines: u32) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let end_idx = max_lines as usize;
    if lines.len() <= end_idx {
        content.to_string()
    } else {
        lines[..end_idx].join("\n")
    }
}

impl PathfinderServer {
    /// Consolidated `read` handler → delegates to `read_file_impl`,
    /// `read_source_file_impl`, or `read_files_impl`.
    ///
    /// Routing:
    /// - `paths` provided → batch mode → `read_files_impl`
    /// - `filepath` provided + source extension → `read_source_file_impl`
    /// - `filepath` provided + config extension → `read_file_impl`
    pub(crate) async fn read_impl(&self, params: ReadParams) -> Result<CallToolResult, ErrorData> {
        // Exactly one of filepath / paths must be set.
        match (&params.filepath, &params.paths) {
            (Some(_), Some(_)) => Err(invalid_params_error(
                "provide either `filepath` (single file) or `paths` (batch), not both",
            )),
            (None, None) => Err(invalid_params_error(
                "provide either `filepath` (single file) or `paths` (batch)",
            )),
            // Batch mode
            (None, Some(_)) => self.read_files_impl(params).await,
            // Single file
            (Some(filepath), None) => {
                if let Some(end) = params.end_line {
                    if end < params.start_line {
                        return Err(invalid_params_error("`end_line` must be >= `start_line`"));
                    }
                }
                if is_source_file(Path::new(filepath)) {
                    self.read_source_file_impl(params).await
                } else {
                    self.read_file_impl(params).await
                }
            }
        }
    }

    /// Core logic for the `read_files` batch tool.
    ///
    /// Reads multiple files in a single call with per-file error resilience.
    /// Files are processed sequentially to avoid file descriptor exhaustion.
    #[tracing::instrument(skip(self, params), fields(count = %params.paths.as_ref().map_or(0, std::vec::Vec::len)))]
    pub(crate) async fn read_files_impl(
        &self,
        params: ReadParams,
    ) -> Result<CallToolResult, ErrorData> {
        let start = std::time::Instant::now();

        tracing::info!(tool = "read_files", "read_files: start");

        let paths = params
            .paths
            .as_ref()
            .ok_or_else(|| io_error_data("paths must be provided"))?;

        if paths.is_empty() {
            tracing::warn!(tool = "read_files", "empty paths");
            return Err(ErrorData::invalid_params(
                "Must provide at least 1 file path",
                Some(serde_json::json!({
                    "provided": 0,
                    "min": 1,
                    "max": 10
                })),
            ));
        }

        if paths.len() > 10 {
            tracing::warn!(tool = "read_files", count = paths.len(), "too many paths");
            return Err(ErrorData::invalid_params(
                "Maximum 10 file paths allowed per call",
                Some(serde_json::json!({
                    "provided": paths.len(),
                    "max": 10
                })),
            ));
        }

        let mut indexed_results: Vec<(usize, FileResult)> = Vec::with_capacity(paths.len());
        let mut set = JoinSet::new();
        let mut spawned = 0;

        for (idx, file_path) in paths.iter().enumerate() {
            let server = self.clone();
            let file_path = file_path.clone();
            let params = params.clone();

            while spawned >= READ_FILES_CONCURRENCY {
                if let Some(res) = set.join_next().await {
                    spawned -= 1;
                    match res {
                        Ok((i, result)) => indexed_results.push((i, result)),
                        Err(e) => {
                            tracing::error!(tool = "read_files", error = %e, "spawned task panicked");
                        }
                    }
                }
            }

            set.spawn(async move { (idx, server.read_single_file(&file_path, &params).await) });
            spawned += 1;
        }

        while let Some(res) = set.join_next().await {
            match res {
                Ok((idx, result)) => indexed_results.push((idx, result)),
                Err(e) => tracing::error!(tool = "read_files", error = %e, "spawned task panicked"),
            }
        }

        indexed_results.sort_by_key(|(idx, _)| *idx);
        let file_results: Vec<FileResult> = indexed_results.into_iter().map(|(_, r)| r).collect();

        let (mut succeeded, mut failed) = (0, 0);
        for result in &file_results {
            if result.error.is_some() {
                failed += 1;
            } else {
                succeeded += 1;
            }
        }

        let duration_ms = start.elapsed().as_millis();
        tracing::info!(
            tool = "read_files",
            succeeded,
            failed,
            duration_ms,
            "read_files: complete"
        );

        let response = ReadFilesResponse {
            files: file_results,
            succeeded,
            failed,
            duration_ms: Some(u64::try_from(duration_ms).unwrap_or(u64::MAX)),
        };

        let mut text_parts: Vec<String> = Vec::new();
        for file in &response.files {
            if let Some(ref content) = file.content {
                text_parts.push(format!("--- {} ---", file.path));
                text_parts.push(content.clone());
            } else if let Some(ref error) = file.error {
                text_parts.push(format!("--- {} (error: {}) ---", file.path, error));
            }
        }
        text_parts.push(format!(
            "[completed in {}ms, {}/{} files read]",
            u64::try_from(duration_ms).unwrap_or(u64::MAX),
            response.succeeded,
            response.succeeded + response.failed
        ));

        let mut result =
            CallToolResult::success(vec![rmcp::model::Content::text(text_parts.join("\n"))]);
        result.structured_content = serialize_metadata(&response);
        Ok(result)
    }

    /// Read a single file and return its result.
    ///
    /// For source files (`.rs`, `.ts`, `.tsx`, `.go`, `.py`, `.pyi`, `.vue`, `.js`, `.jsx`, `.mjs`, `.cjs`, `.java`),
    /// this delegates to `read_source_file_impl` to get full content without symbols (for token efficiency).
    /// For config files and other files, reads raw content directly.
    #[allow(clippy::too_many_lines)]
    async fn read_single_file(&self, file_path: &str, params: &ReadParams) -> FileResult {
        let path = Path::new(file_path);

        if let Err(e) = self.sandbox.check(path) {
            return FileResult {
                path: file_path.to_string(),
                content: None,
                language: None,
                total_lines: None,
                version_hash: None,
                error: Some(format!("sandbox denied: {}", e.error_code())),
            };
        }

        let absolute_path = self.workspace_root.resolve(path);

        if is_source_file(path) {
            let rs_params = crate::server::types::ReadParams {
                filepath: Some(file_path.to_string()),
                paths: None,
                detail_level: params.detail_level.clone(),
                start_line: 1,
                end_line: None,
                max_lines_per_file: params.max_lines_per_file,
            };

            match self.read_source_file_impl(rs_params).await {
                Ok(result) => {
                    let structured_content_cloned = result.structured_content.clone();
                    let metadata = structured_content_cloned.and_then(|v| {
                        serde_json::from_value::<crate::server::types::ReadSourceFileMetadata>(v)
                            .ok()
                    });

                    // Prefer clean content from metadata (without timing line appended).
                    // Fall back to text output for backward compat.
                    let content = metadata
                        .as_ref()
                        .and_then(|m| m.content.clone())
                        .or_else(|| {
                            result.content.first().and_then(|c| match &c.raw {
                                rmcp::model::RawContent::Text(t) => Some(t.text.clone()),
                                _ => None,
                            })
                        });

                    let language = metadata.as_ref().map(|m| m.language.clone()).or_else(|| {
                        let lang = language_from_path(Path::new(file_path));
                        if lang == "text" {
                            None
                        } else {
                            Some(lang.to_owned())
                        }
                    });

                    let (content, total_lines, version_hash) = if let Some(content) = content {
                        // short() = 7-char hex; consistent with the explore tool's version_hashes format.
                        let version_hash =
                            pathfinder_common::types::VersionHash::compute(content.as_bytes())
                                .short()
                                .to_owned();
                        let truncated = truncate_content(&content, params.max_lines_per_file);
                        let total = u32::try_from(truncated.lines().count()).unwrap_or(u32::MAX);
                        (Some(truncated), Some(total), Some(version_hash))
                    } else {
                        (None, None, None)
                    };

                    FileResult {
                        path: file_path.to_string(),
                        content,
                        language,
                        total_lines,
                        version_hash,
                        error: None,
                    }
                }
                Err(e) => {
                    let error = e
                        .data
                        .as_ref()
                        .and_then(|d| d.get("error"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("failed to read file")
                        .to_string();

                    FileResult {
                        path: file_path.to_string(),
                        content: None,
                        language: None,
                        total_lines: None,
                        version_hash: None,
                        error: Some(error),
                    }
                }
            }
        } else {
            let content = match tokio::fs::read_to_string(&absolute_path).await {
                Ok(c) => c,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return FileResult {
                        path: file_path.to_string(),
                        content: None,
                        language: None,
                        total_lines: None,
                        version_hash: None,
                        error: Some("file not found".to_string()),
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                    return FileResult {
                        path: file_path.to_string(),
                        content: None,
                        language: None,
                        total_lines: None,
                        version_hash: None,
                        error: Some("file appears to be binary (not valid UTF-8)".to_string()),
                    }
                }
                Err(e) => {
                    return FileResult {
                        path: file_path.to_string(),
                        content: None,
                        language: None,
                        total_lines: None,
                        version_hash: None,
                        error: Some(format!("failed to read file: {e}")),
                    }
                }
            };

            // short() = 7-char hex; consistent with the explore tool's version_hashes format.
            let version_hash = pathfinder_common::types::VersionHash::compute(content.as_bytes())
                .short()
                .to_owned();
            let content = truncate_content(&content, params.max_lines_per_file);
            let total_lines = u32::try_from(content.lines().count()).unwrap_or(u32::MAX);
            let language = language_from_path(Path::new(file_path));
            let language = if language == "text" {
                String::new()
            } else {
                language.to_owned()
            };

            FileResult {
                path: file_path.to_string(),
                content: Some(content),
                language: Some(language),
                total_lines: Some(total_lines),
                version_hash: Some(version_hash),
                error: None,
            }
        }
    }
}

#[cfg(test)]
#[path = "read_files_test.rs"]
mod tests;
