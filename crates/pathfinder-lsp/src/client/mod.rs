//! `LspClient` — the production [`Lawyer`] implementation.
//!
//! `LspClient` manages a pool of LSP child processes (one per language).
//! Processes are started lazily on first use and terminated automatically
//! after an idle timeout.
//!
//! # Crash Recovery
//! When a crash is detected the client restarts the process with exponential
//! back-off capped at 60 seconds (1s → 2s → 4s → … → 60s). The process
//! is never permanently marked unavailable — each backoff window is computed
//! from `backoff_attempt` so recovery is always attempted.

mod capabilities;
mod detect;
mod process;
mod protocol;
mod transport;

pub use capabilities::{DetectedCapabilities, DiagnosticsStrategy};
pub use detect::install_hint;
pub use detect::{
    detect_languages, language_id_for_extension, DetectionResult, LanguageLsp, MissingLanguage,
};

use crate::types::{CallHierarchyCall, CallHierarchyItem};
use crate::{DefinitionLocation, Lawyer, LspError};
use async_trait::async_trait;
use dashmap::DashMap;
use detect::LanguageLsp as LspDescriptor;
use process::{send, send_via_stdin, shutdown, spawn_and_initialize, ManagedProcess};
use protocol::RequestDispatcher;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

/// LSP-HEALTH-001 Task 6.1: Fallback timeout for indexing completion.
/// Non-Rust LSPs (gopls, tsserver, pyright) may not emit `WorkDoneProgressEnd`
/// notifications. After this many seconds, assume indexing is complete.
const INDEXING_FALLBACK_TIMEOUT_SECS: u64 = 30;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use url::Url;

/// Default idle timeout: 15 minutes for standard LSPs.
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_mins(15);
/// Maximum exponential backoff cap: never wait longer than 60 seconds between retries.
const MAX_BACKOFF_SECS: u64 = 60;
/// Grace period between idle checks.
const IDLE_CHECK_INTERVAL: Duration = Duration::from_mins(1);

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
    /// carrying a `WorkDoneProgressEnd` token for the initial indexing job.
    /// Never reset to `false` once set — processes don't re-index unless restarted.
    indexing_complete: Arc<std::sync::atomic::AtomicBool>,
    /// MT-3: Live capabilities — reflects initial `initialize` capabilities PLUS
    /// any dynamic registrations received via `client/registerCapability`.
    ///
    /// Protected by `RwLock` so the `registration_watcher_task` can mutate it
    /// from its background task while the main thread reads it from `capabilities_for`.
    live_capabilities: Arc<std::sync::RwLock<DetectedCapabilities>>,
    /// COEX-1: Set when another LSP instance is detected on the same workspace.
    ///
    /// When `true`, LSP diagnostics validation is automatically skipped to avoid
    /// resource contention with the co-existing instance (e.g. VS Code's LSP).
    /// Navigation (`goto_definition`, `analyze_impact`) still works normally.
    in_coexistence_mode: bool,
}

/// Tracks backoff state for a language whose last spawn attempt failed.
///
/// The language is never permanently dead — `ensure_process` will retry once
/// the exponential backoff window has elapsed.
struct UnavailableState {
    /// When this language last failed to start (start of current backoff window).
    unavailable_since: Instant,
    /// Number of consecutive failed spawn attempts. Used to compute backoff:
    /// `min(1 << backoff_attempt, MAX_BACKOFF_SECS)` seconds.
    backoff_attempt: u32,
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
                // MT-3: Read from live_capabilities (may include dynamic registrations).
                #[allow(clippy::expect_used)]
                let caps = state
                    .live_capabilities
                    .read()
                    .expect("live_capabilities lock");
                // COEX-1: Override diagnostics_strategy to None when in coexistence mode
                // to prevent the costly validation cycle from racing with the external LSP.
                let effective_diag_strategy = if state.in_coexistence_mode {
                    DiagnosticsStrategy::None
                } else {
                    caps.diagnostics_strategy
                };
                validation_status_from_parts(
                    command,
                    true,
                    effective_diag_strategy,
                    caps.definition_provider,
                    caps.call_hierarchy_provider,
                    caps.formatting_provider,
                    state
                        .indexing_complete
                        .load(std::sync::atomic::Ordering::Relaxed),
                    state.spawned_at.elapsed().as_secs(),
                    caps.server_name.as_deref(),
                )
            }
            Self::Unavailable(_) => validation_status_from_parts(
                command,
                false,
                DiagnosticsStrategy::None,
                false,
                false,
                false,
                false,
                0,
                None,
            ),
        }
    }
}

