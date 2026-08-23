use super::test_mocks::MockProcessSpawner;
use super::*;
use tempfile::tempdir;

#[tokio::test]
async fn test_path_to_file_uri_file() {
    let dir = tempdir().expect("temp dir");
    let file_path = dir.path().join("test file.txt");
    std::fs::write(&file_path, "content").expect("write");

    let uri = path_to_file_uri(&file_path).await.expect("ok");
    assert!(uri.starts_with("file://"));
    assert!(
        uri.ends_with("test%20file.txt"),
        "Should percent-encode spaces"
    );
}

#[tokio::test]
async fn test_path_to_file_uri_dir() {
    let dir = tempdir().expect("temp dir");
    let uri = path_to_file_uri(dir.path()).await.expect("ok");
    assert!(uri.starts_with("file://"));
    assert!(uri.ends_with('/'), "Should end with slash for directories");
}

#[tokio::test]
async fn test_build_initialize_request_structure() {
    let dir = tempdir().expect("temp dir");
    let request = build_initialize_request(42, dir.path(), "rust", &[], serde_json::Value::Null)
        .await
        .expect("ok");

    assert_eq!(request["jsonrpc"], "2.0");
    assert_eq!(request["id"], 42);
    assert_eq!(request["method"], "initialize");

    let params = &request["params"];
    assert!(params["rootUri"]
        .as_str()
        .expect("rootUri should be a string")
        .starts_with("file://"));
    assert_eq!(params["clientInfo"]["name"], "pathfinder");
    assert_eq!(
        params["clientInfo"]["version"].as_str(),
        Some(env!("CARGO_PKG_VERSION")),
        "clientInfo.version should match Cargo.toml version"
    );
    assert!(
        params["processId"]
            .as_u64()
            .expect("processId should be a u64")
            > 0
    );
    assert!(params["workspaceFolders"].is_array());
}

#[tokio::test]
async fn test_build_initialize_request_workspace_name() {
    let dir = tempdir().expect("temp dir");
    // Create a directory with a name
    let named_dir = dir.path().join("my_project");
    std::fs::create_dir_all(&named_dir).expect("create dir");

    let request = build_initialize_request(1, &named_dir, "rust", &[], serde_json::Value::Null)
        .await
        .expect("ok");
    let folders = request["params"]["workspaceFolders"]
        .as_array()
        .expect("array");
    assert_eq!(folders[0]["name"], "my_project");
}

#[tokio::test]
async fn test_build_initialize_request_capabilities() {
    let dir = tempdir().expect("temp dir");
    let request = build_initialize_request(1, dir.path(), "rust", &[], serde_json::Value::Null)
        .await
        .expect("ok");

    let caps = &request["params"]["capabilities"];
    assert!(!caps["textDocument"]["definition"]["dynamicRegistration"]
        .as_bool()
        .unwrap_or(true));
    assert!(caps["workspace"]["workspaceFolders"]
        .as_bool()
        .unwrap_or(false));
    assert!(
        caps["window"]["workDoneProgress"]
            .as_bool()
            .unwrap_or(false),
        "workDoneProgress must be opted into for progress tracking"
    );
}

#[tokio::test]
async fn test_initialize_includes_plugins_when_present() {
    let dir = tempdir().expect("temp dir");
    let plugins = vec!["@vue/typescript-plugin".to_owned()];
    let request = build_initialize_request(
        1,
        dir.path(),
        "typescript",
        &plugins,
        serde_json::Value::Null,
    )
    .await
    .expect("ok");

    let init_opts = &request["params"]["initializationOptions"];
    assert!(
        !init_opts.is_null(),
        "initializationOptions should not be null"
    );
    assert!(!init_opts["plugins"].is_null(), "plugins should be present");

    let plugin_array = init_opts["plugins"]
        .as_array()
        .expect("plugins should be an array");
    assert_eq!(plugin_array.len(), 1);
    assert_eq!(
        plugin_array[0]["name"].as_str(),
        Some("@vue/typescript-plugin")
    );

    // Check tsserver extraFileExtensions for .vue
    let tsserver = &init_opts["tsserver"];
    assert!(!tsserver.is_null(), "tsserver config should be present");
    let extensions = tsserver["extraFileExtensions"]
        .as_array()
        .expect("should be array");
    assert_eq!(extensions.len(), 1);
    assert_eq!(extensions[0]["extension"].as_str(), Some("vue"));
}

#[tokio::test]
async fn test_initialize_empty_when_no_plugins() {
    let dir = tempdir().expect("temp dir");
    let request = build_initialize_request(1, dir.path(), "rust", &[], serde_json::Value::Null)
        .await
        .expect("ok");

    let init_opts = &request["params"]["initializationOptions"];
    assert!(
        init_opts.is_null() || init_opts.as_object().is_none_or(serde_json::Map::is_empty),
        "initializationOptions should be null or empty when no plugins"
    );
}

#[tokio::test]
async fn test_initialize_includes_vue_file_extension() {
    let dir = tempdir().expect("temp dir");
    let plugins = vec!["@vue/typescript-plugin".to_owned()];
    let request = build_initialize_request(
        1,
        dir.path(),
        "typescript",
        &plugins,
        serde_json::Value::Null,
    )
    .await
    .expect("ok");

    let init_opts = &request["params"]["initializationOptions"];
    let tsserver = &init_opts["tsserver"];
    let extensions = tsserver["extraFileExtensions"]
        .as_array()
        .expect("should be array");

    let vue_ext = extensions
        .iter()
        .find(|e| e["extension"].as_str() == Some("vue"));
    assert!(vue_ext.is_some(), "Vue extension should be present");
    assert_eq!(
        vue_ext.expect("checked above")["scriptKind"].as_u64(),
        Some(3),
        "Vue should use TS script kind"
    );
}

