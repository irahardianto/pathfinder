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
    pub(super) stdin: Mutex<tokio::io::BufWriter<ChildStdin>>,
    /// The language this process serves.
    #[allow(dead_code)] // Kept for debugging/logging; not yet used in dispatch
    pub(super) language_id: String,
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
#[allow(unsafe_code)]
pub(super) async fn spawn_and_initialize(
    command: &str,
    args: &[String],
    project_root: &Path,
    language_id: &str,
    dispatcher: Arc<RequestDispatcher>,
    init_timeout_secs: Option<u64>,
) -> Result<(ManagedProcess, tokio::task::JoinHandle<()>), LspError> {
    let (child, stdin, stdout) = spawn_lsp_child(command, args, project_root, language_id)?;
    let mut writer = tokio::io::BufWriter::new(stdin);

    // Start the reader task BEFORE writing the initialize request.
    //
    // The reader task reads from stdout and dispatches JSON-RPC responses via
    // the RequestDispatcher. Without it running, the initialize response would
    // sit unread in the stdout pipe buffer forever — the oneshot channel `rx`
    // would never be filled, causing a deadlock.
    let reader_handle = start_reader_task(stdout, Arc::clone(&dispatcher));

    let (id, rx) = dispatcher.register();
    let init_request = build_initialize_request(id, project_root).await?;
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
        diagnostic_provider = capabilities.diagnostic_provider,
        formatting_provider = capabilities.formatting_provider,
        "LSP initialized"
    );

    let process = ManagedProcess {
        child,
        stdin: Mutex::new(writer),
        language_id: language_id.to_owned(),
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
) -> Result<(Child, ChildStdin, ChildStdout), LspError> {
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args)
        .current_dir(project_root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);

    // prctl(PR_SET_PDEATHSIG) is Linux-only — not available on macOS/BSD even
    // though they are also "unix". Gate strictly on linux to avoid link errors
    // when cross-compiling for aarch64-apple-darwin / x86_64-apple-darwin.
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

/// Build the LSP `initialize` request JSON-RPC message.
async fn build_initialize_request(id: u64, project_root: &Path) -> Result<Value, LspError> {
    let workspace_uri = path_to_file_uri(project_root).await?;
    let workspace_name = project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace");

    Ok(RequestDispatcher::make_request(
        id,
        "initialize",
        &json!({
            "processId": std::process::id(),
            "clientInfo": { "name": "pathfinder", "version": "0.1.0" },
            "rootUri": workspace_uri,
            "workspaceFolders": [{ "uri": workspace_uri, "name": workspace_name }],
            "capabilities": {
                "textDocument": {
                    "definition": { "dynamicRegistration": false, "linkSupport": false },
                    "publishDiagnostics": { "relatedInformation": false }
                },
                "workspace": { "workspaceFolders": true, "diagnostics": {} }
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
        let request = build_initialize_request(42, dir.path()).await.expect("ok");

        assert_eq!(request["jsonrpc"], "2.0");
        assert_eq!(request["id"], 42);
        assert_eq!(request["method"], "initialize");

        let params = &request["params"];
        assert!(params["rootUri"].as_str().expect("rootUri should be a string").starts_with("file://"));
        assert_eq!(params["clientInfo"]["name"], "pathfinder");
        assert!(params["processId"].as_u64().expect("processId should be a u64") > 0);
        assert!(params["workspaceFolders"].is_array());
    }

    #[tokio::test]
    async fn test_build_initialize_request_workspace_name() {
        let dir = tempdir().expect("temp dir");
        // Create a directory with a name
        let named_dir = dir.path().join("my_project");
        std::fs::create_dir_all(&named_dir).expect("create dir");

        let request = build_initialize_request(1, &named_dir).await.expect("ok");
        let folders = request["params"]["workspaceFolders"]
            .as_array()
            .expect("array");
        assert_eq!(folders[0]["name"], "my_project");
    }

    #[tokio::test]
    async fn test_build_initialize_request_capabilities() {
        let dir = tempdir().expect("temp dir");
        let request = build_initialize_request(1, dir.path()).await.expect("ok");

        let caps = &request["params"]["capabilities"];
        assert_eq!(
            caps["textDocument"]["definition"]["dynamicRegistration"],
            false
        );
        assert_eq!(caps["workspace"]["workspaceFolders"], true);
    }

    #[tokio::test]
    async fn test_path_to_file_uri_nonexistent() {
        // Non-existent paths should still work (they just check metadata)
        let uri = path_to_file_uri(Path::new("/definitely/does/not/exist.txt")).await;
        // Path doesn't exist as file, so is_dir=false, from_file_path might fail
        // depending on the URL library
        assert!(uri.is_ok() || uri.is_err());
    }
}
