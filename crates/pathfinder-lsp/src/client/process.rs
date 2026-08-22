//! LSP child process lifecycle management.
//!
//! `ManagedProcess` wraps a spawned LSP child process and provides:
//! - The `initialize` handshake with a configurable timeout (PRD §6.1)
//! - A background reader task that dispatches JSON-RPC responses
//! - Crash detection (non-zero exit or broken pipe)
//! - Activity timestamp and in-flight request tracking
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
use async_trait::async_trait;
use command_group::AsyncCommandGroup as _;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

/// I/O boundary between `LspClient` and the LSP child process.
///
/// Production: [`ManagedProcess`] (real OS child via `tokio::process`).
/// Tests: `FakeTransport` (in-memory channels, no OS process).
///
/// This trait captures ONLY the operations `LspClient` performs on a running
/// process. Process lifecycle (spawn, shutdown, reap) remains in
/// `ManagedProcess` and is not trait-virtualized — those are tested via
/// integration tests with real child processes.
#[async_trait]
pub(crate) trait LspTransport: Send + Sync {
    /// Write a JSON-RPC message to the process stdin.
    async fn send(&self, message: &Value) -> Result<(), LspError>;

    /// Check if the transport is alive (process still running).
    fn is_alive(&self) -> bool;

    /// Get a snapshot of the detected capabilities.
    #[allow(dead_code)] // Will be used in Phase 3C/3D tests
    fn capabilities(&self) -> DetectedCapabilities;

    /// Terminate the LSP process gracefully.
    ///
    /// Sends `shutdown` + `exit` requests, then force-kills after 2s.
    ///
    /// LSP-INIT-002: `language_id` is used to tag the pending shutdown request
    /// so it can be properly tracked in the per-language dispatcher.
    async fn shutdown(&self, dispatcher: &RequestDispatcher, language_id: &str);
}

/// Abstraction over process spawning for testability.
///
/// Production uses [`RealProcessSpawner`] which calls `tokio::process::Command`.
/// Tests can provide a mock that validates argument construction without
/// spawning real processes.
pub(crate) trait ProcessSpawner: Send + Sync {
    fn spawn(
        &self,
        command: &str,
        args: &[String],
        project_root: &Path,
        language_id: &str,
        isolate_target_dir: bool,
    ) -> Result<(Child, ChildStdin, ChildStdout, Option<std::fs::File>), LspError>;
}

pub(crate) struct RealProcessSpawner;

impl ProcessSpawner for RealProcessSpawner {
    fn spawn(
        &self,
        command: &str,
        args: &[String],
        project_root: &Path,
        language_id: &str,
        isolate_target_dir: bool,
    ) -> Result<(Child, ChildStdin, ChildStdout, Option<std::fs::File>), LspError> {
        spawn_lsp_child(command, args, project_root, language_id, isolate_target_dir)
    }
}

/// A running LSP child process with its I/O handles.
pub(super) struct ManagedProcess {
    /// The child process handle — kept alive until explicitly dropped.
    ///
    /// Wrapped in `Arc<Mutex>` so that:
    /// - `is_alive()` can call `try_wait()` with `&self` (required for trait)
    /// - `ProcessLifecycle` can share the same handle for zombie reaping
    ///
    /// The lock is never contended: single reader task, single supervisor.
    pub(super) child: Arc<tokio::sync::Mutex<Child>>,
    /// Exclusive write handle to the LSP's stdin.
    ///
    /// Wrapped in `Arc` so that `registration_watcher_task` (MT-3) can obtain
    /// a clone and write `client/registerCapability` responses without holding
    /// a reference to the full `ManagedProcess`.
    pub(super) stdin: Arc<Mutex<tokio::io::BufWriter<ChildStdin>>>,
    /// Capabilities negotiated during `initialize`.
    pub(super) capabilities: DetectedCapabilities,
    /// Advisory lock on the jdtls data directory (Java only).
    ///
    /// Held for the lifetime of the jdtls process to prevent concurrent
    /// Pathfinder instances from selecting the same data directory.
    /// Dropping this field releases the advisory lock.
    _jdtls_lock: Option<std::fs::File>,
}