#[tokio::test]
async fn test_path_to_file_uri_nonexistent() {
    // Non-existent absolute paths should still produce a valid file URI.
    // `path_to_file_uri` only checks metadata for is_dir; a missing file
    // falls through to `from_file_path`, which succeeds for absolute paths.
    let uri = path_to_file_uri(Path::new("/definitely/does/not/exist.txt")).await;
    assert!(
        uri.is_ok(),
        "absolute nonexistent path should produce a valid file URI"
    );
    let uri_str = uri.expect("should be Ok");
    assert!(
        uri_str.starts_with("file:///"),
        "URI should start with file:///: {uri_str}"
    );
    assert!(
        uri_str.ends_with("exist.txt"),
        "URI should contain the filename: {uri_str}"
    );
}

#[tokio::test]
async fn test_shutdown_terminates_process() {
    // Arrange
    // Use absolute path to avoid failures when PATH is temporarily replaced
    // by another test (e.g. detect::tests::test_with_fake_python_binaries).
    let sleep_bin = which::which("sleep")
        .or_else(|_| {
            which::which("/usr/bin/sleep").map(|_| std::path::PathBuf::from("/usr/bin/sleep"))
        })
        .or_else(|_| which::which("/bin/sleep").map(|_| std::path::PathBuf::from("/bin/sleep")))
        .unwrap_or_else(|_| std::path::PathBuf::from("/usr/bin/sleep"));
    let mut child_process = tokio::process::Command::new(&sleep_bin)
        .arg("10")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("Failed to spawn sleep");

    let stdin = child_process.stdin.take().expect("Failed to take stdin");

    let process = ManagedProcess {
        child: Arc::new(tokio::sync::Mutex::new(child_process)),
        stdin: std::sync::Arc::new(tokio::sync::Mutex::new(tokio::io::BufWriter::new(stdin))),
        capabilities: crate::client::capabilities::DetectedCapabilities::default(),
        _jdtls_lock: None,
    };

    let dispatcher = std::sync::Arc::new(RequestDispatcher::new());

    // Act
    process.shutdown(&dispatcher, "test").await;

    // Assert
    // Give the OS a moment to reap the process
    let mut child = process.child.lock().await;
    let status = child.wait().await.expect("Failed to wait on child");
    assert!(
        !status.success(),
        "Process should have been killed, but exited successfully"
    );
}

// ── gitignore helper tests ──────────────────────────────────────

// ── ManagedProcess::is_alive tests ───────────────────────────

/// Helper: spawn a sleep process and wrap it in a `ManagedProcess`.
fn make_managed_process(sleep_secs: &str) -> ManagedProcess {
    let sleep_bin = which::which("sleep")
        .or_else(|_| {
            which::which("/usr/bin/sleep").map(|_| std::path::PathBuf::from("/usr/bin/sleep"))
        })
        .unwrap_or_else(|_| std::path::PathBuf::from("/usr/bin/sleep"));
    let mut child = tokio::process::Command::new(&sleep_bin)
        .arg(sleep_secs)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("Failed to spawn sleep");
    let stdin = child.stdin.take().expect("stdin piped");
    ManagedProcess {
        child: Arc::new(tokio::sync::Mutex::new(child)),
        stdin: std::sync::Arc::new(tokio::sync::Mutex::new(tokio::io::BufWriter::new(stdin))),
        capabilities: crate::client::capabilities::DetectedCapabilities::default(),
        _jdtls_lock: None,
    }
}

#[tokio::test]
async fn test_is_alive_returns_true_for_running_process() {
    let process = make_managed_process("10");
    assert!(
        process.is_alive(),
        "Running sleep process should report alive"
    );
}

#[tokio::test]
async fn test_is_alive_returns_false_after_kill() {
    let process = make_managed_process("10");
    // Kill the process
    let _ = process.child.lock().await.kill().await;
    // Give OS time to mark it as exited
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !process.is_alive(),
        "Killed process should report not alive"
    );
}

#[tokio::test]
async fn test_is_alive_returns_false_for_exited_process() {
    // Use sleep 0 so the process exits immediately
    let process = make_managed_process("0");
    // Wait for it to exit naturally
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !process.is_alive(),
        "Naturally exited process should report not alive"
    );
}

#[test]
fn test_ensure_pathfinder_in_gitignore_creates_new() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gitignore = dir.path().join(".gitignore");
    assert!(!gitignore.exists());

    ensure_pathfinder_in_gitignore(dir.path());

    let content = std::fs::read_to_string(&gitignore).expect("read");
    assert!(
        content.contains(".pathfinder/"),
        "should contain .pathfinder/ entry"
    );
}

#[test]
fn test_ensure_pathfinder_in_gitignore_appends_to_existing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gitignore = dir.path().join(".gitignore");
    std::fs::write(&gitignore, "node_modules/\n/target/\n").expect("write");

    ensure_pathfinder_in_gitignore(dir.path());

    let content = std::fs::read_to_string(&gitignore).expect("read");
    assert!(
        content.contains("node_modules/"),
        "should preserve existing entries"
    );
    assert!(
        content.contains(".pathfinder/"),
        "should add .pathfinder/ entry"
    );
}

#[test]
fn test_ensure_pathfinder_in_gitignore_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gitignore = dir.path().join(".gitignore");
    std::fs::write(&gitignore, "node_modules/\n.pathfinder/\n").expect("write");

    ensure_pathfinder_in_gitignore(dir.path());

    let content = std::fs::read_to_string(&gitignore).expect("read");
    // Should not duplicate the entry
    let count = content.matches(".pathfinder/").count();
    assert_eq!(count, 1, "should not duplicate .pathfinder/ entry");
}

#[test]
fn test_ensure_pathfinder_in_gitignore_idempotent_with_slash_prefix() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gitignore = dir.path().join(".gitignore");
    std::fs::write(&gitignore, "node_modules/\n/.pathfinder/\n").expect("write");

    ensure_pathfinder_in_gitignore(dir.path());

    let content = std::fs::read_to_string(&gitignore).expect("read");
    let count = content.matches(".pathfinder/").count();
    assert_eq!(count, 1, "should not duplicate /.pathfinder/ entry");
}

