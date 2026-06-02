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
            doc_versions: Arc::new(DashMap::new()),
            warm_start_complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };

        let processes = Arc::clone(&client.processes);
        let dispatcher = Arc::clone(&client.dispatcher);
        let shutdown_rx = shutdown_tx.subscribe();
        tokio::spawn(idle_timeout_task(processes, dispatcher, shutdown_rx));

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
                let _ = handle.await;
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

        if let Some((_, ProcessEntry::Running(state))) = self.processes.remove(language_id) {
            tracing::info!(
                language = %language_id,
                "LSP: force_respawn — killing existing process before respawn"
            );
            state.reader_handle.abort();
            state.transport.shutdown(&self.dispatcher).await;
            if let Some(ref lifecycle) = state.lifecycle {
                let _ = lifecycle.child.lock().await.wait().await;
            }
        }

        self.start_process(descriptor, 0).await
    }

    pub(crate) async fn ensure_process(&self, language_id: &str) -> Result<(), LspError> {
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

        let init_lock = self
            .init_locks
            .entry(language_id.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _guard = init_lock.lock().await;

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
        indexing_completion_source: &Arc<std::sync::Mutex<Option<IndexingCompletionSource>>>,
        indexing_duration_secs: &Arc<std::sync::Mutex<Option<u64>>>,
        spawned_at: Instant,
    ) {
        let timeout_flag = Arc::clone(indexing_complete);
        let source_flag = Arc::clone(indexing_completion_source);
        let duration_flag = Arc::clone(indexing_duration_secs);
        let timeout_lang = language_id.to_owned();
        let timeout_duration = indexing_timeout_for_language(language_id);
        tokio::spawn(async move {
            tokio::time::sleep(timeout_duration).await;
            if !timeout_flag.load(std::sync::atomic::Ordering::Relaxed) {
                timeout_flag.store(true, std::sync::atomic::Ordering::Relaxed);
                let duration_secs = spawned_at.elapsed().as_secs();
                #[allow(clippy::expect_used)]
                {
                    *source_flag.lock().expect("source_flag lock") =
                        Some(IndexingCompletionSource::TimeoutFallback);
                    *duration_flag.lock().expect("duration_flag lock") = Some(duration_secs);
                }
                tracing::info!(
                    language = %timeout_lang,
                    duration_sec = duration_secs,
                    source = "timeout_fallback",
                    "LSP: no WorkDoneProgressEnd received — \
                     assuming indexing complete (timeout fallback)"
                );
            }
        });
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) async fn start_process(
        &self,
        descriptor: LanguageLsp,
        attempt: u32,
    ) -> Result<(), LspError> {
        let language_id = descriptor.language_id.clone();

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

        let in_coexistence_mode = self.detect_concurrent_lsp(&language_id, &descriptor.command);
        let isolate_target_dir = in_coexistence_mode;

        let plugins = descriptor.auto_plugins.clone();
        let init_options = descriptor.init_options.clone();
        let spawn_result = spawn_and_initialize(
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

        let supervisor_handle = tokio::spawn(reader_supervisor_task(
            language_id.clone(),
            reader_handle,
            Arc::clone(&self.processes),
        ));

        if attempt > 0 {
            tracing::info!(
                language = %language_id,
                attempt,
                "LSP: recovery successful after backoff"
            );
        }

        let indexing_complete = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let indexing_completion_source = Arc::new(std::sync::Mutex::new(None));
        let indexing_duration_secs = Arc::new(std::sync::Mutex::new(None));
        let indexing_progress = Arc::new(std::sync::Mutex::new(None::<u8>));
        let spawned_at = Instant::now();

        let indexing_flag = Arc::clone(&indexing_complete);
        let indexing_source_flag = Arc::clone(&indexing_completion_source);
        let indexing_duration_flag = Arc::clone(&indexing_duration_secs);
        let indexing_progress_flag = Arc::clone(&indexing_progress);
        let lang_id_for_watcher = language_id.clone();
        let dispatcher_for_watcher = Arc::clone(&self.dispatcher);
        let spawned_at_for_watcher = spawned_at;
        tokio::spawn(async move {
            progress_watcher_task(
                lang_id_for_watcher,
                dispatcher_for_watcher,
                indexing_flag,
                indexing_source_flag,
                indexing_duration_flag,
                indexing_progress_flag,
                spawned_at_for_watcher,
            )
            .await;
        });

        Self::spawn_indexing_timeout_fallback(
            &language_id,
            &indexing_complete,
            &indexing_completion_source,
            &indexing_duration_secs,
            spawned_at,
        );

        let live_capabilities = Arc::new(std::sync::RwLock::new(process.capabilities.clone()));

        let child_handle = process.child_handle();
        let lifecycle = ProcessLifecycle {
            child: child_handle,
        };

        let transport: Arc<dyn LspTransport> = Arc::new(process);
        let transport_for_reg = Arc::clone(&transport);

        let caps_for_reg = Arc::clone(&live_capabilities);
        let lang_id_for_reg = language_id.clone();
        let dispatcher_for_reg = Arc::clone(&self.dispatcher);
        tokio::spawn(async move {
            registration_watcher_task(
                lang_id_for_reg,
                dispatcher_for_reg,
                caps_for_reg,
                transport_for_reg,
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
                transport,
                lifecycle: Some(lifecycle),
                reader_handle: supervisor_handle,
                restart_count: attempt,
                spawned_at,
                indexing_complete,
                indexing_completion_source,
                indexing_duration_secs,
                indexing_progress_percent: indexing_progress,
                live_capabilities,
                in_coexistence_mode,
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

    pub(crate) async fn request(
        &self,
        language_id: &str,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, LspError> {
        let (id, rx) = self.dispatcher.register();
        let message = RequestDispatcher::make_request(id, method, &params);

        let _in_flight_guard = {
            let Some(entry) = self.processes.get(language_id) else {
                return Err(LspError::NoLspAvailable);
            };
            let ProcessEntry::Running(state) = entry.value() else {
                return Err(LspError::NoLspAvailable);
            };
            if state.reader_handle.is_finished() {
                state.reader_handle.abort();
                let transport = Arc::clone(&state.transport);
                let lifecycle = state.lifecycle.clone();
                drop(entry);
                self.processes.remove(language_id);
                transport.shutdown(&self.dispatcher).await;
                if let Some(ref lc) = lifecycle {
                    let _ = lc.child.lock().await.wait().await;
                }
                tracing::warn!(
                    language = %language_id,
                    "LSP: reader task not alive, removed stale entry for recovery"
                );
                return Err(LspError::ConnectionLost);
            }
            let counter = Arc::clone(state.transport.in_flight());
            InFlightGuard::new(counter)
        };

        {
            let Some(entry) = self.processes.get(language_id) else {
                return Err(LspError::NoLspAvailable);
            };
            let ProcessEntry::Running(state) = entry.value() else {
                return Err(LspError::NoLspAvailable);
            };
            state.transport.send(&message).await?;
        }

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
            self.processes.remove(language_id);
            transport.shutdown(&self.dispatcher).await;
            if let Some(ref lc) = lifecycle {
                let _ = lc.child.lock().await.wait().await;
            }
            tracing::warn!(
                language = %language_id,
                "LSP: reader task not alive in notify, removed stale entry for recovery"
            );
            return Err(LspError::ConnectionLost);
        }
        state.transport.send(&message).await
    }

    pub(crate) fn capabilities_for(
        &self,
        language_id: &str,
    ) -> Result<crate::client::DetectedCapabilities, LspError> {
        match self.processes.get(language_id) {
            None => Err(LspError::NoLspAvailable),
            Some(entry) => match entry.value() {
                ProcessEntry::Unavailable(_) => Err(LspError::NoLspAvailable),
                #[allow(clippy::expect_used)]
                ProcessEntry::Running(state) => Ok(state
                    .live_capabilities
                    .read()
                    .expect("live_capabilities lock")
                    .clone()),
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
    async fn test_ensure_process_unavailable_cooldown_elapsed_removes_entry() {
        let processes = HashMap::from([(
            "rust".to_owned(),
            ProcessEntry::Unavailable(UnavailableState {
                backoff_attempt: 0,
                unavailable_since: Instant::now().checked_sub(Duration::from_mins(10)).unwrap(),
            }),
        )]);
        let client = client_with_descriptors(vec!["rust"], processes);

        let result = client.ensure_process("rust").await;
        assert!(
            result.is_ok(),
            "cooldown-elapsed should clear unavailable and return Ok: {result:?}"
        );

        assert!(
            client.processes.get("rust").is_none(),
            "entry should be removed after cooldown-elapsed path"
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

        let mut caps = crate::client::DetectedCapabilities::default();
        caps.definition_provider = true;
        caps.call_hierarchy_provider = true;
        fake.with_capabilities(caps.clone());

        if let Some(entry) = client.processes.get("rust") {
            if let ProcessEntry::Running(state) = entry.value() {
                let mut live_caps = state
                    .live_capabilities
                    .write()
                    .expect("live_capabilities lock");
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
    async fn test_request_with_running_process_times_out() {
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

        assert!(
            client.processes.get("rust").is_none(),
            "stale entry should be removed after dead reader detection"
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
                    .set_last_used(Instant::now() - Duration::from_secs(20 * 60));
            }
        }

        assert!(client.processes.get("rust").is_some());

        let entry = client.processes.get("rust").unwrap();
        if let ProcessEntry::Running(state) = entry.value() {
            assert!(
                state.transport.last_used().elapsed() > Duration::from_secs(15 * 60),
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
                    .set_last_used(Instant::now() - Duration::from_secs(20 * 60));
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
            "should have navigation_ready or indexing_complete: {:?}",
            rust_status
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
                unavailable_since: Instant::now() - Duration::from_secs(100),
            }),
        );

        let _ = client.ensure_process("rust").await;

        let entry = client.processes.get("rust");
        if let Some(entry) = entry {
            if let ProcessEntry::Unavailable(state) = entry.value() {
                assert!(
                    state.backoff_attempt > 2,
                    "backoff_attempt should be incremented: got {}",
                    state.backoff_attempt
                );
            }
        }
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
            doc_versions: Arc::new(DashMap::new()),
            warm_start_complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
        let indexing_completion_source = Arc::new(std::sync::Mutex::new(None));
        let indexing_duration_secs = Arc::new(std::sync::Mutex::new(None));
        let spawned_at = Instant::now();

        crate::client::LspClient::spawn_indexing_timeout_fallback(
            "rust",
            &indexing_complete,
            &indexing_completion_source,
            &indexing_duration_secs,
            spawned_at,
        );

        assert!(!indexing_complete.load(std::sync::atomic::Ordering::Relaxed));
        assert_eq!(*indexing_completion_source.lock().unwrap(), None);
        assert_eq!(*indexing_duration_secs.lock().unwrap(), None);

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_mins(1) + Duration::from_millis(10)).await;

        tokio::task::yield_now().await;

        assert!(
            indexing_complete.load(std::sync::atomic::Ordering::Relaxed),
            "should be marked complete after timeout"
        );
        assert_eq!(
            *indexing_completion_source.lock().unwrap(),
            Some(IndexingCompletionSource::TimeoutFallback),
            "should indicate source is timeout fallback"
        );
        assert!(
            indexing_duration_secs.lock().unwrap().is_some(),
            "should have a duration"
        );
    }

    #[tokio::test]
    async fn test_spawn_indexing_timeout_fallback_noop_if_already_complete() {
        tokio::time::pause();

        let indexing_complete = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let indexing_completion_source = Arc::new(std::sync::Mutex::new(Some(
            IndexingCompletionSource::Progress,
        )));
        let indexing_duration_secs = Arc::new(std::sync::Mutex::new(Some(42)));
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
            *indexing_completion_source.lock().unwrap(),
            Some(IndexingCompletionSource::Progress),
            "should preserve Progress source (not overwrite with TimeoutFallback)"
        );
        assert_eq!(
            *indexing_duration_secs.lock().unwrap(),
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
}