impl ManagedProcess {
    /// Get a shared handle to the child process for lifecycle management.
    ///
    /// Used by `start_process` to create `ProcessLifecycle` alongside the
    /// transport, so that `reader_supervisor_task` and the process supervisor
    /// can reap the OS zombie via `child.wait()`.
    pub(super) fn child_handle(&self) -> Arc<tokio::sync::Mutex<Child>> {
        Arc::clone(&self.child)
    }
}

#[allow(clippy::expect_used)]
#[async_trait]
impl LspTransport for ManagedProcess {
    async fn send(&self, message: &Value) -> Result<(), LspError> {
        // Separate lock-acquisition timeout from write timeout to prevent
        // queueing behind a stuck write. If the lock can't be acquired in 3s,
        // another write is blocking stdin — fail fast instead of waiting up to 10s.
        let mut stdin = tokio::time::timeout(std::time::Duration::from_secs(3), self.stdin.lock())
            .await
            .map_err(|_| LspError::Timeout {
                operation: "send_lock".to_owned(),
                timeout_ms: 3_000,
            })?;

        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            write_message(&mut *stdin, message),
        )
        .await
        .map_err(|_| LspError::Timeout {
            operation: "send".to_owned(),
            timeout_ms: 10_000,
        })?
    }

    fn is_alive(&self) -> bool {
        let Ok(mut child) = self.child.try_lock() else {
            // Lock contended (supervisor is reaping) — conservatively report not-alive.
            // This avoids delaying zombie detection by a supervisor check interval.
            // If the process IS alive, the next check will confirm it.
            return false;
        };
        match child.try_wait() {
            Ok(None) => true,
            Ok(Some(status)) => {
                tracing::debug!(
                    pid = ?child.id(),
                    exit_status = ?status,
                    "ManagedProcess::is_alive: child has exited"
                );
                false
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "ManagedProcess::is_alive: try_wait failed — treating as dead"
                );
                false
            }
        }
    }

    fn capabilities(&self) -> DetectedCapabilities {
        self.capabilities.clone()
    }

    async fn shutdown(&self, dispatcher: &RequestDispatcher, language_id: &str) {
        let (id, rx) = dispatcher.register(language_id);
        let shutdown_req = RequestDispatcher::make_request(id, "shutdown", &Value::Null);
        if let Ok(mut stdin) =
            tokio::time::timeout(std::time::Duration::from_secs(2), self.stdin.lock()).await
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
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
    }
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
/// Parameters for spawning and initializing an LSP process.
pub(super) struct SpawnParams<'a> {
    pub(super) spawner: &'a dyn ProcessSpawner,
    pub(super) command: &'a str,
    pub(super) args: &'a [String],
    pub(super) project_root: &'a Path,
    pub(super) language_id: &'a str,
    pub(super) dispatcher: Arc<RequestDispatcher>,
    pub(super) init_timeout_secs: Option<u64>,
    pub(super) isolate_target_dir: bool,
    pub(super) plugins: Vec<String>,
    pub(super) init_options: serde_json::Value,
}

#[allow(unsafe_code)]
pub(super) async fn spawn_and_initialize(
    params: SpawnParams<'_>,
) -> Result<(ManagedProcess, tokio::task::JoinHandle<()>), LspError> {
    // M-3: Re-validate Python venv path at spawn time. The venv may have been
    // created or deleted since detection. Re-detect and update init_options.
    let init_options = if params.language_id == "python" {
        revalidate_python_init_options(params.init_options, params.project_root)
    } else {
        params.init_options
    };

    let (child, stdin, stdout, jdtls_lock) = params.spawner.spawn(
        params.command,
        params.args,
        params.project_root,
        params.language_id,
        params.isolate_target_dir,
    )?;
    let mut writer = tokio::io::BufWriter::new(stdin);

    // Start the reader task BEFORE writing the initialize request.
    //
    // The reader task reads from stdout and dispatches JSON-RPC responses via
    // the RequestDispatcher. Without it running, the initialize response would
    // sit unread in the stdout pipe buffer forever — the oneshot channel `rx`
    // would never be filled, causing a deadlock.
    //
    // LSP-INIT-002: Pass language_id to enable per-language isolation.
    let reader_handle = start_reader_task(
        stdout,
        Arc::clone(&params.dispatcher),
        params.language_id.to_owned(),
    );

    let (id, rx) = params.dispatcher.register(params.language_id);
    let init_request =
        build_initialize_request(id, params.project_root, &params.plugins, init_options).await?;
    write_message(&mut writer, &init_request).await?;

    let timeout_secs = params.init_timeout_secs.unwrap_or(INIT_TIMEOUT_SECS);
    let response = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), rx)
        .await
        .map_err(|_| {
            params.dispatcher.remove(id);
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
        language = params.language_id,
        definition_provider = capabilities.flags.definition_provider,
        diagnostics_strategy = %capabilities.diagnostics_strategy.as_str(),
        formatting_provider = capabilities.flags.formatting_provider,
        "LSP initialized"
    );

    let process = ManagedProcess {
        child: Arc::new(tokio::sync::Mutex::new(child)),
        stdin: Arc::new(Mutex::new(writer)),
        capabilities,
        _jdtls_lock: jdtls_lock,
    };

    Ok((process, reader_handle))
}