// ── jdtls data directory isolation tests ─────────────────────────────────

#[test]
fn test_resolve_jdtls_data_dir_creates_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (data_dir, lock) = resolve_jdtls_data_dir(dir.path());
    assert!(data_dir.exists(), "data dir must be created");
    assert!(data_dir.is_dir(), "data dir must be a directory");
    assert!(
        data_dir.starts_with(dir.path().join(".pathfinder")),
        "data dir must be under .pathfinder/"
    );
    assert!(lock.is_some(), "primary instance should acquire lock");
}

#[test]
fn test_resolve_jdtls_data_dir_primary_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (data_dir, _lock) = resolve_jdtls_data_dir(dir.path());
    assert_eq!(
        data_dir,
        dir.path().join(".pathfinder").join("jdtls-data"),
        "primary instance should use jdtls-data (no PID suffix)"
    );
}

#[test]
fn test_resolve_jdtls_data_dir_concurrent_uses_pid_fallback() {
    let dir = tempfile::tempdir().expect("tempdir");

    // First instance acquires the primary lock
    let (primary_dir, primary_lock) = resolve_jdtls_data_dir(dir.path());
    assert!(primary_lock.is_some(), "first call should get the lock");
    assert_eq!(
        primary_dir,
        dir.path().join(".pathfinder").join("jdtls-data"),
    );

    // Second instance should fall back to PID-suffixed directory
    let (fallback_dir, fallback_lock) = resolve_jdtls_data_dir(dir.path());
    assert_ne!(
        fallback_dir, primary_dir,
        "concurrent instance must use a different directory"
    );
    let expected_suffix = format!("jdtls-data-{}", std::process::id());
    assert!(
        fallback_dir.ends_with(&expected_suffix),
        "fallback dir should end with PID suffix, got: {}",
        fallback_dir.display()
    );
    // Fallback doesn't need a lock (PID is unique per process)
    assert!(
        fallback_lock.is_none(),
        "fallback should not hold primary lock"
    );
    assert!(fallback_dir.exists(), "fallback dir must be created");
}

#[test]
fn test_resolve_jdtls_data_dir_lock_released_on_drop() {
    let dir = tempfile::tempdir().expect("tempdir");

    // Acquire and immediately drop the lock
    {
        let (_data_dir, _lock) = resolve_jdtls_data_dir(dir.path());
        // lock dropped here
    }

    // Should be able to acquire the primary lock again
    let (data_dir, lock) = resolve_jdtls_data_dir(dir.path());
    assert_eq!(
        data_dir,
        dir.path().join(".pathfinder").join("jdtls-data"),
        "after lock release, primary dir should be available again"
    );
    assert!(lock.is_some(), "should re-acquire lock after previous drop");
}

#[test]
fn test_cleanup_orphaned_jdtls_data_dirs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pathfinder_dir = dir.path().join(".pathfinder");
    std::fs::create_dir_all(&pathfinder_dir).expect("create .pathfinder dir");

    // Create a directory for a dead PID (e.g. 999999)
    let dead_pid_dir = pathfinder_dir.join("jdtls-data-999999");
    std::fs::create_dir_all(&dead_pid_dir).expect("create dead pid dir");

    // Create a directory for our own PID (definitely alive)
    let alive_pid_dir = pathfinder_dir.join(format!("jdtls-data-{}", std::process::id()));
    std::fs::create_dir_all(&alive_pid_dir).expect("create alive pid dir");

    assert!(dead_pid_dir.exists());
    assert!(alive_pid_dir.exists());

    cleanup_orphaned_jdtls_data_dirs(&pathfinder_dir);

    // Dead PID directory should be cleaned up
    assert!(
        !dead_pid_dir.exists(),
        "dead PID jdtls data dir should be cleaned up"
    );
    // Alive PID directory should remain
    assert!(
        alive_pid_dir.exists(),
        "alive PID jdtls data dir should NOT be cleaned up"
    );
}

#[tokio::test]
async fn test_initialize_java_init_options_passed() {
    use serde_json::json;
    let dir = tempfile::tempdir().expect("tempdir");
    let java_opts = json!({
        "java": {
            "import": {
                "gradle": { "enabled": true },
                "maven": { "enabled": true }
            }
        }
    });

    let request = build_initialize_request(1, dir.path(), "java", &[], java_opts)
        .await
        .expect("ok");

    let init_opts = &request["params"]["initializationOptions"];
    assert!(
        !init_opts.is_null(),
        "init_options should be present for java"
    );
    assert!(
        init_opts["java"]["import"]["maven"]["enabled"]
            .as_bool()
            .unwrap_or(false),
        "Maven import should be enabled in initialize request"
    );
    assert!(
        init_opts["java"]["import"]["gradle"]["enabled"]
            .as_bool()
            .unwrap_or(false),
        "Gradle import should be enabled in initialize request"
    );
}

#[tokio::test]
async fn test_initialize_python_init_options_passed() {
    use serde_json::json;
    let dir = tempfile::tempdir().expect("tempdir");
    let fake_python = dir.path().join("fake_python");
    std::fs::write(&fake_python, "#!/bin/sh").expect("write");
    let python_path = fake_python.to_string_lossy().into_owned();

    let python_opts = json!({
        "python": {
            "pythonPath": python_path
        }
    });

    let request = build_initialize_request(1, dir.path(), "python", &[], python_opts)
        .await
        .expect("ok");

    let init_opts = &request["params"]["initializationOptions"];
    assert!(!init_opts.is_null(), "init_options should be present");
    assert_eq!(
        init_opts["python"]["pythonPath"].as_str(),
        Some(python_path.as_str()),
        "pythonPath should be passed through to initialize request"
    );
}

