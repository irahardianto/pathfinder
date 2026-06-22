//! `locate` tool handler (`get_semantic_path` mode).
//!
//! Resolves a file path + 1-indexed line number to the semantic path of the
//! innermost enclosing symbol (e.g. `src/auth.rs::AuthService.login`).
//!
//! This is the reverse of the `inspect` tool. Agents that receive
//! a stack trace, diff hunk, or LSP location (file + line) can use this tool
//! to obtain the semantic path they need to call other Pathfinder tools.

use crate::server::helpers::{pathfinder_to_error_data, serialize_metadata};
use crate::server::types::{GetSemanticPathResult, LocateParams};
use crate::server::PathfinderServer;
use rmcp::model::{CallToolResult, ErrorData};

impl PathfinderServer {
    /// Core logic for the `locate` tool (file+line → semantic path mode).
    ///
    /// Walks the Tree-sitter AST of the file and returns the
    /// semantic path of the symbol that encloses the line.
    #[expect(
        clippy::too_many_lines,
        reason = "Sequential validation pipeline: params → sandbox → file existence → tree-sitter. Extraction would fragment the linear flow."
    )]
    pub(crate) async fn get_semantic_path_impl(
        &self,
        params: LocateParams,
    ) -> Result<CallToolResult, ErrorData> {
        let start = std::time::Instant::now();
        let file = params
            .file
            .as_ref()
            .ok_or_else(|| rmcp::model::ErrorData::invalid_params("file is required", None))?;
        if file.is_empty() {
            return Err(rmcp::model::ErrorData::invalid_params(
                "file must not be empty",
                None,
            ));
        }
        let line = params
            .line
            .ok_or_else(|| rmcp::model::ErrorData::invalid_params("line is required", None))?;
        if line == 0 {
            return Err(rmcp::model::ErrorData::invalid_params(
                "line must be >= 1 (1-indexed)",
                None,
            ));
        }

        tracing::info!(
            tool = "get_semantic_path",
            file = %file,
            line = line,
            "get_semantic_path: start"
        );

        // Sandbox check — prevent path traversal to sensitive files.
        if let Err(e) = self.sandbox.check(std::path::Path::new(file)) {
            let duration_ms = start.elapsed().as_millis();
            tracing::warn!(
                tool = "get_semantic_path",
                error_code = e.error_code(),
                duration_ms,
                "sandbox check failed"
            );
            return Err(pathfinder_to_error_data(&e));
        }

        // Verify the file exists before calling Tree-sitter.
        let abs_path = self.workspace_root.path().join(file);
        if !abs_path.exists() {
            let err = pathfinder_common::error::PathfinderError::FileNotFound { path: abs_path };
            tracing::warn!(
                tool = "get_semantic_path",
                path = %file,
                "file not found"
            );
            return Err(pathfinder_to_error_data(&err));
        }

        // Delegate to Tree-sitter surgeon to find the enclosing symbol.
        let line_idx = line as usize;
        let symbol_result = self
            .surgeon
            .enclosing_symbol(
                self.workspace_root.path(),
                std::path::Path::new(file),
                line_idx,
            )
            .await;

        let symbol = match symbol_result {
            Ok(sym) => sym,
            Err(e) => {
                let duration_ms = start.elapsed().as_millis();
                tracing::warn!(
                    tool = "get_semantic_path",
                    file = %file,
                    line = line_idx,
                    error = %e,
                    duration_ms,
                    "enclosing_symbol failed"
                );
                return Err(crate::server::helpers::treesitter_error_to_error_data(e));
            }
        };

        let duration_ms = start.elapsed().as_millis();

        let semantic_path = symbol.as_deref().map(|sym| format!("{file}::{sym}"));

        let result = GetSemanticPathResult {
            semantic_path: semantic_path.clone(),
            symbol: symbol.clone(),
            file: file.clone(),
            line,
        };

        let text = match &semantic_path {
            Some(sp) => format!("{sp}\n\n[resolved in {duration_ms}ms]"),
            None => format!(
                "Line {line} in '{file}' is not inside a named symbol.\n\n\
                 The line may be a module-level attribute, blank line, or top-level import. \
                 Use `read(filepath=\"{file}\", detail_level=\"symbols\")` to see \
                 the available symbols in this file.\n\n\
                 [resolved in {duration_ms}ms]",
            ),
        };

        tracing::info!(
            tool = "get_semantic_path",
            file = %file,
            line = line_idx,
            semantic_path = ?semantic_path,
            duration_ms,
            "get_semantic_path: complete"
        );

        let mut call_result = CallToolResult::success(vec![rmcp::model::Content::text(text)]);
        call_result.structured_content = serialize_metadata(&result);
        Ok(call_result)
    }
}

#[cfg(test)]
#[path = "semantic_path_test.rs"]
mod tests;
