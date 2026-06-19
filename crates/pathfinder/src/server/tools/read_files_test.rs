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

    let params = ReadParams {
        paths: Some(vec![
            "file1.rs".to_string(),
            "file2.ts".to_string(),
            "file3.py".to_string(),
        ]),
        detail_level: "source_only".to_string(),
        max_lines_per_file: 500,
        ..Default::default()
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

    let params = ReadParams {
        paths: Some(vec![
            "valid1.txt".to_string(),
            "valid2.txt".to_string(),
            "missing.txt".to_string(),
        ]),
        detail_level: "source_only".to_string(),
        max_lines_per_file: 500,
        ..Default::default()
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

    let params = ReadParams {
        paths: Some(vec![".git/HEAD".to_string()]),
        detail_level: "source_only".to_string(),
        max_lines_per_file: 500,
        ..Default::default()
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

    let params = ReadParams {
        paths: Some((0..11).map(|i| format!("file{i}.txt")).collect()),
        detail_level: "source_only".to_string(),
        max_lines_per_file: 500,
        ..Default::default()
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

    let params = ReadParams {
        paths: Some(vec![]),
        detail_level: "source_only".to_string(),
        max_lines_per_file: 500,
        ..Default::default()
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

    let params = ReadParams {
        paths: Some(vec!["main.rs".to_string(), "config.toml".to_string()]),
        detail_level: "source_only".to_string(),
        max_lines_per_file: 500,
        ..Default::default()
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

    let params1 = ReadParams {
        paths: Some(vec!["test.rs".to_string()]),
        detail_level: "source_only".to_string(),
        max_lines_per_file: 500,
        ..Default::default()
    };
    let result1 = server
        .read_files_impl(params1)
        .await
        .expect("should succeed");
    let response1: ReadFilesResponse =
        serde_json::from_value(result1.structured_content.unwrap()).unwrap();
    let hash1 = response1.files[0].version_hash.as_ref().unwrap().clone();

    // read tool now emits short() — 7-char hex, consistent with explore's version_hashes.
    assert_eq!(hash1.len(), 7, "hash should be 7 hex chars (short format)");
    assert!(
        hash1.chars().all(|c| c.is_ascii_hexdigit()),
        "hash should be lowercase hex only, got: {hash1}"
    );
    assert!(
        !hash1.starts_with("sha256:"),
        "read tool must not emit sha256: prefix; that would diverge from explore"
    );

    let binding = pathfinder_common::types::VersionHash::compute(content.as_bytes());
    let expected_hash = binding.short();
    assert_eq!(
        hash1, expected_hash,
        "hash should match VersionHash::short()"
    );
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

    let params = ReadParams {
        paths: Some(vec!["test.txt".to_string()]),
        detail_level: "source_only".to_string(),
        max_lines_per_file: 3,
        ..Default::default()
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

    let params = ReadParams {
        paths: Some(vec!["empty.rs".to_string()]),
        detail_level: "source_only".to_string(),
        max_lines_per_file: 500,
        ..Default::default()
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

    let params = ReadParams {
        paths: Some(vec!["binary.bin".to_string()]),
        detail_level: "source_only".to_string(),
        max_lines_per_file: 500,
        ..Default::default()
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

    let params = ReadParams {
        paths: Some(vec!["test.rs".to_string(), "test.rs".to_string()]),
        detail_level: "source_only".to_string(),
        max_lines_per_file: 500,
        ..Default::default()
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

    let params = ReadParams {
        paths: Some(vec!["test.mjs".to_string(), "test.cjs".to_string()]),
        detail_level: "source_only".to_string(),
        max_lines_per_file: 500,
        ..Default::default()
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

    let params = ReadParams {
        paths: Some(vec!["image.png".to_string()]),
        detail_level: "source_only".to_string(),
        max_lines_per_file: 500,
        ..Default::default()
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

    let params = ReadParams {
        paths: Some(vec!["config.toml".to_string()]),
        detail_level: "source_only".to_string(),
        max_lines_per_file: 500,
        ..Default::default()
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
    // short format: 7-char hex, no sha256: prefix
    assert_eq!(hash.len(), 7, "hash must be 7-char short format");
    assert!(!hash.starts_with("sha256:"), "must not have sha256: prefix");
    let expected = pathfinder_common::types::VersionHash::compute(content.as_bytes());
    assert_eq!(hash, expected.short());
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

    let params = ReadParams {
        paths: Some(vec!["exact.txt".to_string()]),
        detail_level: "source_only".to_string(),
        max_lines_per_file: 5,
        ..Default::default()
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

#[tokio::test]
async fn test_read_files_text_content_includes_file_data() {
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

    fs::write(ws_dir.path().join("hello.txt"), "hello world").expect("write");
    fs::write(
        ws_dir.path().join("second.toml"),
        "[settings]\nkey = \"val\"",
    )
    .expect("write");

    let params = ReadParams {
        paths: Some(vec!["hello.txt".to_string(), "second.toml".to_string()]),
        detail_level: "source_only".to_string(),
        max_lines_per_file: 500,
        ..Default::default()
    };

    let result = server
        .read_files_impl(params)
        .await
        .expect("should succeed");

    let text = match result.content.first() {
        Some(rmcp::model::Content {
            raw: rmcp::model::RawContent::Text(t),
            ..
        }) => t.text.clone(),
        _ => panic!("expected text content"),
    };

    assert!(
        text.contains("hello.txt"),
        "text output must include file path, got: {text}"
    );
    assert!(
        text.contains("hello world"),
        "text output must include file content, got: {text}"
    );
    assert!(
        text.contains("second.toml"),
        "text output must include second file path, got: {text}"
    );
    assert!(
        text.contains("key = \"val\""),
        "text output must include second file content, got: {text}"
    );

    assert!(
        !text.starts_with("[completed in"),
        "text output must NOT be just the timing footer, got: {text}"
    );
}

#[tokio::test]
async fn test_read_files_text_content_shows_errors_for_missing() {
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

    fs::write(ws_dir.path().join("exists.txt"), "content").expect("write");

    let params = ReadParams {
        paths: Some(vec!["exists.txt".to_string(), "missing.txt".to_string()]),
        detail_level: "source_only".to_string(),
        max_lines_per_file: 500,
        ..Default::default()
    };

    let result = server
        .read_files_impl(params)
        .await
        .expect("should succeed");

    let text = match result.content.first() {
        Some(rmcp::model::Content {
            raw: rmcp::model::RawContent::Text(t),
            ..
        }) => t.text.clone(),
        _ => panic!("expected text content"),
    };

    assert!(
        text.contains("exists.txt"),
        "text must include successful file path"
    );
    assert!(
        text.contains("missing.txt"),
        "text must include failed file path"
    );
    assert!(
        text.contains("error") || text.contains("failed") || text.contains("not found"),
        "text must indicate the error for missing file, got: {text}"
    );
}

// ── Concurrency: >5 files triggers the while-loop drain ─────────────────

#[tokio::test]
async fn test_read_files_concurrency_above_limit() {
    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let mock_surgeon = Arc::new(MockSurgeon::new());

    // Create 7 source files (> READ_FILES_CONCURRENCY = 5)
    let file_count = 7;
    let mut paths = Vec::new();
    for i in 0..file_count {
        let name = format!("file{i}.rs");
        let content = format!("fn func_{i}() {{}}");
        fs::write(ws_dir.path().join(&name), &content).expect("write");
        mock_surgeon
            .read_source_file_results
            .lock()
            .unwrap()
            .push(Ok((content, "rust".to_string(), vec![])));
        paths.push(name);
    }

    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        mock_surgeon,
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = ReadParams {
        paths: Some(paths),
        detail_level: "source_only".to_string(),
        max_lines_per_file: 500,
        ..Default::default()
    };

    let result = server
        .read_files_impl(params)
        .await
        .expect("should succeed");
    let response: ReadFilesResponse =
        serde_json::from_value(result.structured_content.unwrap()).unwrap();

    assert_eq!(response.files.len(), file_count);
    assert_eq!(response.succeeded, u32::try_from(file_count).unwrap());
    assert_eq!(response.failed, 0);
}

// ── Source file error path (non-UnsupportedLanguage) ────────────────────

#[tokio::test]
async fn test_read_files_source_file_read_error() {
    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let mock_surgeon = Arc::new(MockSurgeon::new());

    // Create a .rs file (source file) that will trigger an error via mock
    fs::write(ws_dir.path().join("broken.rs"), "fn main() {}").expect("write");
    mock_surgeon
        .read_source_file_results
        .lock()
        .unwrap()
        .push(Err(pathfinder_treesitter::error::SurgeonError::Io(
            std::sync::Arc::new(std::io::Error::other("disk failure")),
        )));

    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        mock_surgeon,
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = ReadParams {
        paths: Some(vec!["broken.rs".to_string()]),
        detail_level: "source_only".to_string(),
        max_lines_per_file: 500,
        ..Default::default()
    };

    let result = server
        .read_files_impl(params)
        .await
        .expect("should succeed even with single file error");
    let response: ReadFilesResponse =
        serde_json::from_value(result.structured_content.unwrap()).unwrap();

    assert_eq!(response.files.len(), 1);
    assert_eq!(
        response.failed, 1,
        "the broken source file should be a failure"
    );
    assert!(
        response.files[0].error.is_some(),
        "error should be populated for broken source file"
    );
}

// ── Binary non-source file (InvalidData IO error) ───────────────────────

#[tokio::test]
async fn test_read_files_binary_non_source_file() {
    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Write invalid UTF-8 bytes to a .txt file (non-source extension)
    fs::write(
        ws_dir.path().join("binary.txt"),
        [0xff, 0xfe, 0x00, 0x80, 0x90, 0xa0],
    )
    .expect("write");

    let server = PathfinderServer::with_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(MockSurgeon::new()),
    );

    let params = ReadParams {
        paths: Some(vec!["binary.txt".to_string()]),
        detail_level: "source_only".to_string(),
        max_lines_per_file: 500,
        ..Default::default()
    };

    let result = server
        .read_files_impl(params)
        .await
        .expect("should succeed");
    let response: ReadFilesResponse =
        serde_json::from_value(result.structured_content.unwrap()).unwrap();

    assert_eq!(response.files.len(), 1);
    assert_eq!(response.failed, 1);
    let err = response.files[0].error.as_deref().unwrap_or("");
    assert!(
        err.contains("binary") || err.contains("UTF-8"),
        "error should indicate binary/non-UTF-8 file, got: {err}"
    );
}

// ── General IO error for non-source file (not NotFound, not InvalidData) ──

#[tokio::test]
async fn test_read_files_non_source_general_io_error() {
    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Create a directory with a .txt extension — reading it as a file triggers a general IO error
    let dir_as_file = ws_dir.path().join("fakefile.txt");
    fs::create_dir_all(&dir_as_file).expect("create dir");

    let server = PathfinderServer::with_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(MockSurgeon::new()),
    );

    let params = ReadParams {
        paths: Some(vec!["fakefile.txt".to_string()]),
        detail_level: "source_only".to_string(),
        max_lines_per_file: 500,
        ..Default::default()
    };

    let result = server
        .read_files_impl(params)
        .await
        .expect("should succeed");
    let response: ReadFilesResponse =
        serde_json::from_value(result.structured_content.unwrap()).unwrap();

    assert_eq!(response.files.len(), 1);
    assert_eq!(response.failed, 1);
    let err = response.files[0].error.as_deref().unwrap_or("");
    assert!(
        err.contains("failed to read file"),
        "error should indicate general read failure, got: {err}"
    );
}

// ── Language "text" mapped to empty string for non-source files ─────────

#[tokio::test]
async fn test_read_files_text_language_mapped_to_empty() {
    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // File with no extension — language_from_path returns "text"
    fs::write(ws_dir.path().join("Makefile"), "all:\n\techo hello").expect("write");

    let server = PathfinderServer::with_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(MockSurgeon::new()),
    );

    let params = ReadParams {
        paths: Some(vec!["Makefile".to_string()]),
        detail_level: "source_only".to_string(),
        max_lines_per_file: 500,
        ..Default::default()
    };

    let result = server
        .read_files_impl(params)
        .await
        .expect("should succeed");
    let response: ReadFilesResponse =
        serde_json::from_value(result.structured_content.unwrap()).unwrap();

    assert_eq!(response.files.len(), 1);
    assert_eq!(response.succeeded, 1);
    let lang = response.files[0].language.as_deref().unwrap_or("MISSING");
    assert_eq!(
        lang, "",
        "language 'text' should be mapped to empty string, got: {lang}"
    );
}

#[tokio::test]
async fn test_read_impl_validation_errors() {
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

    // Both filepath and paths set
    let params = ReadParams {
        filepath: Some("test.txt".to_string()),
        paths: Some(vec!["test.txt".to_string()]),
        ..Default::default()
    };
    let res = server.read_impl(params).await;
    assert!(res.is_err());
    assert_eq!(
        res.unwrap_err().code,
        rmcp::model::ErrorCode::INVALID_PARAMS
    );

    // Neither filepath nor paths set
    let params = ReadParams {
        filepath: None,
        paths: None,
        ..Default::default()
    };
    let res = server.read_impl(params).await;
    assert!(res.is_err());
    assert_eq!(
        res.unwrap_err().code,
        rmcp::model::ErrorCode::INVALID_PARAMS
    );

    // end_line < start_line
    let params = ReadParams {
        filepath: Some("test.txt".to_string()),
        start_line: 10,
        end_line: Some(5),
        ..Default::default()
    };
    let res = server.read_impl(params).await;
    assert!(res.is_err());
    assert_eq!(
        res.unwrap_err().code,
        rmcp::model::ErrorCode::INVALID_PARAMS
    );
}

#[tokio::test]
async fn test_read_files_missing_paths() {
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

    let params = ReadParams {
        paths: None,
        ..Default::default()
    };
    let res = server.read_files_impl(params).await;
    assert!(res.is_err());
}

#[tokio::test]
async fn test_read_single_file_edge_cases() {
    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let mock_surgeon = Arc::new(MockSurgeon::new());

    // Write source file
    fs::write(ws_dir.path().join("test.rs"), "fn main() {}").expect("write");

    // Case 1: mock_surgeon returns success but structured_content / metadata is missing,
    // raw text fallback is used.
    let mut call_res = CallToolResult::success(vec![rmcp::model::Content::text("raw text")]);
    call_res.structured_content = None; // Ensure structured_content is None

    mock_surgeon
        .read_source_file_results
        .lock()
        .unwrap()
        .push(Ok((String::new(), "text".to_string(), vec![]))); // raw fallback maps language

    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        mock_surgeon,
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = ReadParams {
        max_lines_per_file: 10,
        ..Default::default()
    };

    // We can invoke read_single_file directly since it's pub(crate)
    let res = server.read_single_file("test.rs", &params).await;
    assert_eq!(res.language.as_deref(), Some("text"));
    assert_eq!(res.content, Some(String::new()));
}