#[allow(unsafe_code)]
fn is_process_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    if let Ok(p) = u32::try_from(pid) {
        if p == std::process::id() {
            return true;
        }
    }

    #[cfg(unix)]
    {
        // SAFETY: kill(pid, 0) is a safe syscall — it sends no signal and
        // only checks process existence. It cannot affect any process state.
        // Returns 0 if process exists, or -1 with EPERM if exists under different user.
        let ret = unsafe { libc::kill(pid, 0) };
        if ret == 0 {
            return true;
        }
        if std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) {
            return true;
        }
    }

    // Fallback: check /proc/{pid} if procfs is available
    let proc_path = std::path::Path::new("/proc");
    if proc_path.exists() {
        return proc_path.join(pid.to_string()).exists();
    }

    #[cfg(not(unix))]
    {
        true
    }
    #[cfg(unix)]
    {
        false
    }
}

fn cleanup_orphaned_jdtls_data_dirs(pathfinder_dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(pathfinder_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if let Some(pid_str) = name.strip_prefix("jdtls-data-") {
                    if let Ok(pid) = pid_str.parse::<i32>() {
                        if !is_process_alive(pid) {
                            tracing::info!(
                                dir = %path.display(),
                                pid,
                                "LSP: cleaning up orphaned jdtls data directory from exited process"
                            );
                            let _ = std::fs::remove_dir_all(&path);
                        }
                    }
                }
            }
        }
    }
}

/// Resolves a unique jdtls `-data` directory for this Pathfinder instance.
///
/// jdtls requires exclusive access to its data directory. When multiple
/// Pathfinder instances target the same workspace, sharing a single
/// `jdtls-data/` directory causes lock conflicts and startup failures.
///
/// Strategy:
/// 1. Try `project_root/.pathfinder/jdtls-data/` with an advisory file lock.
/// 2. If the lock is already held (concurrent instance), fall back to
///    `project_root/.pathfinder/jdtls-data-{pid}/`.
///
/// Returns `(data_dir_path, lock_guard)`. The caller must keep `lock_guard`
/// alive for the lifetime of the jdtls process; dropping it releases the
/// advisory lock.
pub(crate) fn resolve_jdtls_data_dir(
    project_root: &Path,
) -> (std::path::PathBuf, Option<std::fs::File>) {
    let pathfinder_dir = project_root.join(".pathfinder");
    cleanup_orphaned_jdtls_data_dirs(&pathfinder_dir);

    let base_dir = pathfinder_dir.join("jdtls-data");
    if let Err(e) = std::fs::create_dir_all(&base_dir) {
        tracing::error!(
            data_dir = %base_dir.display(),
            error = %e,
            "LSP: failed to create jdtls data directory"
        );
        // Return the base dir anyway — jdtls will error on its own
        return (base_dir, None);
    }

    // Try to acquire an advisory lock on a sentinel file.
    // File::try_lock is stable since Rust 1.84.
    let lock_path = base_dir.join(".pathfinder-lock");
    match std::fs::File::create(&lock_path) {
        Ok(lock_file) => {
            if lock_file.try_lock().is_ok() {
                tracing::info!(
                    data_dir = %base_dir.display(),
                    "LSP: acquired jdtls data directory lock (primary instance)"
                );
                (base_dir, Some(lock_file))
            } else {
                // Lock held by another instance — use PID-suffixed fallback
                let pid = std::process::id();
                let fallback_dir = project_root
                    .join(".pathfinder")
                    .join(format!("jdtls-data-{pid}"));
                if let Err(e) = std::fs::create_dir_all(&fallback_dir) {
                    tracing::error!(
                        data_dir = %fallback_dir.display(),
                        error = %e,
                        "LSP: failed to create fallback jdtls data directory"
                    );
                }
                tracing::info!(
                    data_dir = %fallback_dir.display(),
                    pid,
                    "LSP: using PID-suffixed jdtls data directory (concurrent instance)"
                );
                (fallback_dir, None)
            }
        }
        Err(e) => {
            tracing::warn!(
                lock_path = %lock_path.display(),
                error = %e,
                "LSP: failed to create jdtls lock file — using primary dir without lock"
            );
            (base_dir, None)
        }
    }
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
) -> Result<(Child, ChildStdin, ChildStdout, Option<std::fs::File>), LspError> {
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args)
        .current_dir(project_root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);

    setup_rust_env(&mut cmd, project_root, isolate_target_dir, language_id);
    setup_go_env(&mut cmd, project_root, isolate_target_dir, language_id);
    setup_typescript_env(&mut cmd, project_root, isolate_target_dir, language_id);
    setup_python_env(&mut cmd, project_root, isolate_target_dir, language_id);
    let jdtls_lock = setup_java_env(&mut cmd, project_root, language_id);

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

    Ok((child, stdin, stdout, jdtls_lock))
}

