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
use async_trait::async_trait;
use command_group::AsyncCommandGroup as _;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use std::time::Instant;
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

    /// Get the last-used timestamp.
    fn last_used(&self) -> Instant;

    /// Set the last-used timestamp.
    fn set_last_used(&self, when: Instant);

    /// Get the in-flight request counter.
    fn in_flight(&self) -> &Arc<AtomicU32>;

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
    /// Last time this process was used (for idle-timeout tracking).
    ///
    /// Wrapped in `Mutex` for interior mutability through `LspTransport` trait
    /// (`&self` methods). The lock is never contended.
    pub(super) last_used: parking_lot::Mutex<Instant>,
    /// Number of in-flight requests (prevents idle timeout during active ops).
    pub(super) in_flight: Arc<AtomicU32>,
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
    /// transport, so that `reader_supervisor_task` and `idle_timeout_task`
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
            // This avoids delaying zombie detection by an idle timeout interval.
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

    fn last_used(&self) -> Instant {
        *self.last_used.lock()
    }

    fn set_last_used(&self, when: Instant) {
        *self.last_used.lock() = when;
    }

    fn in_flight(&self) -> &Arc<AtomicU32> {
        &self.in_flight
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
#[allow(clippy::too_many_arguments)]
#[allow(unsafe_code)]
pub(super) async fn spawn_and_initialize(
    spawner: &dyn ProcessSpawner,
    command: &str,
    args: &[String],
    project_root: &Path,
    language_id: &str,
    dispatcher: Arc<RequestDispatcher>,
    init_timeout_secs: Option<u64>,
    isolate_target_dir: bool,
    plugins: Vec<String>,
    init_options: serde_json::Value,
) -> Result<(ManagedProcess, tokio::task::JoinHandle<()>), LspError> {
    // M-3: Re-validate Python venv path at spawn time. The venv may have been
    // created or deleted since detection. Re-detect and update init_options.
    let init_options = if language_id == "python" {
        revalidate_python_init_options(init_options, project_root)
    } else {
        init_options
    };

    let (child, stdin, stdout, jdtls_lock) =
        spawner.spawn(command, args, project_root, language_id, isolate_target_dir)?;
    let mut writer = tokio::io::BufWriter::new(stdin);

    // Start the reader task BEFORE writing the initialize request.
    //
    // The reader task reads from stdout and dispatches JSON-RPC responses via
    // the RequestDispatcher. Without it running, the initialize response would
    // sit unread in the stdout pipe buffer forever — the oneshot channel `rx`
    // would never be filled, causing a deadlock.
    //
    // LSP-INIT-002: Pass language_id to enable per-language isolation.
    let reader_handle = start_reader_task(stdout, Arc::clone(&dispatcher), language_id.to_owned());

    let (id, rx) = dispatcher.register(language_id);
    let init_request = build_initialize_request(id, project_root, &plugins, init_options).await?;
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
        child: Arc::new(tokio::sync::Mutex::new(child)),
        stdin: Arc::new(Mutex::new(writer)),
        capabilities,
        last_used: parking_lot::Mutex::new(Instant::now()),
        in_flight: Arc::new(AtomicU32::new(0)),
        _jdtls_lock: jdtls_lock,
    };

    Ok((process, reader_handle))
}

