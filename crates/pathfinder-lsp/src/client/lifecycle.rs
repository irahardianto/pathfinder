//! Process lifecycle management for `LspClient`.
//!
//! Handles construction, spawning, request routing, warm start,
//! and process health monitoring.

use crate::client::background::{
    process_supervisor_task, progress_watcher_task, reader_supervisor_task,
    registration_watcher_task, MAX_BACKOFF_SECS,
};
use crate::client::detect::{
    detect_languages, install_hint, language_id_for_extension, LanguageLsp,
};
use crate::client::process::{spawn_and_initialize, LspTransport, SpawnParams};
use crate::client::protocol::RequestDispatcher;
use crate::client::{
    LanguageState, ProcessEntry, ProcessLifecycle, ReadinessState, UnavailableState,
};
use crate::types::{IndexingCompletionSource, Readiness};
use crate::LspError;
use dashmap::DashMap;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

/// Readiness/indexing timeout budget for a language.
///
/// Delegates to `LanguagePlugin::readiness_budget()` (Axis 2 of the plugin
/// trait, `crate::plugin`) — the single authoritative source for this value.
/// This used to be a hardcoded match here that had drifted out of sync with
/// the plugin registry (wrong values for `java`/`typescript`/`python`) and
/// silently ignored any `readiness_budget_secs` a user declared for a custom
/// language via `.pathfinder.toml`, since a TOML-defined language id could
/// never match one of this function's hardcoded arms. Falls back to the
/// trait's own default (120s) only when a language has no registered plugin
/// at all (e.g. a stale/unknown id passed defensively).
pub(crate) fn indexing_timeout_for_language(lang: &str) -> Duration {
    crate::plugin::plugin_for_language(lang).map_or(
        Duration::from_secs(120),
        crate::plugin::LanguagePlugin::readiness_budget,
    )
}

/// Human-readable description of the build-artifact isolation directory used
/// for a language, for the concurrent-LSP-instance warning log line (e.g.
/// "Cargo target directory build artifact isolation will be applied...").
///
/// Delegates to `LanguagePlugin::isolation_description()` (Axis 11) instead
/// of a hardcoded match — this used to be duplicated verbatim across
/// `detect_concurrent_lsp_linux` and `detect_concurrent_lsp_macos`, and (like
/// `indexing_timeout_for_language` above) silently ignored TOML-declared
/// custom languages, which could never match a hardcoded arm. Falls back to
/// "No" (matching the trait default of `None`) so the log sentence
/// ("No build artifact isolation will be applied...") stays grammatical.
fn isolation_description_for_language(language_id: &str) -> &'static str {
    crate::plugin::plugin_for_language(language_id)
        .and_then(crate::plugin::LanguagePlugin::isolation_description)
        .unwrap_or("No")
}

/// Watchers set up for tracking LSP indexing progress and completion.
pub(crate) type IndexingWatchers = (
    Arc<std::sync::atomic::AtomicBool>,
    Arc<parking_lot::Mutex<Option<IndexingCompletionSource>>>,
    Arc<parking_lot::Mutex<Option<u64>>>,
    Arc<parking_lot::Mutex<Option<u8>>>,
    tokio::task::JoinHandle<()>,
    tokio::task::JoinHandle<()>,
);