#[test]
fn test_revalidate_python_init_options_valid_path_preserved() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake_python = dir.path().join("fake_python");
    std::fs::write(&fake_python, "#!/bin/sh").expect("write");
    let path_str = fake_python.to_string_lossy().into_owned();

    let init_opts = serde_json::json!({
        "python": { "pythonPath": path_str }
    });

    let result = revalidate_python_init_options(init_opts.clone(), dir.path());

    assert_eq!(
        result["python"]["pythonPath"].as_str(),
        Some(path_str.as_str()),
        "should preserve valid pythonPath"
    );
}

#[test]
fn test_revalidate_python_init_options_invalid_path_redetects() {
    let dir = tempfile::tempdir().expect("tempdir");
    let venv_dir = dir.path().join(".venv").join("bin");
    std::fs::create_dir_all(&venv_dir).expect("create venv bin");
    let venv_python = venv_dir.join("python");
    std::fs::write(&venv_python, "#!/bin/sh").expect("write python");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&venv_python, std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
    }

    let init_opts = serde_json::json!({
        "python": { "pythonPath": "/nonexistent/path/bin/python" }
    });

    let result = revalidate_python_init_options(init_opts, dir.path());

    assert!(
        !result.is_null(),
        "should re-detect venv when old path is invalid"
    );
    assert!(
        result["python"]["pythonPath"]
            .as_str()
            .is_some_and(|p| p.contains(".venv")),
        "should contain re-detected venv path"
    );
}

#[test]
fn test_revalidate_python_init_options_no_existing_path_no_venv() {
    let dir = tempfile::tempdir().expect("tempdir");

    let init_opts = serde_json::json!({});
    let result = revalidate_python_init_options(init_opts, dir.path());

    assert!(result.is_null(), "should return Null when no venv found");
}

#[test]
fn test_revalidate_python_init_options_invalid_path_no_venv() {
    let dir = tempfile::tempdir().expect("tempdir");

    let init_opts = serde_json::json!({
        "python": { "pythonPath": "/old/venv/bin/python" }
    });

    let result = revalidate_python_init_options(init_opts, dir.path());

    assert!(
        result.is_null(),
        "should return Null when old path invalid and no new venv"
    );
}

#[tokio::test]
async fn test_build_initialize_request_null_init_options() {
    let dir = tempfile::tempdir().expect("tempdir");

    let request = build_initialize_request(1, dir.path(), "rust", &[], serde_json::Value::Null)
        .await
        .expect("ok");

    let init_opts = &request["params"]["initializationOptions"];
    assert!(
        init_opts.is_null() || init_opts.as_object().is_none_or(serde_json::Map::is_empty),
        "initializationOptions should be null or empty when init_options is Null and no plugins"
    );
}

#[tokio::test]
async fn test_build_initialize_request_with_plugins_and_init_options_merged() {
    let dir = tempfile::tempdir().expect("tempdir");

    let plugins = vec!["@vue/typescript-plugin".to_owned()];
    let init_opts = serde_json::json!({"custom": "value"});

    let request = build_initialize_request(1, dir.path(), "typescript", &plugins, init_opts)
        .await
        .expect("ok");

    let opts = &request["params"]["initializationOptions"];
    assert!(opts["plugins"].is_array(), "plugins should be present");
    assert_eq!(
        opts["custom"], "value",
        "custom init_options should be merged"
    );
}

#[tokio::test]
async fn test_path_to_file_uri_with_spaces() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file_path = dir.path().join("my file.rs");
    std::fs::write(&file_path, "fn main() {}").expect("write");

    let uri = path_to_file_uri(&file_path).await.expect("ok");
    assert!(
        uri.contains("my%20file.rs"),
        "should percent-encode spaces in file URI"
    );
}

#[tokio::test]
async fn test_managed_process_capabilities_default() {
    let process = make_managed_process("10");

    let caps = process.capabilities();
    assert!(
        !caps.flags.definition_provider,
        "default capabilities should have definition_provider=false"
    );
}

#[tokio::test]
async fn test_ensure_pathfinder_in_gitignore_no_gitignore() {
    let dir = tempfile::tempdir().expect("tempdir");
    ensure_pathfinder_in_gitignore(dir.path());

    let gitignore = dir.path().join(".gitignore");
    assert!(gitignore.exists(), "should create .gitignore");
    let content = std::fs::read_to_string(gitignore).expect("read");
    assert!(content.contains(".pathfinder/"));
}

#[test]
fn test_spawn_lsp_child_nonexistent_binary() {
    let dir = tempfile::tempdir().expect("tempdir");

    let result = spawn_lsp_child(
        "/absolutely/nonexistent/binary",
        &[],
        dir.path(),
        "test",
        false,
    );

    assert!(result.is_err(), "should fail for nonexistent binary");
    match result {
        Err(LspError::Io(e)) => {
            assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
        }
        _ => panic!("expected Io(NotFound) error"),
    }
}

#[tokio::test]
async fn test_spawn_lsp_child_env_isolation_rust() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fake_bin = which::which("sh").unwrap_or_else(|_| std::path::PathBuf::from("/bin/sh"));

    let target_dir = dir.path().join("target").join("pathfinder-lsp");

    let result = spawn_lsp_child(
        fake_bin.to_str().unwrap_or("/bin/sh"),
        &["-c".to_owned(), "echo $CARGO_TARGET_DIR".to_owned()],
        dir.path(),
        "rust",
        true,
    );

    if result.is_ok() {
        assert!(
            target_dir.exists(),
            "CARGO_TARGET_DIR directory should be created for rust isolation"
        );
    }
}

// D-4: Tests for handle_reader_result

