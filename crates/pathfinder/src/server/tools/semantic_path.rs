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
    pub(crate) async fn get_semantic_path_impl(
        &self,
        params: LocateParams,
    ) -> Result<CallToolResult, ErrorData> {
        let start = std::time::Instant::now();
        let file = params
            .file
            .as_ref()
            .ok_or_else(|| rmcp::model::ErrorData::invalid_params("file is required", None))?;
        let line = params
            .line
            .ok_or_else(|| rmcp::model::ErrorData::invalid_params("line is required", None))?;

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
            let err = pathfinder_common::error::PathfinderError::FileNotFound {
                path: abs_path.clone(),
            };
            tracing::warn!(
                tool = "get_semantic_path",
                path = %abs_path.display(),
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
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::WorkspaceRoot;
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::mock::MockSurgeon;
    use std::sync::Arc;

    fn make_server(ws: WorkspaceRoot, mock_surgeon: MockSurgeon) -> PathfinderServer {
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);
        PathfinderServer::with_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            Arc::new(mock_surgeon),
        )
    }

    #[tokio::test]
    async fn test_get_semantic_path_found() {
        let ws_dir = tempfile::tempdir().expect("temp dir");
        // Create a real file so the existence check passes.
        let file_rel = "src/auth.rs";
        let file_abs = ws_dir.path().join(file_rel);
        std::fs::create_dir_all(file_abs.parent().unwrap()).expect("create dir");
        std::fs::write(&file_abs, "fn login() {}").expect("write file");

        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let mock_surgeon = MockSurgeon::new();
        mock_surgeon
            .enclosing_symbol_results
            .lock()
            .unwrap()
            .push(Ok(Some("login".to_owned())));

        let server = make_server(ws, mock_surgeon);
        let params = LocateParams {
            file: Some(file_rel.to_owned()),
            line: Some(1),
            ..Default::default()
        };

        let result = server.get_semantic_path_impl(params).await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());
        let call = result.unwrap();

        // Verify structured content has semantic_path
        let meta: GetSemanticPathResult =
            serde_json::from_value(call.structured_content.unwrap()).unwrap();
        assert_eq!(meta.semantic_path, Some("src/auth.rs::login".to_owned()));
        assert_eq!(meta.symbol, Some("login".to_owned()));
        assert_eq!(meta.line, 1);
    }

    #[tokio::test]
    async fn test_get_semantic_path_not_in_symbol() {
        let ws_dir = tempfile::tempdir().expect("temp dir");
        let file_rel = "src/lib.rs";
        let file_abs = ws_dir.path().join(file_rel);
        std::fs::create_dir_all(file_abs.parent().unwrap()).expect("create dir");
        std::fs::write(&file_abs, "use std::io;").expect("write file");

        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let mock_surgeon = MockSurgeon::new();
        // None = line is not inside any named symbol
        mock_surgeon
            .enclosing_symbol_results
            .lock()
            .unwrap()
            .push(Ok(None));

        let server = make_server(ws, mock_surgeon);
        let params = LocateParams {
            file: Some(file_rel.to_owned()),
            line: Some(1),
            ..Default::default()
        };

        let result = server.get_semantic_path_impl(params).await;
        assert!(result.is_ok());
        let call = result.unwrap();

        let meta: GetSemanticPathResult =
            serde_json::from_value(call.structured_content.unwrap()).unwrap();
        assert!(meta.semantic_path.is_none());
        assert!(meta.symbol.is_none());
    }

    #[tokio::test]
    async fn test_get_semantic_path_file_not_found() {
        let ws_dir = tempfile::tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let mock_surgeon = MockSurgeon::new();

        let server = make_server(ws, mock_surgeon);
        let params = LocateParams {
            file: Some("nonexistent.rs".to_owned()),
            line: Some(5),
            ..Default::default()
        };

        let result = server.get_semantic_path_impl(params).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        // Should be INVALID_PARAMS (-32602) for a missing file
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn test_get_semantic_path_sandbox_denied() {
        let ws_dir = tempfile::tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let mock_surgeon = MockSurgeon::new();

        let server = make_server(ws, mock_surgeon);
        // .env is a hardcoded sandbox deny
        let params = LocateParams {
            file: Some(".env".to_owned()),
            line: Some(1),
            ..Default::default()
        };

        let result = server.get_semantic_path_impl(params).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode(-32001));
    }
}
