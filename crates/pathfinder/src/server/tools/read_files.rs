//! `read_files` tool — batch read multiple files in a single call.

use crate::server::helpers::{language_from_path, serialize_metadata};
use crate::server::types::{FileResult, ReadFilesParams, ReadFilesResponse};
use crate::server::PathfinderServer;
use rmcp::model::{CallToolResult, ErrorData};
use std::path::Path;
use tokio::task::JoinSet;

const READ_FILES_CONCURRENCY: usize = 5;

/// Source file extensions that get AST-based processing via `read_source_file`.
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
    /// Core logic for the `read_files` batch tool.
    ///
    /// Reads multiple files in a single call with per-file error resilience.
    /// Files are processed sequentially to avoid file descriptor exhaustion.
    #[tracing::instrument(skip(self, params), fields(count = %params.paths.len()))]
    pub(crate) async fn read_files_impl(
        &self,
        params: ReadFilesParams,
    ) -> Result<CallToolResult, ErrorData> {
        let start = std::time::Instant::now();

        tracing::info!(tool = "read_files", "read_files: start");

        if params.paths.is_empty() {
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

        if params.paths.len() > 10 {
            tracing::warn!(
                tool = "read_files",
                count = params.paths.len(),
                "too many paths"
            );
            return Err(ErrorData::invalid_params(
                "Maximum 10 file paths allowed per call",
                Some(serde_json::json!({
                    "provided": params.paths.len(),
                    "max": 10
                })),
            ));
        }

        let mut set = JoinSet::new();
        let mut spawned = 0;

        for (idx, file_path) in params.paths.iter().enumerate() {
            let server = self.clone();
            let file_path = file_path.clone();
            let params = params.clone();

            while spawned >= READ_FILES_CONCURRENCY {
                if let Some(res) = set.join_next().await {
                    spawned -= 1;
                    if let Err(e) = res {
                        tracing::error!(tool = "read_files", error = %e, "spawned task panicked");
                    }
                }
            }

            set.spawn(async move { (idx, server.read_single_file(&file_path, &params).await) });
            spawned += 1;
        }

        let mut indexed_results: Vec<(usize, FileResult)> = Vec::with_capacity(params.paths.len());
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
        let mut result = CallToolResult::success(vec![rmcp::model::Content::text(format!(
            "[completed in {}ms]",
            u64::try_from(duration_ms).unwrap_or(u64::MAX)
        ))]);
        result.structured_content = serialize_metadata(&response);
        Ok(result)
    }

    /// Read a single file and return its result.
    ///
    /// For source files (`.rs`, `.ts`, `.tsx`, `.go`, `.py`, `.pyi`, `.vue`, `.js`, `.jsx`, `.mjs`, `.cjs`, `.java`),
    /// this delegates to `read_source_file_impl` to get full content without symbols (for token efficiency).
    /// For config files and other files, reads raw content directly.
    #[allow(clippy::too_many_lines)]
    async fn read_single_file(&self, file_path: &str, params: &ReadFilesParams) -> FileResult {
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
            let rs_params = crate::server::types::ReadSourceFileParams {
                filepath: file_path.to_string(),
                start_line: 1,
                end_line: None,
                detail_level: params.detail_level.clone(),
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
                            Some(lang)
                        }
                    });

                    let (content, total_lines, version_hash) = if let Some(content) = content {
                        let version_hash =
                            pathfinder_common::types::VersionHash::compute(content.as_bytes())
                                .as_str()
                                .to_string();
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

            let version_hash = pathfinder_common::types::VersionHash::compute(content.as_bytes())
                .as_str()
                .to_string();
            let content = truncate_content(&content, params.max_lines_per_file);
            let total_lines = u32::try_from(content.lines().count()).unwrap_or(u32::MAX);
            let language = language_from_path(Path::new(file_path));
            let language = if language == "text" {
                String::new()
            } else {
                language
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
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::WorkspaceRoot;
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::mock::MockSurgeon;
    use std::fs;
    use std::sync::Arc;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_read_files_happy_path() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let mock_surgeon = Arc::new(MockSurgeon::new());
        let content1 = "fn main() {}\nfn test() {}";
        let content2 = "const x = 1;\nconst y = 2;";
        let content3 = "def foo():\n    pass";

        fs::write(ws_dir.path().join("file1.rs"), content1).expect("write");
        fs::write(ws_dir.path().join("file2.ts"), content2).expect("write");
        fs::write(ws_dir.path().join("file3.py"), content3).expect("write");

        mock_surgeon
            .read_source_file_results
            .lock()
            .unwrap()
            .push(Ok((content1.to_string(), "rust".to_string(), vec![])));
        mock_surgeon
            .read_source_file_results
            .lock()
            .unwrap()
            .push(Ok((content2.to_string(), "typescript".to_string(), vec![])));
        mock_surgeon
            .read_source_file_results
            .lock()
            .unwrap()
            .push(Ok((content3.to_string(), "python".to_string(), vec![])));

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            mock_surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params = ReadFilesParams {
            paths: vec![
                "file1.rs".to_string(),
                "file2.ts".to_string(),
                "file3.py".to_string(),
            ],
            detail_level: "source_only".to_string(),
            max_lines_per_file: 500,
        };

        let result = server
            .read_files_impl(params)
            .await
            .expect("should succeed");
        let response: ReadFilesResponse =
            serde_json::from_value(result.structured_content.unwrap()).unwrap();

        assert_eq!(response.files.len(), 3);
        assert_eq!(response.succeeded, 3);
        assert_eq!(response.failed, 0);

        for file in &response.files {
            assert!(file.content.is_some());
            assert!(file.language.is_some());
            assert!(file.total_lines.is_some());
            assert!(file.version_hash.is_some());
            assert!(file.error.is_none());
        }
    }

    #[tokio::test]
    async fn test_read_files_partial_failure() {
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

        fs::write(ws_dir.path().join("valid1.txt"), "content1").expect("write");
        fs::write(ws_dir.path().join("valid2.txt"), "content2").expect("write");

        let params = ReadFilesParams {
            paths: vec![
                "valid1.txt".to_string(),
                "valid2.txt".to_string(),
                "missing.txt".to_string(),
            ],
            detail_level: "source_only".to_string(),
            max_lines_per_file: 500,
        };

        let result = server
            .read_files_impl(params)
            .await
            .expect("should succeed");
        let response: ReadFilesResponse =
            serde_json::from_value(result.structured_content.unwrap()).unwrap();

        assert_eq!(response.files.len(), 3);
        assert_eq!(response.succeeded, 2);
        assert_eq!(response.failed, 1);

        assert!(response.files[0].content.is_some());
        assert!(response.files[0].error.is_none());
        assert!(response.files[1].content.is_some());
        assert!(response.files[1].error.is_none());

        assert!(response.files[2].content.is_none());
        assert!(response.files[2].error.is_some());
        assert_eq!(response.files[2].error.as_ref().unwrap(), "file not found");
    }

    #[tokio::test]
    async fn test_read_files_sandbox_denial() {
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

        let params = ReadFilesParams {
            paths: vec![".git/HEAD".to_string()],
            detail_level: "source_only".to_string(),
            max_lines_per_file: 500,
        };

        let result = server
            .read_files_impl(params)
            .await
            .expect("should succeed");
        let response: ReadFilesResponse =
            serde_json::from_value(result.structured_content.unwrap()).unwrap();

        assert_eq!(response.files.len(), 1);
        assert_eq!(response.succeeded, 0);
        assert_eq!(response.failed, 1);
        assert!(response.files[0].content.is_none());
        assert!(response.files[0].error.is_some());
        assert!(response.files[0]
            .error
            .as_ref()
            .unwrap()
            .contains("ACCESS_DENIED"));
    }

    #[tokio::test]
    async fn test_read_files_max_limit() {
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

        let params = ReadFilesParams {
            paths: (0..11).map(|i| format!("file{i}.txt")).collect(),
            detail_level: "source_only".to_string(),
            max_lines_per_file: 500,
        };

        let result = server.read_files_impl(params).await;
        assert!(result.is_err(), "should fail with >10 paths");
        let err = result.unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn test_read_files_empty_paths() {
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

        let params = ReadFilesParams {
            paths: vec![],
            detail_level: "source_only".to_string(),
            max_lines_per_file: 500,
        };

        let result = server.read_files_impl(params).await;

        // Empty paths should error (1-10 paths required by spec)
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn test_read_files_mixed_source_and_config() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let mock_surgeon = Arc::new(MockSurgeon::new());
        let rust_content = "fn main() {}";
        fs::write(ws_dir.path().join("main.rs"), rust_content).expect("write");
        fs::write(
            ws_dir.path().join("config.toml"),
            "[settings]\nkey = \"value\"",
        )
        .expect("write");

        mock_surgeon
            .read_source_file_results
            .lock()
            .unwrap()
            .push(Ok((rust_content.to_string(), "rust".to_string(), vec![])));

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            mock_surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params = ReadFilesParams {
            paths: vec!["main.rs".to_string(), "config.toml".to_string()],
            detail_level: "source_only".to_string(),
            max_lines_per_file: 500,
        };

        let result = server
            .read_files_impl(params)
            .await
            .expect("should succeed");
        let response: ReadFilesResponse =
            serde_json::from_value(result.structured_content.unwrap()).unwrap();

        assert_eq!(response.files.len(), 2);
        assert_eq!(response.succeeded, 2);

        let source_file = response.files.iter().find(|f| f.path == "main.rs").unwrap();
        assert_eq!(source_file.language.as_deref(), Some("rust"));

        let config_file = response
            .files
            .iter()
            .find(|f| f.path == "config.toml")
            .unwrap();
        assert_eq!(config_file.language.as_deref(), Some("toml"));
    }

    #[tokio::test]
    async fn test_read_files_version_hash_format() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let mock_surgeon = Arc::new(MockSurgeon::new());
        let content = "fn main() {}\n";
        fs::write(ws_dir.path().join("test.rs"), content).expect("write");
        mock_surgeon
            .read_source_file_results
            .lock()
            .unwrap()
            .push(Ok((content.to_string(), "rust".to_string(), vec![])));

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            mock_surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params1 = ReadFilesParams {
            paths: vec!["test.rs".to_string()],
            detail_level: "source_only".to_string(),
            max_lines_per_file: 500,
        };
        let result1 = server
            .read_files_impl(params1)
            .await
            .expect("should succeed");
        let response1: ReadFilesResponse =
            serde_json::from_value(result1.structured_content.unwrap()).unwrap();
        let hash1 = response1.files[0].version_hash.as_ref().unwrap().clone();

        assert!(
            hash1.starts_with("sha256:"),
            "hash should start with 'sha256:'"
        );
        let hex_part = &hash1[7..];
        assert_eq!(
            hex_part.len(),
            64,
            "hash should have 64 hex chars after prefix"
        );
        assert!(
            hex_part.chars().all(|c| c.is_ascii_hexdigit()),
            "hash should be hex only"
        );

        let binding = pathfinder_common::types::VersionHash::compute(content.as_bytes());
        let expected_hash = binding.as_str();
        assert_eq!(hash1, expected_hash, "hash should match computed SHA-256");
    }

    #[tokio::test]
    async fn test_read_files_max_lines_per_file_truncation() {
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

        let lines: Vec<String> = (1..=10).map(|i| format!("line{i}")).collect();
        let content = lines.join("\n");
        fs::write(ws_dir.path().join("test.txt"), &content).expect("write");

        let params = ReadFilesParams {
            paths: vec!["test.txt".to_string()],
            detail_level: "source_only".to_string(),
            max_lines_per_file: 3,
        };

        let result = server
            .read_files_impl(params)
            .await
            .expect("should succeed");
        let response: ReadFilesResponse =
            serde_json::from_value(result.structured_content.unwrap()).unwrap();

        assert_eq!(response.succeeded, 1);
        let file_result = &response.files[0];

        let content_lines: Vec<&str> = file_result.content.as_ref().unwrap().lines().collect();
        assert_eq!(content_lines.len(), 3);
        assert_eq!(file_result.total_lines.unwrap(), 3);
        assert_eq!(content_lines[0], "line1");
        assert_eq!(content_lines[2], "line3");
    }

    #[test]
    fn test_is_source_file() {
        assert!(is_source_file(Path::new("test.rs")));
        assert!(is_source_file(Path::new("test.ts")));
        assert!(is_source_file(Path::new("test.tsx")));
        assert!(is_source_file(Path::new("test.go")));
        assert!(is_source_file(Path::new("test.py")));
        assert!(is_source_file(Path::new("test.pyi")));
        assert!(is_source_file(Path::new("test.vue")));
        assert!(is_source_file(Path::new("test.jsx")));
        assert!(is_source_file(Path::new("test.js")));
        assert!(is_source_file(Path::new("test.mjs")));
        assert!(is_source_file(Path::new("test.cjs")));
        assert!(is_source_file(Path::new("test.java")));

        assert!(!is_source_file(Path::new("test.json")));
        assert!(!is_source_file(Path::new("test.toml")));
        assert!(!is_source_file(Path::new("test.yaml")));
        assert!(!is_source_file(Path::new("test.yml")));
        assert!(!is_source_file(Path::new("test.txt")));
        assert!(!is_source_file(Path::new("test.md")));
        assert!(!is_source_file(Path::new("Dockerfile")));
        assert!(!is_source_file(Path::new("Makefile")));
        assert!(!is_source_file(Path::new(".gitignore")));
    }

    #[test]
    fn test_truncate_content() {
        let content = "line1\nline2\nline3\nline4\nline5";
        assert_eq!(truncate_content(content, 3), "line1\nline2\nline3");
        assert_eq!(truncate_content(content, 10), content);
        assert_eq!(truncate_content("", 5), "");
    }

    #[tokio::test]
    async fn test_read_files_empty_file() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let mock_surgeon = Arc::new(MockSurgeon::new());
        let content = "";
        fs::write(ws_dir.path().join("empty.rs"), content).expect("write");
        mock_surgeon
            .read_source_file_results
            .lock()
            .unwrap()
            .push(Ok((content.to_string(), "rust".to_string(), vec![])));

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            mock_surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params = ReadFilesParams {
            paths: vec!["empty.rs".to_string()],
            detail_level: "source_only".to_string(),
            max_lines_per_file: 500,
        };

        let result = server
            .read_files_impl(params)
            .await
            .expect("should succeed");
        let response: ReadFilesResponse =
            serde_json::from_value(result.structured_content.unwrap()).unwrap();

        assert_eq!(response.succeeded, 1);
        let file_result = &response.files[0];
        assert_eq!(file_result.content, Some(String::new()));
        assert_eq!(file_result.total_lines, Some(0));
        assert!(file_result.version_hash.is_some());
    }

    #[tokio::test]
    async fn test_read_files_binary_file() {
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

        fs::write(
            ws_dir.path().join("binary.bin"),
            b"\x00\x01\x02\x03\xFF\xFE",
        )
        .expect("write");

        let params = ReadFilesParams {
            paths: vec!["binary.bin".to_string()],
            detail_level: "source_only".to_string(),
            max_lines_per_file: 500,
        };

        let result = server
            .read_files_impl(params)
            .await
            .expect("should succeed");
        let response: ReadFilesResponse =
            serde_json::from_value(result.structured_content.unwrap()).unwrap();

        assert_eq!(response.succeeded, 0);
        assert_eq!(response.failed, 1);
        assert!(response.files[0].content.is_none());
        assert!(response.files[0].error.is_some());
        assert!(response.files[0].error.as_ref().unwrap().contains("binary"));
    }

    #[tokio::test]
    async fn test_read_files_duplicate_paths() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let mock_surgeon = Arc::new(MockSurgeon::new());
        let content = "fn main() {}";
        fs::write(ws_dir.path().join("test.rs"), content).expect("write");
        mock_surgeon
            .read_source_file_results
            .lock()
            .unwrap()
            .push(Ok((content.to_string(), "rust".to_string(), vec![])));
        mock_surgeon
            .read_source_file_results
            .lock()
            .unwrap()
            .push(Ok((content.to_string(), "rust".to_string(), vec![])));

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            mock_surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params = ReadFilesParams {
            paths: vec!["test.rs".to_string(), "test.rs".to_string()],
            detail_level: "source_only".to_string(),
            max_lines_per_file: 500,
        };

        let result = server
            .read_files_impl(params)
            .await
            .expect("should succeed");
        let response: ReadFilesResponse =
            serde_json::from_value(result.structured_content.unwrap()).unwrap();

        assert_eq!(response.files.len(), 2);
        assert_eq!(response.succeeded, 2);

        assert_eq!(response.files[0].content, response.files[1].content);
        assert_eq!(
            response.files[0].version_hash,
            response.files[1].version_hash
        );
    }

    #[tokio::test]
    async fn test_read_files_mjs_cjs_extensions() {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        let mock_surgeon = Arc::new(MockSurgeon::new());
        mock_surgeon
            .read_source_file_results
            .lock()
            .unwrap()
            .push(Ok((
                "export default {}".to_string(),
                "javascript".to_string(),
                vec![],
            )));
        mock_surgeon
            .read_source_file_results
            .lock()
            .unwrap()
            .push(Ok((
                "module.exports = {}".to_string(),
                "javascript".to_string(),
                vec![],
            )));

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            mock_surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        fs::write(ws_dir.path().join("test.mjs"), "export default {}").expect("write");
        fs::write(ws_dir.path().join("test.cjs"), "module.exports = {}").expect("write");

        let params = ReadFilesParams {
            paths: vec!["test.mjs".to_string(), "test.cjs".to_string()],
            detail_level: "source_only".to_string(),
            max_lines_per_file: 500,
        };

        let result = server
            .read_files_impl(params)
            .await
            .expect("should succeed");
        let response: ReadFilesResponse =
            serde_json::from_value(result.structured_content.unwrap()).unwrap();

        assert_eq!(response.succeeded, 2);
        assert!(response.files[0].language.is_some());
        assert!(response.files[1].language.is_some());
    }

    #[tokio::test]
    async fn test_read_files_binary_file_png_header() {
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

        // PNG header bytes
        let png_header = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01";
        fs::write(ws_dir.path().join("image.png"), png_header).expect("write");

        let params = ReadFilesParams {
            paths: vec!["image.png".to_string()],
            detail_level: "source_only".to_string(),
            max_lines_per_file: 500,
        };

        let result = server
            .read_files_impl(params)
            .await
            .expect("should succeed");
        let response: ReadFilesResponse =
            serde_json::from_value(result.structured_content.unwrap()).unwrap();

        assert_eq!(response.succeeded, 0);
        assert_eq!(response.failed, 1);
        assert!(response.files[0].content.is_none());
        assert!(response.files[0].error.is_some());
        assert!(response.files[0].error.as_ref().unwrap().contains("binary"));
    }

    #[tokio::test]
    async fn test_read_files_version_hash_non_source_file() {
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

        let content = "[settings]\nkey = \"value\"";
        fs::write(ws_dir.path().join("config.toml"), content).expect("write");

        let params = ReadFilesParams {
            paths: vec!["config.toml".to_string()],
            detail_level: "source_only".to_string(),
            max_lines_per_file: 500,
        };

        let result = server
            .read_files_impl(params)
            .await
            .expect("should succeed");
        let response: ReadFilesResponse =
            serde_json::from_value(result.structured_content.unwrap()).unwrap();

        assert_eq!(response.succeeded, 1);
        let file_result = &response.files[0];
        assert!(file_result.version_hash.is_some());
        let hash = file_result.version_hash.as_ref().unwrap();
        assert!(hash.starts_with("sha256:"));
        let expected = pathfinder_common::types::VersionHash::compute(content.as_bytes());
        assert_eq!(hash, expected.as_str());
    }

    #[tokio::test]
    async fn test_read_files_truncation_exact_boundary() {
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

        // Create content with exactly 5 lines
        let content = "line1\nline2\nline3\nline4\nline5";
        fs::write(ws_dir.path().join("exact.txt"), content).expect("write");

        let params = ReadFilesParams {
            paths: vec!["exact.txt".to_string()],
            detail_level: "source_only".to_string(),
            max_lines_per_file: 5, // Exactly matches content line count
        };

        let result = server
            .read_files_impl(params)
            .await
            .expect("should succeed");
        let response: ReadFilesResponse =
            serde_json::from_value(result.structured_content.unwrap()).unwrap();

        assert_eq!(response.succeeded, 1);
        let file_result = &response.files[0];
        // Should NOT be truncated since content has exactly max_lines lines
        assert_eq!(file_result.content.as_deref(), Some(content));
        assert_eq!(file_result.total_lines, Some(5));
    }
}