fn setup_rust_env(
    cmd: &mut tokio::process::Command,
    project_root: &Path,
    isolate_target_dir: bool,
    language_id: &str,
) {
    // When concurrent LSP instances are detected for Rust, isolate the build
    // artifacts to avoid cargo cache lock contention. Two rust-analyzer processes
    // sharing the same target/ directory will fight over .cargo-lock and
    // build cache, causing one or both to stall indefinitely during indexing.
    if isolate_target_dir && language_id == "rust" {
        let isolated_target = project_root.join("target").join("pathfinder-lsp");
        // L-1: Pre-create isolation directory to prevent first-write race.
        if let Err(e) = std::fs::create_dir_all(&isolated_target) {
            tracing::warn!(
                language = language_id,
                directory = %isolated_target.display(),
                error = %e,
                "LSP: failed to create isolated target directory — cache contention may occur"
            );
        }
        cmd.env("CARGO_TARGET_DIR", isolated_target);
        tracing::info!(
            language = language_id,
            "LSP: set CARGO_TARGET_DIR to isolated target to avoid cache contention"
        );
    }
}

fn setup_go_env(
    cmd: &mut tokio::process::Command,
    project_root: &Path,
    isolate_target_dir: bool,
    language_id: &str,
) {
    // LSP-HEALTH-001 Task 4.1: gopls cache isolation
    // When concurrent gopls instances are detected, isolate GOCACHE and GOMODCACHE
    // to avoid Go module cache lock contention between IDE's gopls and Pathfinder's gopls.
    if isolate_target_dir && language_id == "go" {
        let isolated_cache = project_root.join(".pathfinder").join("gopls-cache");
        // L-1: Pre-create isolation directories to prevent first-write race.
        let build_cache = isolated_cache.join("build");
        let mod_cache = isolated_cache.join("mod");
        if let Err(e) = std::fs::create_dir_all(&build_cache) {
            tracing::warn!(
                language = language_id,
                directory = %build_cache.display(),
                error = %e,
                "LSP: failed to create isolated gopls build cache — cache contention may occur"
            );
        }
        if let Err(e) = std::fs::create_dir_all(&mod_cache) {
            tracing::warn!(
                language = language_id,
                directory = %mod_cache.display(),
                error = %e,
                "LSP: failed to create isolated gopls mod cache — cache contention may occur"
            );
        }
        cmd.env("GOCACHE", build_cache);
        cmd.env("GOMODCACHE", mod_cache);
        tracing::info!(
            language = language_id,
            "LSP: set isolated GOCACHE/GOMODCACHE for gopls to avoid cache contention"
        );
        ensure_pathfinder_in_gitignore(project_root);
    }
}