#[test]
fn test_handle_reader_result_success_resets_io_errors() {
    let mut consecutive_io_errors = 3;
    let mut malformed_message_count = 5;
    let result = Ok(&serde_json::json!({"result": "ok"}));

    let action = handle_reader_result(
        result,
        &mut consecutive_io_errors,
        &mut malformed_message_count,
        "test",
    );

    assert_eq!(action, ReaderAction::Continue);
    assert_eq!(
        consecutive_io_errors, 0,
        "should reset consecutive_io_errors on success"
    );
    assert_eq!(
        malformed_message_count, 5,
        "should not change malformed_message_count"
    );
}

#[test]
fn test_handle_reader_result_connection_lost_cancels() {
    let mut consecutive_io_errors = 3;
    let mut malformed_message_count = 5;
    let result = Err(&LspError::ConnectionLost);

    let action = handle_reader_result(
        result,
        &mut consecutive_io_errors,
        &mut malformed_message_count,
        "test",
    );

    assert_eq!(action, ReaderAction::CancelAndBreak);
    assert_eq!(consecutive_io_errors, 3, "should not change counters");
    assert_eq!(malformed_message_count, 5);
}

#[test]
fn test_handle_reader_result_protocol_error_increments_malformed() {
    let mut consecutive_io_errors = 2;
    let mut malformed_message_count = 0;
    let result = Err(&LspError::Protocol("bad JSON".to_owned()));

    let action = handle_reader_result(
        result,
        &mut consecutive_io_errors,
        &mut malformed_message_count,
        "test",
    );

    assert_eq!(
        action,
        ReaderAction::Continue,
        "protocol errors should continue reading"
    );
    assert_eq!(
        consecutive_io_errors, 2,
        "protocol errors should not count toward consecutive_io_errors"
    );
    assert_eq!(
        malformed_message_count, 1,
        "should increment malformed_message_count"
    );
}

#[test]
fn test_handle_reader_result_io_error_increments_counter() {
    let mut consecutive_io_errors = 0;
    let mut malformed_message_count = 0;
    let result = Err(&LspError::Io(std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "broken pipe",
    )));

    let action = handle_reader_result(
        result,
        &mut consecutive_io_errors,
        &mut malformed_message_count,
        "test",
    );

    assert_eq!(
        action,
        ReaderAction::Continue,
        "below threshold should continue"
    );
    assert_eq!(
        consecutive_io_errors, 1,
        "should increment io error counter"
    );
    assert_eq!(malformed_message_count, 0);
}

#[test]
fn test_handle_reader_result_io_error_at_threshold_cancels() {
    let mut consecutive_io_errors = MAX_CONSECUTIVE_READER_ERRORS - 1;
    let mut malformed_message_count = 0;
    let result = Err(&LspError::Io(std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "broken pipe",
    )));

    let action = handle_reader_result(
        result,
        &mut consecutive_io_errors,
        &mut malformed_message_count,
        "test",
    );

    assert_eq!(
        action,
        ReaderAction::CancelAndBreak,
        "at threshold should cancel"
    );
    assert_eq!(consecutive_io_errors, MAX_CONSECUTIVE_READER_ERRORS);
    assert_eq!(malformed_message_count, 0);
}

#[test]
fn test_handle_reader_result_io_error_above_threshold_cancels() {
    let mut consecutive_io_errors = MAX_CONSECUTIVE_READER_ERRORS;
    let mut malformed_message_count = 0;
    let result = Err(&LspError::Io(std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "broken pipe",
    )));

    let action = handle_reader_result(
        result,
        &mut consecutive_io_errors,
        &mut malformed_message_count,
        "test",
    );

    assert_eq!(
        action,
        ReaderAction::CancelAndBreak,
        "above threshold should cancel"
    );
    assert_eq!(consecutive_io_errors, MAX_CONSECUTIVE_READER_ERRORS + 1);
}

#[test]
fn test_handle_reader_result_mixed_errors() {
    let mut consecutive_io_errors = 2;
    let mut malformed_message_count = 1;

    let result = Err(&LspError::Protocol("invalid JSON".to_owned()));
    let action = handle_reader_result(
        result,
        &mut consecutive_io_errors,
        &mut malformed_message_count,
        "test",
    );
    assert_eq!(action, ReaderAction::Continue);
    assert_eq!(
        consecutive_io_errors, 2,
        "protocol error doesn't affect IO counter"
    );
    assert_eq!(malformed_message_count, 2);

    let result = Err(&LspError::Io(std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "broken pipe",
    )));
    let action = handle_reader_result(
        result,
        &mut consecutive_io_errors,
        &mut malformed_message_count,
        "test",
    );
    assert_eq!(action, ReaderAction::Continue);
    assert_eq!(consecutive_io_errors, 3);
    assert_eq!(malformed_message_count, 2);

    let result = Ok(&serde_json::json!({"result": "ok"}));
    let action = handle_reader_result(
        result,
        &mut consecutive_io_errors,
        &mut malformed_message_count,
        "test",
    );
    assert_eq!(action, ReaderAction::Continue);
    assert_eq!(consecutive_io_errors, 0, "success resets IO counter");
    assert_eq!(malformed_message_count, 2);
}

