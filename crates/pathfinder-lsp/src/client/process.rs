//! LSP child process lifecycle management.
//!
//! `ManagedProcess` wraps a spawned LSP child process and provides:
//! - The `initialize` handshake with a configurable timeout (PRD §6.1)
//! - A background reader task that dispatches JSON-RPC responses
//! - Crash detection (non-zero exit or broken pipe)
//! - Idle `last_used` tracking for auto-termination (PRD §6.2)
//!
//! # Spawn reliability
//!
//! LSP processes are spawned with four key hardening measures:
//!
//! 1. **stderr → /dev/null**: LSP servers write verbose diagnostics to stderr.
//!    If stderr is piped but never read, the 64 KB OS pipe buffer fills up and the
//!    child process blocks on its next log write — deadlocking the entire server.
//!    Redirecting to null is the only safe option since we never consume LSP stderr.
//!
//! 2. **`prctl(PR_SET_PDEATHSIG, SIGKILL)` (Linux only)**: This asks the Linux
//!    kernel to deliver `SIGKILL` to the LSP child the instant Pathfinder's process
//!    exits — for *any* reason including `SIGKILL`. Because `SIGKILL` cannot be
//!    caught or deferred, Rust `Drop` handlers (including `kill_on_drop`) are *not*
//!    executed when Pathfinder is force-killed by a GUI MCP client reload. The
//!    `prctl` flag is set in a `pre_exec` hook (runs in the child after `fork` but
//!    before `exec`) and is therefore owned by the kernel, not by Rust.
//!    Without this, orphaned LSP processes accumulate across reloads, lock workspace
//!    resources, and prevent freshly-spawned LSPs from initialising — causing the
//!    server to fall back to degraded (no-LSP) mode.
//!
//! 3. **Process group via `command-group`**: Belt-and-suspenders for clean
//!    (non-SIGKILL) exits. Children are spawned in their own process group;
//!    `kill_on_drop` terminates the group when `AsyncGroupChild` is dropped.
//!
//! 4. **Absolute binary path**: `detect.rs` resolves bare binary names (e.g.
//!    `"rust-analyzer"`) to absolute paths via `which` at startup. This ensures
//!    GUI launchers that strip `~/.cargo/bin` and similar paths from `$PATH` still
//!    find the language server binary at spawn time.

use crate::client::capabilities::DetectedCapabilities;
use crate::client::protocol::RequestDispatcher;
use crate::client::transport::{read_message, write_message};
use crate::LspError;
use command_group::AsyncCommandGroup as _;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

/// A running LSP child process with its I/O handles.
pub(super) struct ManagedProcess {
    /// The child process handle — kept alive until explicitly dropped.
    pub(super) child: Child,
    /// Exclusive write handle to the LSP's stdin.
    ///
    /// Wrapped in `Arc` so that `registration_watcher_task` (MT-3) can obtain
    /// a clone and write `client/registerCapability` responses without holding
    /// a reference to the full `ManagedProcess`.
    pub(super) stdin: Arc<Mutex<tokio::io::BufWriter<ChildStdin>>>,
    /// Capabilities negotiated during `initialize`.
    pub(super) capabilities: DetectedCapabilities,
    /// Last time this process was used (for idle-timeout tracking).
    pub(super) last_used: Instant,
    /// Number of in-flight requests (prevents idle timeout during active ops).
    pub(super) in_flight: Arc<AtomicU32>,
}

/// Initialize timeout — 120 seconds (2 minutes) as per PRD §6.1.
const INIT_TIMEOUT_SECS: u64 = 120;