fn setup_typescript_env(
    cmd: &mut tokio::process::Command,
    project_root: &Path,
    isolate_target_dir: bool,
    language_id: &str,
) {
    // TypeScript cache isolation: tsserver uses TMPDIR for .tsbuildinfo files.
    // Concurrent tsserver instances (IDE + Pathfinder) sharing the same TMPDIR
    // can corrupt build info files. Isolate to a per-Pathfinder temp directory.
    if isolate_target_dir && language_id == "typescript" {
        let isolated_tmp = project_root.join(".pathfinder").join("tsserver-tmp");
        // L-1: Pre-create isolation directory to prevent first-write race.
        if let Err(e) = std::fs::create_dir_all(&isolated_tmp) {
            tracing::warn!(
                language = language_id,
                directory = %isolated_tmp.display(),
                error = %e,
                "LSP: failed to create isolated tsserver TMPDIR — .tsbuildinfo contention may occur"
            );
        }
        cmd.env("TMPDIR", &isolated_tmp);
        tracing::info!(
            language = language_id,
            "LSP: set isolated TMPDIR for tsserver to avoid .tsbuildinfo contention"
        );
        ensure_pathfinder_in_gitignore(project_root);
    }
}

fn setup_python_env(
    cmd: &mut tokio::process::Command,
    project_root: &Path,
    isolate_target_dir: bool,
    language_id: &str,
) {
    // Python cache isolation: isolate __pycache__ output to avoid conflicts
    // between concurrent pyright/ruff instances.
    if isolate_target_dir && language_id == "python" {
        let isolated_cache = project_root.join(".pathfinder").join("python-cache");
        let pyc_dir = isolated_cache.join("pyc");
        // L-1: Pre-create isolation directory to prevent first-write race.
        if let Err(e) = std::fs::create_dir_all(&pyc_dir) {
            tracing::warn!(
                language = language_id,
                directory = %pyc_dir.display(),
                error = %e,
                "LSP: failed to create isolated Python pycache — cache contention may occur"
            );
        }
        cmd.env("PYTHONPYCACHEPREFIX", &pyc_dir);
        tracing::info!(
            language = language_id,
            "LSP: set isolated PYTHONPYCACHEPREFIX for Python LSP to avoid cache contention"
        );
        ensure_pathfinder_in_gitignore(project_root);
    }
}

fn setup_java_env(
    cmd: &mut tokio::process::Command,
    project_root: &Path,
    language_id: &str,
) -> Option<std::fs::File> {
    // jdtls always needs a unique data directory per workspace — NOT gated on
    // isolate_target_dir because this is a functional requirement, not an
    // isolation concern. Without -data, jdtls fails or shares state between projects.
    // jdtls data directory isolation: resolve a unique data dir with advisory
    // file locking to prevent concurrent Pathfinder instances from colliding.
    if language_id == "java" {
        let (data_dir, lock) = resolve_jdtls_data_dir(project_root);
        cmd.arg("-data").arg(&data_dir);
        tracing::info!(
            language = language_id,
            data_dir = %data_dir.display(),
            "LSP: set jdtls data directory"
        );
        // Ensure .pathfinder/ is in .gitignore (jdtls always creates files here)
        ensure_pathfinder_in_gitignore(project_root);
        lock
    } else {
        None
    }
}

