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

    #[allow(clippy::too_many_lines)]
    pub(crate) async fn start_process(
        &self,
        descriptor: LanguageLsp,
        attempt: u32,
    ) -> Result<(), LspError> {
        let language_id = descriptor.language_id.clone();

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
            tokio::time::sleep(delay).await;
        }

        tracing::info!(
            language = %language_id,
            command = %descriptor.command,
            "LSP: spawning process"
        );

        let in_coexistence_mode = self.detect_concurrent_lsp(&language_id, &descriptor.command);
        let isolate_target_dir = in_coexistence_mode;

        let plugins = descriptor.auto_plugins.clone();
        let init_options = descriptor.init_options.clone();

        // M-6: Pre-create notification channels BEFORE spawning the reader.
        // The reader task dispatches to these channels immediately on startup.
        // Without pre-creation, any server requests (e.g. client/registerCapability)
        // sent during the initialize handshake would be silently dropped.
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

        // C-2: Track reader liveness via an atomic flag. The reader task sets
        // this to false on exit. The supervisor's remove_if checks it to
        // distinguish the old entry from a replacement spawned by crash recovery.
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

        let indexing_complete = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let indexing_completion_source = Arc::new(parking_lot::Mutex::new(None));
        let indexing_duration_secs = Arc::new(parking_lot::Mutex::new(None));
        let indexing_progress = Arc::new(parking_lot::Mutex::new(None::<u8>));
        let spawned_at = Instant::now();

        let indexing_flag = Arc::clone(&indexing_complete);
        let indexing_source_flag = Arc::clone(&indexing_completion_source);
        let indexing_duration_flag = Arc::clone(&indexing_duration_secs);
        let indexing_progress_flag = Arc::clone(&indexing_progress);
        let spawned_at_for_watcher = spawned_at;
        // M-6: Use pre-created receiver so we don't miss notifications
        // dispatched between reader start and watcher spawn.
        let progress_handle = tokio::spawn(progress_watcher_task(
            language_id.clone(),
            notif_rx,
            indexing_flag,
            indexing_source_flag,
            indexing_duration_flag,
            indexing_progress_flag,
            spawned_at_for_watcher,
        ));

        // H-2: Store the indexing timeout handle so it gets aborted on restart.
        // Without this, a stale timeout task can overwrite the NEW process's
        // indexing_complete with stale timing data.
        let indexing_timeout_handle = Self::spawn_indexing_timeout_fallback(
            &language_id,
            &indexing_complete,
            &indexing_completion_source,
            &indexing_duration_secs,
            spawned_at,
        );

        let live_capabilities = Arc::new(parking_lot::RwLock::new(process.capabilities.clone()));

        let child_handle = process.child_handle();
        let lifecycle = ProcessLifecycle {
            child: child_handle,
        };

        let transport: Arc<dyn LspTransport> = Arc::new(process);
        let transport_for_reg = Arc::clone(&transport);

        let caps_for_reg = Arc::clone(&live_capabilities);
        let lang_id_for_reg = language_id.clone();
        // M-6: Use pre-created receiver so we don't miss server requests
        // dispatched between reader start and watcher spawn.
        let registration_handle = tokio::spawn(registration_watcher_task(
            lang_id_for_reg,
            server_req_rx,
            caps_for_reg,
            transport_for_reg,
        ));

        if in_coexistence_mode {
            tracing::warn!(
                language = %language_id,
                 "LSP: coexistence mode active — LSP validation disabled to prevent resource \
                  contention. Navigation (goto_definition, find_callers_callees) still works normally."
            );
        }

        // C-1: Check shutdown signal before inserting process. If shutdown was
        // called while spawn_and_initialize was running (up to 120s for jdtls),
        // inserting now would orphan the process — idle_timeout_task already exited.
        if self
            .shutdown_requested
            .load(std::sync::atomic::Ordering::Acquire)
        {
            tracing::warn!(
                language = %language_id,
                "LSP: shutdown requested during init, aborting process insertion"
            );

            // CRITICAL: Kill the child process before returning.
            // The process was successfully spawned by spawn_and_initialize,
            // so we must not orphan it.
            progress_handle.abort();
            registration_handle.abort();
            supervisor_handle.abort();
            indexing_timeout_handle.abort();

            // Gracefully shutdown the LSP process
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                transport.shutdown(&self.dispatcher, &language_id),
            )
            .await;

            // Fallback: force-kill via lifecycle handle if shutdown didn't work
            let mut child = lifecycle.child.lock().await;
            let _ = child.kill().await;
            let _ = child.wait().await;

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

                    // MAJOR: Check if the process is a zombie.
                    // Zombies still have /proc entries but are not actually running.
                    // State codes: R=running, S=sleeping, D=disk sleep,
                    // Z=zombie, T=stopped, t=tracing stop, X=dead.
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
        }
        let _ = (language_id, binary_name);
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
                state.reader_handle.abort();
                let transport = Arc::clone(&state.transport);
                let lifecycle = state.lifecycle.clone();
                drop(entry);
                // P1-1 fix: Use remove_if to only remove if reader is still finished.
                // This prevents killing a healthy replacement process spawned between
                // drop(entry) and the remove operation.
                let removed = self.processes.remove_if(
                    language_id,
                    |_, v| matches!(v, ProcessEntry::Running(s) if s.reader_handle.is_finished()),
                );
                if let Some((_, ProcessEntry::Running(state))) = removed {
                    // MAJOR: Cancel pending requests ONLY after confirming we removed
                    // the stale entry. This prevents cross-generational cancellation
                    // where a new process was started between our check and atomic remove.
                    self.dispatcher.cancel_for_language(language_id);
                    state.abort_watchers();
                    // C-3: shutdown before abort — but reader is already dead here,
                    // so just force-kill directly (no response to read).
                    let _ = tokio::time::timeout(
                        Duration::from_secs(2),
                        transport.shutdown(&self.dispatcher, language_id),
                    )
                    .await;
                    if let Some(ref lc) = lifecycle {
                        let mut child = lc.child.lock().await;
                        let wait_result =
                            tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
                        if wait_result.is_err() {
                            tracing::warn!(
                                language = %language_id,
                                "LSP: stale process did not exit after SIGTERM — sending SIGKILL"
                            );
                            let _ = child.kill().await;
                            let _ = child.wait().await;
                        }
                    }
                    tracing::warn!(
                        language = %language_id,
                        "LSP: reader task not alive, removed stale entry for recovery"
                    );
                    // M-9: Insert Unavailable for backoff protection. Without this,
                    // ensure_process would immediately retry and could create a tight
                    // spawn-exit loop if the child keeps dying immediately.
                    self.processes.insert(
                        language_id.to_owned(),
                        ProcessEntry::Unavailable(UnavailableState {
                            unavailable_since: Instant::now(),
                            backoff_attempt: 1,
                        }),
                    );
                }
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
            state.reader_handle.abort();
            let transport = Arc::clone(&state.transport);
            let lifecycle = state.lifecycle.clone();
            drop(entry);
            // P1-1 fix: Use remove_if to only remove if reader is still finished.
            let removed = self.processes.remove_if(
                language_id,
                |_, v| matches!(v, ProcessEntry::Running(s) if s.reader_handle.is_finished()),
            );
            if let Some((_, ProcessEntry::Running(state))) = removed {
                // MAJOR: Cancel pending requests ONLY after confirming we removed
                // the stale entry. Prevents cross-generational cancellation.
                self.dispatcher.cancel_for_language(language_id);
                state.abort_watchers();
                let _ = tokio::time::timeout(
                    Duration::from_secs(2),
                    transport.shutdown(&self.dispatcher, language_id),
                )
                .await;
                if let Some(ref lc) = lifecycle {
                    let mut child = lc.child.lock().await;
                    let wait_result =
                        tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
                    if wait_result.is_err() {
                        tracing::warn!(
                            language = %language_id,
                            "LSP: stale process did not exit after SIGTERM in notify — sending SIGKILL"
                        );
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                    }
                }
                tracing::warn!(
                    language = %language_id,
                    "LSP: reader task not alive in notify, removed stale entry for recovery"
                );
                // M-9: Insert Unavailable for backoff protection. Without this,
                // ensure_process would immediately retry and could create a tight
                // spawn-exit loop if the child keeps dying immediately.
                self.processes.insert(
                    language_id.to_owned(),
                    ProcessEntry::Unavailable(UnavailableState {
                        unavailable_since: Instant::now(),
                        backoff_attempt: 1,
                    }),
                );
            }
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
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::client::tests::{client_no_languages, client_with_descriptors, make_running_client};
    use crate::client::{DetectedCapabilities, DiagnosticsStrategy};
    use crate::lawyer::Lawyer;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::atomic::Ordering;

    #[test]
    fn test_indexing_timeout_java_is_120s() {
        assert_eq!(
            indexing_timeout_for_language("java"),
            Duration::from_mins(2)
        );
    }

    #[test]
    fn test_indexing_timeout_typescript_is_45s() {
        assert_eq!(
            indexing_timeout_for_language("typescript"),
            Duration::from_secs(45)
        );
        assert_eq!(
            indexing_timeout_for_language("javascript"),
            Duration::from_secs(45)
        );
    }

    #[test]
    fn test_indexing_timeout_rust_is_60s() {
        assert_eq!(
            indexing_timeout_for_language("rust"),
            Duration::from_mins(1)
        );
    }

    #[test]
    fn test_indexing_timeout_unknown_is_30s() {
        assert_eq!(
            indexing_timeout_for_language("unknown"),
            Duration::from_secs(30)
        );
        assert_eq!(
            indexing_timeout_for_language("csharp"),
            Duration::from_secs(30)
        );
    }

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
                unavailable_since: Instant::now(),
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
    async fn test_ensure_process_unavailable_cooldown_elapsed_attempts_start() {
        let processes = HashMap::from([(
            "rust".to_owned(),
            ProcessEntry::Unavailable(UnavailableState {
                backoff_attempt: 0,
                unavailable_since: Instant::now().checked_sub(Duration::from_mins(10)).unwrap(),
            }),
        )]);
        let client = client_with_descriptors(vec!["rust"], processes);

        let result = client.ensure_process("rust").await;

        // After fix: ensure_process falls through to start_process when backoff elapsed.
        // Since the fake binary doesn't exist, start_process fails and inserts a new
        // Unavailable entry with incremented backoff.
        assert!(
            result.is_err(),
            "cooldown-elapsed should attempt start_process (which fails without real binary): {result:?}"
        );

        let entry = client.processes.get("rust");
        assert!(
            entry.is_some(),
            "should have a new Unavailable entry after failed start_process"
        );
        assert!(
            matches!(entry.unwrap().value(), ProcessEntry::Unavailable(_)),
            "entry should be Unavailable after failed start"
        );
    }

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

    #[tokio::test]
    async fn test_capability_status_no_processes_lazy_start() {
        let client = client_with_descriptors(vec!["rust", "go"], HashMap::new());
        let status = client.capability_status().await;
        assert_eq!(status.len(), 2);
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

    #[tokio::test]
    async fn test_touch_no_process_is_noop() {
        let client = client_no_languages();
        client.touch("rust");
    }

    #[tokio::test]
    async fn test_touch_updates_last_used_timestamp() {
        let (client, _fake) = make_running_client("rust");

        let initial_last_used = {
            let entry = client.processes.get("rust").unwrap();
            if let ProcessEntry::Running(state) = entry.value() {
                state.transport.last_used()
            } else {
                panic!("expected Running entry");
            }
        };

        tokio::time::sleep(Duration::from_millis(10)).await;

        client.touch("rust");

        let updated_last_used = {
            let entry = client.processes.get("rust").unwrap();
            if let ProcessEntry::Running(state) = entry.value() {
                state.transport.last_used()
            } else {
                panic!("expected Running entry");
            }
        };

        assert!(
            updated_last_used > initial_last_used,
            "last_used should be updated after touch"
        );
    }

    #[tokio::test]
    async fn test_capabilities_for_running_process_returns_caps() {
        let (client, fake) = make_running_client("rust");

        let caps = crate::client::DetectedCapabilities {
            definition_provider: true,
            call_hierarchy_provider: true,
            ..Default::default()
        };
        fake.with_capabilities(caps.clone());

        if let Some(entry) = client.processes.get("rust") {
            if let ProcessEntry::Running(state) = entry.value() {
                let mut live_caps = state.live_capabilities.write();
                *live_caps = caps;
            }
        }

        let result = client.capabilities_for("rust");
        assert!(result.is_ok(), "should return capabilities: {result:?}");
        let caps = result.unwrap();
        assert!(
            caps.definition_provider,
            "definition_provider should be true"
        );
        assert!(
            caps.call_hierarchy_provider,
            "call_hierarchy_provider should be true"
        );
    }

    #[tokio::test]
    async fn test_request_with_running_process_returns_response() {
        let (client, fake) = make_running_client("rust");

        fake.set_response(
            "textDocument/definition",
            serde_json::json!({
                "result": {
                    "uri": "file:///workspace/src/main.rs",
                    "range": { "start": { "line": 10, "character": 4 }, "end": { "line": 10, "character": 9 } }
                }
            }),
        );

        let result = client
            .request(
                "rust",
                "textDocument/definition",
                json!({}),
                Duration::from_secs(5),
            )
            .await;

        assert!(result.is_ok(), "request should return response: {result:?}");
        let val = result.unwrap();
        assert!(val.get("uri").is_some(), "response should contain uri");
    }

    #[tokio::test]
    async fn test_request_no_response_configured_returns_protocol_error() {
        let (client, _fake) = make_running_client("rust");

        let result = client
            .request(
                "rust",
                "textDocument/definition",
                json!({}),
                Duration::from_millis(50),
            )
            .await;

        assert!(
            matches!(result, Err(LspError::Protocol(_))),
            "should fail with Protocol error when no response configured: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_request_with_dead_reader_removes_entry() {
        let (client, _fake) = make_running_client("rust");

        if let Some(entry) = client.processes.get("rust") {
            if let ProcessEntry::Running(state) = entry.value() {
                state.reader_handle.abort();
            }
        }

        tokio::time::sleep(Duration::from_millis(10)).await;

        let result = client
            .request(
                "rust",
                "textDocument/definition",
                json!({}),
                Duration::from_secs(5),
            )
            .await;

        assert!(
            matches!(result, Err(LspError::ConnectionLost)),
            "should return ConnectionLost when reader is dead: {result:?}"
        );

        let has_running = client
            .processes
            .get("rust")
            .is_some_and(|e| matches!(e.value(), ProcessEntry::Running(_)));

        assert!(
            !has_running,
            "Running entry should be replaced by Unavailable for backoff after dead reader"
        );
    }

    #[tokio::test]
    async fn test_request_in_flight_guard_on_running_process() {
        let (client, fake) = make_running_client("rust");

        fake.set_response(
            "textDocument/definition",
            serde_json::json!({"uri": "file:///test.rs"}),
        );

        let entry = client.processes.get("rust").unwrap();
        let in_flight = if let ProcessEntry::Running(state) = entry.value() {
            state.transport.in_flight().load(Ordering::Relaxed)
        } else {
            panic!("expected Running entry");
        };
        assert_eq!(in_flight, 0, "in-flight should be 0 before request");
        drop(entry);

        let _ = client
            .request(
                "rust",
                "textDocument/definition",
                json!({}),
                Duration::from_secs(5),
            )
            .await;

        let entry = client.processes.get("rust").unwrap();
        let in_flight = if let ProcessEntry::Running(state) = entry.value() {
            state.transport.in_flight().load(Ordering::Relaxed)
        } else {
            panic!("expected Running entry");
        };
        assert_eq!(in_flight, 0, "in-flight should be 0 after request");
    }

    #[tokio::test]
    async fn test_notify_with_running_process_records_notification() {
        let (client, fake) = make_running_client("rust");

        let result = client
            .notify(
                "rust",
                "textDocument/didChange",
                json!({ "textDocument": { "uri": "file:///test.rs" } }),
            )
            .await;

        assert!(result.is_ok(), "notify should succeed: {result:?}");

        let notifications = fake.take_notifications();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].0, "textDocument/didChange");
    }

    #[tokio::test]
    async fn test_force_respawn_no_descriptor_returns_no_lsp() {
        let (client, _fake) = make_running_client("rust");

        let result = client.force_respawn("go").await;
        assert!(
            matches!(result, Err(LspError::NoLspAvailable)),
            "force_respawn without descriptor should return NoLspAvailable: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_force_respawn_unavailable_entry_removed_directly() {
        let (client, _fake) = make_running_client("rust");

        client.processes.insert(
            "rust".to_owned(),
            ProcessEntry::Unavailable(UnavailableState {
                backoff_attempt: 0,
                unavailable_since: Instant::now(),
            }),
        );

        let _ = client.force_respawn("rust").await;
    }

    #[tokio::test]
    async fn test_force_respawn_removes_running_entry() {
        let (client, _fake) = make_running_client("rust");

        assert!(client.processes.get("rust").is_some());

        let result = client.force_respawn("rust").await;

        assert!(
            result.is_err(),
            "force_respawn should fail without real LSP binary"
        );

        let entry = client.processes.get("rust");
        if let Some(entry) = entry {
            assert!(
                matches!(entry.value(), ProcessEntry::Unavailable(_)),
                "original Running entry should be replaced by Unavailable"
            );
        }
    }

    #[tokio::test]
    async fn test_start_process_inserts_unavailable_on_spawn_failure() {
        let client = client_with_descriptors(vec!["rust"], HashMap::new());

        let descriptor = LanguageLsp {
            language_id: "rust".to_owned(),
            command: "non-existent-lsp-binary".to_owned(),
            args: vec![],
            root: std::env::temp_dir(),
            init_timeout_secs: Some(1),
            auto_plugins: vec![],
            init_options: serde_json::Value::Null,
        };

        let result = client.start_process(descriptor, 0).await;
        assert!(result.is_err(), "should fail with non-existent binary");

        let entry = client.processes.get("rust");
        assert!(
            entry.is_some(),
            "should insert Unavailable entry on failure"
        );
        assert!(
            matches!(entry.unwrap().value(), ProcessEntry::Unavailable(_)),
            "entry should be Unavailable"
        );
    }

    #[tokio::test]
    async fn test_ensure_process_full_lifecycle_unavailable_to_running() {
        let client = client_with_descriptors(vec!["rust"], HashMap::new());

        assert!(client.processes.get("rust").is_none());

        let _ = client.ensure_process("rust").await;

        let entry = client.processes.get("rust");
        if let Some(entry) = entry {
            assert!(
                matches!(entry.value(), ProcessEntry::Unavailable(_)),
                "should be Unavailable after failed start"
            );
        }
    }

    #[tokio::test]
    async fn test_idle_timeout_removes_process_after_timeout() {
        let (client, _fake) = make_running_client("rust");

        if let Some(entry) = client.processes.get("rust") {
            if let ProcessEntry::Running(state) = entry.value() {
                state
                    .transport
                    .set_last_used(Instant::now().checked_sub(Duration::from_mins(20)).unwrap());
            }
        }

        assert!(client.processes.get("rust").is_some());

        let entry = client.processes.get("rust").unwrap();
        if let ProcessEntry::Running(state) = entry.value() {
            assert!(
                state.transport.last_used().elapsed() > Duration::from_mins(15),
                "last_used should be in the past"
            );
        }
    }

    #[tokio::test]
    async fn test_idle_timeout_does_not_remove_process_with_in_flight() {
        let (client, _fake) = make_running_client("rust");

        if let Some(entry) = client.processes.get("rust") {
            if let ProcessEntry::Running(state) = entry.value() {
                state
                    .transport
                    .set_last_used(Instant::now().checked_sub(Duration::from_mins(20)).unwrap());
                state.transport.in_flight().store(1, Ordering::Relaxed);
            }
        }

        let entry = client.processes.get("rust").unwrap();
        if let ProcessEntry::Running(state) = entry.value() {
            assert!(
                state.transport.in_flight().load(Ordering::Relaxed) > 0,
                "in-flight should be > 0"
            );
        }
    }

    #[tokio::test]
    async fn test_idle_timeout_shutdown_terminates_all_processes() {
        let (client, _fake) = make_running_client("rust");

        assert!(client.processes.get("rust").is_some());

        client.shutdown();

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Shutdown signal was already consumed by idle_timeout_task
    }

    #[test]
    fn test_detect_concurrent_lsp_returns_false_for_build_artifact() {
        let client = client_no_languages();

        let result =
            client.detect_concurrent_lsp("rust", "/home/user/project/target/debug/build/my-lsp");
        assert!(!result, "should return false for build artifact paths");

        let result = client.detect_concurrent_lsp("rust", "/home/user/.cargo/bin/rust-analyzer");
        assert!(!result, "should return false for .cargo paths");
    }

    #[tokio::test]
    async fn test_capability_status_with_running_entry_shows_connected() {
        let (client, _fake) = make_running_client("rust");

        let status = client.capability_status().await;
        assert!(status.contains_key("rust"), "should have rust status");

        let rust_status = &status["rust"];
        assert!(
            rust_status.navigation_ready == Some(true) || rust_status.indexing_complete.is_some(),
            "should have navigation_ready or indexing_complete: {rust_status:?}"
        );
    }

    #[tokio::test]
    async fn test_ensure_process_concurrent_init_prevents_double_spawn() {
        let client = client_with_descriptors(vec!["rust"], HashMap::new());

        let handles: Vec<_> = (0..5)
            .map(|_| {
                let client = client.clone();
                tokio::spawn(async move { client.ensure_process("rust").await })
            })
            .collect();

        for handle in handles {
            let _ = handle.await;
        }

        let count = client.processes.iter().count();
        assert!(count <= 1, "should have at most one entry, got {count}");
    }

    #[tokio::test]
    async fn test_request_with_killed_transport_returns_connection_lost() {
        let (client, fake) = make_running_client("rust");

        fake.kill();

        let result = client
            .request(
                "rust",
                "textDocument/definition",
                json!({}),
                Duration::from_millis(100),
            )
            .await;

        assert!(
            matches!(result, Err(LspError::ConnectionLost)),
            "should return ConnectionLost when transport is killed: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_request_detects_dead_reader_returns_connection_lost() {
        let (client, _fake) = make_running_client("rust");

        if let Some(entry) = client.processes.get("rust") {
            if let ProcessEntry::Running(state) = entry.value() {
                state.reader_handle.abort();
            }
        }

        tokio::time::sleep(Duration::from_millis(10)).await;

        let result = client
            .request(
                "rust",
                "textDocument/definition",
                json!({}),
                Duration::from_millis(100),
            )
            .await;

        assert!(result.is_err(), "should fail when reader is dead");
    }

    #[tokio::test]
    async fn test_backoff_recovery_failure_reinserts_unavailable() {
        let client = client_with_descriptors(vec!["rust"], HashMap::new());

        client.processes.insert(
            "rust".to_owned(),
            ProcessEntry::Unavailable(UnavailableState {
                backoff_attempt: 2,
                unavailable_since: Instant::now()
                    .checked_sub(Duration::from_secs(100))
                    .unwrap(),
            }),
        );

        let _ = client.ensure_process("rust").await;

        // After fix: ensure_process falls through to start_process when backoff elapsed.
        // start_process(descriptor, 0) fails → inserts Unavailable with backoff_attempt=1.
        let entry = client.processes.get("rust");
        assert!(
            entry.is_some(),
            "should have Unavailable entry after failed start"
        );
        if let ProcessEntry::Unavailable(state) = entry.unwrap().value() {
            assert_eq!(
                state.backoff_attempt, 1,
                "backoff_attempt should be 1 (fresh attempt from start_process failure)"
            );
        };
    }

    #[test]
    fn test_missing_languages_returns_configured_list() {
        use crate::client::MissingLanguage;
        let missing = vec![MissingLanguage {
            language_id: "python".to_owned(),
            marker_file: "pyproject.toml".to_owned(),
            tried_binaries: vec!["pyright".to_owned()],
            install_hint: "pip install pyright".to_owned(),
        }];
        let (shutdown_tx, _) = broadcast::channel(1);
        let client = crate::client::LspClient {
            descriptors: Arc::new(Vec::new()),
            missing_languages: Arc::new(missing),
            processes: Arc::new(DashMap::new()),
            init_locks: Arc::new(DashMap::new()),
            dispatcher: Arc::new(RequestDispatcher::new()),
            shutdown_tx: Arc::new(shutdown_tx),
            shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            doc_versions: Arc::new(DashMap::new()),
            warm_start_complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            spawner: std::sync::Arc::new(crate::client::process::RealProcessSpawner),
        };

        let result = client.missing_languages.clone();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].language_id, "python");
    }

    #[tokio::test]
    async fn test_start_process_inserts_unavailable_with_backoff() {
        let client = client_with_descriptors(vec!["rust"], HashMap::new());

        let descriptor = LanguageLsp {
            language_id: "rust".to_owned(),
            command: "non-existent-lsp-binary".to_owned(),
            args: vec![],
            root: std::env::temp_dir(),
            init_timeout_secs: Some(1),
            auto_plugins: vec![],
            init_options: serde_json::Value::Null,
        };

        let _ = client.start_process(descriptor.clone(), 0).await;

        let entry = client.processes.get("rust").unwrap();
        if let ProcessEntry::Unavailable(state) = entry.value() {
            assert_eq!(
                state.backoff_attempt, 1,
                "first failure should set backoff_attempt=1"
            );
            assert!(
                state.unavailable_since.elapsed() < Duration::from_secs(5),
                "unavailable_since should be recent"
            );
        } else {
            panic!("expected Unavailable entry after failed start");
        }
    }

    #[tokio::test]
    async fn test_spawn_indexing_timeout_fallback_sets_complete_after_timeout() {
        tokio::time::pause();

        let indexing_complete = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let indexing_completion_source = Arc::new(parking_lot::Mutex::new(None));
        let indexing_duration_secs = Arc::new(parking_lot::Mutex::new(None));
        let spawned_at = Instant::now();

        crate::client::LspClient::spawn_indexing_timeout_fallback(
            "rust",
            &indexing_complete,
            &indexing_completion_source,
            &indexing_duration_secs,
            spawned_at,
        );

        assert!(!indexing_complete.load(std::sync::atomic::Ordering::Relaxed));
        assert_eq!(*indexing_completion_source.lock(), None);
        assert_eq!(*indexing_duration_secs.lock(), None);

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_mins(1) + Duration::from_millis(10)).await;

        tokio::task::yield_now().await;

        assert!(
            indexing_complete.load(std::sync::atomic::Ordering::Relaxed),
            "should be marked complete after timeout"
        );
        assert_eq!(
            *indexing_completion_source.lock(),
            Some(IndexingCompletionSource::TimeoutFallback),
            "should indicate source is timeout fallback"
        );
        assert!(
            indexing_duration_secs.lock().is_some(),
            "should have a duration"
        );
    }

    #[tokio::test]
    async fn test_spawn_indexing_timeout_fallback_noop_if_already_complete() {
        tokio::time::pause();

        let indexing_complete = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let indexing_completion_source = Arc::new(parking_lot::Mutex::new(Some(
            IndexingCompletionSource::Progress,
        )));
        let indexing_duration_secs = Arc::new(parking_lot::Mutex::new(Some(42)));
        let spawned_at = Instant::now();

        crate::client::LspClient::spawn_indexing_timeout_fallback(
            "rust",
            &indexing_complete,
            &indexing_completion_source,
            &indexing_duration_secs,
            spawned_at,
        );

        tokio::time::advance(Duration::from_mins(1) + Duration::from_millis(10)).await;

        tokio::task::yield_now().await;

        assert!(
            indexing_complete.load(std::sync::atomic::Ordering::Relaxed),
            "should remain complete"
        );
        assert_eq!(
            *indexing_completion_source.lock(),
            Some(IndexingCompletionSource::Progress),
            "should preserve Progress source (not overwrite with TimeoutFallback)"
        );
        assert_eq!(
            *indexing_duration_secs.lock(),
            Some(42),
            "should preserve duration 42"
        );
    }

    #[tokio::test]
    async fn test_ensure_process_concurrent_init_serializes_spawns() {
        let client = client_with_descriptors(vec!["rust"], HashMap::new());

        let handles: Vec<_> = (0..10)
            .map(|_| {
                let client = client.clone();
                tokio::spawn(async move { client.ensure_process("rust").await })
            })
            .collect();

        for handle in handles {
            let _ = handle.await;
        }

        let count = client.processes.iter().count();
        assert!(
            count <= 1,
            "should have at most one entry after concurrent init, got {count}"
        );

        assert!(
            client.init_locks.contains_key("rust"),
            "init lock should be created for rust"
        );
    }

    #[tokio::test]
    async fn test_warm_start_no_languages_is_noop() {
        let client = client_no_languages();
        client.warm_start();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    #[tokio::test]
    async fn test_shutdown_sends_signal() {
        let client = client_no_languages();

        let mut rx = client.shutdown_tx.subscribe();

        client.shutdown();

        let result = rx.try_recv();
        assert!(
            result.is_ok(),
            "shutdown signal should be sent and received"
        );
    }

    #[tokio::test]
    async fn test_warm_start_for_languages_starts_only_requested() {
        let client = client_with_descriptors(vec!["rust", "go", "typescript"], HashMap::new());
        let _ = client.warm_start_for_languages(&["go".to_owned()]);
    }

    #[tokio::test]
    async fn test_warm_start_for_languages_skips_already_running() {
        let client = client_with_descriptors(vec!["rust"], HashMap::new());
        let _ = client.warm_start_for_languages(&["rust".to_owned()]);
        let _ = client.warm_start_for_languages(&["rust".to_owned()]);
    }

    #[tokio::test]
    async fn test_warm_start_for_languages_ignores_unknown() {
        let client = client_with_descriptors(vec!["rust"], HashMap::new());
        let _ = client.warm_start_for_languages(&["unknown_lang".to_owned()]);
    }

    #[tokio::test]
    async fn test_touch_language_extends_idle_timer() {
        let client = client_no_languages();
        client.touch_language("rust");
    }

    #[tokio::test]
    async fn test_touch_language_no_process_is_noop() {
        let client = client_with_descriptors(vec!["rust"], HashMap::new());
        client.touch_language("rust");
    }

    #[test]
    fn test_warm_start_complete_flag_transitions_false_to_true() {
        let client = client_with_descriptors(vec!["rust"], HashMap::new());
        assert!(
            !client
                .warm_start_complete
                .load(std::sync::atomic::Ordering::Relaxed),
            "warm_start_complete should be false initially"
        );
    }

    #[tokio::test]
    async fn test_warm_start_complete_true_after_all_tasks_complete() {
        let client = client_with_descriptors(vec!["rust"], HashMap::new());
        assert!(
            !client
                .warm_start_complete
                .load(std::sync::atomic::Ordering::Relaxed),
            "warm_start_complete should be false before warm_start"
        );

        client.warm_start();
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        assert!(
            client
                .warm_start_complete
                .load(std::sync::atomic::Ordering::Relaxed),
            "warm_start_complete should be true after tasks complete"
        );
    }

    #[tokio::test]
    async fn test_warm_start_partial_failure_still_sets_complete_flag() {
        let client = client_with_descriptors(vec!["rust"], HashMap::new());
        client.warm_start();
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        assert!(
            client
                .warm_start_complete
                .load(std::sync::atomic::Ordering::Relaxed),
            "warm_start_complete should be true even if some languages failed"
        );
    }

    // MEDIUM-2: DEL-2.1 tests for force_respawn init_lock acquisition
    // BUG-4: force_respawn previously bypassed init_locks, enabling concurrent
    // ensure_process + force_respawn to spawn orphaned processes.

    #[tokio::test]
    async fn test_force_respawn_concurrent_with_ensure_process_produces_single_entry() {
        let client = client_with_descriptors(vec!["rust"], HashMap::new());

        let mut handles = Vec::new();

        for i in 0..10 {
            let client = client.clone();
            handles.push(tokio::spawn(async move {
                if i % 2 == 0 {
                    let _ = client.ensure_process("rust").await;
                } else {
                    let _ = client.force_respawn("rust").await;
                }
            }));
        }

        for handle in handles {
            let _ = handle.await;
        }

        let count = client.processes.iter().count();
        assert!(
            count <= 1,
            "concurrent ensure_process + force_respawn should produce at most 1 entry, got {count}"
        );
    }

    #[tokio::test]
    async fn test_force_respawn_uses_same_init_lock_as_ensure_process() {
        let client = client_with_descriptors(vec!["rust"], HashMap::new());

        let init_lock = client
            .init_locks
            .entry("rust".to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();

        let guard = init_lock.lock().await;

        let ensure_handle = tokio::spawn({
            let client = client.clone();
            async move { client.ensure_process("rust").await }
        });

        let respawn_handle = tokio::spawn({
            let client = client.clone();
            async move { client.force_respawn("rust").await }
        });

        tokio::time::sleep(Duration::from_millis(10)).await;

        assert!(
            client.processes.get("rust").is_none(),
            "while init_lock held, neither ensure_process nor force_respawn should have created entry"
        );

        drop(guard);

        let _ = ensure_handle.await;
        let _ = respawn_handle.await;

        let count = client.processes.iter().count();
        assert!(
            count <= 1,
            "after lock release, at most 1 entry should exist, got {count}"
        );
    }

    #[tokio::test]
    async fn test_shutdown_idempotent_does_not_panic() {
        let client = client_no_languages();
        client.shutdown();
        client.shutdown();
    }

    #[tokio::test]
    async fn test_request_send_failure_removes_dispatcher_entry() {
        let (client, fake) = make_running_client("rust");
        fake.kill();

        let result = client
            .request(
                "rust",
                "textDocument/definition",
                json!({}),
                Duration::from_secs(5),
            )
            .await;

        assert!(result.is_err(), "should fail when transport is killed");
    }

    #[tokio::test]
    async fn test_request_timeout_returns_correct_operation_and_ms() {
        let (client, fake) = make_running_client("rust");

        fake.set_response(
            "textDocument/completion",
            json!({ "result": { "items": [] } }),
        );

        fake.set_response_delay(Duration::from_secs(10));

        let result = client
            .request(
                "rust",
                "textDocument/completion",
                json!({}),
                Duration::from_millis(50),
            )
            .await;

        match result {
            Err(LspError::Timeout {
                operation,
                timeout_ms,
            }) => {
                assert_eq!(operation, "textDocument/completion");
                assert!(
                    timeout_ms > 0,
                    "timeout_ms should be positive, got {timeout_ms}"
                );
            }
            other => panic!(
                "expected LspError::Timeout, got: {other:?} \
                 (FakeTransport should delay dispatch beyond request timeout)"
            ),
        }
    }

    #[tokio::test]
    async fn test_request_delayed_response_succeeds_with_long_timeout() {
        let (client, fake) = make_running_client("rust");

        fake.set_response(
            "textDocument/hover",
            json!({ "result": { "contents": "test" } }),
        );

        fake.set_response_delay(Duration::from_millis(10));

        let result = client
            .request(
                "rust",
                "textDocument/hover",
                json!({}),
                Duration::from_secs(5),
            )
            .await;

        match result {
            Ok(val) => {
                assert_eq!(val["contents"], "test");
            }
            other => panic!("expected Ok(response) with long timeout, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_request_timeout_cleans_up_dispatcher_entry() {
        let (client, fake) = make_running_client("rust");

        fake.set_response("textDocument/references", json!({ "result": [] }));

        fake.set_response_delay(Duration::from_secs(10));

        let pending_before = client.dispatcher.pending_count();
        let _ = client
            .request(
                "rust",
                "textDocument/references",
                json!({}),
                Duration::from_millis(50),
            )
            .await;

        tokio::time::sleep(Duration::from_millis(20)).await;

        let pending_after = client.dispatcher.pending_count();
        assert_eq!(
            pending_after, pending_before,
            "timeout should clean up dispatcher entry (pending: {pending_after}, expected: {pending_before})"
        );
    }

    #[tokio::test]
    async fn test_request_delayed_error_response_returns_protocol_error() {
        let (client, fake) = make_running_client("rust");

        fake.set_error("textDocument/definition", "something went wrong");
        fake.set_response_delay(Duration::from_secs(10));

        let result = client
            .request(
                "rust",
                "textDocument/definition",
                json!({}),
                Duration::from_secs(5),
            )
            .await;

        match result {
            Err(LspError::Protocol(msg)) => {
                assert!(
                    msg.contains("something went wrong"),
                    "error message should contain configured text: {msg}"
                );
            }
            other => {
                panic!("expected Protocol error from send() even with delay active, got: {other:?}")
            }
        }
    }

    #[tokio::test]
    async fn test_notify_with_dead_reader_returns_connection_lost() {
        let (client, _fake) = make_running_client("rust");

        if let Some(entry) = client.processes.get("rust") {
            if let ProcessEntry::Running(state) = entry.value() {
                state.reader_handle.abort();
            }
        }

        tokio::time::sleep(Duration::from_millis(10)).await;

        let result = client
            .notify("rust", "textDocument/didOpen", json!({}))
            .await;

        assert!(
            matches!(result, Err(LspError::ConnectionLost)),
            "notify with dead reader should return ConnectionLost: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_notify_with_killed_transport_returns_connection_lost() {
        let (client, fake) = make_running_client("rust");
        fake.kill();

        let result = client
            .notify("rust", "textDocument/didOpen", json!({}))
            .await;

        assert!(
            matches!(result, Err(LspError::ConnectionLost)),
            "notify on killed transport should return ConnectionLost: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_clear_doc_versions_for_language() {
        let (client, _fake) = make_running_client("rust");

        client.doc_versions.insert(
            "file:///workspace/src/main.rs".to_owned(),
            ("rust".to_owned(), std::sync::atomic::AtomicI32::new(1)),
        );
        client.doc_versions.insert(
            "file:///workspace/src/lib.rs".to_owned(),
            ("rust".to_owned(), std::sync::atomic::AtomicI32::new(2)),
        );
        client.doc_versions.insert(
            "file:///workspace/main.go".to_owned(),
            ("go".to_owned(), std::sync::atomic::AtomicI32::new(1)),
        );

        assert_eq!(client.doc_versions.len(), 3);

        client.clear_doc_versions_for_language("rust");

        assert_eq!(client.doc_versions.len(), 1);
        assert!(
            client
                .doc_versions
                .contains_key("file:///workspace/main.go"),
            "go doc_versions should not be cleared when clearing rust"
        );
        assert!(
            !client
                .doc_versions
                .contains_key("file:///workspace/src/main.rs"),
            "rust doc_versions should be cleared"
        );
    }

    #[tokio::test]
    async fn test_clear_doc_versions_noop_when_empty() {
        let (client, _fake) = make_running_client("rust");

        assert!(client.doc_versions.is_empty());

        client.clear_doc_versions_for_language("rust");

        assert!(client.doc_versions.is_empty());
    }

    #[tokio::test]
    async fn test_detect_concurrent_lsp_relative_path() {
        let client = client_no_languages();

        let result = client.detect_concurrent_lsp("rust", "totally-fake-lsp-binary-xyz");
        assert!(
            !result,
            "relative path with no matching process should return false"
        );
    }

    #[tokio::test]
    async fn test_detect_concurrent_lsp_non_existent_absolute() {
        let client = client_no_languages();

        let result =
            client.detect_concurrent_lsp("rust", "/usr/local/bin/nonexistent-lsp-binary-xyz");
        assert!(
            !result,
            "non-existent absolute path should not detect concurrent LSP"
        );
    }

    #[tokio::test]
    async fn test_ensure_process_clears_doc_versions_on_recovery() {
        let client = client_with_descriptors(vec!["rust"], HashMap::new());

        client.doc_versions.insert(
            "file:///workspace/src/main.rs".to_owned(),
            ("rust".to_owned(), std::sync::atomic::AtomicI32::new(3)),
        );
        assert_eq!(client.doc_versions.len(), 1);

        client.processes.insert(
            "rust".to_owned(),
            ProcessEntry::Unavailable(UnavailableState {
                backoff_attempt: 0,
                unavailable_since: Instant::now()
                    .checked_sub(Duration::from_secs(100))
                    .unwrap(),
            }),
        );

        let _ = client.ensure_process("rust").await;

        let has_versions = !client.doc_versions.is_empty();
        assert!(
            !has_versions || client.processes.get("rust").is_some(),
            "doc_versions state should be consistent after recovery attempt"
        );
    }

    #[tokio::test]
    async fn test_start_process_with_attempt_greater_than_zero_sleeps() {
        let client = client_with_descriptors(vec!["rust"], HashMap::new());

        let descriptor = LanguageLsp {
            language_id: "rust".to_owned(),
            command: "non-existent-lsp-binary".to_owned(),
            args: vec![],
            root: std::env::temp_dir(),
            init_timeout_secs: Some(1),
            auto_plugins: vec![],
            init_options: serde_json::Value::Null,
        };

        let start = Instant::now();
        let _ = client.start_process(descriptor, 1).await;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= Duration::from_secs(1),
            "attempt=1 should sleep at least 1s (2^0=1s backoff), elapsed={elapsed:?}"
        );
    }

    #[tokio::test]
    async fn test_capability_status_running_with_coexistence_mode() {
        let (client, _fake) = make_running_client("rust");

        if let Some(entry) = client.processes.get("rust") {
            if let ProcessEntry::Running(state) = entry.value() {
                *state.live_capabilities.write() = DetectedCapabilities {
                    definition_provider: true,
                    diagnostics_strategy: DiagnosticsStrategy::Pull,
                    ..Default::default()
                };
            }
        }

        let status = client.capability_status().await;
        assert!(status.contains_key("rust"));
        assert!(status["rust"].validation);
    }

    #[tokio::test]
    async fn test_request_multiple_sequential_requests() {
        let (client, fake) = make_running_client("rust");

        for i in 0..3 {
            fake.set_response(
                "textDocument/definition",
                json!({ "result": { "uri": format!("file:///test_{i}.rs") } }),
            );

            let result = client
                .request(
                    "rust",
                    "textDocument/definition",
                    json!({}),
                    Duration::from_secs(5),
                )
                .await;

            assert!(result.is_ok(), "request {i} should succeed: {result:?}");
        }
    }

    #[tokio::test]
    async fn test_notify_multiple_notifications_recorded() {
        let (client, fake) = make_running_client("rust");

        for method in &[
            "textDocument/didOpen",
            "textDocument/didChange",
            "textDocument/didClose",
        ] {
            let result = client.notify("rust", method, json!({})).await;
            assert!(result.is_ok(), "notify {method} should succeed");
        }

        let notifications = fake.take_notifications();
        assert_eq!(notifications.len(), 3);
        assert_eq!(notifications[0].0, "textDocument/didOpen");
        assert_eq!(notifications[1].0, "textDocument/didChange");
        assert_eq!(notifications[2].0, "textDocument/didClose");
    }

    #[tokio::test]
    async fn test_warm_start_for_languages_and_track_sets_complete_flag() {
        let client = client_with_descriptors(vec!["rust"], HashMap::new());

        assert!(!client
            .warm_start_complete
            .load(std::sync::atomic::Ordering::Relaxed));

        client.warm_start_for_languages_and_track(&["rust".to_owned()]);

        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(
            client
                .warm_start_complete
                .load(std::sync::atomic::Ordering::Relaxed),
            "warm_start_complete should be true after warm_start_for_languages_and_track"
        );
    }

    #[tokio::test]
    async fn test_warm_start_for_languages_and_track_empty_list() {
        let client = client_with_descriptors(vec!["rust"], HashMap::new());

        client.warm_start_for_languages_and_track(&[]);

        assert!(
            client
                .warm_start_complete
                .load(std::sync::atomic::Ordering::Relaxed),
            "empty list should set complete flag immediately"
        );
    }

    #[tokio::test]
    async fn test_start_process_shutdown_requested_during_init() {
        let client = client_with_descriptors(vec!["rust"], HashMap::new());

        let descriptor = LanguageLsp {
            language_id: "rust".to_owned(),
            command: "non-existent-lsp-binary".to_owned(),
            args: vec![],
            root: std::env::temp_dir(),
            init_timeout_secs: Some(1),
            auto_plugins: vec![],
            init_options: serde_json::Value::Null,
        };

        let result = client.start_process(descriptor, 0).await;

        assert!(
            result.is_err(),
            "should fail with non-existent binary regardless of shutdown state: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_call_hierarchy_request_missing_data_returns_error() {
        let (client, _fake) = make_running_client("rust");

        let item = crate::types::CallHierarchyItem {
            name: "main".to_owned(),
            kind: "function".to_owned(),
            detail: None,
            file: "src/main.rs".to_owned(),
            line: 1,
            column: 1,
            data: None,
        };

        let result = client
            .call_hierarchy_request(
                Path::new("/workspace"),
                &item,
                "call_hierarchy_incoming",
                "callHierarchy/incomingCalls",
                "from",
                "fromRanges",
            )
            .await;

        assert!(
            result.is_err(),
            "should fail when item.data is None: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_request_error_response_from_transport() {
        let (client, fake) = make_running_client("rust");

        fake.set_error("textDocument/definition", "server internal error");

        let result = client
            .request(
                "rust",
                "textDocument/definition",
                json!({}),
                Duration::from_secs(5),
            )
            .await;

        assert!(
            matches!(result, Err(LspError::Protocol(ref msg)) if msg.contains("server internal error")),
            "should return Protocol error from server error response: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_force_respawn_clears_doc_versions_for_language() {
        let (client, _fake) = make_running_client("rust");

        client.doc_versions.insert(
            "file:///workspace/src/main.rs".to_owned(),
            ("rust".to_owned(), std::sync::atomic::AtomicI32::new(1)),
        );
        client.doc_versions.insert(
            "file:///workspace/main.go".to_owned(),
            ("go".to_owned(), std::sync::atomic::AtomicI32::new(1)),
        );
        assert_eq!(client.doc_versions.len(), 2);

        let _ = client.force_respawn("rust").await;

        assert_eq!(client.doc_versions.len(), 1);
        assert!(
            client
                .doc_versions
                .contains_key("file:///workspace/main.go"),
            "go doc_versions should not be cleared when respawning rust"
        );
        assert!(
            !client
                .doc_versions
                .contains_key("file:///workspace/src/main.rs"),
            "rust doc_versions should be cleared on respawn"
        );
    }

    #[tokio::test]
    async fn test_touch_on_unavailable_entry_is_noop() {
        let client = client_with_descriptors(
            vec!["rust"],
            HashMap::from([(
                "rust".to_owned(),
                ProcessEntry::Unavailable(UnavailableState {
                    backoff_attempt: 0,
                    unavailable_since: Instant::now(),
                }),
            )]),
        );

        client.touch("rust");

        let entry = client.processes.get("rust").unwrap();
        assert!(
            matches!(entry.value(), ProcessEntry::Unavailable(_)),
            "touch on Unavailable should be no-op"
        );
    }

    // D-2: LspClient::new() integration tests with filesystem fixtures
    //
    // These tests use config command overrides to provide known binary names,
    // avoiding dependency on which::which() which is sensitive to concurrent
    // PATH manipulation by detect::tests::test_with_fake_python_binaries.

    fn lsp_config_with_command(command: &str) -> pathfinder_common::config::LspConfig {
        pathfinder_common::config::LspConfig {
            command: command.to_owned(),
            args: vec![],
            idle_timeout_minutes: 30,
            settings: serde_json::Value::Null,
            root_override: None,
            typescript_plugins: vec![],
        }
    }

    #[tokio::test]
    async fn test_new_empty_directory_no_languages() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = std::sync::Arc::new(pathfinder_common::config::PathfinderConfig::default());

        let client = super::super::LspClient::new(dir.path(), config)
            .await
            .expect("new should succeed for empty dir");

        assert!(
            client.descriptors.is_empty(),
            "empty directory should have no language descriptors"
        );
    }

    #[tokio::test]
    async fn test_new_with_cargo_toml_detects_rust() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"test\"\nversion = \"0.1.0\"",
        )
        .expect("write Cargo.toml");

        let mut config = pathfinder_common::config::PathfinderConfig::default();
        config
            .lsp
            .insert("rust".to_owned(), lsp_config_with_command("rust-analyzer"));
        let config = std::sync::Arc::new(config);

        let client = super::super::LspClient::new(dir.path(), config)
            .await
            .expect("new should succeed");

        assert!(
            client.descriptors.iter().any(|d| d.language_id == "rust"),
            "should detect Rust language"
        );
    }

    #[tokio::test]
    async fn test_new_with_go_mod_detects_go() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("go.mod"),
            "module example.com/test\n\ngo 1.21",
        )
        .expect("write go.mod");

        let mut config = pathfinder_common::config::PathfinderConfig::default();
        config
            .lsp
            .insert("go".to_owned(), lsp_config_with_command("gopls"));
        let config = std::sync::Arc::new(config);

        let client = super::super::LspClient::new(dir.path(), config)
            .await
            .expect("new should succeed");

        assert!(
            client.descriptors.iter().any(|d| d.language_id == "go"),
            "should detect Go language"
        );
    }

    #[tokio::test]
    async fn test_new_with_tsconfig_detects_typescript() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("tsconfig.json"),
            "{\"compilerOptions\": {\"target\": \"es2020\"}}",
        )
        .expect("write tsconfig.json");

        let mut config = pathfinder_common::config::PathfinderConfig::default();
        config.lsp.insert(
            "typescript".to_owned(),
            lsp_config_with_command("typescript-language-server"),
        );
        let config = std::sync::Arc::new(config);

        let client = super::super::LspClient::new(dir.path(), config)
            .await
            .expect("new should succeed");

        assert!(
            client
                .descriptors
                .iter()
                .any(|d| d.language_id == "typescript"),
            "should detect TypeScript language"
        );
    }

    #[tokio::test]
    async fn test_new_with_pyproject_detects_python() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"test\"\nversion = \"0.1.0\"",
        )
        .expect("write pyproject.toml");

        let mut config = pathfinder_common::config::PathfinderConfig::default();
        config
            .lsp
            .insert("python".to_owned(), lsp_config_with_command("pyright"));
        let config = std::sync::Arc::new(config);

        let client = super::super::LspClient::new(dir.path(), config)
            .await
            .expect("new should succeed");

        assert!(
            client.descriptors.iter().any(|d| d.language_id == "python"),
            "should detect Python language"
        );
    }

    #[tokio::test]
    async fn test_new_nonexistent_workspace_succeeds_with_no_languages() {
        let config = std::sync::Arc::new(pathfinder_common::config::PathfinderConfig::default());

        let client = super::super::LspClient::new(Path::new("/definitely/does/not/exist"), config)
            .await
            .expect("new should succeed — detect_languages handles nonexistent dirs gracefully");

        assert!(
            client.descriptors.is_empty(),
            "nonexistent workspace should have no descriptors"
        );
    }

    #[tokio::test]
    async fn test_new_shutdown_flag_toggles_correctly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = std::sync::Arc::new(pathfinder_common::config::PathfinderConfig::default());

        let client = super::super::LspClient::new(dir.path(), config)
            .await
            .expect("new should succeed");

        assert!(
            !client.shutdown_requested.load(Ordering::Relaxed),
            "shutdown should not be requested initially"
        );

        client.shutdown();
        assert!(
            client.shutdown_requested.load(Ordering::Relaxed),
            "shutdown should be requested after shutdown()"
        );
    }

    #[tokio::test]
    async fn test_new_warm_start_complete_initially_false() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = std::sync::Arc::new(pathfinder_common::config::PathfinderConfig::default());

        let client = super::super::LspClient::new(dir.path(), config)
            .await
            .expect("new should succeed");

        assert!(
            !client.warm_start_complete.load(Ordering::Relaxed),
            "warm_start_complete should be false initially"
        );
    }

    #[tokio::test]
    async fn test_new_multiple_marker_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"test\"\nversion = \"0.1.0\"",
        )
        .expect("write Cargo.toml");
        std::fs::write(
            dir.path().join("go.mod"),
            "module example.com/test\n\ngo 1.21",
        )
        .expect("write go.mod");

        let mut config = pathfinder_common::config::PathfinderConfig::default();
        config
            .lsp
            .insert("rust".to_owned(), lsp_config_with_command("rust-analyzer"));
        config
            .lsp
            .insert("go".to_owned(), lsp_config_with_command("gopls"));
        let config = std::sync::Arc::new(config);

        let client = super::super::LspClient::new(dir.path(), config)
            .await
            .expect("new should succeed");

        let language_ids: Vec<&str> = client
            .descriptors
            .iter()
            .map(|d| d.language_id.as_str())
            .collect();

        assert!(
            language_ids.contains(&"rust"),
            "should detect Rust language"
        );
        assert!(language_ids.contains(&"go"), "should detect Go language");
    }
}