#[test]
fn test_process_spawner_trait_real_spawner_nonexistent_binary() {
    let spawner = RealProcessSpawner;
    let dir = tempfile::tempdir().expect("tempdir");

    let result = spawner.spawn("/nonexistent/binary", &[], dir.path(), "test", false);

    assert!(result.is_err(), "should fail for nonexistent binary");
    match result {
        Err(LspError::Io(e)) => {
            assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
        }
        other => panic!("expected Io error, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_process_spawner_trait_mock_records_calls() {
    let mock = MockProcessSpawner::failing();
    let dir = tempfile::tempdir().expect("tempdir");

    let _ = mock.spawn("gopls", &["--arg1".to_owned()], dir.path(), "rust", true);

    assert_eq!(mock.call_count(), 1);
    let call = mock.last_call().expect("should have a call");
    assert_eq!(call.command, "gopls");
    assert_eq!(call.args, vec!["--arg1"]);
    assert_eq!(call.language_id, "rust");
    assert!(call.isolate_target_dir);
    assert_eq!(call.project_root, dir.path());
}

#[test]
fn test_process_spawner_trait_mock_failing() {
    let mock = MockProcessSpawner::failing();
    let dir = tempfile::tempdir().expect("tempdir");

    let result = mock.spawn("gopls", &[], dir.path(), "go", false);

    assert!(result.is_err());
    assert_eq!(mock.call_count(), 1);
}

#[tokio::test]
async fn test_process_spawner_trait_mock_multiple_calls() {
    let mock = MockProcessSpawner::failing();
    let dir = tempfile::tempdir().expect("tempdir");

    let _ = mock.spawn("cmd1", &[], dir.path(), "rust", false);
    let _ = mock.spawn("cmd2", &[], dir.path(), "go", true);
    let _ = mock.spawn("cmd3", &[], dir.path(), "python", false);

    assert_eq!(mock.call_count(), 3);
    let calls = mock.spawn_calls.lock().expect("lock");
    assert_eq!(calls[0].language_id, "rust");
    assert_eq!(calls[1].language_id, "go");
    assert_eq!(calls[2].language_id, "python");
    assert!(!calls[0].isolate_target_dir);
    assert!(calls[1].isolate_target_dir);
}

#[tokio::test]
async fn test_process_spawner_trait_real_spawn() {
    // Test RealProcessSpawner can spawn a real process.
    let spawner = RealProcessSpawner;

    let dir = tempfile::tempdir().expect("tempdir");
    let result = spawner.spawn("sleep", &["60".to_owned()], dir.path(), "test", false);

    assert!(result.is_ok(), "should succeed with real spawn");
    let (mut child, _stdin, _stdout, _lock) = result.expect("should succeed");
    assert!(child.id().is_some_and(|pid| pid > 0));
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
async fn test_mock_spawner_succeeding_returns_real_process() {
    let mock = MockProcessSpawner::succeeding();
    let dir = tempfile::tempdir().expect("tempdir");

    let result = mock.spawn("gopls", &["--arg1".to_owned()], dir.path(), "rust", false);

    assert!(result.is_ok(), "succeeding mode should return Ok");
    let (mut child, _stdin, _stdout, _lock) = result.expect("succeeding mode should return Ok");
    assert!(child.id().is_some_and(|pid| pid > 0));

    // Verify call was recorded
    assert_eq!(mock.call_count(), 1);
    let call = mock.last_call().expect("should have a call");
    assert_eq!(call.command, "gopls");
    assert_eq!(call.args, vec!["--arg1"]);
    assert_eq!(call.language_id, "rust");
    assert!(!call.isolate_target_dir);

    // Cleanup: kill the sleep process
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[tokio::test]
async fn test_mock_spawner_succeeding_records_multiple_calls() {
    let mock = MockProcessSpawner::succeeding();
    let dir = tempfile::tempdir().expect("tempdir");

    let r1 = mock.spawn("cmd1", &[], dir.path(), "rust", false);
    let r2 = mock.spawn("cmd2", &["--stdio".to_owned()], dir.path(), "go", true);
    let r3 = mock.spawn("cmd3", &[], dir.path(), "python", false);

    assert!(r1.is_ok(), "r1 failed with: {:?}", r1.as_ref().err());
    assert!(r2.is_ok(), "r2 failed with: {:?}", r2.as_ref().err());
    assert!(r3.is_ok(), "r3 failed with: {:?}", r3.as_ref().err());

    assert_eq!(mock.call_count(), 3);
    {
        let calls = mock.spawn_calls.lock().expect("lock");
        assert_eq!(calls[0].language_id, "rust");
        assert_eq!(calls[1].language_id, "go");
        assert_eq!(calls[2].language_id, "python");
        assert!(!calls[0].isolate_target_dir);
        assert!(calls[1].isolate_target_dir);
    }

    // Cleanup all spawned sleep processes
    for (mut child, _, _, _) in [r1.expect("r1"), r2.expect("r2"), r3.expect("r3")] {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

#[tokio::test]
async fn test_mock_spawner_succeeding_is_alive_after_spawn() {
    let mock = MockProcessSpawner::succeeding();
    let dir = tempfile::tempdir().expect("tempdir");

    let result = mock.spawn("sleep", &["60".to_owned()], dir.path(), "test", false);
    assert!(result.is_ok());
    let (mut child, _stdin, _stdout, _lock) = result.expect("should succeed");

    // Process should be alive immediately after spawn
    assert!(child.id().is_some(), "child should have a PID");

    // Cleanup
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[test]
fn test_setup_rust_env() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = tokio::process::Command::new("cargo");

    // Gated on isolate_target_dir && language_id == "rust"
    super::setup_rust_env(&mut cmd, dir.path(), true, "rust");
    let std_cmd = cmd.as_std();
    let envs: std::collections::HashMap<_, _> = std_cmd.get_envs().collect();
    let cargo_target_dir_opt = envs
        .get(std::ffi::OsStr::new("CARGO_TARGET_DIR"))
        .expect("CARGO_TARGET_DIR env var present");
    let cargo_target_dir = cargo_target_dir_opt.expect("CARGO_TARGET_DIR value present");
    assert_eq!(
        cargo_target_dir,
        dir.path().join("target").join("pathfinder-lsp").as_os_str()
    );

    // Not rust language_id
    let mut cmd = tokio::process::Command::new("cargo");
    super::setup_rust_env(&mut cmd, dir.path(), true, "go");
    let std_cmd = cmd.as_std();
    assert!(std_cmd.get_envs().next().is_none());
}

#[test]
fn test_setup_go_env() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = tokio::process::Command::new("go");

    // Gated on isolate_target_dir && language_id == "go"
    super::setup_go_env(&mut cmd, dir.path(), true, "go");
    let std_cmd = cmd.as_std();
    let envs: std::collections::HashMap<_, _> = std_cmd.get_envs().collect();
    let gocache = envs
        .get(std::ffi::OsStr::new("GOCACHE"))
        .expect("GOCACHE env var present")
        .expect("GOCACHE value present");
    assert_eq!(
        gocache,
        dir.path()
            .join(".pathfinder")
            .join("gopls-cache")
            .join("build")
            .as_os_str()
    );
    let gomodcache = envs
        .get(std::ffi::OsStr::new("GOMODCACHE"))
        .expect("GOMODCACHE env var present")
        .expect("GOMODCACHE value present");
    assert_eq!(
        gomodcache,
        dir.path()
            .join(".pathfinder")
            .join("gopls-cache")
            .join("mod")
            .as_os_str()
    );

    // Not go language_id
    let mut cmd = tokio::process::Command::new("go");
    super::setup_go_env(&mut cmd, dir.path(), true, "rust");
    let std_cmd = cmd.as_std();
    assert!(std_cmd.get_envs().next().is_none());
}

#[test]
fn test_setup_typescript_env() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = tokio::process::Command::new("node");

    // Gated on isolate_target_dir && language_id == "typescript"
    super::setup_typescript_env(&mut cmd, dir.path(), true, "typescript");
    let std_cmd = cmd.as_std();
    let envs: std::collections::HashMap<_, _> = std_cmd.get_envs().collect();
    let tmpdir = envs
        .get(std::ffi::OsStr::new("TMPDIR"))
        .expect("TMPDIR env var present")
        .expect("TMPDIR value present");
    assert_eq!(
        tmpdir,
        dir.path()
            .join(".pathfinder")
            .join("tsserver-tmp")
            .as_os_str()
    );
}

#[test]
fn test_setup_python_env() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = tokio::process::Command::new("python");

    // Gated on isolate_target_dir && language_id == "python"
    super::setup_python_env(&mut cmd, dir.path(), true, "python");
    let std_cmd = cmd.as_std();
    let envs: std::collections::HashMap<_, _> = std_cmd.get_envs().collect();
    let pycache = envs
        .get(std::ffi::OsStr::new("PYTHONPYCACHEPREFIX"))
        .expect("PYTHONPYCACHEPREFIX env var present")
        .expect("PYTHONPYCACHEPREFIX value present");
    assert_eq!(
        pycache,
        dir.path()
            .join(".pathfinder")
            .join("python-cache")
            .join("pyc")
            .as_os_str()
    );
}

#[test]
fn test_setup_java_env() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = tokio::process::Command::new("java");

    // Gated on language_id == "java"
    let lock = super::setup_java_env(&mut cmd, dir.path(), "java");
    assert!(lock.is_some());
    let std_cmd = cmd.as_std();
    let args: Vec<_> = std_cmd.get_args().collect();
    assert!(args.contains(&std::ffi::OsStr::new("-data")));
    // Verify the -data argument value points to the jdtls data directory
    let data_idx = args
        .iter()
        .position(|a| *a == std::ffi::OsStr::new("-data"))
        .expect("-data arg position");
    let data_dir = args[data_idx + 1]
        .to_str()
        .expect("data dir is valid UTF-8");
    // Primary instance (no contention) gets 'jdtls-data' base dir (no PID suffix).
    // The assertion uses ends_with to reject the PID-suffixed fallback path
    // 'jdtls-data-<pid>' — both contain the string 'jdtls-data' but only
    // the primary ends with it exactly.
    assert!(
        data_dir.ends_with("jdtls-data"),
        "Primary instance should use 'jdtls-data' base dir (no PID suffix), got: '{data_dir}'"
    );
    assert!(
        data_dir.contains(".pathfinder"),
        "data dir '{data_dir}' should be under .pathfinder/"
    );
}

/// PATCH-006 Spike B: The `initialize` request MUST declare
/// `textDocument.callHierarchy` so TypeScript/Vue LSP servers enable
/// `callHierarchyProvider` in their response capabilities.
/// Without this, `callHierarchy/incomingCalls` and `outgoingCalls`
/// requests are rejected by the server.
#[tokio::test]
async fn test_build_initialize_request_declares_call_hierarchy() {
    let dir = tempdir().expect("temp dir");
    let request = build_initialize_request(1, dir.path(), "rust", &[], serde_json::Value::Null)
        .await
        .expect("ok");

    let caps = &request["params"]["capabilities"];
    let call_hierarchy = &caps["textDocument"]["callHierarchy"];
    assert!(
        !call_hierarchy.is_null(),
        "textDocument.callHierarchy must be declared in client capabilities"
    );
    assert_eq!(
        call_hierarchy["dynamicRegistration"].as_bool(),
        Some(false),
        "callHierarchy should declare dynamicRegistration false on static queries"
    );
}

/// PATCH-006 Spike B: The `initialize` request MUST declare
/// `textDocument.references` so LSP servers enable reference finding.
#[tokio::test]
async fn test_build_initialize_request_declares_references() {
    let dir = tempdir().expect("temp dir");
    let request = build_initialize_request(1, dir.path(), "rust", &[], serde_json::Value::Null)
        .await
        .expect("ok");

    let caps = &request["params"]["capabilities"];
    let references = &caps["textDocument"]["references"];
    assert!(
        !references.is_null(),
        "textDocument.references must be declared in client capabilities"
    );
}

#[test]
fn test_handle_reader_result_lifecycle() {
    let mut consecutive_io_errors = 3;
    let mut malformed_message_count = 2;
    let language_id = "rust";

    // 1. Ok resets consecutive_io_errors to 0 and returns ReaderAction::Continue
    let ok_val = serde_json::json!({});
    let action = handle_reader_result(
        Ok(&ok_val),
        &mut consecutive_io_errors,
        &mut malformed_message_count,
        language_id,
    );
    assert_eq!(action, ReaderAction::Continue);
    assert_eq!(consecutive_io_errors, 0);
    assert_eq!(malformed_message_count, 2);

    // 2. LspError::ConnectionLost (EOF) returns ReaderAction::CancelAndBreak
    let action = handle_reader_result(
        Err(&LspError::ConnectionLost),
        &mut consecutive_io_errors,
        &mut malformed_message_count,
        language_id,
    );
    assert_eq!(action, ReaderAction::CancelAndBreak);

    // 3. LspError::Protocol (malformed message) increments malformed_message_count and returns ReaderAction::Continue
    let action = handle_reader_result(
        Err(&LspError::Protocol("test".to_string())),
        &mut consecutive_io_errors,
        &mut malformed_message_count,
        language_id,
    );
    assert_eq!(action, ReaderAction::Continue);
    assert_eq!(malformed_message_count, 3);

    // 4. Other errors increment consecutive_io_errors
    let other_err = LspError::NoLspAvailable;
    consecutive_io_errors = 0;
    for i in 1..MAX_CONSECUTIVE_READER_ERRORS {
        let action = handle_reader_result(
            Err(&other_err),
            &mut consecutive_io_errors,
            &mut malformed_message_count,
            language_id,
        );
        assert_eq!(action, ReaderAction::Continue, "at iteration {i}");
        assert_eq!(consecutive_io_errors, i);
    }

    // At MAX_CONSECUTIVE_READER_ERRORS, it returns ReaderAction::CancelAndBreak
    let action = handle_reader_result(
        Err(&other_err),
        &mut consecutive_io_errors,
        &mut malformed_message_count,
        language_id,
    );
    assert_eq!(action, ReaderAction::CancelAndBreak);
    assert_eq!(consecutive_io_errors, MAX_CONSECUTIVE_READER_ERRORS);
}

#[test]
fn test_setup_rust_env_isolate_disabled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = tokio::process::Command::new("cargo");

    // When isolate_target_dir is false, CARGO_TARGET_DIR should not be set
    super::setup_rust_env(&mut cmd, dir.path(), false, "rust");
    let std_cmd = cmd.as_std();
    assert!(std_cmd.get_envs().next().is_none());
}

#[test]
fn test_setup_go_env_isolate_disabled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = tokio::process::Command::new("go");

    // When isolate_target_dir is false, GOCACHE / GOMODCACHE should not be set
    super::setup_go_env(&mut cmd, dir.path(), false, "go");
    let std_cmd = cmd.as_std();
    assert!(std_cmd.get_envs().next().is_none());
}