/// Ensure `.pathfinder/` is listed in the project's `.gitignore`.
///
/// Called when cache isolation creates files under `.pathfinder/`.
/// This prevents the isolated cache directories from being tracked by git.
/// The function is idempotent — it checks for existing entries before appending.
fn ensure_pathfinder_in_gitignore(project_root: &Path) {
    use std::io::{Read, Seek, SeekFrom, Write};
    let gitignore_path = project_root.join(".gitignore");

    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&gitignore_path)
    {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(
                path = %gitignore_path.display(),
                error = %e,
                "L-2: Failed to open or create .gitignore — \
                 isolated cache directories may be tracked by git"
            );
            return;
        }
    };

    let mut existing = String::default();
    if let Err(e) = file.read_to_string(&mut existing) {
        tracing::warn!(
            path = %gitignore_path.display(),
            error = %e,
            "L-2: Failed to read .gitignore — \
             isolated cache directories may be tracked by git"
        );
        return;
    }

    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed == ".pathfinder" || trimmed == ".pathfinder/" || trimmed == "/.pathfinder/" {
            return; // Already present
        }
    }

    let mut to_write = String::default();
    if existing.is_empty() {
        to_write.push_str("# Pathfinder LSP cache isolation\n.pathfinder/\n");
    } else {
        if !existing.ends_with('\n') {
            to_write.push('\n');
        }
        to_write.push_str("\n# Pathfinder LSP cache isolation\n.pathfinder/\n");
    }

    if let Err(e) = file
        .seek(SeekFrom::End(0))
        .and_then(|_| file.write_all(to_write.as_bytes()))
    {
        tracing::warn!(
            path = %gitignore_path.display(),
            error = %e,
            "L-2: Failed to append to .gitignore — \
             isolated cache directories may be tracked by git"
        );
    } else {
        tracing::info!(path = %gitignore_path.display(), "Updated .gitignore with .pathfinder/ entry");
    }
}

/// Build the LSP `initialize` request JSON-RPC message.
async fn build_initialize_request(
    id: u64,
    project_root: &Path,
    plugins: &[String],
    init_options: serde_json::Value,
) -> Result<Value, LspError> {
    let workspace_uri = path_to_file_uri(project_root).await?;
    let workspace_name = project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace");

    let mut initialization_options = json!({});

    if !init_options.is_null() {
        if let Some(obj) = init_options.as_object() {
            initialization_options = json!(obj.clone());
        } else {
            initialization_options = init_options;
        }
    }

    if !plugins.is_empty() {
        let plugin_entries: Vec<Value> = plugins
            .iter()
            .map(|name| {
                json!({
                    "name": name
                })
            })
            .collect();

        if let Some(obj) = initialization_options.as_object_mut() {
            obj.insert("plugins".to_owned(), json!(plugin_entries));
            obj.insert(
                "tsserver".to_owned(),
                json!({
                    "extraFileExtensions": [
                        { "extension": "vue", "scriptKind": 3 }
                    ]
                }),
            );
        } else {
            initialization_options = json!({
                "plugins": plugin_entries,
                "tsserver": {
                    "extraFileExtensions": [
                        { "extension": "vue", "scriptKind": 3 }
                    ]
                }
            });
        }
    }

    Ok(RequestDispatcher::make_request(
        id,
        "initialize",
        &json!({
            "processId": std::process::id(),
            "clientInfo": { "name": "pathfinder", "version": env!("CARGO_PKG_VERSION") },
            "rootUri": workspace_uri,
            "workspaceFolders": [{ "uri": workspace_uri, "name": workspace_name }],
            "initializationOptions": initialization_options,
            "capabilities": {
                "textDocument": {
                    "definition": { "dynamicRegistration": false, "linkSupport": false },
                    "references": { "dynamicRegistration": false },
                    "callHierarchy": { "dynamicRegistration": false },
                    "implementation": { "dynamicRegistration": false, "linkSupport": false },
                    "synchronization": {
                        "dynamicRegistration": false,
                        "willSave": false,
                        "willSaveWaitUntil": false,
                        "didSave": false
                    },
                    "publishDiagnostics": { "relatedInformation": false }
                },
                "workspace": {
                    "workspaceFolders": true,
                    "configuration": true,
                    "didChangeWatchedFiles": { "dynamicRegistration": true },
                    "diagnostics": {}
                },
                // Opt into work done progress so LSPs like rust-analyzer send
                // $/progress notifications during initial workspace indexing.
                // Without this, the progress_watcher_task never sees WorkDoneProgressEnd
                // and indexing_complete stays false forever.
                "window": { "workDoneProgress": true },
                "general": {
                    "positionEncodings": ["utf-8", "utf-16"]
                }
            }
        }),
    ))
}