impl super::LspClient {
    /// Create a new `LspClient` for the given workspace root.
    ///
    /// Performs Zero-Config language detection immediately (cheap directory
    /// scan). Language server processes are spawned eagerly in the background
    /// via `warm_start()`.
    ///
    /// Starts the process supervisor and watcher background tasks.
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
            init_locks: Arc::new(DashMap::new()),
            dispatcher: Arc::new(RequestDispatcher::new()),
            shutdown_tx: Arc::clone(&shutdown_tx),
            shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            doc_versions: Arc::new(DashMap::new()),
            warm_start_complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            spawner: std::sync::Arc::new(crate::client::process::RealProcessSpawner),
            clock: Arc::new(crate::clock::TokioClock),
        };

        let processes = Arc::clone(&client.processes);
        let dispatcher = Arc::clone(&client.dispatcher);
        let shutdown_rx = shutdown_tx.subscribe();
        let doc_versions = Arc::clone(&client.doc_versions);
        tokio::spawn(process_supervisor_task(
            processes,
            dispatcher,
            doc_versions,
            shutdown_rx,
        ));

        Ok(client)
    }

    /// Warm starts all registered LSP language servers in parallel.
    ///
    /// This triggers background spawning and handshakes for every configured
    /// language server descriptor, ensuring they are active before any operations.
    pub fn warm_start(&self) {
        let all: Vec<String> = self
            .descriptors
            .iter()
            .map(|d| d.language_id.clone())
            .collect();
        self.warm_start_for_languages_and_track(&all);
    }

    /// Warm starts a specific list of LSP language servers in parallel and tracks
    /// their initializations.
    ///
    /// Only triggers background start tasks for language IDs that are known and configured.
    pub fn warm_start_for_languages_and_track(&self, language_ids: &[String]) {
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
            tracing::debug!(
                "PATCH-004: warm_start_for_languages_and_track: no new languages to start"
            );
            self.warm_start_complete
                .store(true, std::sync::atomic::Ordering::Release);
            return;
        }

        tracing::info!(
            languages = ?to_start,
            "PATCH-004: warm_start_for_languages_and_track — pre-warming and tracking"
        );

        let warm_flag = Arc::clone(&self.warm_start_complete);
        let mut handles = Vec::new();
        let num_languages = to_start.len();

        for lang in to_start {
            let client = self.clone();
            let lang = lang.clone();
            handles.push(tokio::spawn(async move {
                tracing::debug!(language = %lang, "PATCH-004: warm_start starting");
                match client.ensure_process(&lang).await {
                    Ok(()) => {
                        tracing::info!(language = %lang, "PATCH-004: warm_start complete");
                    }
                    Err(e) => {
                        tracing::warn!(
                            language = %lang,
                            error = %e,
                            // Genuine failure-retry path: warm start failed during eager init;
                            // requests will retry lazily on demand.
                            "PATCH-004: warm_start failed (will retry lazily on request)"
                        );
                    }
                }
            }));
        }

        tokio::spawn(async move {
            for handle in handles {
                // L-5: Log JoinError from panics instead of silently swallowing.
                if let Err(e) = handle.await {
                    tracing::error!(
                        error = %e,
                        "L-5: warm_start task panicked"
                    );
                }
            }
            warm_flag.store(true, std::sync::atomic::Ordering::Release);
            tracing::info!(
                "PATCH-004: warm_start_complete flag set after {} languages",
                num_languages
            );
        });
    }

    /// Warm starts a specific list of LSP language servers in parallel and returns
    /// their tokio `JoinHandle`s.
    ///
    /// This is used internally and externally to kick off background spawn tasks
    /// and track them.
    pub fn warm_start_for_languages(
        &self,
        language_ids: &[String],
    ) -> Vec<tokio::task::JoinHandle<()>> {
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
            return Vec::new();
        }

        tracing::info!(
            languages = ?to_start,
            "LT-4: warm_start_for_languages — pre-warming requested languages"
        );

        let mut handles = Vec::new();
        for lang in to_start {
            let client = self.clone();
            let lang = lang.clone();
            handles.push(tokio::spawn(async move {
                tracing::debug!(language = %lang, "LT-4: warm_start_for_languages — starting");
                match client.ensure_process(&lang).await {
                    Ok(()) => {
                        tracing::info!(language = %lang, "LT-4: warm_start_for_languages — ready");
                    }
                    Err(e) => {
                        tracing::warn!(
                            language = %lang,
                            error = %e,
                            // Genuine failure-retry path: pre-warming failed;
                            // requests will retry lazily on demand.
                            "LT-4: warm_start_for_languages — failed (will retry lazily on demand)"
                        );
                    }
                }
            }));
        }
        handles
    }

    /// Gracefully shuts down all active LSP processes and terminates background tasks.
    ///
    /// Sends a shutdown signal to all running language servers and signals the
    /// internal tokio supervisors to stop.
    pub fn shutdown(&self) {
        // L-6: Log if shutdown was already called to aid debugging.
        if self
            .shutdown_requested
            .swap(true, std::sync::atomic::Ordering::Release)
        {
            tracing::debug!("LspClient: shutdown called again (already shutting down)");
            return;
        }
        tracing::info!("LspClient: shutdown requested");
        // ALLOW: RS-E1015 / clippy::unit_arg: The channel message type is unit `()`, so passing `()` is intentional and required here.
        #[allow(clippy::unit_arg)]
        let _ = self.shutdown_tx.send(());
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

        let descriptor = self
            .descriptors
            .iter()
            .find(|d| d.language_id == language_id)
            .ok_or(LspError::NoLspAvailable)?
            .clone();

        // DEL-2.1: Acquire init_lock BEFORE killing/removing process to prevent
        // race with concurrent ensure_process or force_respawn calls.
        // Follows exact same pattern as ensure_process.
        let init_lock = self
            .init_locks
            .entry(language_id.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _guard = init_lock.lock().await;

        if let Some((_, ProcessEntry::Running(state))) = self.processes.remove(language_id) {
            tracing::info!(
                language = %language_id,
                "LSP: force_respawn — killing existing process before respawn"
            );
            // C-3: Send shutdown BEFORE aborting reader so response can be read.
            // H-4: Wrap in timeout to prevent blocking.
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                state.transport.shutdown(&self.dispatcher, language_id),
            )
            .await;
            state.reader_handle.abort();
            state.abort_watchers();
            // BUG-4 fix: reader is aborted, call cancel_for_language explicitly
            // to unblock any pending requests for this language.
            self.dispatcher.cancel_for_language(language_id);
            if let Some(ref lifecycle) = state.lifecycle {
                // Grace period: wait up to 3s for the process to exit after SIGTERM.
                // If it doesn't exit, send SIGKILL and wait again.
                let mut child = lifecycle.child.lock().await;
                let wait_result = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
                if wait_result.is_err() {
                    tracing::warn!(
                        language = %language_id,
                        "LSP: process did not exit after SIGTERM within 3s — sending SIGKILL"
                    );
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
            }
            // Clear stale doc_versions for this language.
            self.clear_doc_versions_for_language(language_id);
        }

        // Extract backoff_attempt from existing Unavailable entry, if any.
        let attempt = match self.processes.remove(language_id) {
            Some((_, ProcessEntry::Unavailable(state))) => state.backoff_attempt,
            _ => 0,
        };

        self.start_process(descriptor, attempt).await
    }

    pub(crate) async fn ensure_process(&self, language_id: &str) -> Result<(), LspError> {
        if let Some(entry) = self.processes.get(language_id) {
            match entry.value() {
                ProcessEntry::Running(_) => return Ok(()),
                ProcessEntry::Unavailable(state) => {
                    let capped_attempt = std::cmp::min(state.backoff_attempt, 30);
                    let backoff_secs = std::cmp::min(1u64 << capped_attempt, MAX_BACKOFF_SECS);
                    let elapsed_secs = state.unavailable_since.elapsed().as_secs();
                    if elapsed_secs >= backoff_secs {
                        drop(entry);
                        tracing::info!(
                            language = %language_id,
                            backoff_secs,
                            elapsed_secs,
                            "LSP: backoff elapsed, attempting recovery"
                        );
                        // Note: Do NOT call remove() here. The removal must be done
                        // inside the init_lock section below to avoid race conditions
                        // where a concurrent task has already started a new process.
                    } else {
                        tracing::debug!(
                            language = %language_id,
                            backoff_secs,
                            elapsed_secs,
                            "LSP: in backoff window, returning NoLspAvailable"
                        );
                        return Err(LspError::NoLspAvailable);
                    }
                }
            }
        }

        let init_lock = self
            .init_locks
            .entry(language_id.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _guard = init_lock.lock().await;

        let attempt_to_start = if let Some(entry) = self.processes.get(language_id) {
            match entry.value() {
                ProcessEntry::Running(_) => return Ok(()),
                ProcessEntry::Unavailable(state) => {
                    let capped_attempt = std::cmp::min(state.backoff_attempt, 30);
                    let backoff_secs = std::cmp::min(1u64 << capped_attempt, MAX_BACKOFF_SECS);
                    let elapsed_secs = state.unavailable_since.elapsed().as_secs();
                    if elapsed_secs >= backoff_secs {
                        tracing::info!(
                            language = %language_id,
                            backoff_secs,
                            elapsed_secs,
                            "LSP: backoff elapsed (post-lock check), attempting recovery"
                        );
                        Some(0)
                    } else {
                        return Err(LspError::NoLspAvailable);
                    }
                }
            }
        } else {
            Some(0)
        };

        if let Some(attempt) = attempt_to_start {
            self.processes.remove(language_id);
            self.clear_doc_versions_for_language(language_id);

            let descriptor = self
                .descriptors
                .iter()
                .find(|d| d.language_id == language_id)
                .ok_or(LspError::NoLspAvailable)?
                .clone();

            return self.start_process(descriptor, attempt).await;
        }

        Ok(())
    }

    pub(crate) fn spawn_indexing_timeout_fallback(
        language_id: &str,
        indexing_complete: &Arc<std::sync::atomic::AtomicBool>,
        indexing_completion_source: &Arc<parking_lot::Mutex<Option<IndexingCompletionSource>>>,
        indexing_duration_secs: &Arc<parking_lot::Mutex<Option<u64>>>,
        spawned_at: Instant,
    ) -> tokio::task::JoinHandle<()> {
        let timeout_flag = Arc::clone(indexing_complete);
        let source_flag = Arc::clone(indexing_completion_source);
        let duration_flag = Arc::clone(indexing_duration_secs);
        let timeout_lang = language_id.to_owned();
        let timeout_duration = indexing_timeout_for_language(language_id);
        tokio::spawn(async move {
            tokio::time::sleep(timeout_duration).await;
            if !timeout_flag.swap(true, std::sync::atomic::Ordering::SeqCst) {
                let duration_secs = spawned_at.elapsed().as_secs();
                *source_flag.lock() = Some(IndexingCompletionSource::TimeoutFallback);
                *duration_flag.lock() = Some(duration_secs);
                tracing::info!(
                    language = %timeout_lang,
                    duration_sec = duration_secs,
                    source = "timeout_fallback",
                    "LSP: no WorkDoneProgressEnd received — \
                     assuming indexing complete (timeout fallback)"
                );
            }
        })
    }

    fn handle_restart_backoff(language_id: &str, attempt: u32) -> Option<Duration> {
        if attempt > 0 {
            let capped_delay_attempt = std::cmp::min(attempt, 31);
            let delay = Duration::from_secs(std::cmp::min(
                1u64 << (capped_delay_attempt - 1),
                MAX_BACKOFF_SECS,
            ));
            tracing::info!(
                language = %language_id,
                attempt,
                delay_ms = delay.as_millis(),
                "LSP: restart with backoff"
            );
            Some(delay)
        } else {
            None
        }
    }

    fn setup_indexing_watchers(
        language_id: &str,
        notif_rx: tokio::sync::broadcast::Receiver<serde_json::Value>,
        spawned_at: Instant,
    ) -> IndexingWatchers {
        let indexing_complete = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let indexing_completion_source = Arc::new(parking_lot::Mutex::new(None));
        let indexing_duration_secs = Arc::new(parking_lot::Mutex::new(None));
        let indexing_progress = Arc::new(parking_lot::Mutex::new(None::<u8>));

        let progress_handle = tokio::spawn(progress_watcher_task(
            language_id.to_string(),
            notif_rx,
            Arc::clone(&indexing_complete),
            Arc::clone(&indexing_completion_source),
            Arc::clone(&indexing_duration_secs),
            Arc::clone(&indexing_progress),
            spawned_at,
        ));

        let indexing_timeout_handle = Self::spawn_indexing_timeout_fallback(
            language_id,
            &indexing_complete,
            &indexing_completion_source,
            &indexing_duration_secs,
            spawned_at,
        );

        (
            indexing_complete,
            indexing_completion_source,
            indexing_duration_secs,
            indexing_progress,
            progress_handle,
            indexing_timeout_handle,
        )
    }

    fn setup_registration_watcher(
        language_id: &str,
        server_req_rx: tokio::sync::broadcast::Receiver<serde_json::Value>,
        live_capabilities: &Arc<
            parking_lot::RwLock<crate::client::capabilities::DetectedCapabilities>,
        >,
        transport: &Arc<dyn LspTransport>,
    ) -> tokio::task::JoinHandle<()> {
        let transport_for_reg = Arc::clone(transport);
        let caps_for_reg = Arc::clone(live_capabilities);
        let lang_id_for_reg = language_id.to_string();
        tokio::spawn(registration_watcher_task(
            lang_id_for_reg,
            server_req_rx,
            caps_for_reg,
            transport_for_reg,
        ))
    }

    async fn handle_shutdown_abort_cleanup(
        &self,
        language_id: &str,
        handles: &[tokio::task::JoinHandle<()>],
        transport: Arc<dyn LspTransport>,
        lifecycle: &ProcessLifecycle,
    ) {
        for handle in handles {
            handle.abort();
        }

        // Gracefully shutdown the LSP process
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            transport.shutdown(&self.dispatcher, language_id),
        )
        .await;

        // Fallback: force-kill via lifecycle handle if shutdown didn't work
        let mut child = lifecycle.child.lock().await;
        let _ = child.kill().await;
        let _ = child.wait().await;
    }

    #[expect(clippy::too_many_lines, reason = "Sequential initialization flow")]
    pub(crate) async fn start_process(
        &self,
        descriptor: LanguageLsp,
        attempt: u32,
    ) -> Result<(), LspError> {
        let language_id = descriptor.language_id.clone();

        if let Some(delay) = Self::handle_restart_backoff(&language_id, attempt) {
            tokio::time::sleep(delay).await;
        }

        tracing::info!(
            language = %language_id,
            command = %descriptor.command,
            "LSP: spawning process"
        );

        let client = self.clone();
        let lang_id = language_id.clone();
        let cmd = descriptor.command.clone();
        let in_coexistence_mode =
            tokio::task::spawn_blocking(move || client.detect_concurrent_lsp(&lang_id, &cmd))
                .await
                .unwrap_or(false);
        let isolate_target_dir = in_coexistence_mode;

        let plugins = descriptor.auto_plugins.clone();
        let init_options = descriptor.init_options.clone();

        // M-6: Pre-create notification channels BEFORE spawning the reader.
        let notif_rx = self
            .dispatcher
            .subscribe_notifications_for_language(&language_id);
        let server_req_rx = self
            .dispatcher
            .subscribe_server_requests_for_language(&language_id);

        let spawn_result = spawn_and_initialize(SpawnParams {
            spawner: self.spawner.as_ref(),
            command: &descriptor.command,
            args: &descriptor.args,
            project_root: &descriptor.root,
            language_id: &language_id,
            dispatcher: Arc::clone(&self.dispatcher),
            init_timeout_secs: descriptor.init_timeout_secs,
            isolate_target_dir,
            plugins,
            init_options,
        })
        .await;

        let (process, reader_handle) = match spawn_result {
            Ok(res) => res,
            Err(e) => {
                let next_attempt = attempt.saturating_add(1);
                let capped_next_attempt = std::cmp::min(next_attempt, 30);
                let next_backoff_secs =
                    std::cmp::min(1u64 << capped_next_attempt, MAX_BACKOFF_SECS);
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
                        reason: Some(format!("initialization failed: {e}")),
                    }),
                );
                return Err(LspError::NoLspAvailable);
            }
        };

        // C-2: Track reader liveness via an atomic flag.
        let reader_alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let reader_alive_for_reader = Arc::clone(&reader_alive);

        // Wrap the raw reader to set reader_alive=false on exit.
        let raw_reader_handle = {
            let alive = reader_alive_for_reader;
            tokio::spawn(async move {
                let result = reader_handle.await;
                alive.store(false, std::sync::atomic::Ordering::Release);
                if let Err(e) = result {
                    tracing::error!(
                        error = %e,
                        "LSP: raw reader panicked"
                    );
                }
            })
        };

        let reader_alive_for_supervisor = Arc::clone(&reader_alive);
        let supervisor_handle = tokio::spawn(reader_supervisor_task(
            language_id.clone(),
            raw_reader_handle,
            reader_alive_for_supervisor,
            Arc::clone(&self.processes),
            Arc::clone(&self.dispatcher),
            Arc::clone(&self.doc_versions),
        ));

        if attempt > 0 {
            tracing::info!(
                language = %language_id,
                attempt,
                "LSP: recovery successful after backoff"
            );
        }

        let spawned_at = Instant::now();

        let (
            indexing_complete,
            indexing_completion_source,
            indexing_duration_secs,
            indexing_progress,
            progress_handle,
            indexing_timeout_handle,
        ) = Self::setup_indexing_watchers(&language_id, notif_rx, spawned_at);

        let live_capabilities = Arc::new(parking_lot::RwLock::new(process.capabilities.clone()));

        let child_handle = process.child_handle();
        let lifecycle = ProcessLifecycle {
            child: child_handle,
        };

        let transport: Arc<dyn LspTransport> = Arc::new(process);

        let registration_handle = Self::setup_registration_watcher(
            &language_id,
            server_req_rx,
            &live_capabilities,
            &transport,
        );

        let (readiness_tx, readiness_rx) = tokio::sync::watch::channel(ReadinessState::Probing {
            started_at: tokio::time::Instant::now(),
        });
        let readiness_tx = Arc::new(readiness_tx);

        let client_for_probe = self.clone();
        let lang_for_probe = language_id.clone();
        let root_for_probe = descriptor.root.clone();
        let readiness_tx_for_probe = Arc::clone(&readiness_tx);
        let probe_handle = tokio::spawn(async move {
            client_for_probe
                .run_readiness_probe(lang_for_probe, root_for_probe, readiness_tx_for_probe)
                .await;
        });

        if in_coexistence_mode {
            tracing::warn!(
                language = %language_id,
                 "LSP: coexistence mode active — LSP validation disabled to prevent resource \
                  contention. Navigation (goto_definition, find_callers_callees) still works normally."
            );
        }

        // C-1: Check shutdown signal before inserting process.
        if self
            .shutdown_requested
            .load(std::sync::atomic::Ordering::Acquire)
        {
            tracing::warn!(
                language = %language_id,
                "LSP: shutdown requested during init, aborting process insertion"
            );

            self.handle_shutdown_abort_cleanup(
                &language_id,
                &[
                    progress_handle,
                    registration_handle,
                    supervisor_handle,
                    indexing_timeout_handle,
                    probe_handle,
                ],
                transport,
                &lifecycle,
            )
            .await;

            return Err(LspError::ConnectionLost);
        }

        self.processes.insert(
            language_id,
            ProcessEntry::Running(Box::new(LanguageState {
                transport,
                lifecycle: Some(lifecycle),
                reader_handle: supervisor_handle,
                reader_alive,
                restart_count: attempt,
                spawned_at,
                indexing_complete,
                indexing_completion_source,
                indexing_duration_secs,
                indexing_progress_percent: indexing_progress,
                live_capabilities,
                in_coexistence_mode,
                readiness_tx,
                readiness_rx,
                probe_handle: Some(probe_handle),
                workspace_root: descriptor.root,
                watcher_handles: vec![
                    progress_handle,
                    registration_handle,
                    indexing_timeout_handle,
                ],
            })),
        );

        Ok(())
    }

    #[allow(clippy::unused_self)]
    pub(crate) fn detect_concurrent_lsp(&self, language_id: &str, command: &str) -> bool {
        let binary_name = Path::new(command)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(command);

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

        #[cfg(target_os = "linux")]
        {
            Self::detect_concurrent_lsp_linux(language_id, binary_name)
        }

        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            Self::detect_concurrent_lsp_macos(language_id, binary_name)
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
        {
            let _ = (language_id, binary_name);
            false
        }
    }

    #[cfg(target_os = "linux")]
    fn detect_concurrent_lsp_linux(language_id: &str, binary_name: &str) -> bool {
        let our_pid = std::process::id();
        let our_proc_pid: u32 = std::fs::read_to_string("/proc/self/status")
            .ok()
            .as_deref()
            .and_then(|status| {
                status
                    .lines()
                    .find(|l| l.starts_with("Pid:"))
                    .and_then(|l| l.split_whitespace().nth(1))
                    .and_then(|v| v.parse().ok())
            })
            .unwrap_or(our_pid);

        if let Ok(entries) = std::fs::read_dir("/proc") {
            let mut external_count = 0;
            for entry in entries.flatten() {
                let path = entry.path();
                let is_numeric = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()));
                if !is_numeric {
                    continue;
                }

                if let Some(pid) = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|s| s.parse::<u32>().ok())
                {
                    if pid == our_pid || pid == our_proc_pid {
                        continue;
                    }
                }

                let cmdline_path = path.join("cmdline");
                let Ok(cmdline) = std::fs::read_to_string(&cmdline_path) else {
                    continue;
                };
                if !cmdline.contains(binary_name) {
                    continue;
                }

                let status_path = path.join("status");
                let status_content = std::fs::read_to_string(&status_path).ok();
                let parent_pid: u32 = status_content
                    .as_deref()
                    .and_then(|status| {
                        status
                            .lines()
                            .find(|l| l.starts_with("PPid:"))
                            .and_then(|l| l.split_whitespace().nth(1))
                            .and_then(|v| v.parse().ok())
                    })
                    .unwrap_or(0);

                let is_zombie = status_content
                    .as_deref()
                    .and_then(|status| {
                        status
                            .lines()
                            .find(|l| l.starts_with("State:"))
                            .and_then(|l| l.split_whitespace().nth(1))
                            .map(|state| state == "Z" || state == "X" || state == "T")
                    })
                    .unwrap_or(false);

                if is_zombie {
                    tracing::trace!(
                        binary = binary_name,
                        "detect_concurrent_lsp: skipping zombie process"
                    );
                    continue;
                }

                if parent_pid == our_pid || parent_pid == our_proc_pid {
                    tracing::trace!(
                        binary = binary_name,
                        "detect_concurrent_lsp: skipping own child process"
                    );
                    continue;
                }

                external_count += 1;
            }

            if external_count > 0 {
                let isolation_desc = isolation_description_for_language(language_id);
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
        false
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    fn detect_concurrent_lsp_macos(language_id: &str, binary_name: &str) -> bool {
        let our_pid = std::process::id();
        let output = std::process::Command::new("ps")
            .args(["-axo", "pid,ppid,state,comm"])
            .output();

        if let Ok(out) = output {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let mut external_count = 0;
            for line in stdout.lines().skip(1) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() < 4 {
                    continue;
                }
                let pid_str = parts[0];
                let ppid_str = parts[1];
                let state = parts[2];
                let comm = parts[3..].join(" ");

                let Ok(pid) = pid_str.parse::<u32>() else {
                    continue;
                };
                if pid == our_pid {
                    continue;
                }

                let Ok(ppid) = ppid_str.parse::<u32>() else {
                    continue;
                };

                if !comm.contains(binary_name) {
                    continue;
                }

                let is_zombie = state.contains('Z') || state.contains('T');
                if is_zombie {
                    continue;
                }

                if ppid == our_pid {
                    continue;
                }

                external_count += 1;
            }

            if external_count > 0 {
                let isolation_desc = isolation_description_for_language(language_id);
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
        false
    }

    /// Clear all `doc_versions` entries whose URIs contain the language's file extensions.
    ///
    /// After a crash recovery, the new LSP instance doesn't know about previously
    /// opened documents. Clearing stale entries ensures documents are re-opened
    /// on next access via `did_open`.
    fn clear_doc_versions_for_language(&self, language_id: &str) {
        // Clear only doc_versions entries for the specified language.
        let mut cleared = 0;
        self.doc_versions.retain(|_uri, entry| {
            if entry.language_id == language_id {
                cleared += 1;
                false
            } else {
                true
            }
        });
        if cleared > 0 {
            tracing::debug!(
                language = %language_id,
                cleared,
                "LSP: cleared stale doc_versions for language after crash recovery"
            );
        }
    }

    pub(crate) async fn request(
        &self,
        language_id: &str,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, LspError> {
        // LSP-INIT-002: Tag pending request with language_id so it can be
        // selectively cancelled if this language's LSP crashes, without
        // affecting other languages' pending requests.
        let (id, rx) = self.dispatcher.register(language_id);
        let message = RequestDispatcher::make_request(id, method, &params);

        let transport = {
            let Some(entry) = self.processes.get(language_id) else {
                self.dispatcher.remove(id);
                return Err(LspError::NoLspAvailable);
            };
            let ProcessEntry::Running(state) = entry.value() else {
                self.dispatcher.remove(id);
                return Err(LspError::NoLspAvailable);
            };
            if !state
                .reader_alive
                .load(std::sync::atomic::Ordering::Acquire)
                || state.reader_handle.is_finished()
            {
                self.dispatcher.remove(id);
                return Err(LspError::ConnectionLost);
            }
            Arc::clone(&state.transport)
        };

        // H-1: Clean up dispatcher entry on send failure to prevent leak.
        let send_result = transport.send(&message).await;
        if send_result.is_err() {
            self.dispatcher.remove(id);
        }
        send_result?;

        tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| {
                self.dispatcher.remove(id);
                // LSP spec: client SHOULD send $/cancelRequest when giving up on a
                // request so the server can stop processing and free resources.
                let cancel_transport = Arc::clone(&transport);
                tokio::spawn(async move {
                    let cancel_notif = RequestDispatcher::make_notification(
                        "$/cancelRequest",
                        &serde_json::json!({"id": id}),
                    );
                    let _ = cancel_transport.send(&cancel_notif).await;
                });
                LspError::Timeout {
                    operation: method.to_owned(),
                    timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                }
            })?
            .map_err(|_| LspError::ConnectionLost)?
    }

    pub(crate) async fn notify(
        &self,
        language_id: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(), LspError> {
        let message = RequestDispatcher::make_notification(method, &params);
        let entry = self
            .processes
            .get(language_id)
            .ok_or(LspError::NoLspAvailable)?;
        let ProcessEntry::Running(state) = entry.value() else {
            return Err(LspError::NoLspAvailable);
        };
        if !state
            .reader_alive
            .load(std::sync::atomic::Ordering::Acquire)
            || state.reader_handle.is_finished()
        {
            return Err(LspError::ConnectionLost);
        }
        // P2-3 fix: Extract transport clone before sending, so we don't hold
        // the DashMap Ref across the send().await. This prevents blocking
        // concurrent remove() operations on the same shard.
        let transport = Arc::clone(&state.transport);
        drop(entry);
        transport.send(&message).await
    }

    pub(crate) fn capabilities_for(
        &self,
        language_id: &str,
    ) -> Result<crate::client::DetectedCapabilities, LspError> {
        match self.processes.get(language_id) {
            None => Err(LspError::NoLspAvailable),
            Some(entry) => match entry.value() {
                ProcessEntry::Unavailable(_) => Err(LspError::NoLspAvailable),
                ProcessEntry::Running(state) => Ok(state.live_capabilities.read().clone()),
            },
        }
    }

    pub(crate) async fn wait_for_capability<F>(
        &self,
        language_id: &str,
        check: F,
    ) -> Result<crate::client::DetectedCapabilities, LspError>
    where
        F: Fn(&crate::client::DetectedCapabilities) -> bool,
    {
        let mut caps = self.capabilities_for(language_id)?;
        if check(&caps) {
            return Ok(caps);
        }

        // Grace period for dynamic capability registration (e.g. jdtls client/registerCapability)
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Ok(c) = self.capabilities_for(language_id) {
                if check(&c) {
                    return Ok(c);
                }
                caps = c;
            }
        }
        Ok(caps)
    }

    pub(crate) async fn run_readiness_probe(
        &self,
        language_id: String,
        workspace_root: std::path::PathBuf,
        readiness_tx: Arc<tokio::sync::watch::Sender<ReadinessState>>,
    ) {
        let start = tokio::time::Instant::now();
        let max_budget = indexing_timeout_for_language(&language_id);
        let mut backoff = Duration::from_millis(250);
        let max_backoff = Duration::from_secs(2);

        let probe_target = find_probe_target(&workspace_root, &language_id);

        loop {
            if start.elapsed() >= max_budget {
                let reason = format!(
                    "readiness probe timed out after {}s for {}",
                    max_budget.as_secs(),
                    language_id
                );
                tracing::warn!(language = %language_id, %reason, "LSP: behavioural probe failed within budget");
                let _ = readiness_tx.send(ReadinessState::Degraded { reason });
                return;
            }

            let probe_passed = if let Some((ref rel_path, line, col)) = probe_target {
                let abs_path = workspace_root.join(rel_path);
                let content = tokio::fs::read_to_string(&abs_path)
                    .await
                    .unwrap_or_default();
                if let Ok(file_uri) = url::Url::from_file_path(&abs_path) {
                    let open_params = json!({
                        "textDocument": {
                            "uri": file_uri.as_str(),
                            "languageId": language_id,
                            "version": 1,
                            "text": content
                        }
                    });
                    let _ = self
                        .notify(&language_id, "textDocument/didOpen", open_params)
                        .await;

                    let def_params = json!({
                        "textDocument": { "uri": file_uri.as_str() },
                        "position": {
                            "line": line.saturating_sub(1),
                            "character": col.saturating_sub(1)
                        }
                    });
                    let result = self
                        .request(
                            &language_id,
                            "textDocument/definition",
                            def_params,
                            Duration::from_secs(5),
                        )
                        .await;

                    let close_params = json!({
                        "textDocument": { "uri": file_uri.as_str() }
                    });
                    let _ = self
                        .notify(&language_id, "textDocument/didClose", close_params)
                        .await;

                    match result {
                        Ok(val) => {
                            // If null or empty array, server responded but found nothing yet
                            !val.is_null() && val.as_array().is_none_or(|arr| !arr.is_empty())
                        }
                        Err(_) => false,
                    }
                } else {
                    false
                }
            } else {
                true
            };

            if probe_passed {
                let caps = self.capabilities_for(&language_id).unwrap_or_default();
                let cap_readiness = crate::types::CapabilityReadiness {
                    definition: caps.flags.definition_provider,
                    references: caps.flags.references_provider,
                    hierarchy: crate::types::HierarchyReadiness {
                        call_hierarchy: caps.flags.call_hierarchy_provider,
                        implementation: caps.flags.implementation_provider,
                    },
                };
                tracing::info!(
                    language = %language_id,
                    definition = cap_readiness.definition,
                    references = cap_readiness.references,
                    call_hierarchy = cap_readiness.call_hierarchy(),
                    implementation = cap_readiness.implementation(),
                    "LSP: behavioural probe passed — language server is Ready"
                );
                let _ = readiness_tx.send(ReadinessState::Ready(cap_readiness));
                return;
            }

            tokio::time::sleep(backoff).await;
            backoff = std::cmp::min(backoff * 2, max_backoff);
        }
    }

    async fn wait_for_readiness_rx(
        rx: &mut tokio::sync::watch::Receiver<ReadinessState>,
        deadline: tokio::time::Instant,
    ) -> Readiness {
        loop {
            let current = rx.borrow_and_update().clone();
            match current {
                ReadinessState::Ready(caps) => return Readiness::Ready(caps),
                ReadinessState::Degraded { reason } => {
                    return Readiness::Degraded {
                        reason,
                        retry_after_secs: None,
                    };
                }
                ReadinessState::Spawning
                | ReadinessState::Handshaking
                | ReadinessState::Probing { .. } => {
                    let now = tokio::time::Instant::now();
                    if now >= deadline {
                        let elapsed_secs = match current {
                            ReadinessState::Probing { started_at } => {
                                u32::try_from(started_at.elapsed().as_secs()).unwrap_or(u32::MAX)
                            }
                            _ => 0,
                        };
                        let phase = match current {
                            ReadinessState::Spawning => "spawning",
                            ReadinessState::Handshaking => "handshaking",
                            ReadinessState::Probing { .. } => "probing",
                            _ => "indexing",
                        }
                        .to_string();
                        return Readiness::NotReady {
                            elapsed_secs,
                            retry_after_secs: 5,
                            eta_secs: None,
                            phase,
                        };
                    }
                    let remaining = deadline - now;
                    if tokio::time::timeout(remaining, rx.changed()).await.is_err() {
                        let elapsed_secs = match current {
                            ReadinessState::Probing { started_at } => {
                                u32::try_from(started_at.elapsed().as_secs()).unwrap_or(u32::MAX)
                            }
                            _ => 0,
                        };
                        return Readiness::NotReady {
                            elapsed_secs,
                            retry_after_secs: 5,
                            eta_secs: None,
                            phase: "probing".to_string(),
                        };
                    }
                }
            }
        }
    }

    /// Wait for the language server to become ready, up to `budget` duration.
    ///
    /// # Errors
    /// Returns `LspError::ConnectionLost` or other fatal transport errors if the LSP process fails to start.
    pub async fn ensure_ready(
        &self,
        language_id: &str,
        budget: Duration,
    ) -> Result<Readiness, LspError> {
        let is_known = self
            .descriptors
            .iter()
            .any(|d| d.language_id == language_id);
        if !is_known {
            let hint = self
                .missing_languages
                .iter()
                .find(|m| m.language_id == language_id)
                .map_or_else(
                    || install_hint(language_id).clone(),
                    |m| m.install_hint.clone(),
                );
            return Ok(Readiness::Unavailable {
                language: language_id.to_string(),
                install_hint: hint,
                alternative: "Use your host's grep and file-read tools".to_string(),
            });
        }

        if let Err(e) = self.ensure_process(language_id).await {
            match e {
                LspError::NoLspAvailable => {
                    if let Some(entry) = self.processes.get(language_id) {
                        if let ProcessEntry::Unavailable(ref state) = *entry {
                            let reason = state.reason.clone().unwrap_or_else(|| {
                                format!("{language_id} LSP is unavailable due to repeated failures")
                            });
                            return Ok(Readiness::Degraded {
                                reason,
                                retry_after_secs: Some(10),
                            });
                        }
                    }
                    let hint = install_hint(language_id).clone();
                    return Ok(Readiness::Unavailable {
                        language: language_id.to_string(),
                        install_hint: hint,
                        alternative: "Use your host's grep and file-read tools".to_string(),
                    });
                }
                _ => return Err(e),
            }
        }

        let rx = {
            let Some(entry) = self.processes.get(language_id) else {
                return Ok(Readiness::Degraded {
                    reason: format!("{language_id} process not found"),
                    retry_after_secs: Some(5),
                });
            };
            let ProcessEntry::Running(ref state) = *entry else {
                return Ok(Readiness::Degraded {
                    reason: format!("{language_id} process unavailable"),
                    retry_after_secs: Some(10),
                });
            };
            state.readiness_rx.clone()
        };

        let mut rx = rx;
        let deadline = tokio::time::Instant::now() + budget;
        Ok(Self::wait_for_readiness_rx(&mut rx, deadline).await)
    }

    pub(crate) async fn call_hierarchy_request(
        &self,
        workspace_root: &Path,
        item: &crate::types::CallHierarchyItem,
        tool_name: &str,
        lsp_method: &str,
        item_key: &str,
        ranges_key: &str,
    ) -> Result<Vec<crate::types::CallHierarchyCall>, LspError> {
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
            .request(language_id, lsp_method, params, Duration::from_secs(10))
            .await
        {
            Ok(res) => res,
            Err(e) => {
                tracing::error!(tool = tool_name, language = language_id, error = %e, "{} failed", lsp_method);
                return Err(e);
            }
        };

        let elapsed = start.elapsed().as_millis();
        tracing::info!(
            language = language_id,
            elapsed_ms = elapsed,
            "{} complete",
            lsp_method
        );

        crate::client::response_parsers::parse_call_hierarchy_calls_response(
            &response,
            workspace_root,
            item_key,
            ranges_key,
        )
    }
}

/// Find a probe target (relative file path, 1-indexed line, 1-indexed col) for a language in a workspace.
///
/// Extensions are resolved in two tiers, matching the pattern already used
/// by `language_to_file_glob` in the navigation tools: prefer the
/// registered `LanguagePlugin`'s extensions (authoritative — e.g.
/// `TypeScriptPlugin` also covers `.vue`, `.mjs`, `.cjs`, `.mts`, `.cts`),
/// falling back to the Tree-sitter `LanguageGrammar` registry for
/// languages with a grammar but no LSP plugin.
///
/// This replaces a hand-rolled per-language-id table that had gone stale
/// (its `"typescript"` arm undercounted `TypeScriptPlugin`'s real
/// extensions, and its `"vue"` arm was unreachable dead code — Vue files
/// are probed under the `"typescript"` language id, since there is no
/// separate `"vue"` plugin). Languages with neither a plugin nor a grammar
/// (e.g. `"c"`, `"cpp"`) correctly yield `None`: without a Tree-sitter
/// grammar, `AstParser::parse_source` below can't validate a probe target
/// for them anyway.
pub(crate) fn find_probe_target(
    workspace_root: &Path,
    language_id: &str,
) -> Option<(std::path::PathBuf, u32, u32)> {
    let plugin_exts = crate::plugin::plugin_for_language(language_id)
        .map(crate::plugin::LanguagePlugin::file_extensions);
    let grammar_exts = pathfinder_treesitter::language::LanguageGrammar::find_by_id(language_id)
        .map(|g| g.extensions);
    let extensions = plugin_exts.or(grammar_exts)?;

    if extensions.is_empty() {
        return None;
    }

    let mut dirs_to_visit = vec![workspace_root.to_path_buf()];
    let mut files_found = Vec::new();

    while let Some(dir) = dirs_to_visit.pop() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let file_name = entry.file_name();
                let name_str = file_name.to_string_lossy();
                if name_str.starts_with('.')
                    || name_str == "target"
                    || name_str == "node_modules"
                    || name_str == "dist"
                    || name_str == "build"
                    || name_str == "vendor"
                {
                    continue;
                }
                if path.is_dir() {
                    if let Ok(rel) = path.strip_prefix(workspace_root) {
                        if rel.components().count() <= 3 {
                            dirs_to_visit.push(path);
                        }
                    }
                } else if path.is_file() {
                    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                        if extensions.contains(&ext) {
                            if let Ok(rel) = path.strip_prefix(workspace_root) {
                                files_found.push((rel.to_path_buf(), path.clone()));
                                if files_found.len() >= 5 {
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
        if files_found.len() >= 5 {
            break;
        }
    }

    for (rel_path, abs_path) in files_found {
        if let Ok(content) = std::fs::read_to_string(&abs_path) {
            if content.trim().is_empty() {
                continue;
            }
            if let Some(lang) =
                pathfinder_treesitter::language::SupportedLanguage::detect(&abs_path)
            {
                if let Ok(tree) = pathfinder_treesitter::parser::AstParser::parse_source(
                    &abs_path,
                    lang,
                    content.as_bytes(),
                ) {
                    let symbols = pathfinder_treesitter::symbols::extract_symbols_from_tree(
                        &tree,
                        content.as_bytes(),
                        lang,
                    );
                    if let Some(first_sym) = symbols
                        .into_iter()
                        .find(|s| s.start_line > 0 || s.name_point.row > 0)
                    {
                        let line = u32::try_from(first_sym.name_point.row + 1).unwrap_or(1);
                        let col = u32::try_from(first_sym.name_point.column + 1).unwrap_or(1);
                        return Some((rel_path, line, col));
                    }
                }
            }
            return Some((rel_path, 1, 1));
        }
    }

    None
}

#[cfg(test)]
#[path = "lifecycle_test.rs"]
mod tests;