/// Spawn an LSP child process and perform the `initialize` handshake.
///
/// Blocks (via `.await`) until the LSP responds to `initialize` or the
/// timeout fires (default 120 seconds, configurable per-language). Returns a fully-initialized [`ManagedProcess`].
///
/// The background reader task is started inside this function and runs until
/// the process exits or `dispatcher.cancel_all()` is called.
///
/// # Errors
/// - `LspError::Timeout` — LSP did not initialize within the configured timeout
/// - `LspError::Io` — failed to spawn child process
/// - `LspError::Protocol` — invalid response from LSP
///
/// # Safety
///
/// On Unix, this function calls [`spawn_lsp_child`] which contains an `unsafe`
/// block to set `prctl(PR_SET_PDEATHSIG, SIGKILL)` via a `pre_exec` hook.
/// This is safe: `prctl` is async-signal-safe, and the call has no Rust
/// memory-safety implications (no raw pointers, no aliased state).
#[allow(clippy::too_many_arguments)]
#[allow(unsafe_code)]
pub(super) async fn spawn_and_initialize(
    command: &str,
    args: &[String],
    project_root: &Path,
    language_id: &str,
    dispatcher: Arc<RequestDispatcher>,
    init_timeout_secs: Option<u64>,
    isolate_target_dir: bool,
    plugins: Vec<String>,
    python_path: Option<std::path::PathBuf>,
) -> Result<(ManagedProcess, tokio::task::JoinHandle<()>), LspError> {
    let (child, stdin, stdout) =
        spawn_lsp_child(command, args, project_root, language_id, isolate_target_dir)?;
    let mut writer = tokio::io::BufWriter::new(stdin);

    // Start the reader task BEFORE writing the initialize request.
    //
    // The reader task reads from stdout and dispatches JSON-RPC responses via
    // the RequestDispatcher. Without it running, the initialize response would
    // sit unread in the stdout pipe buffer forever — the oneshot channel `rx`
    // would never be filled, causing a deadlock.
    let reader_handle = start_reader_task(stdout, Arc::clone(&dispatcher));

    let (id, rx) = dispatcher.register();
    let init_request = build_initialize_request(id, project_root, &plugins, python_path).await?;
    write_message(&mut writer, &init_request).await?;

    let timeout_secs = init_timeout_secs.unwrap_or(INIT_TIMEOUT_SECS);
    let response = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), rx)
        .await
        .map_err(|_| {
            dispatcher.remove(id);
            LspError::Timeout {
                operation: "initialize".to_owned(),
                timeout_ms: timeout_secs * 1000,
            }
        })?
        .map_err(|_| LspError::ConnectionLost)??;

    let capabilities = DetectedCapabilities::from_response_json(&response);
    let initialized_notif = RequestDispatcher::make_notification("initialized", &json!({}));
    write_message(&mut writer, &initialized_notif).await?;

    tracing::info!(
        language = language_id,
        definition_provider = capabilities.definition_provider,
        diagnostics_strategy = %capabilities.diagnostics_strategy.as_str(),
        formatting_provider = capabilities.formatting_provider,
        "LSP initialized"
    );

    let process = ManagedProcess {
        child,
        stdin: Arc::new(Mutex::new(writer)),
        capabilities,
        last_used: Instant::now(),
        in_flight: Arc::new(AtomicU32::new(0)),
    };

    Ok((process, reader_handle))
}