/// Start the background reader task that dispatches incoming messages.
///
/// The task runs until EOF on stdout (i.e., the LSP process exits),
/// then calls `dispatcher.cancel_for_language(language_id)`.
///
/// LSP-INIT-002: `language_id` ensures the reader task:
/// 1. Only dispatches notifications/server-requests to its own language's subscribers
/// 2. Only cancels pending requests from its own language on EOF
///
/// Maximum consecutive non-ConnectionLost IO errors before aborting the reader.
/// Prevents CPU-spin on persistent IO errors (e.g., EBADF after child exits).
/// Protocol errors (malformed messages) do NOT count toward this limit — they
/// are the server's fault, not a broken pipe.
const MAX_CONSECUTIVE_READER_ERRORS: u32 = 5;

/// Action to take after processing a reader result.
#[derive(Debug, PartialEq, Eq)]
enum ReaderAction {
    /// Continue reading messages.
    Continue,
    /// Cancel pending requests and break out of the read loop.
    CancelAndBreak,
}

/// Process the result of `read_message()` and determine the next action.
///
/// This function encapsulates the reader task error handling logic:
/// - Reset `consecutive_io_errors` on successful reads
/// - Increment `malformed_message_count` on protocol errors
/// - Count IO errors toward the consecutive limit
/// - Cancel and break after too many consecutive IO errors
///
/// Returns the action to take in the reader loop.
fn handle_reader_result(
    result: Result<&serde_json::Value, &LspError>,
    consecutive_io_errors: &mut u32,
    malformed_message_count: &mut u32,
    language_id: &str,
) -> ReaderAction {
    match result {
        Ok(_) => {
            *consecutive_io_errors = 0;
            ReaderAction::Continue
        }
        Err(LspError::ConnectionLost) => {
            tracing::info!(
                language = %language_id,
                malformed_messages = *malformed_message_count,
                "LSP stdout EOF — cancelling pending requests for language"
            );
            ReaderAction::CancelAndBreak
        }
        Err(LspError::Protocol(msg)) => {
            *malformed_message_count += 1;
            tracing::warn!(
                error = %msg,
                language = %language_id,
                malformed_message_count = *malformed_message_count,
                error_category = "malformed_message",
                "LSP reader: malformed message from server (protocol violation)"
            );
            ReaderAction::Continue
        }
        Err(e) => {
            *consecutive_io_errors += 1;
            tracing::warn!(
                error = %e,
                language = %language_id,
                consecutive_io_errors = *consecutive_io_errors,
                error_category = "io_error",
                "LSP reader: IO error"
            );
            if *consecutive_io_errors >= MAX_CONSECUTIVE_READER_ERRORS {
                tracing::error!(
                    language = %language_id,
                    consecutive_io_errors = *consecutive_io_errors,
                    malformed_messages = *malformed_message_count,
                    "LSP: too many consecutive IO errors, aborting reader"
                );
                ReaderAction::CancelAndBreak
            } else {
                ReaderAction::Continue
            }
        }
    }
}

pub(super) fn start_reader_task(
    stdout: ChildStdout,
    dispatcher: Arc<RequestDispatcher>,
    language_id: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        let mut consecutive_io_errors: u32 = 0;
        let mut malformed_message_count: u32 = 0;
        loop {
            let result = read_message(&mut reader).await;
            let action = handle_reader_result(
                result.as_ref(),
                &mut consecutive_io_errors,
                &mut malformed_message_count,
                &language_id,
            );

            match action {
                ReaderAction::Continue => {
                    if let Ok(msg) = result {
                        dispatcher.dispatch_response_for_language(&language_id, &msg);
                    }
                }
                ReaderAction::CancelAndBreak => {
                    dispatcher.cancel_for_language(&language_id);
                    break;
                }
            }
        }
    })
}

