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
//! LSP processes are spawned with three key hardening measures:
//!
//! 1. **stderr → /dev/null**: LSP servers write verbose diagnostics to stderr.
//!    If stderr is piped but never read, the 64 KB OS pipe buffer fills up and the
//!    child process blocks on its next log write — deadlocking the entire server.
//!    Redirecting to null is the only safe option since we never consume LSP stderr.
//!
//! 2. **Process group via `command-group`**: Children are spawned in their own
//!    process group. When Pathfinder exits — even via SIGKILL where Rust `Drop`
//!    handlers do not run — the OS terminates the entire group, preventing orphaned
//!    LSP processes that would hold file locks (e.g. `.rust-analyzer/` directories).
//!
//! 3. **Absolute binary path**: `detect.rs` resolves bare binary names (e.g.
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
pub(super) async fn spawn_and_initialize(
    command: &str,
    args: &[String],
    project_root: &Path,
    language_id: &str,
    dispatcher: Arc<RequestDispatcher>,
    init_timeout_secs: Option<u64>,
) -> Result<(ManagedProcess, ChildStdout), LspError> {
    // Spawn the child into its own process group with piped stdio.
    //
    // Process group (command-group): when Pathfinder exits — even via SIGKILL
    // where Rust Drop handlers do not run — the OS sends termination to the
    // entire group, preventing orphaned LSP processes that hold file locks.
    //
    // stderr → null: a piped-but-never-read stderr fills its 64 KB OS buffer
    // and blocks the child on the next log write, deadlocking the server.
    // See module-level doc for full rationale.
    let mut child_group = tokio::process::Command::new(command)
        .args(args)
        .current_dir(project_root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        // Redirect stderr to /dev/null — see module-level doc for rationale.
        .stderr(std::process::Stdio::null())
        // Belt-and-suspenders: also kill on Drop for clean (non-SIGKILL) exits.
        .kill_on_drop(true)
        .group_spawn()
        .map_err(|e: std::io::Error| {
            // Emit a targeted diagnostic so operators know exactly what to fix.
            if e.kind() == std::io::ErrorKind::NotFound {
                tracing::error!(
                    command = command,
                    language = language_id,
                    "LSP: binary not found — ensure the language server is installed \
                     and on your PATH, or set `lsp.{language_id}.command` in \
                     .pathfinder.toml to an absolute path"
                );
            } else if e.kind() == std::io::ErrorKind::PermissionDenied {
                tracing::error!(
                    command = command,
                    language = language_id,
                    "LSP: permission denied spawning binary — check file permissions"
                );
            } else {
                tracing::error!(
                    command = command,
                    language = language_id,
                    error = %e,
                    "LSP: unexpected spawn error"
                );
            }
            LspError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to spawn LSP '{command}': {e}"),
            ))
        })?;

    // Extract stdio handles from the AsyncGroupChild before converting to plain Child.
    // `group_spawn()` returns an AsyncGroupChild wrapping the inner tokio Child;
    // we take the stdio handles first, then call `into_inner()` to recover
    // the plain `tokio::process::Child` that ManagedProcess stores for liveness checks.
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

    let mut writer = tokio::io::BufWriter::new(stdin);

    // Build workspace URI string (file:///path/to/workspace/)
    let workspace_uri = path_to_file_uri(project_root).await?;
    let workspace_name = project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace");

    // Build initialize request manually to avoid lsp-types URI type issues
    let (id, rx) = dispatcher.register();
    let init_request = RequestDispatcher::make_request(
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
                "workspace": {
                    "workspaceFolders": true,
                    "diagnostics": true
                }
            }
        }),
    );
    write_message(&mut writer, &init_request).await?;

    // Use configured timeout or default (120 seconds)
    let timeout_secs = init_timeout_secs.unwrap_or(INIT_TIMEOUT_SECS);

    // Await the `initialize` response with configured timeout
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

    // Parse capabilities from the initialize result
    let capabilities = DetectedCapabilities::from_response_json(&response);

    // Send `initialized` notification NOW, so the server can complete setup
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

    Ok((process, stdout))
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
    let is_dir = tokio::fs::metadata(path)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false);

    let uri = if is_dir {
        url::Url::from_directory_path(path)
    } else {
        url::Url::from_file_path(path)
    }
    .map_err(|()| LspError::Protocol(format!("cannot convert path to URI: {}", path.display())))?;

    Ok(uri.to_string())
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
}