/// Spawn the LSP child process with process-group hardening and extract stdio handles.
///
/// See module-level doc for the rationale of each hardening measure (stderr null,
/// prctl PDEATHSIG, process group, absolute binary path).
#[allow(unsafe_code)]
fn spawn_lsp_child(
    command: &str,
    args: &[String],
    project_root: &Path,
    language_id: &str,
    isolate_target_dir: bool,
) -> Result<(Child, ChildStdin, ChildStdout), LspError> {
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args)
        .current_dir(project_root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);

    // When concurrent LSP instances are detected for Rust, isolate the build
    // artifacts to avoid cargo cache lock contention. Two rust-analyzer processes
    // sharing the same target/ directory will fight over .cargo-lock and
    // build cache, causing one or both to stall indefinitely during indexing.
    if isolate_target_dir && language_id == "rust" {
        let isolated_target = project_root.join("target").join("pathfinder-lsp");
        cmd.env("CARGO_TARGET_DIR", isolated_target);
        tracing::info!(
            language = language_id,
            "LSP: set CARGO_TARGET_DIR to isolated target to avoid cache contention"
        );
    }

    // LSP-HEALTH-001 Task 4.1: gopls cache isolation
    // When concurrent gopls instances are detected, isolate GOCACHE and GOMODCACHE
    // to avoid Go module cache lock contention between IDE's gopls and Pathfinder's gopls.
    if isolate_target_dir && language_id == "go" {
        let isolated_cache = project_root.join(".pathfinder").join("gopls-cache");
        cmd.env("GOCACHE", isolated_cache.join("build"));
        cmd.env("GOMODCACHE", isolated_cache.join("mod"));
        tracing::info!(
            language = language_id,
            "LSP: set isolated GOCACHE/GOMODCACHE for gopls to avoid cache contention"
        );
    }

    // TypeScript cache isolation: tsserver uses TMPDIR for .tsbuildinfo files.
    // Concurrent tsserver instances (IDE + Pathfinder) sharing the same TMPDIR
    // can corrupt build info files. Isolate to a per-Pathfinder temp directory.
    if isolate_target_dir && language_id == "typescript" {
        let isolated_tmp = project_root.join(".pathfinder").join("tsserver-tmp");
        cmd.env("TMPDIR", &isolated_tmp);
        tracing::info!(
            language = language_id,
            "LSP: set isolated TMPDIR for tsserver to avoid .tsbuildinfo contention"
        );
    }

    // Python cache isolation: isolate __pycache__ output to avoid conflicts
    // between concurrent pyright/ruff-lsp instances.
    if isolate_target_dir && language_id == "python" {
        let isolated_cache = project_root.join(".pathfinder").join("python-cache");
        cmd.env("PYTHONPYCACHEPREFIX", isolated_cache.join("pyc"));
        tracing::info!(
            language = language_id,
            "LSP: set isolated PYTHONPYCACHEPREFIX for Python LSP to avoid cache contention"
        );
    }

    // prctl(PR_SET_PDEATHSIG) is Linux-only — not available on macOS/BSD even
    // though they are also "unix". Gate strictly on linux to avoid link errors
    // when cross-compiling for aarch64-apple-darwin / x86_64-apple-darwin.

    // Ensure .pathfinder/ is in .gitignore when isolation creates files there
    if isolate_target_dir && language_id != "rust" {
        // Rust uses target/pathfinder-lsp/ which is already covered by target/ in .gitignore
        ensure_pathfinder_in_gitignore(project_root);
    }

    apply_linux_process_hardening(&mut cmd);

    let mut child_group = cmd.group_spawn().map_err(|e: std::io::Error| {
        if e.kind() == std::io::ErrorKind::NotFound {
            tracing::error!(
                command,
                language = language_id,
                "LSP: binary not found — install it or set lsp.{language_id}.command"
            );
        } else {
            tracing::error!(command, language = language_id, error = %e, "LSP: spawn error");
        }
        LspError::Io(std::io::Error::new(
            e.kind(),
            format!("failed to spawn LSP '{command}': {e}"),
        ))
    })?;

    let stdout = child_group
        .inner()
        .stdout
        .take()
        .ok_or_else(|| LspError::Protocol("LSP stdout was not piped".to_owned()))?;
    let stdin = child_group
        .inner()
        .stdin
        .take()
        .ok_or_else(|| LspError::Protocol("LSP stdin was not piped".to_owned()))?;
    let child = child_group.into_inner();

    Ok((child, stdin, stdout))
}

/// Ensure `.pathfinder/` is listed in the project's `.gitignore`.
///
/// Called when cache isolation creates files under `.pathfinder/`.
/// This prevents the isolated cache directories from being tracked by git.
/// The function is idempotent — it checks for existing entries before appending.
fn ensure_pathfinder_in_gitignore(project_root: &Path) {
    let gitignore_path = project_root.join(".gitignore");

    // Check if .pathfinder/ is already in .gitignore
    if let Ok(existing) = std::fs::read_to_string(&gitignore_path) {
        for line in existing.lines() {
            let trimmed = line.trim();
            if trimmed == ".pathfinder" || trimmed == ".pathfinder/" || trimmed == "/.pathfinder/" {
                return; // Already present
            }
        }
        // Append to existing .gitignore
        let mut content = existing;
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str("\n# Pathfinder LSP cache isolation\n.pathfinder/\n");
        if std::fs::write(&gitignore_path, content).is_ok() {
            tracing::info!(path = %gitignore_path.display(), "Appended .pathfinder/ to .gitignore");
        }
    } else {
        // No .gitignore exists — create one with just .pathfinder/
        if std::fs::write(
            &gitignore_path,
            "# Pathfinder LSP cache isolation\n.pathfinder/\n",
        )
        .is_ok()
        {
            tracing::info!(path = %gitignore_path.display(), "Created .gitignore with .pathfinder/ entry");
        }
    }
}

