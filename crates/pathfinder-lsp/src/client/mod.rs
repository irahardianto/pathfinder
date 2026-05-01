//! `LspClient` — the production [`Lawyer`] implementation.
//!
//! `LspClient` manages a pool of LSP child processes (one per language).
//! Processes are started lazily on first use and terminated automatically
//! after an idle timeout.
//!
//! # Crash Recovery (PRD §6.3)
//! When a crash is detected the client restarts the process with exponential
//! back-off (1s → 2s → 4s, 3 attempts). After 3 failures the language is
//! marked `LSP_UNAVAILABLE` and all subsequent calls degrade gracefully.

mod capabilities;
mod detect;
mod process;
mod protocol;
mod transport;

pub use capabilities::{DetectedCapabilities, DiagnosticsStrategy};
pub use detect::{detect_languages, language_id_for_extension, DetectionResult, LanguageLsp, MissingLanguage};
pub use detect::{install_hint};

use crate::types::{CallHierarchyCall, CallHierarchyItem, LspDiagnostic, LspDiagnosticSeverity};
use crate::{DefinitionLocation, Lawyer, LspError};
use async_trait::async_trait;
use dashmap::DashMap;
use detect::LanguageLsp as LspDescriptor;
use process::{send, shutdown, spawn_and_initialize, ManagedProcess};
use protocol::RequestDispatcher;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use url::Url;

/// Default idle timeout: 15 minutes for standard LSPs.
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_mins(15);
/// Maximum restart attempts before marking a language as unavailable.
const MAX_RESTART_ATTEMPTS: u32 = 3;
/// Grace period between idle checks.
const IDLE_CHECK_INTERVAL: Duration = Duration::from_mins(1);
/// Recovery cooldown: time to wait before retrying a permanently unavailable LSP.
const RECOVERY_COOLDOWN: Duration = Duration::from_mins(5);

struct LanguageState {
    /// The running LSP process.
    process: ManagedProcess,
    /// Background reader task handle.
    reader_handle: tokio::task::JoinHandle<()>,
    /// Number of times we have restarted this LSP (used in M3 crash recovery UI).
    restart_count: u32,
    /// When this process was first spawned successfully.
    ///
    /// Used to compute `uptime_seconds` in `capability_status()`.
    spawned_at: Instant,
    /// Whether the LSP has finished initial workspace indexing.
    ///
    /// Set to `true` when the reader task observes a `$/progress` notification
    /// carrying a `WorkDoneProgressEnd` token for the initial indexing job
    /// (title typically contains "Indexing" or "cargo check" for rust-analyzer).
    /// Never reset to `false` once set — processes don't re-index unless restarted.
    indexing_complete: Arc<std::sync::atomic::AtomicBool>,
}

/// Marks a language as permanently unavailable after repeated crashes.
struct UnavailableState {
    /// When this language was marked unavailable (for cooldown recovery).
    unavailable_since: Instant,
}

enum ProcessEntry {
    /// Active LSP process. Boxed to equalise variant sizes.
    Running(Box<LanguageState>),
    Unavailable(UnavailableState),
}

impl ProcessEntry {
    fn to_validation_status(&self, command: &str) -> crate::types::LspLanguageStatus {
        match self {
            Self::Running(state) => {
                let caps = &state.process.capabilities;
                validation_status_from_parts(
                    command,
                    true,
                    state.process.capabilities.diagnostics_strategy,
                    caps.definition_provider,
                    caps.call_hierarchy_provider,
                    caps.formatting_provider,
                    state
                        .indexing_complete
                        .load(std::sync::atomic::Ordering::Relaxed),
                    state.spawned_at.elapsed().as_secs(),
                )
            }
            Self::Unavailable(_) => {
                validation_status_from_parts(
                    command,
                    false,
                    DiagnosticsStrategy::None,
                    false,
                    false,
                    false,
                    false,
                    0,
                )
            }
        }
    }
}

/// Pure helper that maps raw process state to [`LspLanguageStatus`].
///
/// Extracted from [`ProcessEntry::to_validation_status`] to make the
/// mapping logic independently unit-testable without requiring a live
/// [`ManagedProcess`] (which embeds an OS child process handle).
#[allow(clippy::too_many_arguments)]
#[allow(clippy::fn_params_excessive_bools)]
fn validation_status_from_parts(
    command: &str,
    running: bool,
    diagnostics_strategy: DiagnosticsStrategy,
    supports_definition: bool,
    supports_call_hierarchy: bool,
    supports_formatting: bool,
    indexing_complete: bool,
    uptime_seconds: u64,
) -> crate::types::LspLanguageStatus {
    if !running {
        return crate::types::LspLanguageStatus {
            validation: false,
            reason: format!("{command} failed to start or crashed repeatedly"),
            indexing_complete: None,
            uptime_seconds: None,
            diagnostics_strategy: None,
            supports_definition: None,
            supports_call_hierarchy: None,
            supports_diagnostics: None,
            supports_formatting: None,
        };
    }
    match diagnostics_strategy {
        DiagnosticsStrategy::Pull | DiagnosticsStrategy::Push => crate::types::LspLanguageStatus {
            validation: true,
            reason: format!(
                "LSP connected and supports validation ({})",
                match diagnostics_strategy {
                    DiagnosticsStrategy::Pull => "pull diagnostics",
                    DiagnosticsStrategy::Push => "push diagnostics",
                    DiagnosticsStrategy::None => unreachable!(),
                }
            ),
            indexing_complete: Some(indexing_complete),
            uptime_seconds: Some(uptime_seconds),
            diagnostics_strategy: Some(diagnostics_strategy.as_str().to_owned()),
            supports_definition: Some(supports_definition),
            supports_call_hierarchy: Some(supports_call_hierarchy),
            supports_diagnostics: Some(true),
            supports_formatting: Some(supports_formatting),
        },
        DiagnosticsStrategy::None => crate::types::LspLanguageStatus {
            validation: false,
            reason: "LSP connected but does not support diagnostics".to_owned(),
            indexing_complete: Some(indexing_complete),
            uptime_seconds: Some(uptime_seconds),
            diagnostics_strategy: Some("none".to_owned()),
            supports_definition: Some(supports_definition),
            supports_call_hierarchy: Some(supports_call_hierarchy),
            supports_diagnostics: Some(false),
            supports_formatting: Some(supports_formatting),
        },
    }
}

/// RAII guard that increments in-flight counter on creation and decrements on drop.
struct InFlightGuard {
    counter: Arc<AtomicU32>,
}

impl InFlightGuard {
    fn new(counter: Arc<AtomicU32>) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self { counter }
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The production `Lawyer` implementation.
///
/// Manages per-language LSP child processes and provides JSON-RPC request
/// routing for `textDocument/definition` and future capabilities.
#[derive(Clone)]
pub struct LspClient {
    /// Known language descriptors (from Zero-Config detection).
    descriptors: Arc<Vec<LspDescriptor>>,
    /// Languages whose markers were found but whose LSP binaries are not on PATH.
    ///
    /// Used to surface actionable install guidance in `lsp_health` responses.
    missing_languages: Arc<Vec<crate::client::detect::MissingLanguage>>,
    /// Running processes keyed by language id.
    processes: Arc<DashMap<String, ProcessEntry>>,
    /// Locks for concurrent initialization to prevent duplicate spawns.
    init_locks: Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    /// Shared JSON-RPC request/response dispatcher.
    dispatcher: Arc<RequestDispatcher>,
    /// Broadcast channel for shutdown signals.
    shutdown_tx: Arc<broadcast::Sender<()>>,
}

impl LspClient {
    /// Create a new `LspClient` for the given workspace root.
    ///
    /// Performs Zero-Config language detection immediately (cheap directory
    /// scan). LSP processes are **not** started until firstuse.
    ///
    /// Starts the idle-timeout background task.
    ///
    /// # Errors
    /// Returns `Err` if the workspace root directory cannot be read.
    pub async fn new(
        workspace_root: &Path,
        config: std::sync::Arc<pathfinder_common::config::PathfinderConfig>,
    ) -> std::io::Result<Self> {
        let detection_result = detect_languages(workspace_root, &config).await?;

        tracing::info!(
            workspace = %workspace_root.display(),
            detected_languages = ?detection_result.detected.iter().map(|l| &l.language_id).collect::<Vec<_>>(),
            missing_languages = ?detection_result.missing.iter().map(|l| &l.language_id).collect::<Vec<_>>(),
            "LspClient: language detection complete"
        );

        let (shutdown_tx, _) = broadcast::channel::<()>(1);
        let shutdown_tx = Arc::new(shutdown_tx);

        let client = Self {
            descriptors: Arc::new(detection_result.detected),
            missing_languages: Arc::new(detection_result.missing),
            processes: Arc::new(DashMap::new()),
            init_locks: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            dispatcher: Arc::new(RequestDispatcher::new()),
            shutdown_tx: Arc::clone(&shutdown_tx),
        };

        // Spawn idle-timeout background task
        let processes = Arc::clone(&client.processes);
        let dispatcher = Arc::clone(&client.dispatcher);
        let shutdown_rx = shutdown_tx.subscribe();
        tokio::spawn(idle_timeout_task(processes, dispatcher, shutdown_rx));

        Ok(client)
    }
    /// Kick off LSP processes for all detected languages in background tasks.
    ///
    /// This is a fire-and-forget call — it returns immediately. Each language's
    /// process is spawned via `tokio::spawn`, giving it a head start on
    /// initialization while the agent issues non-LSP tool calls (e.g.
    /// `get_repo_map`, `search_codebase`).
    ///
    /// Failures are logged as warnings. The lazy `ensure_process` path will
    /// handle retries on the first actual LSP tool call.
    pub fn warm_start(&self) {
        let languages: Vec<String> = self
            .descriptors
            .iter()
            .map(|d| d.language_id.clone())
            .collect();

        if languages.is_empty() {
            tracing::debug!("LSP warm_start: no languages detected, skipping");
            return;
        }

        tracing::info!(
            languages = ?languages,
            "LSP warm_start: spawning background initialization"
        );

        for lang in languages {
            let client = self.clone();
            tokio::spawn(async move {
                tracing::debug!(language = %lang, "LSP warm_start: starting process");
                match client.ensure_process(&lang).await {
                    Ok(()) => {
                        tracing::info!(language = %lang, "LSP warm_start: ready");
                    }
                    Err(e) => {
                        tracing::warn!(
                            language = %lang,
                            error = %e,
                            "LSP warm_start: failed (will retry lazily on first use)"
                        );
                    }
                }
            });
        }
    }

