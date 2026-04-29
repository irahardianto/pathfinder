//! File operation tools — `create_file`, `delete_file`, `read_file`, `write_file`.

use crate::server::helpers::{
    check_occ, io_error_data, language_from_path, pathfinder_to_error_data, serialize_metadata,
};
use crate::server::types::{
    CreateFileParams, CreateFileResponse, DeleteFileParams, DeleteFileResponse, ReadFileParams,
    Replacement, ValidationResult, WriteFileParams,
};
use crate::server::PathfinderServer;
use pathfinder_common::error::{compute_lines_changed, PathfinderError};
use pathfinder_common::types::VersionHash;
use pathfinder_lsp::types::{FileChangeType, FileEvent};
use rmcp::handler::server::wrapper::Json;
use rmcp::model::{CallToolResult, ErrorData};
use std::path::Path;
use tokio::fs as tfs;
use tokio::io::AsyncWriteExt as _;

// ── Pure helpers ──────────────────────────────────────────────────────────────

/// Apply a sequence of search-and-replace operations to `content`.
///
/// Each replacement must match the source text **exactly once** —
/// zero matches returns [`PathfinderError::MatchNotFound`],
/// more than one returns [`PathfinderError::AmbiguousMatch`].
fn apply_replacements(
    content: String,
    replacements: &[Replacement],
    relative_path: &Path,
) -> Result<String, PathfinderError> {
    let mut working = content;
    for replacement in replacements {
        let occurrences = working.matches(replacement.old_text.as_str()).count();
        match occurrences {
            0 => {
                return Err(PathfinderError::MatchNotFound {
                    filepath: relative_path.to_path_buf(),
                });
            }
            1 => {
                working = working.replacen(&replacement.old_text, &replacement.new_text, 1);
            }
            n => {
                return Err(PathfinderError::AmbiguousMatch {
                    filepath: relative_path.to_path_buf(),
                    occurrences: n,
                });
            }
        }
    }
    Ok(working)
}

// ── Tool handlers ─────────────────────────────────────────────────────────────

/// Validate file path and sandbox access. Returns early error if validation fails.
fn validate_file_path(
    sandbox: &pathfinder_common::sandbox::Sandbox,
    relative_path: &Path,
    tool_name: &str,
) -> Result<(), ErrorData> {
    if let Err(e) = sandbox.check(relative_path) {
        tracing::warn!(tool = tool_name, error = %e, "sandbox check failed");
        return Err(pathfinder_to_error_data(&e));
    }
    Ok(())
}

/// Prepare file for writing: validate sandbox, read content, check OCC.
/// Returns `(current_content, current_hash)` on success.
async fn prepare_file_write(
    sandbox: &pathfinder_common::sandbox::Sandbox,
    workspace_root: &pathfinder_common::types::WorkspaceRoot,
    relative_path: &Path,
    base_version: &str,
    tool_name: &str,
) -> Result<(String, VersionHash), ErrorData> {
    // Sandbox check
    validate_file_path(sandbox, relative_path, tool_name)?;

    // Read current content
    let absolute_path = workspace_root.resolve(relative_path);
    let current_content = match tfs::read_to_string(&absolute_path).await {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let err = PathfinderError::FileNotFound {
                path: relative_path.to_path_buf(),
            };
            tracing::warn!(tool = tool_name, error = %err, "file not found");
            return Err(pathfinder_to_error_data(&err));
        }
        Err(e) => {
            tracing::warn!(tool = tool_name, error = %e, "failed to read file");
            return Err(io_error_data(format!("failed to read file: {e}")));
        }
    };

    // OCC check — uses VersionHash::matches() under the hood, accepting
    // 7-char short hashes (preferred), sha256: prefixed, or full hashes.
    let current_hash = VersionHash::compute(current_content.as_bytes());
    check_occ(base_version, &current_hash, relative_path.to_path_buf())?;

    Ok((current_content, current_hash))
}

impl PathfinderServer {
    /// Core logic for the `create_file` tool.
    ///
    /// Sandbox-checks the target path, creates parent directories, then
    /// atomically writes the file with `O_CREAT | O_EXCL` (create-new).
    #[tracing::instrument(skip(self, params), fields(filepath = %params.filepath))]
    pub(crate) async fn create_file_impl(
        &self,
        params: CreateFileParams,
    ) -> Result<Json<CreateFileResponse>, ErrorData> {
        let start = std::time::Instant::now();
        let relative_path = Path::new(&params.filepath);
        let absolute_path = self.workspace_root.resolve(relative_path);

        tracing::info!(tool = "create_file", "create_file: start");

        // 1. Sandbox check
        validate_file_path(&self.sandbox, relative_path, "create_file")?;

        // 2. Create parent directories
        if let Some(parent) = absolute_path.parent() {
            if let Err(e) = tfs::create_dir_all(parent).await {
                tracing::warn!(tool = "create_file", error = %e, "failed to create parent directories");
                return Err(io_error_data(format!(
                    "failed to create parent directories: {e}"
                )));
            }
        }

        // 3. Atomically create file via tokio::fs::OpenOptions
        let io_start = std::time::Instant::now();
        let open_result = tfs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&absolute_path)
            .await;