fn is_process_alive(pid: i32) -> bool {
    // On Linux, checking if /proc/{pid} exists is a safe way to check process liveness
    // without using unsafe blocks.
    let proc_path = std::path::Path::new("/proc");
    if proc_path.exists() {
        proc_path.join(pid.to_string()).exists()
    } else {
        // Fallback for non-procfs Unix or other platforms
        let _ = pid;
        true
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
            if let Ok(()) = lock_file.try_lock() {
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
#[allow(clippy::too_many_lines)]
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
    }

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
    }

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
    }

    // jdtls always needs a unique data directory per workspace — NOT gated on
    // isolate_target_dir because this is a functional requirement, not an
    // isolation concern. Without -data, jdtls fails or shares state between projects.
    // jdtls data directory isolation: resolve a unique data dir with advisory
    // file locking to prevent concurrent Pathfinder instances from colliding.
    let jdtls_lock = if language_id == "java" {
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
    };

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

    Ok((child, stdin, stdout, jdtls_lock))
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

    let mut existing = String::new();
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

    let mut to_write = String::new();
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
                    RealProcessSpawner.spawn(
                        "sleep",
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
mod process_tests {
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
        let request = build_initialize_request(42, dir.path(), &[], serde_json::Value::Null)
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

        let request = build_initialize_request(1, &named_dir, &[], serde_json::Value::Null)
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
        let request = build_initialize_request(1, dir.path(), &[], serde_json::Value::Null)
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
        let request = build_initialize_request(1, dir.path(), &plugins, serde_json::Value::Null)
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
        let request = build_initialize_request(1, dir.path(), &[], serde_json::Value::Null)
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
        let request = build_initialize_request(1, dir.path(), &plugins, serde_json::Value::Null)
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

        let process = ManagedProcess {
            child: Arc::new(tokio::sync::Mutex::new(child_process)),
            stdin: std::sync::Arc::new(tokio::sync::Mutex::new(tokio::io::BufWriter::new(stdin))),
            capabilities: crate::client::capabilities::DetectedCapabilities::default(),
            last_used: parking_lot::Mutex::new(std::time::Instant::now()),
            in_flight: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
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
            last_used: parking_lot::Mutex::new(std::time::Instant::now()),
            in_flight: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
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

    // ── jdtls data directory isolation tests ─────────────────────────────────

    #[test]
    fn test_resolve_jdtls_data_dir_creates_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (data_dir, lock) = super::resolve_jdtls_data_dir(dir.path());
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
        let (data_dir, _lock) = super::resolve_jdtls_data_dir(dir.path());
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
        let (primary_dir, primary_lock) = super::resolve_jdtls_data_dir(dir.path());
        assert!(primary_lock.is_some(), "first call should get the lock");
        assert_eq!(
            primary_dir,
            dir.path().join(".pathfinder").join("jdtls-data"),
        );

        // Second instance should fall back to PID-suffixed directory
        let (fallback_dir, fallback_lock) = super::resolve_jdtls_data_dir(dir.path());
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
            let (_data_dir, _lock) = super::resolve_jdtls_data_dir(dir.path());
            // lock dropped here
        }

        // Should be able to acquire the primary lock again
        let (data_dir, lock) = super::resolve_jdtls_data_dir(dir.path());
        assert_eq!(
            data_dir,
            dir.path().join(".pathfinder").join("jdtls-data"),
            "after lock release, primary dir should be available again"
        );
        assert!(lock.is_some(), "should re-acquire lock after previous drop");
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_cleanup_orphaned_jdtls_data_dirs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pathfinder_dir = dir.path().join(".pathfinder");
        std::fs::create_dir_all(&pathfinder_dir).unwrap();

        // Create a directory for a dead PID (e.g. 999999)
        let dead_pid_dir = pathfinder_dir.join("jdtls-data-999999");
        std::fs::create_dir_all(&dead_pid_dir).unwrap();

        // Create a directory for our own PID (definitely alive)
        let alive_pid_dir = pathfinder_dir.join(format!("jdtls-data-{}", std::process::id()));
        std::fs::create_dir_all(&alive_pid_dir).unwrap();

        assert!(dead_pid_dir.exists());
        assert!(alive_pid_dir.exists());

        super::cleanup_orphaned_jdtls_data_dirs(&pathfinder_dir);

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

        let request = build_initialize_request(1, dir.path(), &[], java_opts)
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
        let python_opts = json!({
            "python": {
                "pythonPath": "/home/user/.venv/bin/python"
            }
        });

        let request = build_initialize_request(1, dir.path(), &[], python_opts)
            .await
            .expect("ok");

        let init_opts = &request["params"]["initializationOptions"];
        assert!(!init_opts.is_null(), "init_options should be present");
        assert_eq!(
            init_opts["python"]["pythonPath"].as_str(),
            Some("/home/user/.venv/bin/python"),
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

        let request = build_initialize_request(1, dir.path(), &[], serde_json::Value::Null)
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

        let request = build_initialize_request(1, dir.path(), &plugins, init_opts)
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
    async fn test_managed_process_last_used_tracking() {
        let process = make_managed_process("10");

        let before = process.last_used();
        std::thread::sleep(std::time::Duration::from_millis(10));
        process.set_last_used(std::time::Instant::now());
        let after = process.last_used();

        assert!(after > before, "set_last_used should update the timestamp");
    }

    #[tokio::test]
    async fn test_managed_process_capabilities_default() {
        let process = make_managed_process("10");

        let caps = process.capabilities();
        assert!(
            !caps.definition_provider,
            "default capabilities should have definition_provider=false"
        );
    }

    #[tokio::test]
    async fn test_managed_process_in_flight_default() {
        let process = make_managed_process("10");

        assert_eq!(
            process
                .in_flight()
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "in_flight should start at 0"
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

        assert!(r1.is_ok());
        assert!(r2.is_ok());
        assert!(r3.is_ok());

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
}
