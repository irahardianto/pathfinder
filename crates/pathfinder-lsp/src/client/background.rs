//! Background task functions for LSP client lifecycle management.
//!
//! These functions run as detached tokio tasks and manage:
//! - Reader supervision (crash recovery, zombie reaping)
//! - Progress watching (indexing completion detection)
//! - Registration watching (dynamic capability registration)
//! - Idle timeout (process termination after inactivity)

use crate::client::protocol::RequestDispatcher;
use crate::client::ProcessEntry;
use crate::types::IndexingCompletionSource;
use dashmap::DashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::broadcast;

const DEFAULT_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(15);
pub(crate) const MAX_BACKOFF_SECS: u64 = 60;
const IDLE_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_mins(1);

pub async fn reader_supervisor_task(
    language_id: String,
    reader_handle: tokio::task::JoinHandle<()>,
    reader_alive: Arc<std::sync::atomic::AtomicBool>,
    processes: Arc<DashMap<String, ProcessEntry>>,
    dispatcher: Arc<RequestDispatcher>,
    doc_versions: Arc<DashMap<String, (String, std::sync::atomic::AtomicI32)>>,
) {
    let crashed = match reader_handle.await {
        Ok(()) => {
            tracing::warn!(
                language = %language_id,
                "LSP: reader task exited normally (EOF), removing process entry"
            );
            false
        }
        Err(e) => {
            tracing::error!(
                language = %language_id,
                error = %e,
                "LSP: reader task crashed (panic or abort), removing process entry"
            );
            true
        }
    };

    // C-2 fix: Mark reader as no longer alive. The remove_if predicate checks
    // this flag to distinguish the old entry from a replacement spawned by
    // crash recovery between reader_handle.await() and this remove operation.
    // Without this, the supervisor's remove_if was dead code (checking its own
    // handle, which is always "not finished" while running).
    reader_alive.store(false, std::sync::atomic::Ordering::Release);

    let removed = processes.remove_if(
        &language_id,
        |_, v| matches!(v, ProcessEntry::Running(s) if !s.reader_alive.load(std::sync::atomic::Ordering::Acquire)),
    );

    if let Some((_lang, ProcessEntry::Running(state))) = removed {
        tracing::debug!(
            language = %language_id,
            "LSP: supervisor reaping child process to free PID slot"
        );
        state.reader_handle.abort();
        state.abort_watchers();

        // MEDIUM-1 fix: Cancel pending requests for this language when reader crashes.
        // Normal EOF path has reader_task itself calling cancel_for_language before
        // exit. But panic/abort bypasses that, so supervisor must do it.
        if crashed {
            dispatcher.cancel_for_language(&language_id);
            // MAJOR: Clear stale doc_versions for this language after crash recovery.
            // A new LSP instance won't know about previously opened documents.
            let mut cleared = 0;
            doc_versions.retain(|_uri, (lang, _)| {
                if lang == &language_id {
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
                    "LSP: cleared stale doc_versions for language after supervisor-detected crash"
                );
            }
        }

        if let Some(ref lifecycle) = state.lifecycle {
            let _ = lifecycle.child.lock().await.wait().await;
        }

        // Insert Unavailable on BOTH crash and normal EOF to prevent unthrottled
        // respawn loops. Without this, an LSP that exits immediately on startup
        // (bad config, missing deps) creates a tight spawn-exit loop.
        // Only the crash path previously got backoff — normal EOF was unprotected.
        tracing::warn!(
            language = %language_id,
            crashed = crashed,
            "LSP: inserting Unavailable entry for backoff protection"
        );
        processes.insert(
            language_id,
            ProcessEntry::Unavailable(super::UnavailableState {
                unavailable_since: std::time::Instant::now(),
                backoff_attempt: 1,
            }),
        );
    } else {
        tracing::debug!(
            language = %language_id,
            "LSP: supervisor found entry already removed (raced with idle-loop or force_respawn) or replaced by recovery"
        );
    }
}

pub async fn progress_watcher_task(
    language_id: String,
    // M-6: Accept pre-created receiver instead of creating via dispatcher.
    mut rx: broadcast::Receiver<serde_json::Value>,
    indexing_complete: Arc<std::sync::atomic::AtomicBool>,
    indexing_completion_source: Arc<parking_lot::Mutex<Option<IndexingCompletionSource>>>,
    indexing_duration_secs: Arc<parking_lot::Mutex<Option<u64>>>,
    indexing_progress_percent: Arc<parking_lot::Mutex<Option<u8>>>,
    spawned_at: Instant,
) {
    // LSP-INIT-002: Receiver was created with per-language subscription.
    // This ensures progress notifications from other languages don't bleed.
    tracing::debug!(language = %language_id, "progress_watcher_task: started");

    loop {
        match rx.recv().await {
            Ok(msg) => {
                let action = extract_progress_action(&msg);
                if let ProgressAction::End { .. } = &action {
                    if indexing_complete.load(std::sync::atomic::Ordering::Relaxed) {
                        let duration_secs = spawned_at.elapsed().as_secs();
                        tracing::warn!(
                            language = %language_id,
                            duration_sec = duration_secs,
                            "LSP: WorkDoneProgressEnd arrived {0}s after timeout fallback already fired — consider increasing timeout",
                            duration_secs
                        );
                        continue;
                    }
                }
                apply_progress_action(
                    action,
                    &indexing_complete,
                    &indexing_completion_source,
                    &indexing_duration_secs,
                    &indexing_progress_percent,
                    spawned_at,
                );
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(
                    language = %language_id,
                    missed = n,
                    "progress_watcher_task: lagged, missed notifications"
                );
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                tracing::debug!(language = %language_id, "progress_watcher_task: channel closed, exiting");
                break;
            }
        }
    }
}

pub async fn registration_watcher_task(
    language_id: String,
    // M-6: Accept pre-created receiver instead of creating via dispatcher.
    mut rx: broadcast::Receiver<serde_json::Value>,
    live_capabilities: Arc<parking_lot::RwLock<crate::client::DetectedCapabilities>>,
    transport: Arc<dyn crate::client::process::LspTransport>,
) {
    // LSP-INIT-002: Receiver was created with per-language subscription.
    // This ensures registrations from other languages don't pollute capabilities.
    tracing::debug!(language = %language_id, "registration_watcher_task: started");

    loop {
        match rx.recv().await {
            Ok(msg) => {
                let action = extract_registration_action(&msg);

                // H-5: Send response BEFORE applying registration locally.
                // If send fails, the LSP server never received the acknowledgment,
                // so applying locally would create state inconsistency.
                if let Some(ref id_val) = action.response_id {
                    let response = build_registration_response(id_val);
                    if let Err(e) = transport.send(&response).await {
                        tracing::warn!(
                            language = %language_id,
                            error = %e,
                            "registration_watcher_task: failed to send response, \
                             skipping local capability update"
                        );
                        // Don't apply registrations if we couldn't ack them.
                        // The server will retry.
                        continue;
                    }
                }

                if !action.registrations.is_empty() {
                    let mut caps = live_capabilities.write();
                    for (reg_method, reg_id, opts) in &action.registrations {
                        if caps.apply_registration(reg_method, reg_id, opts) {
                            tracing::info!(
                                language = %language_id,
                                method = reg_method,
                                id = reg_id,
                                "LSP: dynamic capability registered"
                            );
                        }
                    }
                }

                if !action.unregistrations.is_empty() {
                    let mut caps = live_capabilities.write();
                    for reg_id in &action.unregistrations {
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

/// Collect languages whose processes are idle (elapsed > timeout, `in_flight` == 0).
///
/// Extracted from [`idle_timeout_task`] for testability. The idle timeout task
/// calls this to determine which processes to remove.
pub(crate) fn collect_idle_languages(processes: &DashMap<String, ProcessEntry>) -> Vec<String> {
    use std::sync::atomic::Ordering;

    processes
        .iter()
        .filter_map(|entry| {
            let lang = entry.key();
            if let ProcessEntry::Running(state) = entry.value() {
                if state.transport.last_used().elapsed() > DEFAULT_IDLE_TIMEOUT
                    && state.transport.in_flight().load(Ordering::Acquire) == 0
                {
                    Some(lang.clone())
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect()
}

#[allow(clippy::too_many_lines)]
pub async fn idle_timeout_task(
    processes: Arc<DashMap<String, ProcessEntry>>,
    dispatcher: Arc<RequestDispatcher>,
    doc_versions: Arc<DashMap<String, (String, std::sync::atomic::AtomicI32)>>,
    mut shutdown_rx: broadcast::Receiver<()>,
) {
    use std::sync::atomic::Ordering;

    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                tracing::info!("LSP: shutdown signal received, terminating all processes");
                let keys: Vec<String> = processes.iter().map(|e| e.key().clone()).collect();
                for lang in keys {
                    if let Some((_lang, ProcessEntry::Running(state))) = processes.remove(&lang) {
                        tracing::debug!(language = %lang, "LSP: shutting down process");
                        // C-3: Send shutdown request BEFORE aborting reader.
                        // If reader is aborted first, nobody reads the shutdown response,
                        // causing the 2s timeout to always fire. With 5 languages,
                        // shutdown takes minimum 10 seconds.
                        // H-4: Wrap transport.shutdown() in timeout to prevent blocking
                        // if child lock is contended (supervisor zombie reaping).
                        let _ = tokio::time::timeout(
                            std::time::Duration::from_secs(5),
                            state.transport.shutdown(&dispatcher, &lang),
                        ).await;
                        state.reader_handle.abort();
                        state.abort_watchers();
                        // BUG-4 fix: reader is aborted so it won't call cancel_for_language.
                        // We must cancel pending requests for this language explicitly.
                        dispatcher.cancel_for_language(&lang);
                        if let Some(ref lifecycle) = state.lifecycle {
                            let _ = lifecycle.child.lock().await.wait().await;
                        }
                    }
                }
                tracing::info!("LSP: all processes terminated");
                break;
            }
            () = tokio::time::sleep(IDLE_CHECK_INTERVAL) => {
                let dead_languages: Vec<String> = processes
                    .iter()
                    .filter_map(|entry| {
                        if let ProcessEntry::Running(state) = entry.value() {
                            if state.transport.is_alive() {
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
                    if let Some((key, entry)) = processes.remove(&lang) {
                        if let ProcessEntry::Running(state) = entry {
                            if state.transport.is_alive() {
                                // Race: process is now alive. Restore the entry.
                                processes.insert(key, ProcessEntry::Running(state));
                            } else {
                                tracing::error!(
                                    language = %lang,
                                    "LSP: zombie reap — process died outside reader task, \
                                     removing entry so recovery can proceed"
                                );
                                state.reader_handle.abort();
                                state.abort_watchers();
                                // Zombie reap: reader may or may not have called cancel_for_language.
                                // Call it again to ensure pending requests are unblocked.
                                dispatcher.cancel_for_language(&lang);
                                if let Some(ref lifecycle) = state.lifecycle {
                                    let _ = lifecycle.child.lock().await.wait().await;
                                }
                                // Clear stale doc_versions for this language from the dead instance.
                                let mut cleared = 0;
                                doc_versions.retain(|_uri, (l, _)| {
                                    if l == &lang {
                                        cleared += 1;
                                        false
                                    } else {
                                        true
                                    }
                                });
                                if cleared > 0 {
                                    tracing::debug!(
                                        language = %lang,
                                        cleared,
                                        "LSP: cleared stale doc_versions for language after zombie reap"
                                    );
                                }
                                // M-9: Insert Unavailable for backoff protection. Without this,
                                // ensure_process would immediately retry and could create a tight
                                // spawn-exit loop if the child keeps dying.
                                processes.insert(
                                    key,
                                    ProcessEntry::Unavailable(super::UnavailableState {
                                        unavailable_since: std::time::Instant::now(),
                                        backoff_attempt: 1,
                                    }),
                                );
                            }
                        } else {
                            // Race: entry is now Unavailable instead of Running. Restore it.
                            processes.insert(key, entry);
                        }
                    }
                }

                let candidates = collect_idle_languages(&processes);

                for lang in candidates {
                    // Atomically check in_flight and remove if still idle.
                    let removed = processes.remove_if(&lang, |_, v| {
                        if let ProcessEntry::Running(state) = v {
                            state.transport.last_used().elapsed() > DEFAULT_IDLE_TIMEOUT
                                && state.transport.in_flight().load(Ordering::Acquire) == 0
                        } else {
                            false
                        }
                    });

                    if let Some((_lang, ProcessEntry::Running(state))) = removed {
                        tracing::info!(
                            language = %lang,
                            restarts = state.restart_count,
                            "LSP: idle timeout — terminating"
                        );
                        // C-3: Send shutdown BEFORE aborting reader so response can be read.
                        // H-4: Wrap in timeout to prevent blocking.
                        let _ = tokio::time::timeout(
                            std::time::Duration::from_secs(5),
                            state.transport.shutdown(&dispatcher, &lang),
                        ).await;
                        state.reader_handle.abort();
                        state.abort_watchers();
                        // Idle timeout: reader is aborted, so call cancel_for_language explicitly.
                        dispatcher.cancel_for_language(&lang);
                        if let Some(ref lifecycle) = state.lifecycle {
                            let _ = lifecycle.child.lock().await.wait().await;
                        }
                        // Clear stale doc_versions for this language from the idle-killed instance.
                        let mut cleared = 0;
                        doc_versions.retain(|_uri, (l, _)| {
                            if l == &lang {
                                cleared += 1;
                                false
                            } else {
                                true
                            }
                        });
                        if cleared > 0 {
                            tracing::debug!(
                                language = %lang,
                                cleared,
                                "LSP: cleared stale doc_versions for language after idle timeout"
                            );
                        }
                    } else {
                        tracing::debug!(
                            language = %lang,
                            "LSP: idle timeout skipped — in_flight request arrived or entry removed"
                        );
                    }
                }
            }
        }
    }
}

#[derive(Debug, PartialEq, Copy, Clone)]
pub enum ProgressAction {
    End { duration_secs: Option<u64> },
    Report { percentage: u8 },
    None,
}

pub fn extract_progress_action(msg: &serde_json::Value) -> ProgressAction {
    let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");

    if method != "$/progress" && !method.starts_with("window/workDoneProgress") {
        return ProgressAction::None;
    }

    let kind = msg.pointer("/params/value/kind").and_then(|v| v.as_str());

    match kind {
        Some("end") => ProgressAction::End {
            duration_secs: None,
        },
        Some("report") => {
            let percentage = msg
                .pointer("/params/value/percentage")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let clamped = u8::try_from(percentage.min(100)).unwrap_or(100);
            ProgressAction::Report {
                percentage: clamped,
            }
        }
        _ => ProgressAction::None,
    }
}

pub fn apply_progress_action(
    action: ProgressAction,
    indexing_complete: &std::sync::atomic::AtomicBool,
    indexing_completion_source: &parking_lot::Mutex<Option<IndexingCompletionSource>>,
    indexing_duration_secs: &parking_lot::Mutex<Option<u64>>,
    indexing_progress_percent: &parking_lot::Mutex<Option<u8>>,
    spawned_at: Instant,
) {
    use std::sync::atomic::Ordering;

    match action {
        ProgressAction::End { .. } => {
            let was_already_complete = indexing_complete.swap(true, Ordering::SeqCst);
            if was_already_complete {
                return;
            }

            let duration = spawned_at.elapsed().as_secs();

            if let Some(mut source) = indexing_completion_source.try_lock() {
                *source = Some(IndexingCompletionSource::Progress);
            }
            if let Some(mut dur) = indexing_duration_secs.try_lock() {
                *dur = Some(duration);
            }
            if let Some(mut progress) = indexing_progress_percent.try_lock() {
                *progress = None;
            }
        }
        ProgressAction::Report { percentage } => {
            if let Some(mut progress) = indexing_progress_percent.try_lock() {
                *progress = Some(percentage);
            }
        }
        ProgressAction::None => {}
    }
}

#[derive(Debug, PartialEq)]
pub struct RegistrationAction {
    pub registrations: Vec<(String, String, serde_json::Value)>,
    pub unregistrations: Vec<String>,
    pub response_id: Option<serde_json::Value>,
}

pub fn extract_registration_action(msg: &serde_json::Value) -> RegistrationAction {
    let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let id = msg.get("id").cloned();

    let mut registrations = Vec::new();
    let mut unregistrations = Vec::new();

    match method {
        "client/registerCapability" => {
            if let Some(regs) = msg
                .pointer("/params/registrations")
                .and_then(|v| v.as_array())
            {
                for reg in regs {
                    let reg_id = reg
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let reg_method = reg
                        .get("method")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let opts = reg
                        .get("registerOptions")
                        .cloned()
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::default()));
                    registrations.push((reg_method, reg_id, opts));
                }
            }
        }
        "client/unregisterCapability" => {
            if let Some(unregs) = msg
                .pointer("/params/unregisterations")
                .or_else(|| msg.pointer("/params/unregistrations"))
                .and_then(|v| v.as_array())
            {
                for unreg in unregs {
                    let reg_id = unreg
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    unregistrations.push(reg_id);
                }
            }
        }
        _ => {}
    }

    RegistrationAction {
        registrations,
        unregistrations,
        response_id: id,
    }
}

pub fn build_registration_response(id: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": null
    })
}

#[cfg(test)]
#[path = "background_test.rs"]
mod tests;