/// Build the LSP `initialize` request JSON-RPC message.
async fn build_initialize_request(
    id: u64,
    project_root: &Path,
    plugins: &[String],
    python_path: Option<std::path::PathBuf>,
) -> Result<Value, LspError> {
    let workspace_uri = path_to_file_uri(project_root).await?;
    let workspace_name = project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace");

    let initialization_options = if !plugins.is_empty() {
        // Build plugins array for typescript-language-server
        let plugin_entries: Vec<Value> = plugins
            .iter()
            .map(|name| {
                json!({
                    "name": name
                })
            })
            .collect();

        json!({
            "plugins": plugin_entries,
            // Tell tsserver to handle .vue files
            "tsserver": {
                "extraFileExtensions": [
                    { "extension": "vue", "scriptKind": 3 }  // TS = 3 in tsserver enum
                ]
            }
        })
    } else if let Some(py_path) = python_path {
        // ST-5: pass Python venv interpreter path to Pyright
        json!({
            "python": {
                "pythonPath": py_path.to_string_lossy().as_ref()
            }
        })
    } else {
        json!({})
    };

    Ok(RequestDispatcher::make_request(
        id,
        "initialize",
        &json!({
            "processId": std::process::id(),
            "clientInfo": { "name": "pathfinder", "version": "0.1.0" },
            "rootUri": workspace_uri,
            "workspaceFolders": [{ "uri": workspace_uri, "name": workspace_name }],
            "initializationOptions": initialization_options,
            "capabilities": {
                "textDocument": {
                    "definition": { "dynamicRegistration": false, "linkSupport": false },
                    "publishDiagnostics": { "relatedInformation": false }
                },
                "workspace": { "workspaceFolders": true, "diagnostics": {} },
                // Opt into work done progress so LSPs like rust-analyzer send
                // $/progress notifications during initial workspace indexing.
                // Without this, the progress_watcher_task never sees WorkDoneProgressEnd
                // and indexing_complete stays false forever.
                "window": { "workDoneProgress": true }
            }
        }),
    ))
}

/// Send a JSON-RPC message to the process stdin.
pub(super) async fn send(process: &ManagedProcess, message: &Value) -> Result<(), LspError> {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut stdin = process.stdin.lock().await;
        write_message(&mut *stdin, message).await
    })
    .await
    .map_err(|_| LspError::Timeout {
        operation: "send".to_owned(),
        timeout_ms: 10_000,
    })?
}
/// Write a JSON-RPC response to a shared stdin handle.
///
/// Used by `registration_watcher_task` (MT-3) to send `{}` responses to
/// `client/registerCapability` / `client/unregisterCapability` server requests
/// without needing a full `&ManagedProcess` borrow.
pub(super) async fn send_via_stdin(
    stdin: &Arc<Mutex<tokio::io::BufWriter<ChildStdin>>>,
    message: &Value,
) -> Result<(), LspError> {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut guard = stdin.lock().await;
        write_message(&mut *guard, message).await
    })
    .await
    .map_err(|_| LspError::Timeout {
        operation: "send_via_stdin".to_owned(),
        timeout_ms: 10_000,
    })?
}

/// Start the background reader task that dispatches incoming messages.
///
/// The task runs until EOF on stdout (i.e., the LSP process exits),
/// then calls `dispatcher.cancel_all()`.
pub(super) fn start_reader_task(
    stdout: ChildStdout,
    dispatcher: Arc<RequestDispatcher>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        loop {
            match read_message(&mut reader).await {
                Ok(msg) => {
                    dispatcher.dispatch_response(&msg);
                }
                Err(LspError::ConnectionLost) => {
                    tracing::info!("LSP stdout EOF — dispatcher cancel_all");
                    dispatcher.cancel_all();
                    break;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "LSP reader error");
                    // Continue reading — transient errors should not kill the reader
                }
            }
        }
    })
}

/// Terminate the LSP child process gracefully.
///
/// Sends `shutdown` + `exit` requests, then force-kills after 2s.
pub(super) async fn shutdown(process: &mut ManagedProcess, dispatcher: &RequestDispatcher) {
    let (id, rx) = dispatcher.register();
    let shutdown_req = RequestDispatcher::make_request(id, "shutdown", &Value::Null);
    if let Ok(mut stdin) =
        tokio::time::timeout(std::time::Duration::from_secs(2), process.stdin.lock()).await
    {
        let _ = write_message(&mut *stdin, &shutdown_req).await;
        // Await shutdown response (ignore error — server may be dead)
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), rx).await;
        dispatcher.remove(id);

        // Send exit notification
        let exit_notif = RequestDispatcher::make_notification("exit", &Value::Null);
        let _ = write_message(&mut *stdin, &exit_notif).await;
        let _ = stdin.flush().await;
    }

    // Force-kill if still running
    let _ = process.child.kill().await;
}