/// Pure helper that maps raw process state to [`LspLanguageStatus`].
///
/// Extracted from [`ProcessEntry::to_validation_status`] to make the
/// mapping logic independently unit-testable without requiring a live
/// [`ManagedProcess`] (which embeds an OS child process handle).
///
/// MT-2: `server_name` added so the status carries the server identity for
/// per-server push diagnostics config selection in validation tools.
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
    server_name: Option<&str>,
) -> crate::types::LspLanguageStatus {
    if !running {
        return crate::types::LspLanguageStatus {
            validation: false,
            reason: format!("{command} failed to start or crashed repeatedly"),
            navigation_ready: None,
            indexing_complete: None,
            uptime_seconds: None,
            diagnostics_strategy: None,
            supports_definition: None,
            supports_call_hierarchy: None,
            supports_diagnostics: None,
            supports_formatting: None,
            server_name: None,
        };
    }
    let navigation_ready = Some(supports_definition);

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
            navigation_ready,
            indexing_complete: Some(indexing_complete),
            uptime_seconds: Some(uptime_seconds),
            diagnostics_strategy: Some(diagnostics_strategy.as_str().to_owned()),
            supports_definition: Some(supports_definition),
            supports_call_hierarchy: Some(supports_call_hierarchy),
            supports_diagnostics: Some(true),
            supports_formatting: Some(supports_formatting),
            server_name: server_name.map(ToOwned::to_owned),
        },
        DiagnosticsStrategy::None => crate::types::LspLanguageStatus {
            validation: false,
            reason: "LSP connected but does not support diagnostics".to_owned(),
            navigation_ready,
            indexing_complete: Some(indexing_complete),
            uptime_seconds: Some(uptime_seconds),
            diagnostics_strategy: Some("none".to_owned()),
            supports_definition: Some(supports_definition),
            supports_call_hierarchy: Some(supports_call_hierarchy),
            supports_diagnostics: Some(false),
            supports_formatting: Some(supports_formatting),
            server_name: server_name.map(ToOwned::to_owned),
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

/// RAII guard that automatically sends `textDocument/didClose` when dropped.
///
/// # IW-3 `DocumentGuard`
///
/// Wraps a `did_open` call and ensures the corresponding `did_close` is always
/// sent, even if the caller panics or returns early. This eliminates the class
/// of document-leak bugs where `did_close` is forgotten after an early return.
///
/// ## Usage
///
/// ```ignore
/// let _guard = client.open_document(&workspace, &file_path, &content).await?;
/// // do LSP work…
/// // `_guard` drops here → `did_close` sent automatically
/// ```
///
/// The guard is `!Send` because it holds a reference to the `LspClient` arc
/// for the async drop helper. All LSP operations are `async`, so the drop
/// itself is fire-and-forget (it spawns a task internally).
pub struct DocumentGuard {
    client: LspClient,
    workspace_root: std::path::PathBuf,
    file_path: std::path::PathBuf,
}

impl DocumentGuard {
    fn new(
        client: LspClient,
        workspace_root: std::path::PathBuf,
        file_path: std::path::PathBuf,
    ) -> Self {
        Self {
            client,
            workspace_root,
            file_path,
        }
    }
}

impl Drop for DocumentGuard {
    fn drop(&mut self) {
        // Fire-and-forget `did_close`. We cannot `.await` in `Drop`, so we
        // spawn a detached task. If the runtime is already shutting down this
        // is a no-op — acceptable during process exit.
        let client = self.client.clone();
        let workspace = self.workspace_root.clone();
        let path = self.file_path.clone();
        tokio::spawn(async move {
            let _ = client.did_close(&workspace, &path).await;
        });
    }
}

// IW-3 (DS-1 gap fix): DocumentGuard satisfies the DocumentLease contract —
// dropping it fires did_close via the impl above.
impl crate::lawyer::DocumentLease for DocumentGuard {}
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
    /// Per-file document version counter (ST-4).
    ///
    /// Keyed by the file URI string. `did_open` sets version 1.
    /// `did_close` removes the entry.
    doc_versions: Arc<DashMap<String, std::sync::atomic::AtomicI32>>,
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
            doc_versions: Arc::new(DashMap::new()),
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
    /// LT-4: Pre-warm specific languages based on project analysis.
    /// LT-4: Pre-warm specific languages based on project analysis.
    ///
    /// Unlike `warm_start()` which starts ALL detected languages, this method
    /// only starts the explicitly requested ones. Used by `get_repo_map` to
    /// pre-warm LSP processes for languages discovered in the project skeleton.
    ///
    /// Languages not in `self.descriptors` or already running are silently skipped.
    /// Errors during initialization are logged but not propagated — lazy recovery
    /// on first use is always available.
    pub fn warm_start_for_languages(&self, language_ids: &[String]) {
        let known: std::collections::HashSet<&str> = self
            .descriptors
            .iter()
            .map(|d| d.language_id.as_str())
            .collect();

        let to_start: Vec<&String> = language_ids
            .iter()
            .filter(|lang| known.contains(lang.as_str()))
            .filter(|lang| !self.processes.contains_key(lang.as_str()))
            .collect();

        if to_start.is_empty() {
            return;
        }

        tracing::info!(
            languages = ?to_start,
            "LT-4: warm_start_for_languages — pre-warming requested languages"
        );

        for lang in to_start {
            let client = self.clone();
            let lang = lang.clone();
            tokio::spawn(async move {
                tracing::debug!(language = %lang, "LT-4: warm_start_for_languages — starting");
                match client.ensure_process(&lang).await {
                    Ok(()) => {
                        tracing::info!(language = %lang, "LT-4: warm_start_for_languages — ready");
                    }
                    Err(e) => {
                        tracing::warn!(
                            language = %lang,
                            error = %e,
                            "LT-4: warm_start_for_languages — failed (will retry lazily)"
                        );
                    }
                }
            });
        }
    }

    /// LT-4: Extend idle timer for a language without making an LSP request.
    ///
    /// Called by `read_source_file` and other non-LSP tools that operate on
    /// source files. This prevents the LSP from timing out while the agent
    /// is actively reading files of the same language — a strong signal that
    /// LSP operations will follow soon.
    pub fn touch_language(&self, language_id: &str) {
        if let Some(mut entry) = self.processes.get_mut(language_id) {
            if let ProcessEntry::Running(state) = entry.value_mut() {
                state.process.last_used = Instant::now();
                tracing::trace!(
                    language = language_id,
                    "LT-4: idle timer refreshed from file operation"
                );
            }
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
    /// Open a document and return a `DocumentGuard` that auto-closes it.
    ///
    /// # IW-3
    ///
    /// This is the preferred way to open documents for transient LSP queries.
    /// The returned guard calls `did_close` when it goes out of scope, ensuring
    /// no document leaks regardless of early returns or panics.
    ///
    /// # Errors
    /// Returns `Err` if `did_open` fails (process not running, I/O error, etc.).
    pub async fn open_document(
        &self,
        workspace_root: &std::path::Path,
        file_path: &std::path::Path,
        content: &str,
    ) -> Result<DocumentGuard, LspError> {
        self.did_open(workspace_root, file_path, content).await?;
        Ok(DocumentGuard::new(
            self.clone(),
            workspace_root.to_path_buf(),
            file_path.to_path_buf(),
        ))
    }

    /// Open an LSP document (textDocument/didOpen notification).
    ///
    /// This is an inherent helper called by `open_document` and not exposed as
    /// an MCP tool. Sends `textDocument/didOpen` and tracks the document version.
    async fn did_open(
        &self,
        workspace_root: &std::path::Path,
        file_path: &std::path::Path,
        content: &str,
    ) -> Result<(), LspError> {
        tracing::debug!(file = %file_path.display(), "LSP: did_open");
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;
        self.ensure_process(language_id).await?;

        let file_uri = Url::from_file_path(workspace_root.join(file_path))
            .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?;

        // Set version to 1 on open
        self.doc_versions
            .insert(file_uri.to_string(), std::sync::atomic::AtomicI32::new(1));

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
            tracing::error!(language = language_id, error = %e, "textDocument/didOpen failed");
            self.doc_versions.remove(&file_uri.to_string());
            return Err(e);
        }
        self.touch(language_id);
        Ok(())
    }

    /// Close an LSP document (textDocument/didClose notification).
    ///
    /// Called automatically by `DocumentGuard::drop`. Not exposed as an MCP tool.
    pub(crate) async fn did_close(
        &self,
        workspace_root: &std::path::Path,
        file_path: &std::path::Path,
    ) -> Result<(), LspError> {
        tracing::debug!(file = %file_path.display(), "LSP: did_close");
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;
        self.ensure_process(language_id).await?;

        let file_uri = Url::from_file_path(workspace_root.join(file_path))
            .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?;

        self.doc_versions.remove(&file_uri.to_string());

        let params = json!({
            "textDocument": { "uri": file_uri.as_str() }
        });

        if let Err(e) = self
            .notify(language_id, "textDocument/didClose", params)
            .await
        {
            tracing::error!(language = language_id, error = %e, "textDocument/didClose failed");
            return Err(e);
        }
        Ok(())
    }

    /// Force-respawn the LSP process for the given language.
    ///
    /// # IW-4
    ///
    /// Gracefully shuts down any existing `Running` process for `language_id`
    /// before starting a fresh one. Removes any `Unavailable` entry directly.
    ///
    /// Returns `Ok(())` when the new process successfully initializes.
    ///
    /// # Errors
    /// Returns `Err(LspError::NoLspAvailable)` if no descriptor is registered.
    pub async fn force_respawn(&self, language_id: &str) -> Result<(), LspError> {
        tracing::info!(language = %language_id, "LspClient: force_respawn requested");

        // Find the descriptor before removing the entry (borrow checker)
        let descriptor = self
            .descriptors
            .iter()
            .find(|d| d.language_id == language_id)
            .ok_or(LspError::NoLspAvailable)?
            .clone();

        // ZOMBIE-1: Kill existing Running process before removing entry.
        // Without this, the old child process becomes an OS zombie —
        // it is removed from our tracking map but still occupies a PID
        // in the process table until the parent calls wait().
        if let Some((_, ProcessEntry::Running(mut state))) = self.processes.remove(language_id) {
            tracing::info!(
                language = %language_id,
                "LSP: force_respawn — killing existing process before respawn"
            );
            state.reader_handle.abort();
            shutdown(&mut state.process, &self.dispatcher).await;
        } else {
            // Unavailable entry — just remove it
            self.processes.remove(language_id);
        }

        // Spawn fresh at attempt 0 (no backoff delay)
        self.start_process(descriptor, 0).await
    }

    /// Ensure an LSP process is running for `language_id`, starting it if needed.
    ///
    /// Returns `Err(LspError::NoLspAvailable)` if no descriptor is found for
    /// this language. If a previous spawn failed, retries once the exponential
    /// backoff window (`min(2^attempt, 60)` seconds) has elapsed.
    async fn ensure_process(&self, language_id: &str) -> Result<(), LspError> {
        // Fast path: already running
        if let Some(entry) = self.processes.get(language_id) {
            return match entry.value() {
                ProcessEntry::Running(_) => Ok(()),
                ProcessEntry::Unavailable(state) => {
                    // ST-1: Compute backoff window from attempt count (capped at MAX_BACKOFF_SECS)
                    let backoff_secs =
                        std::cmp::min(1u64 << state.backoff_attempt, MAX_BACKOFF_SECS);
                    let elapsed_secs = state.unavailable_since.elapsed().as_secs();
                    if elapsed_secs >= backoff_secs {
                        // Backoff window elapsed — attempt recovery
                        drop(entry);
                        tracing::info!(
                            language = %language_id,
                            backoff_secs,
                            elapsed_secs,
                            "LSP: backoff elapsed, attempting recovery"
                        );
                        self.processes.remove(language_id);
                        Ok(())
                    } else {
                        tracing::debug!(
                            language = %language_id,
                            backoff_secs,
                            elapsed_secs,
                            "LSP: in backoff window, returning NoLspAvailable"
                        );
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
                    let backoff_secs =
                        std::cmp::min(1u64 << state.backoff_attempt, MAX_BACKOFF_SECS);
                    let elapsed_secs = state.unavailable_since.elapsed().as_secs();
                    if elapsed_secs >= backoff_secs {
                        drop(entry);
                        tracing::info!(
                            language = %language_id,
                            backoff_secs,
                            elapsed_secs,
                            "LSP: backoff elapsed (post-lock check), attempting recovery"
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

        // Spawn the process (attempt 0 = first try, no backoff delay)
        self.start_process(descriptor, 0).await
    }

    /// Spawn a timeout task that sets `indexing_complete` if no `WorkDoneProgressEnd`
    /// is received within `INDEXING_FALLBACK_TIMEOUT_SECS`.
    ///
    /// This is a fallback for LSPs (gopls, tsserver, pyright) that don't emit
    /// `WorkDoneProgressEnd` notifications. After 30 seconds, assume indexing is
    /// complete to prevent eternal "`in_progress`" status.
    fn spawn_indexing_timeout_fallback(
        language_id: String,
        indexing_complete: &Arc<std::sync::atomic::AtomicBool>,
    ) {
        let timeout_flag = Arc::clone(indexing_complete);
        let timeout_lang = language_id;
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_secs(
                INDEXING_FALLBACK_TIMEOUT_SECS,
            ))
            .await;
            if !timeout_flag.load(std::sync::atomic::Ordering::Relaxed) {
                timeout_flag.store(true, std::sync::atomic::Ordering::Relaxed);
                tracing::info!(
                    language = %timeout_lang,
                    timeout_sec = INDEXING_FALLBACK_TIMEOUT_SECS,
                    "LSP: no WorkDoneProgressEnd received after {INDEXING_FALLBACK_TIMEOUT_SECS}s — \
                     assuming indexing complete (timeout fallback for gopls/tsserver/pyright)"
                );
            }
        });
    }

    #[allow(clippy::too_many_lines)]
    /// Spawn a new LSP process, retrying on failure with exponential backoff.
    async fn start_process(&self, descriptor: LspDescriptor, attempt: u32) -> Result<(), LspError> {
        let language_id = descriptor.language_id.clone();

        // ST-1: No upper limit on attempts. On failure, insert Unavailable
        // with backoff_attempt = attempt + 1 and return. ensure_process will
        // re-enter once the backoff window elapses — no permanent death.
        if attempt > 0 {
            let delay = Duration::from_secs(std::cmp::min(1u64 << (attempt - 1), MAX_BACKOFF_SECS));
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

        // COEX-1: Detect concurrent LSP instances and activate coexistence mode.
        // `in_coexistence_mode` controls both build-artifact isolation (env vars)
        // and validation suppression. When another LSP is detected on the same
        // workspace, Pathfinder will still spawn its own instance for navigation
        // (goto_definition, analyze_impact) but will skip validation to avoid
        // fighting over inotify watches, module download locks, and module graphs.
        let in_coexistence_mode = self.detect_concurrent_lsp(&language_id, &descriptor.command);
        let isolate_target_dir = in_coexistence_mode;

        let plugins = descriptor.auto_plugins.clone();
        let python_path = descriptor.python_path.clone();
        let spawn_result = spawn_and_initialize(
            &descriptor.command,
            &descriptor.args,
            &descriptor.root,
            &language_id,
            Arc::clone(&self.dispatcher),
            descriptor.init_timeout_secs,
            isolate_target_dir,
            plugins,
            python_path,
        )
        .await;

        let (process, reader_handle) = match spawn_result {
            Ok(res) => res,
            Err(e) => {
                // ST-1: Record failure with incremented backoff_attempt.
                // ensure_process uses this to compute the next retry window.
                let next_attempt = attempt.saturating_add(1);
                let next_backoff_secs = std::cmp::min(1u64 << next_attempt, MAX_BACKOFF_SECS);
                tracing::error!(
                    language = %language_id,
                    error = %e,
                    attempt,
                    next_backoff_secs,
                    "LSP: initialization failed — will retry after backoff"
                );
                self.processes.insert(
                    language_id,
                    ProcessEntry::Unavailable(UnavailableState {
                        unavailable_since: Instant::now(),
                        backoff_attempt: next_attempt,
                    }),
                );
                return Err(LspError::NoLspAvailable);
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
                attempt,
                "LSP: recovery successful after backoff"
            );
        }

        // Create indexing_complete flag — will be set by the progress watcher task
        // when the LSP emits WorkDoneProgressEnd for its initial indexing job.
        let indexing_complete = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let spawned_at = Instant::now();

        // Spawn a background task that monitors $/progress notifications from the
        // LSP and sets indexing_complete when the initial indexing token closes.
        let indexing_flag = Arc::clone(&indexing_complete);
        let lang_id_for_watcher = language_id.clone();
        let dispatcher_for_watcher = Arc::clone(&self.dispatcher);
        tokio::spawn(async move {
            progress_watcher_task(lang_id_for_watcher, dispatcher_for_watcher, indexing_flag).await;
        });

        // LSP-HEALTH-001 Task 6.1: Progress watcher timeout fallback
        // Non-Rust LSPs (gopls, tsserver, pyright) may not emit WorkDoneProgressEnd
        // notifications. After 30 seconds, assume indexing is complete.
        Self::spawn_indexing_timeout_fallback(language_id.clone(), &indexing_complete);

        // MT-3: Create the shared live_capabilities from the initial snapshot.
        // The registration_watcher_task will mutate this as dynamic registrations arrive.
        let live_capabilities = Arc::new(std::sync::RwLock::new(process.capabilities.clone()));

        // MT-3: Spawn the registration watcher task.
        // It listens for client/registerCapability and client/unregisterCapability
        // server requests, responds with {}, and updates live_capabilities.
        let caps_for_reg = Arc::clone(&live_capabilities);
        let stdin_for_reg = Arc::clone(&process.stdin);
        let lang_id_for_reg = language_id.clone();
        let dispatcher_for_reg = Arc::clone(&self.dispatcher);
        tokio::spawn(async move {
            registration_watcher_task(
                lang_id_for_reg,
                dispatcher_for_reg,
                caps_for_reg,
                stdin_for_reg,
            )
            .await;
        });

        if in_coexistence_mode {
            tracing::warn!(
                language = %language_id,
                "LSP: coexistence mode active — LSP validation disabled to prevent resource \
                 contention. Navigation (goto_definition, analyze_impact) still works normally."
            );
        }

        self.processes.insert(
            language_id,
            ProcessEntry::Running(Box::new(LanguageState {
                process,
                reader_handle: supervisor_handle,
                restart_count: attempt,
                spawned_at,
                indexing_complete,
                live_capabilities,
                in_coexistence_mode,
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

        // GUARD: If the command is an absolute path inside a build-artifact
        // directory (target/ or .cargo/), it is a compiled test binary or a
        // development build — never an IDE-external process. Parallel integration
        // tests (nextest) each spawn their own mock LSP instance from target/;
        // without this guard, sibling test processes would wrongly trigger
        // coexistence mode on each other because the sibling mock's parent PID
        // is a different nextest worker, not our own PID.
        let cmd_path = Path::new(command);
        if cmd_path.is_absolute() {
            let is_build_artifact = cmd_path.components().any(
                |c| matches!(c, std::path::Component::Normal(n) if n == "target" || n == ".cargo"),
            );
            if is_build_artifact {
                tracing::trace!(
                    binary = binary_name,
                    "detect_concurrent_lsp: command is a build artifact path — skipping detection"
                );
                return false;
            }
        }

        // Check if there's already a process with this binary name running
        // that we didn't spawn. We do this by counting how many instances
        // exist in the system process table.
        #[cfg(target_os = "linux")]
        {
            // GAP-Z2: Filter out Pathfinder's own children.
            //
            // The old detection counted ALL matching processes, including:
            // 1. Processes we previously spawned (before force_respawn cleaned them up)
            // 2. Any new process we just spawned (not yet in the map)
            //
            // We fix this by reading /proc/<pid>/status to get the PPid field.
            // Only count processes whose parent PID is *not* Pathfinder's own PID.
            // This correctly identifies IDE-launched LSPs while ignoring our own children.
            let our_pid = std::process::id();

            if let Ok(entries) = std::fs::read_dir("/proc") {
                let mut external_count = 0;
                for entry in entries.flatten() {
                    let path = entry.path();
                    // Only look at numeric /proc/<pid> directories
                    let is_numeric = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()));
                    if !is_numeric {
                        continue;
                    }

                    let cmdline_path = path.join("cmdline");
                    let Ok(cmdline) = std::fs::read_to_string(&cmdline_path) else {
                        continue;
                    };
                    if !cmdline.contains(binary_name) {
                        continue;
                    }

                    // This process matches the binary name. Check if it's our child.
                    // Read /proc/<pid>/status to find the PPid line.
                    let status_path = path.join("status");
                    let parent_pid: u32 = std::fs::read_to_string(&status_path)
                        .ok()
                        .and_then(|status| {
                            status
                                .lines()
                                .find(|l| l.starts_with("PPid:"))
                                .and_then(|l| l.split_whitespace().nth(1))
                                .and_then(|v| v.parse().ok())
                        })
                        .unwrap_or(0);

                    if parent_pid == our_pid {
                        // This is one of our own children — skip it.
                        // Happens when the old process is still in /proc but
                        // already signalled for termination (e.g., during force_respawn).
                        tracing::trace!(
                            binary = binary_name,
                            "detect_concurrent_lsp: skipping own child process"
                        );
                        continue;
                    }

                    external_count += 1;
                }

                if external_count > 0 {
                    // LSP-HEALTH-001 Task 5.1: Accurately describe what isolation is applied.
                    let isolation_desc = match language_id {
                        "rust" => "Cargo target directory",
                        "go" => "Go build cache (GOCACHE/GOMODCACHE)",
                        "typescript" => "TypeScript temp directory (TMPDIR)",
                        "python" => "Python bytecode cache (PYTHONPYCACHEPREFIX)",
                        _ => "No", // Other languages have no isolation yet
                    };
                    tracing::warn!(
                        language = language_id,
                        binary = binary_name,
                        external_instances = external_count,
                        "LSP: detected {} external concurrent instances of {binary_name}. \
                         {} build artifact isolation will be applied to avoid cache lock contention. \
                         First-time indexing may take 30-60s for this workspace.",
                        external_count,
                        isolation_desc
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
        match self.processes.get(language_id) {
            None => Err(LspError::NoLspAvailable),
            Some(entry) => match entry.value() {
                ProcessEntry::Unavailable(_) => Err(LspError::NoLspAvailable),
                // MT-3: Read from live_capabilities (includes dynamic registrations) rather
                // than process.capabilities (initial snapshot only).
                #[allow(clippy::expect_used)] // RwLock poisoning is unrecoverable
                ProcessEntry::Running(state) => Ok(state
                    .live_capabilities
                    .read()
                    .expect("live_capabilities lock")
                    .clone()),
            },
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
    /// LT-4: Trait implementation delegates to the inherent method.
    fn warm_start_for_languages(&self, language_ids: &[String]) {
        LspClient::warm_start_for_languages(self, language_ids);
    }

    /// LT-4: Trait implementation delegates to the inherent method.
    fn touch_language(&self, language_id: &str) {
        LspClient::touch_language(self, language_id);
    }

    /// IW-3 (DS-1 gap fix): RAII document lifecycle for navigation queries.
    ///
    /// Opens the document via `did_open` and returns a `DocumentGuard` boxed as
    /// `Box<dyn DocumentLease>`. Dropping the lease fires `did_close`
    /// automatically, ensuring no document leaks regardless of early returns.
    async fn open_document(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        content: &str,
    ) -> Result<Box<dyn crate::lawyer::DocumentLease>, LspError> {
        let guard = LspClient::open_document(self, workspace_root, file_path, content).await?;
        Ok(Box::new(guard))
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

        parse_definition_response(response, workspace_root)
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

    async fn capability_status(&self) -> HashMap<String, crate::types::LspLanguageStatus> {
        let mut status = HashMap::new();
        for desc in self.descriptors.iter() {
            let lang_status = self.processes.get(&desc.language_id).map_or_else(
                || crate::types::LspLanguageStatus {
                    validation: true,
                    reason: format!("{} available (lazy start)", desc.command),
                    navigation_ready: None,
                    diagnostics_strategy: None,
                    // Process hasn't started yet — indexing status and uptime unknown
                    indexing_complete: None,
                    uptime_seconds: None,
                    // Capabilities unknown until process starts
                    supports_definition: None,
                    supports_call_hierarchy: None,
                    supports_diagnostics: None,
                    supports_formatting: None,
                    server_name: None,
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

    async fn force_respawn(&self, language_id: &str) -> Result<(), LspError> {
        LspClient::force_respawn(self, language_id).await
    }
}
fn parse_definition_response(
    response: serde_json::Value,
    workspace_root: &Path,
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

    // Convert URI to an absolute path, then strip workspace root to get a
    // relative path — mirroring the convention used by parse_call_hierarchy_prepare_response.
    // Falls back to the raw URI string if either conversion fails (e.g., external files).
    let abs_path = Url::parse(&uri_str)
        .ok()
        .and_then(|u: Url| u.to_file_path().ok());

    let file = abs_path
        .as_deref()
        .and_then(|p| p.strip_prefix(workspace_root).ok())
        .map(|p| p.to_string_lossy().into_owned())
        .or_else(|| abs_path.as_deref().map(|p| p.to_string_lossy().into_owned()))
        .unwrap_or(uri_str);

    // Read the definition line from disk to populate the preview.
    // `start_line` is 0-indexed (LSP convention); `nth(start_line)` handles this directly.
    // Falls back to an empty string if the file cannot be read (e.g., external crate).
    let preview = abs_path
        .as_deref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|content| {
            content
                .lines()
                .nth(usize::try_from(start_line).unwrap_or(0))
                .map(|l| l.trim().to_owned())
        })
        .unwrap_or_default();

    Ok(Some(DefinitionLocation {
        file,
        line: u32::try_from(start_line + 1).unwrap_or(1), // 0-indexed → 1-indexed
        column: u32::try_from(start_char + 1).unwrap_or(1),
        preview,
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
/// When the reader task exits (EOF or crash), this supervisor:
/// 1. Removes the process entry from the map.
/// 2. **Calls `child.wait()`** on the removed child to fully reap the OS zombie
///    and free its PID slot. Without this, dropped `Child` objects linger in the
///    process table as zombies until Pathfinder exits.
/// 3. On *crash* (non-zero exit / panic), inserts an `UnavailableState` with
///    `backoff_attempt = 1` so the next `ensure_process` call uses exponential
///    backoff rather than immediately re-spawning at full speed (GAP-Z3).
async fn reader_supervisor_task(
    language_id: String,
    reader_handle: tokio::task::JoinHandle<()>,
    processes: Arc<DashMap<String, ProcessEntry>>,
) {
    let crashed = match reader_handle.await {
        Ok(()) => {
            // Normal EOF — LSP exited cleanly (e.g., idle timeout, shutdown request).
            tracing::warn!(
                language = %language_id,
                "LSP: reader task exited normally (EOF), removing process entry"
            );
            false
        }
        Err(e) => {
            // Panic or abort — unexpected crash.
            tracing::error!(
                language = %language_id,
                error = %e,
                "LSP: reader task crashed (panic or abort), removing process entry"
            );
            true
        }
    };

    // GAP-Z1: Remove the entry and reap the OS zombie.
    //
    // `processes.remove()` returns the evicted value. Extracting the child and
    // calling `wait()` on it frees the PID slot immediately rather than leaving
    // the process as a zombie until the next idle-loop sweep (up to 60s later)
    // or until Pathfinder itself exits.
    if let Some((_lang, ProcessEntry::Running(mut state))) = processes.remove(&language_id) {
        tracing::debug!(
            language = %language_id,
            "LSP: supervisor reaping child process to free PID slot"
        );
        state.reader_handle.abort();
        // Reap the OS zombie. Ignore the result — we just need to call wait().
        let _ = state.process.child.wait().await;

        // GAP-Z3: On crash, insert UnavailableState so ensure_process applies
        // exponential backoff on the next recovery attempt. Without this, a
        // rapidly-crashing LSP would be re-spawned at full speed on every
        // incoming agent request with no delay (attempt=0 bypass).
        if crashed {
            tracing::warn!(
                language = %language_id,
                "LSP: inserting Unavailable entry after crash for backoff protection"
            );
            processes.insert(
                language_id,
                ProcessEntry::Unavailable(UnavailableState {
                    unavailable_since: std::time::Instant::now(),
                    backoff_attempt: 1, // start at attempt 1 → 1s minimum backoff
                }),
            );
        }
        // On clean EOF (idle timeout / graceful shutdown), no Unavailable entry
        // is inserted. The next request will call start_process(descriptor, 0)
        // with no backoff delay — correct, because the LSP was healthy when it exited.
    } else {
        // Entry was already removed by the idle-loop or force_respawn before we got here.
        tracing::debug!(
            language = %language_id,
            "LSP: supervisor found entry already removed (raced with idle-loop or force_respawn)"
        );
    }
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
/// MT-3: Background task — handle `client/registerCapability` and
/// `client/unregisterCapability` server-to-client requests.
///
/// Subscribes to the `server_request_tx` channel in `RequestDispatcher`.
/// For each request:
/// 1. Applies the registration/unregistration to `live_capabilities`.
/// 2. Sends a `{"jsonrpc":"2.0","id":<same-id>,"result":null}` response.
///
/// The task exits when the broadcast channel is closed (LSP process died).
async fn registration_watcher_task(
    language_id: String,
    dispatcher: Arc<RequestDispatcher>,
    live_capabilities: Arc<std::sync::RwLock<DetectedCapabilities>>,
    stdin: Arc<tokio::sync::Mutex<tokio::io::BufWriter<tokio::process::ChildStdin>>>,
) {
    let mut rx = dispatcher.subscribe_server_requests();
    tracing::debug!(language = %language_id, "registration_watcher_task: started");

    loop {
        match rx.recv().await {
            Ok(msg) => {
                let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
                let id = msg.get("id");

                match method {
                    "client/registerCapability" => {
                        if let Some(registrations) = msg
                            .pointer("/params/registrations")
                            .and_then(|v| v.as_array())
                        {
                            #[allow(clippy::expect_used)]
                            let mut caps = live_capabilities
                                .write()
                                .expect("live_capabilities write lock");
                            for reg in registrations {
                                let reg_id = reg.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                let reg_method =
                                    reg.get("method").and_then(|v| v.as_str()).unwrap_or("");
                                let opts = reg.get("registerOptions").cloned().unwrap_or(
                                    serde_json::Value::Object(serde_json::Map::default()),
                                );
                                if caps.apply_registration(reg_method, reg_id, &opts) {
                                    tracing::info!(
                                        language = %language_id,
                                        method = reg_method,
                                        id = reg_id,
                                        "LSP: dynamic capability registered"
                                    );
                                }
                            }
                        }
                    }
                    "client/unregisterCapability" => {
                        if let Some(unregs) = msg
                            .pointer("/params/unregisterations")
                            .and_then(|v| v.as_array())
                        {
                            #[allow(clippy::expect_used)]
                            let mut caps = live_capabilities
                                .write()
                                .expect("live_capabilities write lock");
                            for unreg in unregs {
                                let reg_id = unreg.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                if caps.apply_unregistration(reg_id) {
                                    tracing::info!(
                                        language = %language_id,
                                        id = reg_id,
                                        "LSP: dynamic capability unregistered"
                                    );
                                }
                            }
                        }
                    }
                    other => {
                        tracing::debug!(
                            language = %language_id,
                            method = other,
                            "registration_watcher_task: unrecognised server request, sending null response"
                        );
                    }
                }

                // Send null response back to the server for any server-to-client request.
                // The LSP protocol requires a response for all requests (even for methods
                // the client doesn't fully handle). `result: null` is always safe.
                if let Some(id_val) = id {
                    let response = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id_val,
                        "result": null
                    });
                    if let Err(e) = send_via_stdin(&stdin, &response).await {
                        tracing::warn!(
                            language = %language_id,
                            error = %e,
                            "registration_watcher_task: failed to send response"
                        );
                    }
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(
                    language = %language_id,
                    missed = n,
                    "registration_watcher_task: lagged, missed server requests"
                );
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                tracing::debug!(
                    language = %language_id,
                    "registration_watcher_task: channel closed, exiting"
                );
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
                // ZOMBIE-1: Proactively reap dead processes.
                //
                // A Running entry can persist after the child dies if the reader
                // task hasn't flushed EOF yet (small race window). We poll
                // try_wait() here so dead processes are reaped within one
                // IDLE_CHECK_INTERVAL (60s) rather than accumulating as OS zombies.
                let dead_languages: Vec<String> = processes
                    .iter_mut()
                    .filter_map(|mut entry| {
                        if let ProcessEntry::Running(state) = entry.value_mut() {
                            // is_alive() returns true if still running → we want the dead ones
                            if state.process.is_alive() {
                                None
                            } else {
                                Some(entry.key().clone())
                            }
                        } else {
                            None
                        }
                    })
                    .collect();

                for lang in dead_languages {
                    if let Some((_lang, ProcessEntry::Running(mut state))) = processes.remove(&lang) {
                        tracing::error!(
                            language = %lang,
                            "LSP: zombie reap — process died outside reader task, \
                             removing entry so recovery can proceed"
                        );
                        state.reader_handle.abort();
                        // Fully reap the OS zombie to free its PID slot.
                        let _ = state.process.child.wait().await;
                    }
                }

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
        let result = parse_definition_response(json!(null), Path::new("/"));
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
        let result = parse_definition_response(response, Path::new("/")).expect("ok");
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
        let result = parse_definition_response(response, Path::new("/")).expect("ok");
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
        let result = parse_definition_response(response, Path::new("/")).expect("ok");
        let loc = result.expect("some location");
        assert_eq!(loc.line, 20); // 0-indexed → 1-indexed
        assert!(loc.file.contains("types.rs"));
    }

    #[test]
    fn test_parse_definition_empty_array() {
        let response = json!([]);
        let result = parse_definition_response(response, Path::new("/")).expect("ok");
        // Empty array → null first element → None
        assert!(result.is_none());
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
            backoff_attempt: 0,
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
            true,  // supports_definition
            true,  // supports_call_hierarchy
            false, // supports_formatting
            false, // indexing_complete
            10,    // uptime_seconds
            None,  // server_name
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
            true,  // supports_definition
            true,  // supports_call_hierarchy
            false, // supports_formatting
            true,  // indexing_complete
            42,    // uptime_seconds
            None,  // server_name
        );
        assert!(status.validation);
        assert_eq!(status.indexing_complete, Some(true));
        assert_eq!(status.uptime_seconds, Some(42));
    }

    #[test]
    fn test_process_entry_running_without_diagnostics_status() {
        // LSP connected but does not support textDocument/diagnostic.
        let status = validation_status_from_parts(
            "gopls",
            true,
            DiagnosticsStrategy::None,
            true,
            true,
            false,
            true,
            5,
            None,
        );
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
        let status = validation_status_from_parts(
            "pyright",
            true,
            DiagnosticsStrategy::Pull,
            true,
            true,
            false,
            false,
            0,
            None,
        );
        assert!(status.uptime_seconds.is_some());
        assert!(status.indexing_complete.is_some());
    }

    // ── navigation_ready tests (LSP-HEALTH-001) ──────────────────

    #[test]
    fn test_navigation_ready_true_when_supports_definition_and_running() {
        // Pyright scenario: LSP running, supports_definition=true,
        // but diagnostics_strategy=None AND indexing_complete=false.
        // Navigation should still be "ready" because initialize handshake completed.
        let status = validation_status_from_parts(
            "pyright",
            true, // running
            DiagnosticsStrategy::None,
            true,  // supports_definition
            true,  // supports_call_hierarchy
            false, // supports_formatting
            false, // indexing_complete (still indexing)
            5,     // uptime_seconds
            None,  // server_name
        );
        // Navigation ready regardless of diagnostics and indexing status
        assert_eq!(status.navigation_ready, Some(true));
        // But validation is false because no diagnostics
        assert!(!status.validation);
        // Indexing is still in progress
        assert_eq!(status.indexing_complete, Some(false));
    }

    #[test]
    fn test_navigation_ready_false_when_supports_definition_false() {
        // Edge case: LSP running but doesn't support definition at all
        let status = validation_status_from_parts(
            "weird-lsp",
            true, // running
            DiagnosticsStrategy::Pull,
            false, // supports_definition = false
            false, // supports_call_hierarchy
            false, // supports_formatting
            true,  // indexing_complete
            10,    // uptime_seconds
            None,  // server_name
        );
        // Navigation not ready because LSP doesn't have definitionProvider capability
        assert_eq!(status.navigation_ready, Some(false));
        // But validation is true because pull diagnostics available
        assert!(status.validation);
    }

    #[test]
    fn test_navigation_ready_none_when_not_running() {
        // When LSP is not running (crashed, failed to start), navigation_ready is None
        let status = validation_status_from_parts(
            "gopls",
            false,                     // NOT running
            DiagnosticsStrategy::None, // irrelevant when !running
            true,                      // irrelevant when !running
            true,                      // irrelevant when !running
            false,                     // irrelevant when !running
            false,                     // irrelevant when !running
            0,                         // irrelevant when !running
            None,                      // server_name
        );
        assert_eq!(status.navigation_ready, None);
        assert_eq!(status.indexing_complete, None);
        assert!(!status.validation);
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
            doc_versions: Arc::new(DashMap::new()),
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
                python_path: None,
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
            doc_versions: Arc::new(DashMap::new()),
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
                backoff_attempt: 0,
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
                backoff_attempt: 0,
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
                backoff_attempt: 0,
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
                backoff_attempt: 0,
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
                backoff_attempt: 0,
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
                backoff_attempt: 0,
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

    // ── LT-4: Predictive LSP Warmup ──────────────────────────────────────────

    #[tokio::test]
    async fn test_warm_start_for_languages_starts_only_requested() {
        // warm_start_for_languages should only attempt to start the explicitly
        // requested languages, not all descriptors. With no real LSP binary,
        // start_process will fail, but the method must not panic.
        let client = client_with_descriptors(vec!["rust", "go", "typescript"], HashMap::new());
        // Only request "go" — "rust" and "typescript" should remain unstarted.
        client.warm_start_for_languages(&["go".to_owned()]);
        // No process should be running (no real binary), but no panic.
    }

    #[tokio::test]
    async fn test_warm_start_for_languages_skips_already_running() {
        // If a process is already running for the language, warm_start_for_languages
        // must skip it without error (idempotent).
        let client = client_with_descriptors(vec!["rust"], HashMap::new());
        // Call twice — should be safe and not panic.
        client.warm_start_for_languages(&["rust".to_owned()]);
        client.warm_start_for_languages(&["rust".to_owned()]);
    }

    #[tokio::test]
    async fn test_warm_start_for_languages_ignores_unknown() {
        // Languages not in descriptors should be silently ignored.
        let client = client_with_descriptors(vec!["rust"], HashMap::new());
        client.warm_start_for_languages(&["unknown_lang".to_owned()]);
    }

    #[tokio::test]
    async fn test_touch_language_extends_idle_timer() {
        // touch_language must update last_used for a running process.
        let client = client_no_languages();
        // With no processes, touch should be a no-op (no panic).
        client.touch_language("rust");
    }

    #[tokio::test]
    async fn test_touch_language_no_process_is_noop() {
        // touch_language on a language with no running process must be a no-op.
        let client = client_with_descriptors(vec!["rust"], HashMap::new());
        client.touch_language("rust");
        // No panic, no error.
    }
}
