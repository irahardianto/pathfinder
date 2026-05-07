//! File operation tools — `read_file`.

use crate::server::helpers::{
    io_error_data, language_from_path, pathfinder_to_error_data, serialize_metadata,
};
use crate::server::types::ReadFileParams;
use crate::server::PathfinderServer;
use pathfinder_common::error::PathfinderError;
use rmcp::model::{CallToolResult, ErrorData};
use std::path::Path;
use tokio::fs as tfs;

// ── Tool handlers ─────────────────────────────────────────────────────────────

impl PathfinderServer {
    /// Core logic for the `read_file` tool.
    ///
    /// Sandbox-checks, reads the file, and paginates by `start_line`/`max_lines`.
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
                    "file appears to be binary (not valid UTF-8). read_file only supports text files.",
                ));
            }
            Err(e) => {
                tracing::warn!(tool = "read_file", error = %e, "failed to read file");
                return Err(io_error_data(format!("failed to read file: {e}")));
            }
        };
        let io_ms = io_start.elapsed().as_millis();

        // 3. Paginate — start_line is 1-indexed
        let all_lines: Vec<&str> = raw_content.lines().collect();
        let total_lines = u32::try_from(all_lines.len()).unwrap_or(u32::MAX);
        let file_size_bytes = u64::try_from(raw_content.len()).unwrap_or(u64::MAX);
        let start_idx = params.start_line.saturating_sub(1) as usize;
        let end_idx = (start_idx + params.max_lines as usize).min(all_lines.len());
        let page_lines = &all_lines[start_idx..end_idx];
        let lines_returned = u32::try_from(page_lines.len()).unwrap_or(u32::MAX);
        let truncated = end_idx < all_lines.len();
        // Prefix each line with its 1-indexed line number so agents can reference
        // locations without a follow-up call. Width is padded to align columns.
        let content: String = page_lines
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:>6} | {}", start_idx + i + 1, line))
            .collect::<Vec<_>>()
            .join("\n");

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
            file_size_bytes,
            truncated,
            language,
        };

        let mut res = CallToolResult::success(vec![rmcp::model::Content::text(content)]);
        res.structured_content = serialize_metadata(&metadata);
        Ok(res)
    }
}