#[test]
fn test_setup_typescript_env_isolate_disabled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = tokio::process::Command::new("node");

    // When isolate_target_dir is false, TMPDIR should not be set
    super::setup_typescript_env(&mut cmd, dir.path(), false, "typescript");
    let std_cmd = cmd.as_std();
    assert!(std_cmd.get_envs().next().is_none());
}

#[test]
fn test_setup_python_env_isolate_disabled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cmd = tokio::process::Command::new("python");

    // When isolate_target_dir is false, PYTHONPYCACHEPREFIX should not be set
    super::setup_python_env(&mut cmd, dir.path(), false, "python");
    let std_cmd = cmd.as_std();
    assert!(std_cmd.get_envs().next().is_none());
}

#[test]
fn test_setup_rust_env_dir_creation_failure() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Create a file where target directory would be created
    std::fs::write(dir.path().join("target"), "blocking_file").expect("write file");

    let mut cmd = tokio::process::Command::new("cargo");
    // Should handle create_dir_all failure gracefully without panicking
    super::setup_rust_env(&mut cmd, dir.path(), true, "rust");
}

#[test]
fn test_ensure_pathfinder_in_gitignore_no_trailing_newline() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gitignore = dir.path().join(".gitignore");
    // Write without trailing newline
    std::fs::write(&gitignore, "target").expect("write");

    ensure_pathfinder_in_gitignore(dir.path());

    let content = std::fs::read_to_string(&gitignore).expect("read");
    assert!(
        content.starts_with("target\n"),
        "should add leading newline if file lacks one"
    );
    assert!(content.contains(".pathfinder/"));
}