    /// Gracefully shut down all LSP processes.
    ///
    /// Sends a shutdown signal to the idle timeout task, which will then
    /// gracefully terminate all LSP processes. This should be called during
    /// server shutdown to prevent orphaned child processes.
    ///
    /// This method is fire-and-forget: it signals the background task to
    /// shut down but does not wait for completion. The actual shutdown
    /// happens asynchronously in the background task.
    pub fn shutdown(&self) {
        tracing::info!("LspClient: shutdown requested");
        let _ = self.shutdown_tx.send(());
    }

    /// Ensure an LSP process is running for `language_id`, starting it if needed.
    ///
    /// Returns `Err(LspError::NoLspAvailable)` if:
    /// - No descriptor found for this language
    /// - The language has been marked unavailable after repeated crashes
    async fn ensure_process(&self, language_id: &str) -> Result<(), LspError> {
        // Fast path: already running
        if let Some(entry) = self.processes.get(language_id) {
            return match entry.value() {
                ProcessEntry::Running(_) => Ok(()),
                ProcessEntry::Unavailable(state) => {
                    // Check if cooldown has elapsed for recovery
                    let cooldown_elapsed_secs = state.unavailable_since.elapsed().as_secs();
                    if state.unavailable_since.elapsed() > RECOVERY_COOLDOWN {
                        // Attempt recovery: remove unavailable entry and proceed to spawn
                        drop(entry);
                        tracing::info!(
                            language = %language_id,
                            cooldown_elapsed_secs,
                            "LSP: recovery cooldown elapsed, attempting restart"
                        );
                        self.processes.remove(language_id);
                        Ok(())
                    } else {
                        Err(LspError::NoLspAvailable)
                    }
                }
            };
        }

        // Acquire the init lock for this language to prevent duplicate spawn races
        let init_lock = {
            let mut locks = self.init_locks.lock().await;
            locks
                .entry(language_id.to_owned())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _guard = init_lock.lock().await;

        // Double-check after acquiring lock
        if let Some(entry) = self.processes.get(language_id) {
            return match entry.value() {
                ProcessEntry::Running(_) => Ok(()),
                ProcessEntry::Unavailable(state) => {
                    // Check if cooldown has elapsed for recovery
                    let cooldown_elapsed_secs = state.unavailable_since.elapsed().as_secs();
                    if state.unavailable_since.elapsed() > RECOVERY_COOLDOWN {
                        // Attempt recovery: remove unavailable entry and proceed to spawn
                        drop(entry);
                        tracing::info!(
                            language = %language_id,
                            cooldown_elapsed_secs,
                            "LSP: recovery cooldown elapsed, attempting restart"
                        );
                        self.processes.remove(language_id);
                        Ok(())
                    } else {
                        Err(LspError::NoLspAvailable)
                    }
                }
            };
        }

        // Find the descriptor for this language
        let descriptor = self
            .descriptors
            .iter()
            .find(|d| d.language_id == language_id)
            .ok_or(LspError::NoLspAvailable)?
            .clone();

        // Spawn the process
        self.start_process(descriptor, 0).await
    }

    /// Spawn a new LSP process, retrying on failure with exponential backoff.
    async fn start_process(&self, descriptor: LspDescriptor, attempt: u32) -> Result<(), LspError> {
        let language_id = descriptor.language_id.clone();

        if attempt >= MAX_RESTART_ATTEMPTS {
            tracing::error!(
                language = %language_id,
                "LSP: max restart attempts reached, marking unavailable"
            );
            self.processes.insert(
                language_id.clone(),
                ProcessEntry::Unavailable(UnavailableState {
                    unavailable_since: Instant::now(),
                }),
            );
            return Err(LspError::NoLspAvailable);
        }

        // Exponential backoff: 1s, 2s, 4s
        if attempt > 0 {
            let delay = Duration::from_secs(1u64 << (attempt - 1));
            tracing::info!(
                language = %language_id,
                attempt,
                delay_ms = delay.as_millis(),
                "LSP: restart with backoff"
            );
            tokio::time::sleep(delay).await;
        }

        tracing::info!(
            language = %language_id,
            command = %descriptor.command,
            "LSP: spawning process"
        );

        // Warn about concurrent instances (e.g., VS Code's RA)
        let isolate_target_dir = self.detect_concurrent_lsp(&language_id, &descriptor.command);

        let plugins = descriptor.auto_plugins.clone();
        let spawn_result = spawn_and_initialize(
            &descriptor.command,
            &descriptor.args,
            &descriptor.root,
            &language_id,
            Arc::clone(&self.dispatcher),
            descriptor.init_timeout_secs,
            isolate_target_dir,
            plugins,
        )
        .await;

        let (process, reader_handle) = match spawn_result {
            Ok(res) => res,
            Err(e) => {
                tracing::error!(
                    language = %language_id,
                    error = %e,
                    attempt,
                    "LSP: initialization failed — retrying"
                );
                // On recovery failure (attempt > 0), reset the unavailable_since timestamp
                if attempt > 0 {
                    if let Some(mut entry) = self.processes.get_mut(&language_id) {
                        if let ProcessEntry::Unavailable(state) = entry.value_mut() {
                            state.unavailable_since = Instant::now();
                            tracing::info!(
                                language = %language_id,
                                "LSP: recovery failed, cooldown reset"
                            );
                        }
                    }
                }
                // Recurse with attempt+1; the guard at the top of this function handles
                // exhaustion (attempt >= MAX_RESTART_ATTEMPTS) by inserting Unavailable.
                return Box::pin(self.start_process(descriptor, attempt + 1)).await;
            }
        };

        // The reader task was started inside spawn_and_initialize (before the
        // initialize handshake) to avoid the stdout-read deadlock. We only need
        // to wrap it in the supervisor task here.
        let supervisor_handle = tokio::spawn(reader_supervisor_task(
            language_id.clone(),
            reader_handle,
            Arc::clone(&self.processes),
        ));

        // Log successful recovery (attempt > 0 means we're recovering from a failure)
        if attempt > 0 {
            tracing::info!(
                language = %language_id,
                "LSP: recovery successful"
            );
        }

        // Create indexing_complete flag — will be set by the progress watcher task
        // when the LSP emits WorkDoneProgressEnd for its initial indexing job.
        let indexing_complete = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let spawned_at = Instant::now();

        // Spawn a background task that monitors $/progress notifications from the
        // LSP and sets indexing_complete when the initial indexing token closes.
        // This gives agents an unambiguous binary signal when LSP navigation is ready.
        let indexing_flag = Arc::clone(&indexing_complete);
        let lang_id_for_watcher = language_id.clone();
        let dispatcher_for_watcher = Arc::clone(&self.dispatcher);
        tokio::spawn(async move {
            progress_watcher_task(lang_id_for_watcher, dispatcher_for_watcher, indexing_flag).await;
        });

        self.processes.insert(
            language_id,
            ProcessEntry::Running(Box::new(LanguageState {
                process,
                reader_handle: supervisor_handle,
                restart_count: attempt,
                spawned_at,
                indexing_complete,
            })),
        );

        Ok(())
    }

    /// Check for concurrent LSP processes for the same language on the same
    /// workspace. Returns true if concurrent instances detected. Logs a warning
    /// if found — two RA instances fighting over the same build cache is a known
    /// cause of indexing stalls.
    #[allow(clippy::unused_self)] // kept as method for consistency
    fn detect_concurrent_lsp(&self, language_id: &str, command: &str) -> bool {
        // Extract the binary name (e.g., "rust-analyzer" from an absolute path)
        let binary_name = Path::new(command)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(command);

        // Check if there's already a process with this binary name running
        // that we didn't spawn. We do this by counting how many instances
        // exist in the system process table.
        #[cfg(target_os = "linux")]
        {
            if let Ok(entries) = std::fs::read_dir("/proc") {
                let mut count = 0;
                for entry in entries.flatten() {
                    let cmdline_path = entry.path().join("cmdline");
                    if let Ok(cmdline) = std::fs::read_to_string(&cmdline_path) {
                        if cmdline.contains(binary_name) {
                            count += 1;
                        }
                    }
                }
                // We count ourselves too, so >1 means another instance exists
                if count > 1 {
                    tracing::warn!(
                        language = language_id,
                        binary = binary_name,
                        instances_found = count,
                        "LSP: detected {} concurrent instances of {binary_name} on this workspace. \
                         Isolating build artifacts to avoid cache lock contention.",
                        count
                    );
                    return true;
                }
            }
        }
        let _ = (language_id, binary_name); // suppress unused warnings on non-linux
        false
    }

    /// Update `last_used` for a language (called after each successful request).
    fn touch(&self, language_id: &str) {
        if let Some(mut entry) = self.processes.get_mut(language_id) {
            if let ProcessEntry::Running(state) = entry.value_mut() {
                state.process.last_used = Instant::now();
            }
        }
    }

    /// Send a JSON-RPC request and await the response.
    ///
    /// Dispatches via the shared `RequestDispatcher` so the background reader
    /// task can fire the correct oneshot.
    async fn request(
        &self,
        language_id: &str,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, LspError> {
        let (id, rx) = self.dispatcher.register();
        let message = RequestDispatcher::make_request(id, method, &params);

        // Increment in-flight counter (decremented on drop) and health check
        let _in_flight_guard = {
            let Some(entry) = self.processes.get(language_id) else {
                return Err(LspError::NoLspAvailable);
            };
            let ProcessEntry::Running(state) = entry.value() else {
                return Err(LspError::NoLspAvailable);
            };
            // Health check: verify reader task is still alive
            if state.reader_handle.is_finished() {
                // Proactively remove the stale process entry so the next request
                // triggers recovery via ensure_process() instead of returning
                // ConnectionLost forever. The reader_supervisor_task will also
                // remove it, but there's a race window where requests pile up.
                drop(entry);
                self.processes.remove(language_id);
                tracing::warn!(
                    language = %language_id,
                    "LSP: reader task not alive, removed stale entry for recovery"
                );
                return Err(LspError::ConnectionLost);
            }
            let counter = Arc::clone(&state.process.in_flight);
            InFlightGuard::new(counter)
        };

        // Write the request to stdin
        {
            let Some(entry) = self.processes.get(language_id) else {
                return Err(LspError::NoLspAvailable);
            };
            let ProcessEntry::Running(state) = entry.value() else {
                return Err(LspError::NoLspAvailable);
            };
            send(&state.process, &message).await?;
        }

        // Await response with timeout
        tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| {
                self.dispatcher.remove(id);
                LspError::Timeout {
                    operation: method.to_owned(),
                    timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                }
            })?
            .map_err(|_| LspError::ConnectionLost)?
    }

    /// Send a JSON-RPC notification (fire-and-forget, no response expected).
    ///
    /// Notifications are sent on the stdin of the target process without
    /// registering a response waiter — the LSP doesn't reply to them.
    async fn notify(
        &self,
        language_id: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(), LspError> {
        let message = RequestDispatcher::make_notification(method, &params);
        match self.processes.get(language_id) {
            Some(entry) => match entry.value() {
                ProcessEntry::Running(state) => send(&state.process, &message).await,
                ProcessEntry::Unavailable(_) => Err(LspError::NoLspAvailable),
            },
            None => Err(LspError::NoLspAvailable),
        }
    }

    /// Retrieve the detected capabilities for a language.
    ///
    /// Returns `Ok(caps)` when the process is running, else `NoLspAvailable`.
    fn capabilities_for(&self, language_id: &str) -> Result<DetectedCapabilities, LspError> {
        let Some(entry) = self.processes.get(language_id) else {
            return Err(LspError::NoLspAvailable);
        };
        match entry.value() {
            ProcessEntry::Running(state) => Ok(state.process.capabilities.clone()),
            ProcessEntry::Unavailable(_) => Err(LspError::NoLspAvailable),
        }
    }

    /// Shared implementation for call hierarchy incoming/outgoing requests.
    ///
    /// Extracted to eliminate 47-line duplication between `call_hierarchy_incoming`
    /// and `call_hierarchy_outgoing`. The only differences are:
    /// - Tool name (for logging)
    /// - LSP method name
    /// - Response parser key (`from` vs `to`)
    async fn call_hierarchy_request(
        &self,
        workspace_root: &Path,
        item: &CallHierarchyItem,
        tool_name: &str,
        lsp_method: &str,
        item_key: &str,
        ranges_key: &str,
    ) -> Result<Vec<CallHierarchyCall>, LspError> {
        let start = Instant::now();
        tracing::info!(tool = tool_name, file = %item.file, "LSP operation started");
        let ext = Path::new(&item.file)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;
        self.ensure_process(language_id).await?;

        let lsp_item = item.data.clone().ok_or_else(|| {
            LspError::Protocol("CallHierarchyItem missing original LSP data".to_owned())
        })?;

        let params = json!({ "item": lsp_item });

        let response = match self
            .request(language_id, lsp_method, params, Duration::from_secs(30))
            .await
        {
            Ok(res) => res,
            Err(e) => {
                tracing::error!(tool = tool_name, language = language_id, error = %e, "{} failed", lsp_method);
                return Err(e);
            }
        };

        self.touch(language_id);

        let elapsed = start.elapsed().as_millis();
        tracing::info!(
            language = language_id,
            elapsed_ms = elapsed,
            "{} complete",
            lsp_method
        );

        parse_call_hierarchy_calls_response(&response, workspace_root, item_key, ranges_key)
    }
}

#[async_trait]
impl Lawyer for LspClient {
    async fn did_change_watched_files(
        &self,
        _changes: Vec<crate::types::FileEvent>,
    ) -> Result<(), LspError> {
        Ok(())
    }
    async fn goto_definition(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: u32,
        column: u32,
    ) -> Result<Option<DefinitionLocation>, LspError> {
        let start = Instant::now();
        tracing::info!(tool = "goto_definition", file = %file_path.display(), "LSP operation started");

        // Determine language from file extension
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;

        // Ensure the LSP process is running
        self.ensure_process(language_id).await?;

        // Build the textDocument/definition request (LSP positions are 0-indexed)
        let file_uri = Url::from_file_path(workspace_root.join(file_path))
            .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?;

        let params = json!({
            "textDocument": { "uri": file_uri.as_str() },
            "position": {
                "line": line.saturating_sub(1),       // Convert 1-indexed → 0-indexed
                "character": column.saturating_sub(1)
            }
        });

        let response = match self
            .request(
                language_id,
                "textDocument/definition",
                params,
                Duration::from_secs(10),
            )
            .await
        {
            Ok(res) => res,
            Err(e) => {
                tracing::error!(tool = "goto_definition", language = language_id, error = %e, "textDocument/definition failed");
                return Err(e);
            }
        };

        self.touch(language_id);

        let elapsed = start.elapsed().as_millis();
        tracing::info!(
            tool = "get_definition",
            language = language_id,
            elapsed_ms = elapsed,
            "textDocument/definition complete"
        );

        parse_definition_response(response)
    }

    async fn call_hierarchy_prepare(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: u32,
        column: u32,
    ) -> Result<Vec<CallHierarchyItem>, LspError> {
        let start = Instant::now();
        tracing::info!(tool = "call_hierarchy_prepare", file = %file_path.display(), "LSP operation started");
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;
        self.ensure_process(language_id).await?;

        let caps = self.capabilities_for(language_id)?;
        if !caps.call_hierarchy_provider {
            return Err(LspError::UnsupportedCapability {
                capability: "callHierarchyProvider".to_owned(),
            });
        }

        let file_uri = Url::from_file_path(workspace_root.join(file_path))
            .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?;

        let params = json!({
            "textDocument": { "uri": file_uri.as_str() },
            "position": {
                "line": line.saturating_sub(1),
                "character": column.saturating_sub(1)
            }
        });

        let response = match self
            .request(
                language_id,
                "textDocument/prepareCallHierarchy",
                params,
                Duration::from_secs(10),
            )
            .await
        {
            Ok(res) => res,
            Err(e) => {
                tracing::error!(tool = "call_hierarchy_prepare", language = language_id, error = %e, "textDocument/prepareCallHierarchy failed");
                return Err(e);
            }
        };

        self.touch(language_id);

        let elapsed = start.elapsed().as_millis();
        tracing::info!(
            language = language_id,
            elapsed_ms = elapsed,
            "textDocument/prepareCallHierarchy complete"
        );

        parse_call_hierarchy_prepare_response(&response, workspace_root)
    }

    async fn call_hierarchy_incoming(
        &self,
        workspace_root: &Path,
        item: &CallHierarchyItem,
    ) -> Result<Vec<CallHierarchyCall>, LspError> {
        self.call_hierarchy_request(
            workspace_root,
            item,
            "call_hierarchy_incoming",
            "callHierarchy/incomingCalls",
            "from",
            "fromRanges",
        )
        .await
    }

    async fn call_hierarchy_outgoing(
        &self,
        workspace_root: &Path,
        item: &CallHierarchyItem,
    ) -> Result<Vec<CallHierarchyCall>, LspError> {
        self.call_hierarchy_request(
            workspace_root,
            item,
            "call_hierarchy_outgoing",
            "callHierarchy/outgoingCalls",
            "to",
            "fromRanges",
        )
        .await
    }

    async fn did_open(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        content: &str,
    ) -> Result<(), LspError> {
        tracing::info!(tool = "did_open", file = %file_path.display(), "LSP operation started");
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;
        self.ensure_process(language_id).await?;

        let file_uri = Url::from_file_path(workspace_root.join(file_path))
            .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?;

        let params = json!({
            "textDocument": {
                "uri": file_uri.as_str(),
                "languageId": language_id,
                "version": 1,
                "text": content
            }
        });

        if let Err(e) = self
            .notify(language_id, "textDocument/didOpen", params)
            .await
        {
            tracing::error!(tool = "did_open", language = language_id, error = %e, "textDocument/didOpen failed");
            return Err(e);
        }
        self.touch(language_id);
        Ok(())
    }

    async fn did_change(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        content: &str,
        version: i32,
    ) -> Result<(), LspError> {
        tracing::info!(tool = "did_change", file = %file_path.display(), "LSP operation started");
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;
        self.ensure_process(language_id).await?;

        let file_uri = Url::from_file_path(workspace_root.join(file_path))
            .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?;

        // Full content sync (TextDocumentSyncKind.Full = 1)
        let params = json!({
            "textDocument": {
                "uri": file_uri.as_str(),
                "version": version
            },
            "contentChanges": [{ "text": content }]
        });

        if let Err(e) = self
            .notify(language_id, "textDocument/didChange", params)
            .await
        {
            tracing::error!(tool = "did_change", language = language_id, error = %e, "textDocument/didChange failed");
            return Err(e);
        }
        self.touch(language_id);
        Ok(())
    }

    async fn did_close(&self, workspace_root: &Path, file_path: &Path) -> Result<(), LspError> {
        tracing::info!(tool = "did_close", file = %file_path.display(), "LSP operation started");
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;
        self.ensure_process(language_id).await?;

        let file_uri = Url::from_file_path(workspace_root.join(file_path))
            .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?;

        let params = json!({
            "textDocument": {
                "uri": file_uri.as_str()
            }
        });

        if let Err(e) = self
            .notify(language_id, "textDocument/didClose", params)
            .await
        {
            tracing::error!(tool = "did_close", language = language_id, error = %e, "textDocument/didClose failed");
            return Err(e);
        }
        // Not touching `last_used` on close since this is a cleanup action.
        Ok(())
    }

    async fn pull_diagnostics(
        &self,
        workspace_root: &Path,
        file_path: &Path,
    ) -> Result<Vec<LspDiagnostic>, LspError> {
        let start = Instant::now();
        tracing::info!(tool = "pull_diagnostics", file = %file_path.display(), "LSP operation started");
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;
        self.ensure_process(language_id).await?;

        // Check capability before sending the request
        let caps = self.capabilities_for(language_id)?;
        if !matches!(caps.diagnostics_strategy, DiagnosticsStrategy::Pull) {
            return Err(LspError::UnsupportedCapability {
                capability: "diagnosticProvider (pull model)".to_owned(),
            });
        }

        let file_uri = Url::from_file_path(workspace_root.join(file_path))
            .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?;

        let params = json!({
            "textDocument": { "uri": file_uri.as_str() }
        });

        let response = match self
            .request(
                language_id,
                "textDocument/diagnostic",
                params,
                Duration::from_secs(30),
            )
            .await
        {
            Ok(res) => res,
            Err(e) => {
                tracing::error!(tool = "pull_diagnostics", language = language_id, error = %e, "textDocument/diagnostic failed");
                return Err(e);
            }
        };

        self.touch(language_id);

        let elapsed = start.elapsed().as_millis();
        tracing::debug!(
            language = language_id,
            elapsed_ms = elapsed,
            "textDocument/diagnostic complete"
        );

        parse_diagnostic_response(&response, file_path)
    }

    async fn collect_diagnostics(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        content: &str,
        version: i32,
        timeout_ms: u64,
    ) -> Result<Vec<LspDiagnostic>, LspError> {
        let start = Instant::now();
        tracing::info!(tool = "collect_diagnostics", file = %file_path.display(), "LSP operation started");
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;
        self.ensure_process(language_id).await?;

        let file_uri = Url::from_file_path(workspace_root.join(file_path))
            .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?;
        let file_uri_str = file_uri.to_string();

        // Send didOpen or didChange depending on version
        if version <= 1 {
            self.did_open(workspace_root, file_path, content).await?;
        } else {
            self.did_change(workspace_root, file_path, content, version)
                .await?;
        }

        // Wait for push diagnostics within timeout
        let raw_diags = self
            .dispatcher
            .collect_push_diagnostics(&file_uri_str, Duration::from_millis(timeout_ms))
            .await;

        // Parse all collected diagnostics
        let mut all_diags = Vec::new();
        for notif in raw_diags {
            if let Some(items) = notif
                .pointer("/params/diagnostics")
                .and_then(|v| v.as_array())
            {
                all_diags.extend(parse_diagnostic_items(items, file_path));
            }
        }

        self.touch(language_id);

        let elapsed = start.elapsed().as_millis();
        tracing::debug!(
            language = language_id,
            elapsed_ms = elapsed,
            "textDocument/publishDiagnostics collection complete"
        );

        Ok(all_diags)
    }

    async fn pull_workspace_diagnostics(
        &self,
        workspace_root: &Path,
        file_path: &Path,
    ) -> Result<Vec<LspDiagnostic>, LspError> {
        let start = Instant::now();
        tracing::info!(tool = "pull_workspace_diagnostics", file = %file_path.display(), "LSP operation started");
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;
        self.ensure_process(language_id).await?;

        let caps = self.capabilities_for(language_id)?;
        if !caps.workspace_diagnostic_provider {
            return Err(LspError::UnsupportedCapability {
                capability: "workspaceDiagnosticProvider".to_owned(),
            });
        }

        // The params for workspace diagnostics are typically quite minimal
        let params = json!({});

        let response = match self
            .request(
                language_id,
                "workspace/diagnostic",
                params,
                Duration::from_mins(1), // Workspace diagnostics might take longer
            )
            .await
        {
            Ok(res) => res,
            Err(e) => {
                tracing::error!(tool = "pull_workspace_diagnostics", language = language_id, error = %e, "workspace/diagnostic failed");
                return Err(e);
            }
        };

        self.touch(language_id);

        let elapsed = start.elapsed().as_millis();
        tracing::debug!(
            language = language_id,
            elapsed_ms = elapsed,
            "workspace/diagnostic complete"
        );

        parse_workspace_diagnostic_response(&response, workspace_root)
    }

    async fn range_formatting(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        start_line: u32,
        end_line: u32,
        _original_content: &str,
    ) -> Result<Option<String>, LspError> {
        tracing::info!(tool = "range_formatting", file = %file_path.display(), "LSP operation started");
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;
        self.ensure_process(language_id).await?;

        // Check capability before sending the request
        let caps = self.capabilities_for(language_id)?;
        if !caps.formatting_provider {
            return Err(LspError::UnsupportedCapability {
                capability: "documentFormattingProvider".to_owned(),
            });
        }

        let file_uri = Url::from_file_path(workspace_root.join(file_path))
            .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?;

        // LSP positions are 0-indexed, our API is 1-indexed
        let params = json!({
            "textDocument": { "uri": file_uri.as_str() },
            "range": {
                "start": { "line": start_line.saturating_sub(1), "character": 0 },
                "end":   { "line": end_line.saturating_sub(1),   "character": 0 }
            },
            "options": { "tabSize": 4, "insertSpaces": true }
        });

        let response = match self
            .request(
                language_id,
                "textDocument/rangeFormatting",
                params,
                Duration::from_secs(10),
            )
            .await
        {
            Ok(res) => res,
            Err(e) => {
                tracing::error!(tool = "range_formatting", language = language_id, error = %e, "textDocument/rangeFormatting failed");
                return Err(e);
            }
        };

        self.touch(language_id);

        tracing::info!(
            tool = "range_formatting",
            language = language_id,
            "textDocument/rangeFormatting complete"
        );

        if response.is_null() {
            return Ok(None);
        }

        if let Some(edits) = response.as_array() {
            tracing::debug!(
                language = language_id,
                edit_count = edits.len(),
                "LSP returned range formatting edits (currently ignored)"
            );
        }

        // response is an array of TextEdit objects; we currently don't apply them
        // (we just signal availability). The Tree-sitter indentation pre-pass
        // is sufficient. Return None to indicate "no formatted text substitution".
        Ok(None)
    }

    async fn capability_status(&self) -> HashMap<String, crate::types::LspLanguageStatus> {
        let mut status = HashMap::new();
        for desc in self.descriptors.iter() {
            let lang_status = self.processes.get(&desc.language_id).map_or_else(
                || crate::types::LspLanguageStatus {
                    validation: true,
                    reason: format!("{} available (lazy start)", desc.command),
                    diagnostics_strategy: None,
                    // Process hasn't started yet — indexing status and uptime unknown
                    indexing_complete: None,
                    uptime_seconds: None,
                    // Capabilities unknown until process starts
                    supports_definition: None,
                    supports_call_hierarchy: None,
                    supports_diagnostics: None,
                    supports_formatting: None,
                },
                |entry| entry.to_validation_status(&desc.command),
            );
            status.insert(desc.language_id.clone(), lang_status);
        }
        status
    }

    fn missing_languages(&self) -> Vec<crate::client::MissingLanguage> {
        self.missing_languages.iter().cloned().collect()
    }
}

/// Parse the `textDocument/diagnostic` response into a `Vec<LspDiagnostic>`.
///
/// The response shape is: `{ "kind": "full", "items": [Diagnostic, ...] }`
/// or `{ "kind": "unchanged", "resultId": "..." }` for unchanged results.
fn parse_diagnostic_response(
    response: &serde_json::Value,
    file_path: &Path,
) -> Result<Vec<LspDiagnostic>, LspError> {
    // "unchanged" kind means diagnostics have not changed since last pull
    if response.get("kind").and_then(|k| k.as_str()) == Some("unchanged") {
        return Ok(vec![]);
    }

    let items = match response.get("items") {
        Some(serde_json::Value::Array(arr)) => arr,
        Some(_) => {
            return Err(LspError::Protocol(
                "diagnostics 'items' is not an array".to_owned(),
            ))
        }
        // Some LSPs return flat arrays (not wrapped in {kind, items})
        None => {
            if let Some(arr) = response.as_array() {
                return Ok(parse_diagnostic_items(arr, file_path));
            }
            return Ok(vec![]);
        }
    };

    Ok(parse_diagnostic_items(items, file_path))
}

/// Parse the `workspace/diagnostic` response into a flat `Vec<LspDiagnostic>`.
///
/// Response shape: `{ "items": [ { "uri": "...", "kind": "full", "items": [Diagnostic, ...] } ] }`
fn parse_workspace_diagnostic_response(
    response: &serde_json::Value,
    workspace_root: &Path,
) -> Result<Vec<LspDiagnostic>, LspError> {
    let mut all_diags = Vec::new();

    let items = response
        .get("items")
        .and_then(|i| i.as_array())
        .ok_or_else(|| {
            LspError::Protocol("workspace.diagnostic 'items' is missing or not an array".to_owned())
        })?;

    for doc_report in items {
        if doc_report.get("kind").and_then(|k| k.as_str()) == Some("unchanged") {
            continue;
        }

        let Some(uri_str) = doc_report.get("uri").and_then(|u| u.as_str()) else {
            continue;
        };

        // Convert URI to relative file path using workspace_root
        let file_path = match Url::parse(uri_str) {
            Ok(url) => match url.to_file_path() {
                Ok(path) => path
                    .strip_prefix(workspace_root)
                    .map_or_else(|_| path.clone(), std::path::Path::to_path_buf),
                Err(()) => continue,
            },
            Err(_) => continue,
        };

        if let Some(doc_items) = doc_report.get("items").and_then(|i| i.as_array()) {
            all_diags.extend(parse_diagnostic_items(doc_items, &file_path));
        }
    }

    Ok(all_diags)
}

/// Parse an array of LSP `Diagnostic` objects.
fn parse_diagnostic_items(items: &[serde_json::Value], file_path: &Path) -> Vec<LspDiagnostic> {
    let mut result = Vec::with_capacity(items.len());
    let file_str = file_path.to_string_lossy().into_owned();

    for item in items {
        let severity = match item["severity"].as_u64().unwrap_or(1) {
            1 => LspDiagnosticSeverity::Error,
            2 => LspDiagnosticSeverity::Warning,
            3 => LspDiagnosticSeverity::Information,
            _ => LspDiagnosticSeverity::Hint,
        };

        let message = item["message"].as_str().unwrap_or("").to_owned();
        if message.is_empty() {
            continue; // Skip diagnostics with no message
        }

        let code = item.get("code").and_then(|c| match c {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        });

        let start_line = item["range"]["start"]["line"]
            .as_u64()
            .map_or(1, |l| u32::try_from(l + 1).unwrap_or(1));
        let end_line = item["range"]["end"]["line"]
            .as_u64()
            .map_or(start_line, |l| u32::try_from(l + 1).unwrap_or(1));

        result.push(LspDiagnostic {
            severity,
            code,
            message,
            file: file_str.clone(),
            start_line,
            end_line,
        });
    }

    result
}

/// Parse the `textDocument/definition` response into a `DefinitionLocation`.
///
/// Returns `Ok(None)` for JSON `null` (no definition found).
fn parse_definition_response(
    response: serde_json::Value,
) -> Result<Option<DefinitionLocation>, LspError> {
    if response.is_null() {
        return Ok(None);
    }

    // The response can be a single Location, a LocationLink, or an array
    let location = if response.is_array() {
        response
            .as_array()
            .and_then(|arr| arr.first())
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    } else {
        response
    };

    if location.is_null() {
        return Ok(None);
    }

    // Location: { uri, range: { start: { line, character } } }
    // LocationLink: { targetUri, targetRange, ... }
    let (uri_str, start_line, start_char) = if location.get("targetUri").is_some() {
        // LocationLink
        (
            location["targetUri"].as_str().unwrap_or("").to_owned(),
            location["targetSelectionRange"]["start"]["line"]
                .as_u64()
                .unwrap_or(0),
            location["targetSelectionRange"]["start"]["character"]
                .as_u64()
                .unwrap_or(0),
        )
    } else {
        // Location
        (
            location["uri"].as_str().unwrap_or("").to_owned(),
            location["range"]["start"]["line"].as_u64().unwrap_or(0),
            location["range"]["start"]["character"]
                .as_u64()
                .unwrap_or(0),
        )
    };

    if uri_str.is_empty() {
        return Err(LspError::Protocol(
            "definition response missing URI".to_owned(),
        ));
    }

    // Convert URI to a relative file path string
    let file = Url::parse(&uri_str)
        .ok()
        .and_then(|u: Url| u.to_file_path().ok())
        .map(|p: std::path::PathBuf| p.to_string_lossy().into_owned())
        .unwrap_or(uri_str);

    Ok(Some(DefinitionLocation {
        file,
        line: u32::try_from(start_line + 1).unwrap_or(1), // 0-indexed → 1-indexed
        column: u32::try_from(start_char + 1).unwrap_or(1),
        preview: String::default(), // Preview populated in future milestone
    }))
}

/// Parse the `textDocument/prepareCallHierarchy` response into a `Vec<CallHierarchyItem>`.
fn parse_call_hierarchy_prepare_response(
    response: &serde_json::Value,
    workspace_root: &Path,
) -> Result<Vec<CallHierarchyItem>, LspError> {
    if response.is_null() {
        return Ok(Vec::new());
    }

    let items = response
        .as_array()
        .ok_or_else(|| LspError::Protocol("expected array".to_owned()))?;

    let mut result = Vec::with_capacity(items.len());
    for item in items {
        let uri_str = item["uri"].as_str().unwrap_or("");
        let file = Url::parse(uri_str)
            .ok()
            .and_then(|u| u.to_file_path().ok())
            .and_then(|p| {
                p.strip_prefix(workspace_root)
                    .map(std::path::Path::to_path_buf)
                    .ok()
            })
            .map_or_else(|| uri_str.to_owned(), |p| p.to_string_lossy().into_owned());

        let line = u32::try_from(
            item["selectionRange"]["start"]["line"]
                .as_u64()
                .unwrap_or(0),
        )
        .unwrap_or(0)
            + 1;
        let column = u32::try_from(
            item["selectionRange"]["start"]["character"]
                .as_u64()
                .unwrap_or(0),
        )
        .unwrap_or(0)
            + 1;

        let kind_int = item["kind"].as_u64().unwrap_or(0);
        let kind = match kind_int {
            5 => "class",
            6 => "method",
            11 => "interface",
            12 => "function",
            _ => "symbol",
        }
        .to_owned();

        result.push(CallHierarchyItem {
            name: item["name"].as_str().unwrap_or("").to_owned(),
            kind,
            detail: item
                .get("detail")
                .and_then(|d| d.as_str())
                .map(ToOwned::to_owned),
            file,
            line,
            column,
            data: Some(item.clone()),
        });
    }

    Ok(result)
}

/// Parse the `callHierarchy/incomingCalls` or `outgoingCalls` response.
fn parse_call_hierarchy_calls_response(
    response: &serde_json::Value,
    workspace_root: &Path,
    item_key: &str,
    ranges_key: &str,
) -> Result<Vec<CallHierarchyCall>, LspError> {
    if response.is_null() {
        return Ok(Vec::new());
    }

    let calls = response
        .as_array()
        .ok_or_else(|| LspError::Protocol("expected array".to_owned()))?;

    let mut result = Vec::with_capacity(calls.len());
    for call in calls {
        let Some(item_val) = call.get(item_key) else {
            continue;
        };
        let mut parsed_items = parse_call_hierarchy_prepare_response(
            &serde_json::Value::Array(vec![item_val.clone()]),
            workspace_root,
        )?;
        if parsed_items.is_empty() {
            continue;
        }
        let item = parsed_items.remove(0);

        let mut call_sites = Vec::new();
        if let Some(ranges) = call.get(ranges_key).and_then(|r| r.as_array()) {
            for range in ranges {
                if let Some(line) = range
                    .get("start")
                    .and_then(|s| s.get("line"))
                    .and_then(serde_json::Value::as_u64)
                {
                    call_sites.push(u32::try_from(line).unwrap_or(0) + 1);
                }
            }
        }

        result.push(CallHierarchyCall { item, call_sites });
    }

    Ok(result)
}

/// Reader task supervisor: monitors the reader handle and cleans up on crash.
///
/// When the reader task exits (EOF or crash), this supervisor removes the
/// process entry from the map, allowing future requests to attempt recovery
/// via the cooldown mechanism.
async fn reader_supervisor_task(
    language_id: String,
    reader_handle: tokio::task::JoinHandle<()>,
    processes: Arc<DashMap<String, ProcessEntry>>,
) {
    match reader_handle.await {
        Ok(()) => {
            tracing::warn!(
                language = %language_id,
                "LSP: reader task exited normally (EOF), removing process entry"
            );
        }
        Err(e) => {
            tracing::error!(
                language = %language_id,
                error = %e,
                "LSP: reader task crashed (panic or abort), removing process entry"
            );
        }
    }
    // Remove the process entry — this allows future requests to retry
    // via the recovery cooldown mechanism in ensure_process()
    processes.remove(&language_id);
}

/// Background task: watch `$/progress` notifications for indexing completion.
///
/// The LSP server emits `window/workDoneProgress/create` followed by
/// `$/progress` notifications to report long-running operations like initial
/// workspace indexing. When the `$/progress` notification has a `kind == "end"`
/// value, the work token is complete.
///
/// We treat any `WorkDoneProgressEnd` notification as evidence that the LSP has
/// finished its initial index build (conservative: we set the flag on the *first*
/// `end` event regardless of the token title). Most LSPs (rust-analyzer, clangd,
/// pyright, tsserver) emit one primary indexing token that completes before they
/// are ready to serve navigation requests.
///
/// The task exits when the broadcast channel is closed (LSP process died / shut down).
async fn progress_watcher_task(
    language_id: String,
    dispatcher: Arc<RequestDispatcher>,
    indexing_complete: Arc<std::sync::atomic::AtomicBool>,
) {
    let mut rx = dispatcher.subscribe_notifications();
    tracing::debug!(language = %language_id, "progress_watcher_task: started");

    loop {
        match rx.recv().await {
            Ok(msg) => {
                let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
                // Both "$/progress" and "window/workDoneProgress/*" are signals.
                if method == "$/progress" || method.starts_with("window/workDoneProgress") {
                    let kind = msg
                        .pointer("/params/value/kind")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if kind == "end" {
                        // WorkDoneProgressEnd — the LSP has finished at least one major
                        // work token (typically initial indexing).
                        if !indexing_complete.load(std::sync::atomic::Ordering::Relaxed) {
                            indexing_complete.store(true, std::sync::atomic::Ordering::Relaxed);
                            tracing::info!(
                                language = %language_id,
                                "LSP: WorkDoneProgressEnd received — indexing_complete = true"
                            );
                        }
                    }
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                // We missed some notifications (slow subscriber). Log and continue.
                tracing::warn!(
                    language = %language_id,
                    missed = n,
                    "progress_watcher_task: lagged, missed notifications"
                );
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                // Dispatcher shut down — LSP process ended.
                tracing::debug!(language = %language_id, "progress_watcher_task: channel closed, exiting");
                break;
            }
        }
    }
}

/// Background task: check for idle processes and terminate them.
async fn idle_timeout_task(
    processes: Arc<DashMap<String, ProcessEntry>>,
    dispatcher: Arc<RequestDispatcher>,
    mut shutdown_rx: broadcast::Receiver<()>,
) {
    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                // Shutdown signal received - gracefully terminate all LSP processes
                tracing::info!("LSP: shutdown signal received, terminating all processes");
                let keys: Vec<String> = processes.iter().map(|e| e.key().clone()).collect();
                for lang in keys {
                    if let Some((_lang, ProcessEntry::Running(mut state))) = processes.remove(&lang) {
                        tracing::debug!(language = %lang, "LSP: shutting down process");
                        state.reader_handle.abort();
                        shutdown(&mut state.process, &dispatcher).await;
                    }
                }
                tracing::info!("LSP: all processes terminated");
                break;
            }
            () = tokio::time::sleep(IDLE_CHECK_INTERVAL) => {
                // Check for idle processes
                let languages_to_remove: Vec<String> = processes
                    .iter()
                    .filter_map(|entry| {
                        let lang = entry.key();
                        if let ProcessEntry::Running(state) = entry.value() {
                            // Only remove if idle timeout elapsed AND no in-flight requests
                            if state.process.last_used.elapsed() > DEFAULT_IDLE_TIMEOUT
                                && state.process.in_flight.load(Ordering::Relaxed) == 0
                            {
                                Some(lang.clone())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    })
                    .collect();

                for lang in languages_to_remove {
                    if let Some((_lang, ProcessEntry::Running(mut state))) = processes.remove(&lang) {
                        tracing::info!(
                            language = %lang,
                            restarts = state.restart_count,
                            "LSP: idle timeout — terminating"
                        );
                        // Abort the supervisor task to prevent it from logging after cleanup
                        state.reader_handle.abort();
                        shutdown(&mut state.process, &dispatcher).await;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_definition_response_null() {
        let result = parse_definition_response(json!(null));
        assert!(result.expect("should not err").is_none());
    }

    #[test]
    fn test_parse_definition_response_location() {
        let response = json!({
            "uri": "file:///home/user/project/src/auth.rs",
            "range": {
                "start": { "line": 41, "character": 4 },
                "end":   { "line": 41, "character": 9 }
            }
        });
        let result = parse_definition_response(response).expect("ok");
        let loc = result.expect("some location");
        assert_eq!(loc.line, 42); // 0-indexed → 1-indexed
        assert_eq!(loc.column, 5);
        assert!(loc.file.contains("auth.rs"));
    }

    #[test]
    fn test_parse_definition_response_array() {
        let response = json!([{
            "uri": "file:///project/src/lib.rs",
            "range": {
                "start": { "line": 9, "character": 0 },
                "end":   { "line": 9, "character": 5 }
            }
        }]);
        let result = parse_definition_response(response).expect("ok");
        let loc = result.expect("some location");
        assert_eq!(loc.line, 10);
        assert!(loc.file.contains("lib.rs"));
    }

    #[test]
    fn test_parse_definition_response_location_link() {
        let response = json!({
            "targetUri": "file:///project/src/types.rs",
            "targetRange": {
                "start": { "line": 19, "character": 0 },
                "end":   { "line": 25, "character": 1 }
            },
            "targetSelectionRange": {
                "start": { "line": 19, "character": 4 },
                "end":   { "line": 19, "character": 9 }
            }
        });
        let result = parse_definition_response(response).expect("ok");
        let loc = result.expect("some location");
        assert_eq!(loc.line, 20); // 0-indexed → 1-indexed
        assert!(loc.file.contains("types.rs"));
    }

    #[test]
    fn test_parse_definition_empty_array() {
        let response = json!([]);
        let result = parse_definition_response(response).expect("ok");
        // Empty array → null first element → None
        assert!(result.is_none());
    }

    // ── parse_diagnostic_response tests ───────────────────────────

    #[test]
    fn test_parse_diagnostic_response_full() {
        let response = json!({
            "kind": "full",
            "items": [
                {
                    "severity": 1,
                    "message": "type mismatch",
                    "range": {
                        "start": { "line": 4, "character": 0 },
                        "end": { "line": 4, "character": 10 }
                    },
                    "code": "E0308"
                }
            ]
        });
        let result = parse_diagnostic_response(&response, Path::new("src/main.rs")).expect("ok");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].severity, LspDiagnosticSeverity::Error);
        assert_eq!(result[0].message, "type mismatch");
        assert_eq!(result[0].start_line, 5);
        assert_eq!(result[0].end_line, 5);
        assert_eq!(result[0].code.as_deref(), Some("E0308"));
    }

    #[test]
    fn test_parse_diagnostic_response_unchanged() {
        let response = json!({"kind": "unchanged", "resultId": "abc"});
        let result = parse_diagnostic_response(&response, Path::new("src/main.rs")).expect("ok");
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_diagnostic_response_flat_array() {
        // Some LSPs return flat arrays without wrapping
        let response = json!([
            {
                "severity": 2,
                "message": "unused variable",
                "range": {
                    "start": { "line": 9, "character": 3 },
                    "end": { "line": 9, "character": 7 }
                }
            }
        ]);
        let result = parse_diagnostic_response(&response, Path::new("src/lib.rs")).expect("ok");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].severity, LspDiagnosticSeverity::Warning);
    }

    #[test]
    fn test_parse_diagnostic_response_empty_object() {
        let response = json!({});
        let result = parse_diagnostic_response(&response, Path::new("src/main.rs")).expect("ok");
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_diagnostic_response_items_not_array() {
        let response = json!({"items": "not_an_array"});
        let result = parse_diagnostic_response(&response, Path::new("src/main.rs"));
        assert!(result.is_err());
        match result {
            Err(LspError::Protocol(msg)) => assert!(msg.contains("not an array")),
            _ => panic!("expected Protocol error"),
        }
    }

    #[test]
    fn test_parse_definition_response_invalid_uri_fallback() {
        // Provide an invalid URL. It should fallback to the raw URI string.
        let response = json!({
            "uri": "invalid_url_no_scheme",
            "range": {
                "start": { "line": 10, "character": 0 },
                "end":   { "line": 10, "character": 5 }
            }
        });
        let result = parse_definition_response(response).expect("ok");
        let loc = result.expect("some location");
        assert_eq!(loc.file, "invalid_url_no_scheme");
    }

    // ── parse_diagnostic_items tests ──────────────────────────────

    #[test]
    fn test_parse_diagnostic_items_severity_mapping() {
        let items = json!([
            {"severity": 1, "message": "err", "range": {"start": {"line": 0}, "end": {"line": 0}}},
            {"severity": 2, "message": "warn", "range": {"start": {"line": 1}, "end": {"line": 1}}},
            {"severity": 3, "message": "info", "range": {"start": {"line": 2}, "end": {"line": 2}}},
            {"severity": 4, "message": "hint", "range": {"start": {"line": 3}, "end": {"line": 3}}}
        ]);
        let result = parse_diagnostic_items(items.as_array().unwrap(), Path::new("test.rs"));
        assert_eq!(result.len(), 4);
        assert_eq!(result[0].severity, LspDiagnosticSeverity::Error);
        assert_eq!(result[1].severity, LspDiagnosticSeverity::Warning);
        assert_eq!(result[2].severity, LspDiagnosticSeverity::Information);
        assert_eq!(result[3].severity, LspDiagnosticSeverity::Hint);
    }

    #[test]
    fn test_parse_diagnostic_items_skips_empty_message() {
        let items = json!([
            {"severity": 1, "message": "", "range": {"start": {"line": 0}, "end": {"line": 0}}},
            {"severity": 1, "message": "real error", "range": {"start": {"line": 1}, "end": {"line": 1}}}
        ]);
        let result = parse_diagnostic_items(items.as_array().unwrap(), Path::new("test.rs"));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].message, "real error");
    }

    #[test]
    fn test_parse_diagnostic_items_numeric_code() {
        let items = json!([
            {"severity": 1, "message": "err", "code": 1234, "range": {"start": {"line": 0}, "end": {"line": 0}}}
        ]);
        let result = parse_diagnostic_items(items.as_array().unwrap(), Path::new("test.rs"));
        assert_eq!(result[0].code.as_deref(), Some("1234"));
    }

    // ── parse_workspace_diagnostic_response tests ─────────────────

    #[test]
    fn test_parse_workspace_diagnostic_response_success() {
        let temp = std::env::temp_dir().join("pathfinder_wd_test");
        let _ = std::fs::create_dir_all(&temp);
        let file_path = temp.join("src/main.rs");
        std::fs::create_dir_all(temp.join("src")).ok();
        std::fs::write(&file_path, "fn main() {}").ok();

        let file_uri = Url::from_file_path(&file_path).unwrap().to_string();
        let response = json!({
            "items": [{
                "uri": file_uri,
                "kind": "full",
                "items": [
                    {
                        "severity": 1,
                        "message": "error here",
                        "range": {"start": {"line": 0}, "end": {"line": 0}}
                    }
                ]
            }]
        });
        let result = parse_workspace_diagnostic_response(&response, &temp).expect("ok");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].message, "error here");

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_parse_workspace_diagnostic_response_missing_items() {
        let response = json!({});
        let result = parse_workspace_diagnostic_response(&response, &std::env::temp_dir());
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_workspace_diagnostic_response_unchanged_skipped() {
        let temp = std::env::temp_dir().join("pathfinder_wd_unchanged");
        let response = json!({
            "items": [{
                "kind": "unchanged",
                "resultId": "abc"
            }]
        });
        let result = parse_workspace_diagnostic_response(&response, &temp).expect("ok");
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_workspace_diagnostic_response_no_uri_skipped() {
        let response = json!({
            "items": [{
                "kind": "full",
                "items": [{"severity": 1, "message": "err", "range": {"start": {"line": 0}, "end": {"line": 0}}}]
            }]
        });
        let result =
            parse_workspace_diagnostic_response(&response, &std::env::temp_dir()).expect("ok");
        assert!(result.is_empty(), "entry without URI should be skipped");
    }

    // ── parse_call_hierarchy_prepare_response tests ───────────────

    #[test]
    fn test_parse_call_hierarchy_prepare_null() {
        let result = parse_call_hierarchy_prepare_response(&json!(null), Path::new("/workspace"));
        assert!(result.expect("ok").is_empty());
    }

    #[test]
    fn test_parse_call_hierarchy_prepare_success() {
        let temp = std::env::temp_dir().join("pathfinder_ch_test");
        let _ = std::fs::create_dir_all(&temp);
        let file_path = temp.join("src/main.rs");
        std::fs::create_dir_all(temp.join("src")).ok();
        std::fs::write(&file_path, "fn main() {}").ok();

        let file_uri = Url::from_file_path(&file_path).unwrap().to_string();
        let response = json!([{
            "name": "main",
            "kind": 12,
            "detail": "fn()",
            "uri": file_uri,
            "selectionRange": {
                "start": { "line": 0, "character": 2 },
                "end": { "line": 0, "character": 6 }
            }
        }]);

        let result = parse_call_hierarchy_prepare_response(&response, &temp).expect("ok");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "main");
        assert_eq!(result[0].kind, "function");
        assert_eq!(result[0].line, 1);
        assert_eq!(result[0].column, 3);
        assert_eq!(result[0].detail.as_deref(), Some("fn()"));
        assert!(result[0].data.is_some());

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_parse_call_hierarchy_prepare_kind_mapping() {
        let temp = tempfile::tempdir().expect("temp dir");
        let file_uri = Url::from_file_path(temp.path().join("test.rs"))
            .unwrap()
            .to_string();
        // Kind 5 = class, 6 = method, 11 = interface, 12 = function, other = symbol
        for (kind_int, expected) in [
            (5, "class"),
            (6, "method"),
            (11, "interface"),
            (12, "function"),
            (99, "symbol"),
        ] {
            let response = json!([{
                "name": "item",
                "kind": kind_int,
                "uri": file_uri,
                "selectionRange": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 0, "character": 4 }
                }
            }]);
            let result = parse_call_hierarchy_prepare_response(&response, temp.path()).expect("ok");
            assert_eq!(
                result[0].kind, expected,
                "kind {kind_int} should map to {expected}"
            );
        }
    }

    #[test]
    fn test_parse_call_hierarchy_prepare_not_array() {
        let result =
            parse_call_hierarchy_prepare_response(&json!({"foo": "bar"}), Path::new("/workspace"));
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_call_hierarchy_prepare_response_invalid_uri_fallback() {
        let response = json!([{
            "name": "main",
            "kind": 12,
            "detail": "fn()",
            "uri": "invalid-uri",
            "selectionRange": {
                "start": { "line": 0, "character": 2 },
                "end": { "line": 0, "character": 6 }
            }
        }]);

        let result =
            parse_call_hierarchy_prepare_response(&response, Path::new("/workspace")).expect("ok");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].file, "invalid-uri");
    }

    // ── parse_call_hierarchy_calls_response tests ─────────────────

    #[test]
    fn test_parse_call_hierarchy_calls_null() {
        let result = parse_call_hierarchy_calls_response(
            &json!(null),
            Path::new("/workspace"),
            "from",
            "fromRanges",
        );
        assert!(result.expect("ok").is_empty());
    }

    #[test]
    fn test_parse_call_hierarchy_calls_incoming() {
        let temp = std::env::temp_dir().join("pathfinder_chi_test");
        let _ = std::fs::create_dir_all(&temp);
        let file_path = temp.join("src/caller.rs");
        std::fs::create_dir_all(temp.join("src")).ok();
        std::fs::write(&file_path, "fn caller() {}").ok();

        let file_uri = Url::from_file_path(&file_path).unwrap().to_string();
        let response = json!([{
            "from": {
                "name": "caller",
                "kind": 12,
                "uri": file_uri,
                "selectionRange": {
                    "start": { "line": 0, "character": 2 },
                    "end": { "line": 0, "character": 8 }
                }
            },
            "fromRanges": [
                { "start": { "line": 5 }, "end": { "line": 5 } }
            ]
        }]);

        let result = parse_call_hierarchy_calls_response(&response, &temp, "from", "fromRanges")
            .expect("ok");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].item.name, "caller");
        assert_eq!(result[0].call_sites, vec![6]); // line 5 → 6 (1-indexed)

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_parse_call_hierarchy_calls_outgoing() {
        let temp = std::env::temp_dir().join("pathfinder_cho_test");
        let _ = std::fs::create_dir_all(&temp);
        let file_path = temp.join("src/callee.rs");
        std::fs::create_dir_all(temp.join("src")).ok();
        std::fs::write(&file_path, "fn callee() {}").ok();

        let file_uri = Url::from_file_path(&file_path).unwrap().to_string();
        let response = json!([{
            "to": {
                "name": "callee",
                "kind": 12,
                "uri": file_uri,
                "selectionRange": {
                    "start": { "line": 0, "character": 2 },
                    "end": { "line": 0, "character": 8 }
                }
            },
            "fromRanges": [
                { "start": { "line": 10 }, "end": { "line": 10 } }
            ]
        }]);

        let result =
            parse_call_hierarchy_calls_response(&response, &temp, "to", "fromRanges").expect("ok");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].item.name, "callee");
        assert_eq!(result[0].call_sites, vec![11]);

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_parse_call_hierarchy_calls_not_array() {
        let result = parse_call_hierarchy_calls_response(
            &json!("not array"),
            Path::new("/workspace"),
            "from",
            "fromRanges",
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_call_hierarchy_calls_missing_item_key_skipped() {
        let response = json!([{
            "wrong_key": {
                "name": "x",
                "kind": 12,
                "uri": "file:///test.rs",
                "selectionRange": {"start": {"line": 0, "character": 0 }, "end": {"line": 0, "character": 1}}
            }
        }]);
        let result = parse_call_hierarchy_calls_response(
            &response,
            Path::new("/workspace"),
            "from",
            "fromRanges",
        )
        .expect("ok");
        assert!(
            result.is_empty(),
            "entry without 'from' key should be skipped"
        );
    }

    // ── ProcessEntry::to_validation_status tests ──────────────────

    #[test]
    fn test_process_entry_unavailable_status() {
        let entry = ProcessEntry::Unavailable(UnavailableState {
            unavailable_since: std::time::Instant::now(),
        });
        let status = entry.to_validation_status("gopls");
        assert!(!status.validation);
        assert!(status.reason.contains("gopls"));
        assert!(status.reason.contains("failed"));
    }

    #[test]
    fn test_process_entry_running_with_diagnostics_status() {
        // validation_status_from_parts is the pure helper extracted from
        // ProcessEntry::to_validation_status so we can test without a
        // live ManagedProcess (which requires a real OS child handle).
        // Future agents: add more variants here as capabilities grow.
        let status = validation_status_from_parts(
            "rust-analyzer",
            true,
            DiagnosticsStrategy::Pull,
            true,   // supports_definition
            true,   // supports_call_hierarchy
            false,  // supports_formatting
            false,  // indexing_complete
            10,     // uptime_seconds
        );
        assert!(
            status.validation,
            "DiagnosticsStrategy::Pull must yield validation=true"
        );
        assert_eq!(
            status.reason,
            "LSP connected and supports validation (pull diagnostics)"
        );
        assert_eq!(status.indexing_complete, Some(false));
        assert_eq!(status.uptime_seconds, Some(10));
    }

    #[test]
    fn test_process_entry_running_with_diagnostics_indexing_complete() {
        let status = validation_status_from_parts(
            "rust-analyzer",
            true,
            DiagnosticsStrategy::Pull,
            true,   // supports_definition
            true,   // supports_call_hierarchy
            false,  // supports_formatting
            true,   // indexing_complete
            42,     // uptime_seconds
        );
        assert!(status.validation);
        assert_eq!(status.indexing_complete, Some(true));
        assert_eq!(status.uptime_seconds, Some(42));
    }

    #[test]
    fn test_process_entry_running_without_diagnostics_status() {
        // LSP connected but does not support textDocument/diagnostic.
        let status =
            validation_status_from_parts("gopls", true, DiagnosticsStrategy::None, true, true, false, true, 5);
        assert!(
            !status.validation,
            "diagnostic_provider=false must yield validation=false"
        );
        assert!(
            status.reason.contains("does not support"),
            "reason must mention lack of support, got: {}",
            status.reason
        );
        assert_eq!(status.indexing_complete, Some(true));
        assert_eq!(status.uptime_seconds, Some(5));
    }

    #[test]
    fn test_process_entry_running_uptime_is_non_none() {
        // Uptime should always be Some for a running process (even if 0 seconds).
        let status =
            validation_status_from_parts("pyright", true, DiagnosticsStrategy::Pull, true, true, false, false, 0);
        assert!(status.uptime_seconds.is_some());
        assert!(status.indexing_complete.is_some());
    }

    // ── WP-1: LspClient Test Harness ──────────────────────────────
    //
    // These tests exercise LspClient's routing and lifecycle logic without
    // spawning real LSP child processes. We use a test-only constructor that
    // injects pre-configured process map entries.

    /// Create a test `LspClient` with empty descriptors (no languages detected).
    ///
    /// Useful for testing error paths where `ensure_process` returns `NoLspAvailable`
    /// because no descriptor was found.
    fn client_no_languages() -> LspClient {
        let (shutdown_tx, _) = broadcast::channel(1);
        LspClient {
            descriptors: Arc::new(Vec::new()),
            missing_languages: Arc::new(Vec::new()),
            processes: Arc::new(DashMap::new()),
            init_locks: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            dispatcher: Arc::new(RequestDispatcher::new()),
            shutdown_tx: Arc::new(shutdown_tx),
        }
    }

    /// Create a test `LspClient` with descriptors for specific languages but no
    /// running processes. The `processes` map can be pre-populated by the caller.
    fn client_with_descriptors(
        languages: Vec<&str>,
        processes: HashMap<String, ProcessEntry>,
    ) -> LspClient {
        let descriptors = languages
            .into_iter()
            .map(|lang| LspDescriptor {
                language_id: lang.to_owned(),
                command: format!("{lang}-lsp-server"),
                args: vec![],
                root: std::env::temp_dir(),
                init_timeout_secs: None,
                auto_plugins: vec![],
            })
            .collect();

        let processes_dashmap = DashMap::new();
        for (k, v) in processes {
            processes_dashmap.insert(k, v);
        }

        let (shutdown_tx, _) = broadcast::channel(1);
        LspClient {
            descriptors: Arc::new(descriptors),
            missing_languages: Arc::new(Vec::new()),
            processes: Arc::new(processes_dashmap),
            init_locks: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            dispatcher: Arc::new(RequestDispatcher::new()),
            shutdown_tx: Arc::new(shutdown_tx),
        }
    }

    // ── ensure_process tests ─────────────────────────────────────

    #[tokio::test]
    async fn test_ensure_process_no_descriptor_returns_no_lsp() {
        let client = client_no_languages();
        let result = client.ensure_process("rust").await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    #[tokio::test]
    async fn test_ensure_process_unavailable_cooldown_not_elapsed() {
        let processes = HashMap::from([(
            "rust".to_owned(),
            ProcessEntry::Unavailable(UnavailableState {
                unavailable_since: Instant::now(), // Just now → cooldown NOT elapsed
            }),
        )]);
        let client = client_with_descriptors(vec!["rust"], processes);
        let result = client.ensure_process("rust").await;
        assert!(
            matches!(result, Err(LspError::NoLspAvailable)),
            "should return NoLspAvailable when cooldown has not elapsed"
        );
    }

    #[tokio::test]
    async fn test_ensure_process_unavailable_cooldown_elapsed_removes_entry() {
        // Simulate a language that was marked unavailable 10 minutes ago
        // (RECOVERY_COOLDOWN is 5 minutes)
        let processes = HashMap::from([(
            "rust".to_owned(),
            ProcessEntry::Unavailable(UnavailableState {
                unavailable_since: Instant::now().checked_sub(Duration::from_mins(10)).unwrap(),
            }),
        )]);
        let client = client_with_descriptors(vec!["rust"], processes);

        // After cooldown, ensure_process should remove the unavailable entry
        // and return Ok(()) (clearing the unavailable state for the next call)
        let result = client.ensure_process("rust").await;
        assert!(
            result.is_ok(),
            "cooldown-elapsed should clear unavailable and return Ok: {result:?}"
        );

        // The unavailable entry should have been removed
        assert!(
            client.processes.get("rust").is_none(),
            "entry should be removed after cooldown-elapsed path"
        );
    }

    // ── request/notify error path tests ───────────────────────────

    #[tokio::test]
    async fn test_request_no_process_returns_no_lsp() {
        let client = client_no_languages();
        let result = client
            .request(
                "rust",
                "textDocument/definition",
                json!({}),
                Duration::from_secs(5),
            )
            .await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    #[tokio::test]
    async fn test_request_unavailable_process_returns_no_lsp() {
        let processes = HashMap::from([(
            "rust".to_owned(),
            ProcessEntry::Unavailable(UnavailableState {
                unavailable_since: Instant::now(),
            }),
        )]);
        let client = client_with_descriptors(vec!["rust"], processes);
        let result = client
            .request(
                "rust",
                "textDocument/definition",
                json!({}),
                Duration::from_secs(5),
            )
            .await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    #[tokio::test]
    async fn test_notify_no_process_returns_no_lsp() {
        let client = client_no_languages();
        let result = client
            .notify("rust", "textDocument/didOpen", json!({}))
            .await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    #[tokio::test]
    async fn test_notify_unavailable_process_returns_no_lsp() {
        let processes = HashMap::from([(
            "rust".to_owned(),
            ProcessEntry::Unavailable(UnavailableState {
                unavailable_since: Instant::now(),
            }),
        )]);
        let client = client_with_descriptors(vec!["rust"], processes);
        let result = client
            .notify("rust", "textDocument/didChange", json!({}))
            .await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    // ── capabilities_for tests ────────────────────────────────────

    #[tokio::test]
    async fn test_capabilities_for_no_process_returns_no_lsp() {
        let client = client_no_languages();
        let result = client.capabilities_for("rust");
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    #[tokio::test]
    async fn test_capabilities_for_unavailable_returns_no_lsp() {
        let processes = HashMap::from([(
            "rust".to_owned(),
            ProcessEntry::Unavailable(UnavailableState {
                unavailable_since: Instant::now(),
            }),
        )]);
        let client = client_with_descriptors(vec!["rust"], processes);
        let result = client.capabilities_for("rust");
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    // ── capability_status tests ───────────────────────────────────

    #[tokio::test]
    async fn test_capability_status_no_processes_lazy_start() {
        let client = client_with_descriptors(vec!["rust", "go"], HashMap::new());
        let status = client.capability_status().await;
        assert_eq!(status.len(), 2);
        // When no process is running, status should say "lazy start"
        assert!(status["rust"].reason.contains("lazy start"));
        assert!(status["go"].reason.contains("lazy start"));
    }

    #[tokio::test]
    async fn test_capability_status_unavailable_shows_failure() {
        let processes = HashMap::from([(
            "go".to_owned(),
            ProcessEntry::Unavailable(UnavailableState {
                unavailable_since: Instant::now(),
            }),
        )]);
        let client = client_with_descriptors(vec!["go"], processes);
        let status = client.capability_status().await;
        assert!(!status["go"].validation);
        assert!(status["go"].reason.contains("failed"));
    }

    #[tokio::test]
    async fn test_capability_status_no_descriptors_empty() {
        let client = client_no_languages();
        let status = client.capability_status().await;
        assert!(status.is_empty());
    }

    // ── Lawyer trait method error paths ───────────────────────────
    //
    // These test the Lawyer for LspClient methods when the LSP is unavailable.
    // They verify graceful degradation returns NoLspAvailable.

    #[tokio::test]
    async fn test_lawyer_goto_definition_no_lsp() {
        let client = client_no_languages();
        let result = client
            .goto_definition(Path::new("/workspace"), Path::new("src/main.rs"), 1, 1)
            .await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    #[tokio::test]
    async fn test_lawyer_call_hierarchy_prepare_no_lsp() {
        let client = client_no_languages();
        let result = client
            .call_hierarchy_prepare(Path::new("/workspace"), Path::new("src/main.rs"), 1, 1)
            .await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    #[tokio::test]
    async fn test_lawyer_call_hierarchy_incoming_no_lsp() {
        let client = client_no_languages();
        let item = CallHierarchyItem {
            name: "main".to_owned(),
            kind: "function".to_owned(),
            detail: None,
            file: "src/main.rs".to_owned(),
            line: 1,
            column: 1,
            data: None,
        };
        let result = client
            .call_hierarchy_incoming(Path::new("/workspace"), &item)
            .await;
        // Should fail because item.data is None (no LSP data)
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_lawyer_call_hierarchy_outgoing_no_lsp() {
        let client = client_no_languages();
        let item = CallHierarchyItem {
            name: "main".to_owned(),
            kind: "function".to_owned(),
            detail: None,
            file: "src/main.rs".to_owned(),
            line: 1,
            column: 1,
            data: None,
        };
        let result = client
            .call_hierarchy_outgoing(Path::new("/workspace"), &item)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_lawyer_did_open_no_lsp() {
        let client = client_no_languages();
        let result = client
            .did_open(
                Path::new("/workspace"),
                Path::new("src/main.rs"),
                "fn main() {}",
            )
            .await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    #[tokio::test]
    async fn test_lawyer_did_change_no_lsp() {
        let client = client_no_languages();
        let result = client
            .did_change(
                Path::new("/workspace"),
                Path::new("src/main.rs"),
                "fn main() { updated }",
                2,
            )
            .await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    #[tokio::test]
    async fn test_lawyer_did_close_no_lsp() {
        let client = client_no_languages();
        let result = client
            .did_close(Path::new("/workspace"), Path::new("src/main.rs"))
            .await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    #[tokio::test]
    async fn test_lawyer_pull_diagnostics_no_lsp() {
        let client = client_no_languages();
        let result = client
            .pull_diagnostics(Path::new("/workspace"), Path::new("src/main.rs"))
            .await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    #[tokio::test]
    async fn test_lawyer_pull_workspace_diagnostics_no_lsp() {
        let client = client_no_languages();
        let result = client
            .pull_workspace_diagnostics(Path::new("/workspace"), Path::new("src/main.rs"))
            .await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    #[tokio::test]
    async fn test_lawyer_range_formatting_no_lsp() {
        let client = client_no_languages();
        let result = client
            .range_formatting(
                Path::new("/workspace"),
                Path::new("src/main.rs"),
                1,
                5,
                "fn main() {}",
            )
            .await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    #[tokio::test]
    async fn test_lawyer_did_change_watched_files_is_noop() {
        // did_change_watched_files on LspClient is currently a no-op
        let client = client_no_languages();
        let result = client.did_change_watched_files(vec![]).await;
        assert!(result.is_ok());
    }

    // ── touch tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_touch_no_process_is_noop() {
        // touch should not panic when no process exists
        let client = client_no_languages();
        client.touch("rust"); // Should not panic
    }

    // ── InFlightGuard tests ──────────────────────────────────────

    #[test]
    fn test_in_flight_guard_increments_and_decrements() {
        let counter = Arc::new(AtomicU32::new(0));
        assert_eq!(counter.load(Ordering::Relaxed), 0);

        {
            let _guard = InFlightGuard::new(Arc::clone(&counter));
            assert_eq!(counter.load(Ordering::Relaxed), 1);

            {
                let _guard2 = InFlightGuard::new(Arc::clone(&counter));
                assert_eq!(counter.load(Ordering::Relaxed), 2);
            }
            assert_eq!(counter.load(Ordering::Relaxed), 1);
        }
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    // ── Warm start tests ─────────────────────────────────────────

    #[tokio::test]
    async fn test_warm_start_no_languages_is_noop() {
        let client = client_no_languages();
        client.warm_start(); // Should not panic
                             // Give spawned tasks a chance to run
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    #[tokio::test]
    async fn test_shutdown_sends_signal() {
        let client = client_no_languages();

        // Subscribe before sending to ensure we catch it
        let mut rx = client.shutdown_tx.subscribe();

        client.shutdown();

        // The receiver should get the shutdown signal
        let result = rx.try_recv();
        assert!(
            result.is_ok(),
            "shutdown signal should be sent and received"
        );
    }
}