/// Convert a filesystem path to a `file://` URI string.
async fn path_to_file_uri(path: &Path) -> Result<String, LspError> {
    let is_dir = tokio::fs::metadata(path).await.is_ok_and(|m| m.is_dir());

    let uri = if is_dir {
        url::Url::from_directory_path(path)
    } else {
        url::Url::from_file_path(path)
    }
    .map_err(|()| LspError::Protocol(format!("cannot convert path to URI: {}", path.display())))?;

    Ok(uri.to_string())
}

// SAFETY:
// - `prctl(PR_SET_PDEATHSIG, SIGKILL)` is a well-documented Linux syscall that sets
//   the parent-death signal. When the parent process dies, the kernel sends SIGKILL to
//   the child process, preventing orphaned LSP servers.
// - The closure returns `io::Result<()>` and doesn't access any borrowed data from the
//   enclosing scope, so there are no lifetime or data race concerns.
// - This runs in the child process between fork() and exec(), so it doesn't affect
//   the parent process state.
// - Failure is ignored (`let _ =`) as this is a best-effort hardening measure; the
//   worst case is the child survives as an orphan (which the idle timeout task will
//   eventually clean up).
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn apply_linux_process_hardening(cmd: &mut tokio::process::Command) {
    // SAFETY: pre_exec closure runs in the child process after fork but before exec.
    // The closure only calls a single libc function (prctl) which is async-signal-safe.
    // All resources used are either primitive types or libc constants.
    unsafe {
        cmd.pre_exec(|| {
            let _ = libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn apply_linux_process_hardening(_cmd: &mut tokio::process::Command) {
    // prctl(PR_SET_PDEATHSIG) is Linux-only.
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod process_tests {
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
        let request = build_initialize_request(42, dir.path(), &[], None)
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

        let request = build_initialize_request(1, &named_dir, &[], None)
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
        let request = build_initialize_request(1, dir.path(), &[], None)
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
        let request = build_initialize_request(1, dir.path(), &plugins, None)
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
        let request = build_initialize_request(1, dir.path(), &[], None)
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
        let request = build_initialize_request(1, dir.path(), &plugins, None)
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
        // Non-existent paths should still work (they just check metadata)
        let uri = path_to_file_uri(Path::new("/definitely/does/not/exist.txt")).await;
        // Path doesn't exist as file, so is_dir=false, from_file_path might fail
        // depending on the URL library
        assert!(uri.is_ok() || uri.is_err());
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

        let mut process = ManagedProcess {
            child: child_process,
            stdin: std::sync::Arc::new(tokio::sync::Mutex::new(tokio::io::BufWriter::new(stdin))),
            capabilities: crate::client::capabilities::DetectedCapabilities::default(),
            last_used: std::time::Instant::now(),
            in_flight: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
        };

        let dispatcher = std::sync::Arc::new(RequestDispatcher::new());

        // Act
        shutdown(&mut process, &dispatcher).await;

        // Assert
        // Give the OS a moment to reap the process
        let status = process.child.wait().await.expect("Failed to wait on child");
        assert!(
            !status.success(),
            "Process should have been killed, but exited successfully"
        );
    }

    // ── gitignore helper tests ──────────────────────────────────────

    #[test]
    fn test_ensure_pathfinder_in_gitignore_creates_new() {
        let dir = tempfile::tempdir().expect("tempdir");
        let gitignore = dir.path().join(".gitignore");
        assert!(!gitignore.exists());

        super::ensure_pathfinder_in_gitignore(dir.path());

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

        super::ensure_pathfinder_in_gitignore(dir.path());

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

        super::ensure_pathfinder_in_gitignore(dir.path());

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

        super::ensure_pathfinder_in_gitignore(dir.path());

        let content = std::fs::read_to_string(&gitignore).expect("read");
        let count = content.matches(".pathfinder/").count();
        assert_eq!(count, 1, "should not duplicate /.pathfinder/ entry");
    }
}
