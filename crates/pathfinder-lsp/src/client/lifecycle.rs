//! Process lifecycle management for `LspClient`.
//!
//! Handles construction, spawning, request routing, warm start,
//! and process health monitoring.

use crate::client::background::{
    idle_timeout_task, progress_watcher_task, reader_supervisor_task, registration_watcher_task,
    MAX_BACKOFF_SECS,
};
use crate::client::detect::{detect_languages, language_id_for_extension, LanguageLsp};
use crate::client::process::{spawn_and_initialize, LspTransport};
use crate::client::protocol::RequestDispatcher;
use crate::client::{
    InFlightGuard, LanguageState, ProcessEntry, ProcessLifecycle, UnavailableState,
};
use crate::types::IndexingCompletionSource;
use crate::LspError;
use dashmap::DashMap;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

#[allow(clippy::match_same_arms)]
pub(crate) fn indexing_timeout_for_language(lang: &str) -> Duration {
    match lang {
        "java" => Duration::from_mins(2),
        "typescript" | "javascript" => Duration::from_secs(45),
        "go" | "python" => Duration::from_secs(30),
        "rust" => Duration::from_mins(1),
        _ => Duration::from_secs(30),
    }
}

impl super::LspClient {
    /// Create a new `LspClient` for the given workspace root.
    ///
    /// Performs Zero-Config language detection immediately (cheap directory
    /// scan). LSP processes are **not** started until first use.
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
            init_locks: Arc::new(DashMap::new()),
            dispatcher: Arc::new(RequestDispatcher::new()),
            shutdown_tx: Arc::clone(&shutdown_tx),
            shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            doc_versions: Arc::new(DashMap::new()),
            warm_start_complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            spawner: std::sync::Arc::new(crate::client::process::RealProcessSpawner),
        };

        let processes = Arc::clone(&client.processes);
        let dispatcher = Arc::clone(&client.dispatcher);
        let shutdown_rx = shutdown_tx.subscribe();
        let doc_versions = Arc::clone(&client.doc_versions);
        tokio::spawn(idle_timeout_task(
            processes,
            dispatcher,
            doc_versions,
            shutdown_rx,
        ));

        Ok(client)
    }

    pub fn warm_start(&self) {
        let all: Vec<String> = self
            .descriptors
            .iter()
            .map(|d| d.language_id.clone())
            .collect();
        self.warm_start_for_languages_and_track(&all);
    }

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
                            "PATCH-004: warm_start failed (will retry lazily)"
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
                            "LT-4: warm_start_for_languages — failed (will retry lazily)"
                        );
                    }
                }
            }));
        }
        handles
    }

    pub fn touch_language(&self, language_id: &str) {
        if let Some(mut entry) = self.processes.get_mut(language_id) {
            if let ProcessEntry::Running(state) = entry.value_mut() {
                state.transport.set_last_used(Instant::now());
                tracing::trace!(
                    language = language_id,
                    "LT-4: idle timer refreshed from file operation"
                );
            }
        }
    }

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
            // DEL-4.1: FUTURE: init_locks cleanup when dynamic language support is added.
            // Currently bounded to 5 languages (rust/go/typescript/python/java),
            // so memory cost is negligible (~5 entries * 100 bytes each).
            // self.init_locks.remove(language_id);
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

        if let Some(entry) = self.processes.get(language_id) {
            match entry.value() {
                ProcessEntry::Running(_) => return Ok(()),
                ProcessEntry::Unavailable(state) => {
                    let capped_attempt = std::cmp::min(state.backoff_attempt, 30);
                    let backoff_secs = std::cmp::min(1u64 << capped_attempt, MAX_BACKOFF_SECS);
                    let elapsed_secs = state.unavailable_since.elapsed().as_secs();
                    if elapsed_secs >= backoff_secs {
                        // Backoff elapsed = fresh attempt. Start from 0, not state.backoff_attempt.
                        // This avoids race conditions: pre-lock no longer removes the entry,
                        // so post-lock must explicitly reset the attempt counter.
                        let attempt = 0;
                        drop(entry);
                        tracing::info!(
                            language = %language_id,
                            backoff_secs,
                            elapsed_secs,
                            "LSP: backoff elapsed (post-lock check), attempting recovery"
                        );
                        self.processes.remove(language_id);
                        // Clear stale doc_versions from the crashed instance.
                        self.clear_doc_versions_for_language(language_id);

                        let descriptor = self
                            .descriptors
                            .iter()
                            .find(|d| d.language_id == language_id)
                            .ok_or(LspError::NoLspAvailable)?
                            .clone();

                        return self.start_process(descriptor, attempt).await;
                    }
                    return Err(LspError::NoLspAvailable);
                }
            }
        }

        let descriptor = self
            .descriptors
            .iter()
            .find(|d| d.language_id == language_id)
            .ok_or(LspError::NoLspAvailable)?
            .clone();

        self.start_process(descriptor, 0).await
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

    #[allow(clippy::type_complexity)]
    fn setup_indexing_watchers(
        language_id: &str,
        notif_rx: tokio::sync::broadcast::Receiver<serde_json::Value>,
        spawned_at: Instant,
    ) -> (
        Arc<std::sync::atomic::AtomicBool>,
        Arc<parking_lot::Mutex<Option<IndexingCompletionSource>>>,
        Arc<parking_lot::Mutex<Option<u64>>>,
        Arc<parking_lot::Mutex<Option<u8>>>,
        tokio::task::JoinHandle<()>,
        tokio::task::JoinHandle<()>,
    ) {
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
        handles: (
            tokio::task::JoinHandle<()>,
            tokio::task::JoinHandle<()>,
            tokio::task::JoinHandle<()>,
            tokio::task::JoinHandle<()>,
        ),
        transport: Arc<dyn LspTransport>,
        lifecycle: &ProcessLifecycle,
    ) {
        handles.0.abort();
        handles.1.abort();
        handles.2.abort();
        handles.3.abort();

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

        let spawn_result = spawn_and_initialize(
            self.spawner.as_ref(),
            &descriptor.command,
            &descriptor.args,
            &descriptor.root,
            &language_id,
            Arc::clone(&self.dispatcher),
            descriptor.init_timeout_secs,
            isolate_target_dir,
            plugins,
            init_options,
        )
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
                (
                    progress_handle,
                    registration_handle,
                    supervisor_handle,
                    indexing_timeout_handle,
                ),
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

                if parent_pid == our_pid {
                    tracing::trace!(
                        binary = binary_name,
                        "detect_concurrent_lsp: skipping own child process"
                    );
                    continue;
                }

                external_count += 1;
            }

            if external_count > 0 {
                let isolation_desc = match language_id {
                    "rust" => "Cargo target directory",
                    "go" => "Go build cache (GOCACHE/GOMODCACHE)",
                    "typescript" => "TypeScript temp directory (TMPDIR)",
                    "python" => "Python bytecode cache (PYTHONPYCACHEPREFIX)",
                    _ => "No",
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
                let isolation_desc = match language_id {
                    "rust" => "Cargo target directory",
                    "go" => "Go build cache (GOCACHE/GOMODCACHE)",
                    "typescript" => "TypeScript temp directory (TMPDIR)",
                    "python" => "Python bytecode cache (PYTHONPYCACHEPREFIX)",
                    _ => "No",
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
        false
    }

    pub(crate) fn touch(&self, language_id: &str) {
        if let Some(mut entry) = self.processes.get_mut(language_id) {
            if let ProcessEntry::Running(state) = entry.value_mut() {
                state.transport.set_last_used(Instant::now());
            }
        }
    }

    /// Clear all `doc_versions` entries whose URIs contain the language's file extensions.
    ///
    /// After a crash recovery, the new LSP instance doesn't know about previously
    /// opened documents. Clearing stale entries ensures documents are re-opened
    /// on next access via `did_open`.
    fn clear_doc_versions_for_language(&self, language_id: &str) {
        // Clear only doc_versions entries for the specified language.
        let mut cleared = 0;
        self.doc_versions.retain(|_uri, (lang, _)| {
            if lang == language_id {
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

        let (_in_flight_guard, transport) = {
            let Some(entry) = self.processes.get(language_id) else {
                self.dispatcher.remove(id);
                return Err(LspError::NoLspAvailable);
            };
            let ProcessEntry::Running(state) = entry.value() else {
                self.dispatcher.remove(id);
                return Err(LspError::NoLspAvailable);
            };
            if state.reader_handle.is_finished() {
                self.dispatcher.remove(id);
                return Err(LspError::ConnectionLost);
            }
            let counter = Arc::clone(state.transport.in_flight());
            let transport = Arc::clone(&state.transport);
            (InFlightGuard::new(counter), transport)
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
        if state.reader_handle.is_finished() {
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
        check_cap: F,
        cap_name: &str,
    ) -> Result<(), LspError>
    where
        F: Fn(&crate::client::DetectedCapabilities) -> bool,
    {
        let grace = crate::client::grace_period_for_language(language_id);
        if grace == 0 {
            let caps = self.capabilities_for(language_id)?;
            if check_cap(&caps) {
                return Ok(());
            }
            return Err(LspError::UnsupportedCapability {
                capability: cap_name.to_owned(),
            });
        }

        loop {
            let uptime = {
                let entry = self
                    .processes
                    .get(language_id)
                    .ok_or(LspError::NoLspAvailable)?;
                match entry.value() {
                    ProcessEntry::Unavailable(_) => return Err(LspError::NoLspAvailable),
                    ProcessEntry::Running(state) => state.spawned_at.elapsed().as_secs(),
                }
            };
            let caps = self.capabilities_for(language_id)?;
            if check_cap(&caps) {
                return Ok(());
            }
            if uptime >= grace {
                return Err(LspError::UnsupportedCapability {
                    capability: cap_name.to_owned(),
                });
            }
            // Sleep for 100ms before retrying to avoid spinning CPU
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
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

        self.touch(language_id);

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

#[cfg(test)]
#[path = "lifecycle_test.rs"]
mod tests;