#[test]
fn test_ensure_pathfinder_in_gitignore_directory_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let gitignore = dir.path().join(".gitignore");
    // Create .gitignore as a directory to trigger read/write error
    std::fs::create_dir_all(&gitignore).expect("create dir");

    // Should log warning and not panic on I/O error
    ensure_pathfinder_in_gitignore(dir.path());
}

#[test]
fn test_resolve_jdtls_data_dir_create_dir_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pathfinder_path = dir.path().join(".pathfinder");
    // Create .pathfinder as a file to block jdtls directory creation
    std::fs::write(&pathfinder_path, "blocking_file").expect("write");

    let (data_dir, lock) = resolve_jdtls_data_dir(dir.path());
    assert_eq!(data_dir, dir.path().join(".pathfinder").join("jdtls-data"));
    assert!(
        lock.is_none(),
        "should return None lock when directory creation fails"
    );
}

#[tokio::test]
async fn test_spawn_and_initialize_failing_spawner() {
    let spawner = MockProcessSpawner::failing();
    let dispatcher = Arc::new(RequestDispatcher::new());
    let dir = tempfile::tempdir().expect("tempdir");

    let params = SpawnParams {
        spawner: &spawner,
        command: "gopls",
        args: &[],
        project_root: dir.path(),
        language_id: "go",
        dispatcher,
        init_timeout_secs: None,
        isolate_target_dir: false,
        plugins: vec![],
        init_options: serde_json::Value::Null,
    };

    let result = spawn_and_initialize(params).await;

    assert!(result.is_err(), "should fail when process spawner fails");
    if let Err(err) = result {
        assert!(
            matches!(err, LspError::Io(_)),
            "expected LspError::Io, got: {err:?}"
        );
    }
}