/// M-3: Re-validate Python venv path at spawn time.
///
/// The `pythonPath` in `init_options` was set at detection time. The venv may
/// have been created (user ran `python -m venv .venv`) or deleted since then.
/// Re-detect and update `init_options` to reflect the current state.
fn revalidate_python_init_options(
    init_options: serde_json::Value,
    project_root: &Path,
) -> serde_json::Value {
    use serde_json::json;

    let current_path = init_options
        .get("python")
        .and_then(|p| p.get("pythonPath"))
        .and_then(|v| v.as_str());

    // Check if the existing path is still valid.
    if let Some(path) = current_path {
        if std::path::Path::new(path).exists() {
            return init_options; // Still valid, no change.
        }
        tracing::info!(
            old_path = path,
            "M-3: Python venv path no longer exists, re-detecting"
        );
    }

    // Re-detect venv from workspace root.
    if let Some(venv_path) = crate::client::detect::detect_venv(project_root) {
        tracing::info!(
            new_path = %venv_path.display(),
            "M-3: Python venv re-detected at spawn time"
        );
        json!({
            "python": {
                "pythonPath": venv_path.to_string_lossy().as_ref()
            }
        })
    } else {
        if current_path.is_some() {
            tracing::info!("M-3: Python venv removed, falling back to system interpreter");
        }
        serde_json::Value::Null
    }
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
//   worst case is the child survives as an orphan (which the process supervisor
//   task will eventually clean up).
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
pub(crate) mod test_mocks {
    use super::{LspError, Path, ProcessSpawner, RealProcessSpawner};
    use std::sync::Mutex;
    use tokio::process::{Child, ChildStdin, ChildStdout};

    #[derive(Clone)]
    pub(crate) struct SpawnCall {
        pub(crate) command: String,
        pub(crate) args: Vec<String>,
        pub(crate) project_root: std::path::PathBuf,
        pub(crate) language_id: String,
        pub(crate) isolate_target_dir: bool,
    }

    /// Controls how `MockProcessSpawner::spawn()` behaves.
    enum SpawnMode {
        /// Always return `Err(LspError::Io(NotFound))`.
        Fail,
        /// Delegate to `RealProcessSpawner` with `sleep 60` — validates
        /// argument construction end-to-end without needing a real LSP binary.
        /// The caller is responsible for killing the child in test teardown.
        Succeed,
    }

    pub(crate) struct MockProcessSpawner {
        pub(crate) spawn_calls: Mutex<Vec<SpawnCall>>,
        mode: SpawnMode,
    }

    impl MockProcessSpawner {
        /// Create a spawner that always fails with `NotFound`.
        /// Use for testing error paths (backoff, `UnavailableState` transitions).
        pub(crate) fn failing() -> Self {
            Self {
                spawn_calls: Mutex::new(Vec::new()),
                mode: SpawnMode::Fail,
            }
        }

        /// Create a spawner that succeeds by delegating to `RealProcessSpawner`
        /// with `sleep 60` as the command. Validates spawn argument construction
        /// (command, args, working dir, process group) end-to-end.
        ///
        /// The spawned `sleep` process must be killed by the test — it will
        /// run for 60 seconds otherwise. Use `child.kill()` in test teardown.
        pub(crate) fn succeeding() -> Self {
            Self {
                spawn_calls: Mutex::new(Vec::new()),
                mode: SpawnMode::Succeed,
            }
        }

        pub(crate) fn call_count(&self) -> usize {
            self.spawn_calls.lock().expect("lock").len()
        }

        pub(crate) fn last_call(&self) -> Option<SpawnCall> {
            self.spawn_calls.lock().expect("lock").last().cloned()
        }
    }

    impl ProcessSpawner for MockProcessSpawner {
        fn spawn(
            &self,
            command: &str,
            args: &[String],
            project_root: &Path,
            language_id: &str,
            isolate_target_dir: bool,
        ) -> Result<(Child, ChildStdin, ChildStdout, Option<std::fs::File>), LspError> {
            self.spawn_calls.lock().expect("lock").push(SpawnCall {
                command: command.to_owned(),
                args: args.to_vec(),
                project_root: project_root.to_owned(),
                language_id: language_id.to_owned(),
                isolate_target_dir,
            });

            match self.mode {
                SpawnMode::Fail => Err(LspError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "mock spawner configured to fail",
                ))),
                SpawnMode::Succeed => {
                    // Delegate to real spawner with a trivial long-running command.
                    // This validates argument construction (command, args, cwd,
                    // process group, stderr null, kill_on_drop) without needing
                    // an actual LSP binary on the test system.
                    let sleep_cmd = which::which("sleep")
                        .map_or_else(|_| "sleep".to_owned(), |p| p.to_string_lossy().into_owned());
                    RealProcessSpawner.spawn(
                        &sleep_cmd,
                        &["60".to_owned()],
                        project_root,
                        language_id,
                        isolate_target_dir,
                    )
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
#[path = "process_test.rs"]
mod process_tests;