        match open_result {
            Ok(mut file) => {
                if let Err(e) = file.write_all(params.content.as_bytes()).await {
                    tracing::warn!(tool = "create_file", error = %e, "failed to write file content");
                    return Err(io_error_data(format!("failed to write file content: {e}")));
                }
                if let Err(e) = file.flush().await {
                    return Err(io_error_data(format!("failed to flush file: {e}")));
                }
                if let Err(e) = file.sync_all().await {
                    return Err(io_error_data(format!("failed to sync file: {e}")));
                }

                // Broadcast file creation to LSP processes
                if let Ok(uri) = url::Url::from_file_path(&absolute_path) {
                    let event = FileEvent {
                        uri: uri.to_string(),
                        change_type: FileChangeType::Created,
                    };
                    if let Err(e) = self.lawyer.did_change_watched_files(vec![event]).await {
                        tracing::warn!(error = %e, "Failed to broadcast didChangeWatchedFiles on create");
                    }
                }

                // Evict the new file from the AST cache so any prior stale entry
                // (from a temp-path overlap) is cleared before the first read.
                self.surgeon.invalidate_cache(relative_path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let err = PathfinderError::FileAlreadyExists {
                    path: relative_path.to_path_buf(),
                };
                tracing::warn!(tool = "create_file", error = %err, "file already exists");
                return Err(pathfinder_to_error_data(&err));
            }
            Err(e) => {
                tracing::warn!(tool = "create_file", error = %e, "failed to create file");
                return Err(io_error_data(format!("failed to create file: {e}")));
            }
        }
        let io_ms = io_start.elapsed().as_millis();

        let version_hash = VersionHash::compute(params.content.as_bytes());
        let duration_ms = start.elapsed().as_millis();

        tracing::info!(
            tool = "create_file",
            version_hash = %version_hash.as_str(),
            io_ms,
            duration_ms,
            engines_used = ?(&[] as &[&str]),
            "create_file: complete"
        );

        Ok(Json(CreateFileResponse {
            success: true,
            version_hash: version_hash.short().to_owned(),
            validation: ValidationResult {
                status: "passed".to_owned(),
                introduced_errors: vec![],
            },
        }))
    }

    /// Core logic for the `delete_file` tool.
    ///
    /// Sandbox-checks, reads the file for OCC verification, deletes.
    /// The `.exists()` precheck is intentionally absent — `NotFound` from
    /// the read step maps directly to `FILE_NOT_FOUND`, eliminating the
    /// TOCTOU race between a precheck and the actual deletion.
    pub(crate) async fn delete_file_impl(
        &self,
        params: DeleteFileParams,
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
            tracing::warn!(tool = "delete_file", error = %e, "sandbox check failed");
            return Err(pathfinder_to_error_data(&e));
        }

        // 2. Read current content (also proves the file exists — no separate exists() check
        //    to avoid a TOCTOU race between the precheck and the deletion).
        let io_start = std::time::Instant::now();
        let current_content = match tfs::read(&absolute_path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let err = PathfinderError::FileNotFound {
                    path: relative_path.to_path_buf(),
                };
                tracing::warn!(tool = "delete_file", error = %err, "file not found");
                return Err(pathfinder_to_error_data(&err));
            }
            Err(e) => {
                tracing::warn!(tool = "delete_file", error = %e, "failed to read file");
                return Err(io_error_data(format!("failed to read file: {e}")));
            }
        };

        // 3. OCC check — accepts short hash (preferred), prefixed short, or full hash.
        let current_hash = VersionHash::compute(&current_content);
        check_occ(
            &params.base_version,
            &current_hash,
            relative_path.to_path_buf(),
        )?;

        // 4. Delete
        if let Err(e) = tfs::remove_file(&absolute_path).await {
            tracing::warn!(tool = "delete_file", error = %e, "failed to delete file");
            return Err(io_error_data(format!("failed to delete file: {e}")));
        }

        // Broadcast file deletion to LSP processes
        if let Ok(uri) = url::Url::from_file_path(&absolute_path) {
            let event = FileEvent {
                uri: uri.to_string(),
                change_type: FileChangeType::Deleted,
            };
            if let Err(e) = self.lawyer.did_change_watched_files(vec![event]).await {
                tracing::warn!(error = %e, "Failed to broadcast didChangeWatchedFiles on delete");
            }
        }

        let io_ms = io_start.elapsed().as_millis();

        let duration_ms = start.elapsed().as_millis();
        tracing::info!(
            tool = "delete_file",
            filepath = %params.filepath,
            io_ms,
            duration_ms,
            engines_used = ?(&[] as &[&str]),
            "delete_file: complete"
        );

        Ok(Json(DeleteFileResponse { success: true }))
    }

    /// Core logic for the `read_file` tool.
    ///
    /// Sandbox-checks, reads the file, paginates by `start_line`/`max_lines`,
    /// and computes a version hash for OCC on subsequent writes.
    pub(crate) async fn read_file_impl(
        &self,
        params: ReadFileParams,
    ) -> Result<CallToolResult, ErrorData> {
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
            tracing::warn!(tool = "read_file", error = %e, "sandbox check failed");
            return Err(pathfinder_to_error_data(&e));
        }

        // 2. Read file
        let io_start = std::time::Instant::now();
        let raw_content = match tfs::read_to_string(&absolute_path).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let err = PathfinderError::FileNotFound {
                    path: relative_path.to_path_buf(),
                };
                tracing::warn!(tool = "read_file", error = %err, "file not found");
                return Err(pathfinder_to_error_data(&err));
            }
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                tracing::warn!(
                    tool = "read_file",
                    path = %relative_path.display(),
                    "file contains invalid UTF-8 (likely binary)"
                );
                return Err(io_error_data(
                    "file appears to be binary (not valid UTF-8). read_file only supports text files."
                ));
            }
            Err(e) => {
                tracing::warn!(tool = "read_file", error = %e, "failed to read file");
                return Err(io_error_data(format!("failed to read file: {e}")));
            }
        };
        let io_ms = io_start.elapsed().as_millis();

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
            io_ms,
            duration_ms,
            engines_used = ?(&[] as &[&str]),
            "read_file: complete"
        );

        let metadata = crate::server::types::ReadFileMetadata {
            start_line: params.start_line,
            lines_returned,
            total_lines,
            truncated,
            version_hash: version_hash.short().to_owned(),
            language,
        };

        // Append version_hash footer to text content so agents can extract it
        let full_content = format!("{}\n---\nversion_hash: {}", content, version_hash.short());

        let mut res = CallToolResult::success(vec![rmcp::model::Content::text(full_content)]);
        res.structured_content = serialize_metadata(&metadata);
        Ok(res)
    }

    /// Core logic for the `write_file` tool.
    ///
    /// Supports two modes: full-content replacement and surgical search-and-replace
    /// (via [`apply_replacements`]). Includes OCC version checking with a late
    /// TOCTOU re-check before the write, and sandbox authorization.
    #[tracing::instrument(skip(self, params), fields(filepath = %params.filepath))]
    #[allow(clippy::too_many_lines)]
    pub(crate) async fn write_file_impl(
        &self,
        params: WriteFileParams,
    ) -> Result<CallToolResult, ErrorData> {
        let start = std::time::Instant::now();
        let relative_path = Path::new(&params.filepath);
        let absolute_path = self.workspace_root.resolve(relative_path);

        tracing::info!(
            tool = "write_file",
            mode = if params.content.is_some() {
                "full_replacement"
            } else {
                "search_and_replace"
            },
            "write_file: start"
        );

        // 1. Validate mutually exclusive modes
        match (&params.content, &params.replacements) {
            (None, None) | (Some(_), Some(_)) => {
                let e = "exactly one of 'content' or 'replacements' must be provided";
                tracing::warn!(tool = "write_file", error = %e, "invalid arguments");
                return Err(io_error_data(e));
            }
            _ => {}
        }

        // 2. Sandbox check, read file, and OCC check
        let (current_content, _current_hash) = prepare_file_write(
            &self.sandbox,
            &self.workspace_root,
            relative_path,
            &params.base_version,
            "write_file",
        )
        .await?;

        // 3. Compute new content (full replacement or search-and-replace)
        let new_content = if let Some(content) = params.content {
            content
        } else {
            // SAFETY: validated above that exactly one of content/replacements is Some.
            let replacements = params.replacements.unwrap_or_default();
            apply_replacements(current_content.clone(), &replacements, relative_path).map_err(
                |e| {
                    tracing::warn!(tool = "write_file", error = %e, "search_and_replace failed");
                    pathfinder_to_error_data(&e)
                },
            )?
        };

        // 4. TOCTOU late-check: re-read and re-hash immediately before write.
        //    At this point we still have the agent's prior content (`current_content`)
        //    and we read fresh disk content (`late_content`) — making this the only
        //    site where both versions are available to compute `lines_changed`.
        let late_content = match tfs::read(&absolute_path).await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(tool = "write_file", error = %e, "TOCTOU re-read failed");
                return Err(io_error_data(format!("TOCTOU re-read failed: {e}")));
            }
        };
        let late_hash = VersionHash::compute(&late_content);
        // TOCTOU check: use matches() to accept short hashes and enrich the
        // error with lines_changed (unavailable in the generic check_occ helper).
        if !late_hash.matches(&params.base_version) {
            let late_str = String::from_utf8_lossy(&late_content);
            let delta = compute_lines_changed(&current_content, &late_str);
            let err = PathfinderError::VersionMismatch {
                path: relative_path.to_path_buf(),
                current_version_hash: late_hash.as_str().to_owned(),
                lines_changed: Some(delta),
            };
            tracing::warn!(tool = "write_file", error = %err, "TOCTOU version mismatch");
            return Err(pathfinder_to_error_data(&err));
        }

        // 5. Write to disk (in-place: preserves inode for HMR/watchers)
        let io_start = std::time::Instant::now();
        if let Err(e) = tfs::write(&absolute_path, new_content.as_bytes()).await {
            tracing::warn!(tool = "write_file", error = %e, "failed to write file");
            return Err(io_error_data(format!("failed to write file: {e}")));
        }

        // Broadcast file change to LSP processes
        if let Ok(uri) = url::Url::from_file_path(&absolute_path) {
            let event = FileEvent {
                uri: uri.to_string(),
                change_type: FileChangeType::Changed,
            };
            if let Err(e) = self.lawyer.did_change_watched_files(vec![event]).await {
                tracing::warn!(error = %e, "Failed to broadcast didChangeWatchedFiles on write_file");
            }
        }

        // Evict file from AST cache — write_file can be called on source files
        // (e.g., Dockerfile, YAML) and a stale cache would return old content
        // to any follow-up read_source_file or read_symbol_scope call.
        self.surgeon.invalidate_cache(relative_path);
        let io_ms = io_start.elapsed().as_millis();

        let new_hash = VersionHash::compute(new_content.as_bytes());
        let duration_ms = start.elapsed().as_millis();

        tracing::info!(
            tool = "write_file",
            io_ms,
            duration_ms,
            engines_used = ?(&[] as &[&str]),
            "write_file: complete"
        );

        let metadata = crate::server::types::WriteFileMetadata {
            success: true,
            new_version_hash: new_hash.short().to_owned(),
        };

        let mut res = CallToolResult::success(vec![rmcp::model::Content::text(
            "File successfully written",
        )]);
        res.structured_content = serialize_metadata(&metadata);
        Ok(res)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::server::types::Replacement;
    use std::path::PathBuf;

    #[test]
    fn test_apply_replacements_success() {
        let content = "hello world\nthis is a test\nend".to_string();
        let replacements = vec![
            Replacement {
                old_text: "world".to_string(),
                new_text: "universe".to_string(),
            },
            Replacement {
                old_text: "is a test".to_string(),
                new_text: "was a success".to_string(),
            },
        ];
        let path = PathBuf::from("test.txt");

        let result = apply_replacements(content, &replacements, &path).expect("should succeed");
        assert_eq!(result, "hello universe\nthis was a success\nend");
    }

    #[test]
    fn test_apply_replacements_match_not_found() {
        let content = "hello world".to_string();
        let replacements = vec![Replacement {
            old_text: "missing".to_string(),
            new_text: "found".to_string(),
        }];
        let path = PathBuf::from("test.txt");

        let result = apply_replacements(content, &replacements, &path);
        assert!(matches!(result, Err(PathfinderError::MatchNotFound { .. })));
    }

    #[test]
    fn test_apply_replacements_ambiguous_match() {
        let content = "hello value and another value".to_string();
        let replacements = vec![Replacement {
            old_text: "value".to_string(),
            new_text: "item".to_string(),
        }];
        let path = PathBuf::from("test.txt");

        let result = apply_replacements(content, &replacements, &path);
        match result {
            Err(PathfinderError::AmbiguousMatch { occurrences, .. }) => {
                assert_eq!(occurrences, 2);
            }
            _ => panic!("expected AmbiguousMatch error"),
        }
    }
}
