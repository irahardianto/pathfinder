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
                    .iter_mut()
                    .filter_map(|mut entry| {
                        if let ProcessEntry::Running(state) = entry.value_mut() {
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

                // P2-2 + P3-2 fix: Use remove_if for atomic check-and-remove.
                // This eliminates the TOCTOU window between checking in_flight and
                // actually removing the entry. First collect candidates, then try
                // to remove each with remove_if.
                let candidates: Vec<String> = processes
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
                    .collect();

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
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    #[test]
    fn test_extract_progress_action_end() {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "$/progress",
            "params": {
                "token": "indexing-token",
                "value": {
                    "kind": "end"
                }
            }
        });

        let action = extract_progress_action(&msg);
        assert!(matches!(action, ProgressAction::End { .. }));
    }

    #[test]
    fn test_extract_progress_action_report_with_percentage() {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "$/progress",
            "params": {
                "token": "indexing-token",
                "value": {
                    "kind": "report",
                    "percentage": 42
                }
            }
        });

        let action = extract_progress_action(&msg);
        assert!(matches!(action, ProgressAction::Report { percentage: 42 }));
    }

    #[test]
    fn test_extract_progress_action_report_clamps_high() {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "$/progress",
            "params": {
                "token": "indexing-token",
                "value": {
                    "kind": "report",
                    "percentage": 150
                }
            }
        });

        let action = extract_progress_action(&msg);
        assert!(matches!(action, ProgressAction::Report { percentage: 100 }));
    }

    #[test]
    fn test_extract_progress_action_none_for_other_method() {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {}
        });

        let action = extract_progress_action(&msg);
        assert!(matches!(action, ProgressAction::None));
    }

    #[test]
    fn test_extract_progress_action_none_for_missing_kind() {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "$/progress",
            "params": {
                "token": "indexing-token",
                "value": {}
            }
        });

        let action = extract_progress_action(&msg);
        assert!(matches!(action, ProgressAction::None));
    }

    #[test]
    fn test_apply_progress_action_end() {
        let indexing_complete = std::sync::atomic::AtomicBool::new(false);
        let indexing_completion_source = parking_lot::Mutex::new(None);
        let indexing_duration_secs = parking_lot::Mutex::new(None);
        let indexing_progress_percent = parking_lot::Mutex::new(Some(50));
        let spawned_at = Instant::now();

        let action = ProgressAction::End {
            duration_secs: None,
        };

        apply_progress_action(
            action,
            &indexing_complete,
            &indexing_completion_source,
            &indexing_duration_secs,
            &indexing_progress_percent,
            spawned_at,
        );

        assert!(indexing_complete.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(
            *indexing_completion_source.lock(),
            Some(IndexingCompletionSource::Progress)
        );
        assert!(indexing_duration_secs.lock().is_some());
        assert_eq!(*indexing_progress_percent.lock(), None);
    }

    #[test]
    fn test_apply_progress_action_end_already_complete() {
        let indexing_complete = std::sync::atomic::AtomicBool::new(true);
        let indexing_completion_source =
            parking_lot::Mutex::new(Some(IndexingCompletionSource::TimeoutFallback));
        let indexing_duration_secs = parking_lot::Mutex::new(Some(100));
        let indexing_progress_percent = parking_lot::Mutex::new(None);
        let spawned_at = Instant::now()
            .checked_sub(Duration::from_secs(200))
            .unwrap();

        let action = ProgressAction::End {
            duration_secs: None,
        };

        apply_progress_action(
            action,
            &indexing_complete,
            &indexing_completion_source,
            &indexing_duration_secs,
            &indexing_progress_percent,
            spawned_at,
        );

        assert!(indexing_complete.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(
            *indexing_completion_source.lock(),
            Some(IndexingCompletionSource::TimeoutFallback)
        );
        assert_eq!(*indexing_duration_secs.lock(), Some(100));
    }

    #[test]
    fn test_apply_progress_action_report() {
        let indexing_complete = std::sync::atomic::AtomicBool::new(false);
        let indexing_completion_source = parking_lot::Mutex::new(None);
        let indexing_duration_secs = parking_lot::Mutex::new(None);
        let indexing_progress_percent = parking_lot::Mutex::new(None);
        let spawned_at = Instant::now();

        let action = ProgressAction::Report { percentage: 75 };

        apply_progress_action(
            action,
            &indexing_complete,
            &indexing_completion_source,
            &indexing_duration_secs,
            &indexing_progress_percent,
            spawned_at,
        );

        assert!(!indexing_complete.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(*indexing_progress_percent.lock(), Some(75));
        assert!(indexing_duration_secs.lock().is_none());
    }

    #[test]
    fn test_extract_registration_action_register() {
        let msg = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "client/registerCapability",
            "params": {
                "registrations": [{
                    "id": "reg-1",
                    "method": "textDocument/didChange",
                    "registerOptions": {
                        "documentSelector": [{ "language": "rust" }]
                    }
                }]
            }
        });

        let action = extract_registration_action(&msg);
        assert_eq!(action.registrations.len(), 1);
        assert_eq!(action.registrations[0].0, "textDocument/didChange");
        assert_eq!(action.registrations[0].1, "reg-1");
        assert_eq!(action.unregistrations.len(), 0);
        assert!(action.response_id.is_some());
    }

    #[test]
    fn test_extract_registration_action_unregister() {
        let msg = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "client/unregisterCapability",
            "params": {
                "unregisterations": [{
                    "id": "reg-1"
                }]
            }
        });

        let action = extract_registration_action(&msg);
        assert_eq!(action.registrations.len(), 0);
        assert_eq!(action.unregistrations.len(), 1);
        assert_eq!(action.unregistrations[0], "reg-1");
    }

    #[test]
    fn test_extract_registration_action_other_method() {
        let msg = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "workspace/configuration",
            "params": {}
        });

        let action = extract_registration_action(&msg);
        assert_eq!(action.registrations.len(), 0);
        assert_eq!(action.unregistrations.len(), 0);
        assert!(action.response_id.is_some());
    }

    #[test]
    fn test_build_registration_response() {
        let id = json!(42);
        let response = build_registration_response(&id);

        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 42);
        assert_eq!(response["result"], json!(null));
    }

    #[tokio::test]
    async fn test_progress_watcher_receives_end_notification() {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "$/progress",
            "params": {
                "token": "indexing-token",
                "value": { "kind": "end" }
            }
        });

        let action = extract_progress_action(&msg);
        assert!(matches!(action, ProgressAction::End { .. }));
    }

    #[tokio::test]
    async fn test_progress_watcher_receives_report_notification() {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "$/progress",
            "params": {
                "token": "indexing-token",
                "value": { "kind": "report", "percentage": 75 }
            }
        });

        let action = extract_progress_action(&msg);
        assert!(matches!(action, ProgressAction::Report { percentage: 75 }));
    }

    #[tokio::test]
    async fn test_progress_watcher_exits_on_channel_close() {
        let dispatcher = Arc::new(RequestDispatcher::new());
        let mut rx = dispatcher.subscribe_notifications();

        drop(dispatcher);

        let result = rx.recv().await;
        assert!(result.is_err(), "should get error when channel closed");
    }

    #[tokio::test]
    async fn test_registration_watcher_handles_register() {
        let msg = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "client/registerCapability",
            "params": {
                "registrations": [{
                    "id": "reg-1",
                    "method": "textDocument/didChange",
                    "registerOptions": {
                        "documentSelector": [{ "language": "rust" }]
                    }
                }]
            }
        });

        let action = extract_registration_action(&msg);
        assert_eq!(action.registrations.len(), 1);
        assert_eq!(action.registrations[0].0, "textDocument/didChange");
        assert_eq!(action.registrations[0].1, "reg-1");
        assert!(action.response_id.is_some());
    }

    #[tokio::test]
    async fn test_registration_watcher_handles_unregister() {
        let msg = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "client/unregisterCapability",
            "params": {
                "unregisterations": [{
                    "id": "reg-1"
                }]
            }
        });

        let action = extract_registration_action(&msg);
        assert_eq!(action.registrations.len(), 0);
        assert_eq!(action.unregistrations.len(), 1);
        assert_eq!(action.unregistrations[0], "reg-1");
    }

    #[test]
    fn test_extract_progress_action_begin_is_none() {
        let msg = json!({
            "method": "$/progress",
            "params": {
                "token": "test",
                "value": { "kind": "begin", "title": "Indexing" }
            }
        });
        let action = extract_progress_action(&msg);
        assert_eq!(action, ProgressAction::None);
    }

    #[test]
    fn test_extract_progress_action_report_zero_percentage() {
        let msg = json!({
            "method": "$/progress",
            "params": {
                "token": "test",
                "value": { "kind": "report", "percentage": 0 }
            }
        });
        let action = extract_progress_action(&msg);
        assert_eq!(action, ProgressAction::Report { percentage: 0 });
    }

    #[test]
    fn test_extract_progress_action_report_with_message() {
        let msg = json!({
            "method": "$/progress",
            "params": {
                "token": "test",
                "value": { "kind": "report", "message": "50/100 files" }
            }
        });
        let action = extract_progress_action(&msg);
        assert_eq!(
            action,
            ProgressAction::Report { percentage: 0 },
            "missing percentage should default to 0"
        );
    }

    #[test]
    fn test_apply_progress_action_report_updates_percentage() {
        let indexing_complete = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let indexing_source = Arc::new(parking_lot::Mutex::new(None));
        let indexing_duration = Arc::new(parking_lot::Mutex::new(None));
        let indexing_progress = Arc::new(parking_lot::Mutex::new(None));
        let spawned_at = Instant::now();

        apply_progress_action(
            ProgressAction::Report { percentage: 42 },
            &indexing_complete,
            &indexing_source,
            &indexing_duration,
            &indexing_progress,
            spawned_at,
        );

        assert_eq!(*indexing_progress.lock(), Some(42));
        assert!(
            !indexing_complete.load(std::sync::atomic::Ordering::SeqCst),
            "report should not set indexing_complete"
        );
    }

    #[test]
    fn test_apply_progress_action_end_sets_complete() {
        let indexing_complete = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let indexing_source = Arc::new(parking_lot::Mutex::new(None));
        let indexing_duration = Arc::new(parking_lot::Mutex::new(None));
        let indexing_progress = Arc::new(parking_lot::Mutex::new(Some(50)));
        let spawned_at = Instant::now();

        apply_progress_action(
            ProgressAction::End {
                duration_secs: None,
            },
            &indexing_complete,
            &indexing_source,
            &indexing_duration,
            &indexing_progress,
            spawned_at,
        );

        assert!(
            indexing_complete.load(std::sync::atomic::Ordering::SeqCst),
            "end should set indexing_complete"
        );
        assert_eq!(
            *indexing_source.lock(),
            Some(IndexingCompletionSource::Progress)
        );
        assert!(indexing_duration.lock().is_some());
        assert_eq!(
            *indexing_progress.lock(),
            None,
            "end should clear progress percentage"
        );
    }

    #[test]
    fn test_apply_progress_action_end_idempotent() {
        let indexing_complete = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let indexing_source = Arc::new(parking_lot::Mutex::new(Some(
            IndexingCompletionSource::TimeoutFallback,
        )));
        let indexing_duration = Arc::new(parking_lot::Mutex::new(Some(99)));
        let indexing_progress = Arc::new(parking_lot::Mutex::new(None));
        let spawned_at = Instant::now();

        apply_progress_action(
            ProgressAction::End {
                duration_secs: None,
            },
            &indexing_complete,
            &indexing_source,
            &indexing_duration,
            &indexing_progress,
            spawned_at,
        );

        assert_eq!(
            *indexing_source.lock(),
            Some(IndexingCompletionSource::TimeoutFallback),
            "should not overwrite existing source"
        );
        assert_eq!(
            *indexing_duration.lock(),
            Some(99),
            "should not overwrite existing duration"
        );
    }

    #[test]
    fn test_apply_progress_action_none_is_noop() {
        let indexing_complete = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let indexing_source = Arc::new(parking_lot::Mutex::new(None));
        let indexing_duration = Arc::new(parking_lot::Mutex::new(None));
        let indexing_progress = Arc::new(parking_lot::Mutex::new(None));
        let spawned_at = Instant::now();

        apply_progress_action(
            ProgressAction::None,
            &indexing_complete,
            &indexing_source,
            &indexing_duration,
            &indexing_progress,
            spawned_at,
        );

        assert!(!indexing_complete.load(std::sync::atomic::Ordering::SeqCst));
        assert!(indexing_source.lock().is_none());
        assert!(indexing_duration.lock().is_none());
        assert!(indexing_progress.lock().is_none());
    }

    #[test]
    fn test_build_registration_response_structure() {
        let id_val = json!(42);
        let response = build_registration_response(&id_val);
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 42);
        assert_eq!(response["result"], serde_json::Value::Null);
    }

    #[test]
    fn test_build_registration_response_string_id() {
        let id_val = json!("reg-001");
        let response = build_registration_response(&id_val);
        assert_eq!(response["id"], "reg-001");
    }

    #[test]
    fn test_extract_registration_action_unknown_method() {
        let msg = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "window/logMessage",
            "params": {}
        });
        let action = extract_registration_action(&msg);
        assert!(action.registrations.is_empty());
        assert!(action.unregistrations.is_empty());
    }

    #[test]
    fn test_extract_registration_action_no_params() {
        let msg = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "client/registerCapability"
        });
        let action = extract_registration_action(&msg);
        assert!(action.registrations.is_empty());
    }

    #[tokio::test]
    async fn test_reader_supervisor_normal_exit_removes_entry() {
        let processes = Arc::new(DashMap::new());
        let dispatcher = Arc::new(RequestDispatcher::new());

        let reader_handle = tokio::spawn(async {});
        let reader_alive = Arc::new(std::sync::atomic::AtomicBool::new(true));

        let (shutdown_tx, _) = broadcast::channel::<()>(1);

        let fake_transport = Arc::new(crate::client::fake_transport::FakeTransport::new());
        fake_transport.set_dispatcher(Arc::clone(&dispatcher));

        processes.insert(
            "rust".to_owned(),
            ProcessEntry::Running(Box::new(crate::client::LanguageState {
                transport: fake_transport as Arc<dyn crate::client::process::LspTransport>,
                lifecycle: None,
                reader_handle: tokio::spawn(async {}),
                reader_alive: Arc::clone(&reader_alive),
                restart_count: 0,
                spawned_at: Instant::now(),
                indexing_complete: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                indexing_completion_source: Arc::new(parking_lot::Mutex::new(None)),
                indexing_duration_secs: Arc::new(parking_lot::Mutex::new(None)),
                indexing_progress_percent: Arc::new(parking_lot::Mutex::new(None)),
                live_capabilities: Arc::new(parking_lot::RwLock::new(
                    crate::client::DetectedCapabilities::default(),
                )),
                in_coexistence_mode: false,
                watcher_handles: vec![],
            })),
        );

        reader_supervisor_task(
            "rust".to_owned(),
            reader_handle,
            reader_alive,
            Arc::clone(&processes),
            dispatcher,
            Arc::new(DashMap::new()),
        )
        .await;

        let entry = processes.get("rust");
        if let Some(e) = entry {
            assert!(
                matches!(e.value(), ProcessEntry::Unavailable(_)),
                "supervisor should insert Unavailable after normal exit"
            );
        }
        let _ = shutdown_tx;
    }

    #[tokio::test]
    async fn test_progress_watcher_lagged_continues() {
        let (tx, rx) = broadcast::channel::<serde_json::Value>(1);
        let indexing_complete = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let indexing_source = Arc::new(parking_lot::Mutex::new(None));
        let indexing_duration = Arc::new(parking_lot::Mutex::new(None));
        let indexing_progress = Arc::new(parking_lot::Mutex::new(None));

        let handle = tokio::spawn(progress_watcher_task(
            "rust".to_owned(),
            rx,
            Arc::clone(&indexing_complete),
            Arc::clone(&indexing_source),
            Arc::clone(&indexing_duration),
            Arc::clone(&indexing_progress),
            Instant::now(),
        ));

        drop(tx);

        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), handle).await;
    }

    #[tokio::test]
    async fn test_registration_watcher_send_failure_skips_local_update() {
        use crate::client::fake_transport::FakeTransport;
        use crate::client::process::LspTransport;

        let dispatcher = Arc::new(RequestDispatcher::new());
        let rx = dispatcher.subscribe_server_requests_for_language("rust");

        let live_capabilities = Arc::new(parking_lot::RwLock::new(
            crate::client::DetectedCapabilities::default(),
        ));

        // Create a FakeTransport that is killed (send will return ConnectionLost)
        let transport = Arc::new(FakeTransport::new());
        transport.kill();

        let caps_for_check = Arc::clone(&live_capabilities);

        let handle = tokio::spawn(registration_watcher_task(
            "rust".to_owned(),
            rx,
            live_capabilities,
            transport as Arc<dyn LspTransport>,
        ));

        // Send a registration message via the dispatcher
        let register_msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "client/registerCapability",
            "params": {
                "registrations": [{
                    "id": "reg-1",
                    "method": "textDocument/definition",
                    "registerOptions": {}
                }]
            }
        });
        dispatcher.dispatch_response_for_language("rust", &register_msg);

        // Give the watcher time to process
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Verify: capability should NOT be updated because send failed
        {
            let caps = caps_for_check.read();
            assert!(
                !caps.definition_provider,
                "registration should be skipped when transport.send() fails"
            );
        }

        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn test_registration_watcher_lagged_continues() {
        let (tx, rx) = broadcast::channel::<serde_json::Value>(1);
        let live_capabilities = Arc::new(parking_lot::RwLock::new(
            crate::client::DetectedCapabilities::default(),
        ));
        let transport = Arc::new(crate::client::fake_transport::FakeTransport::new());

        let handle = tokio::spawn(registration_watcher_task(
            "rust".to_owned(),
            rx,
            live_capabilities,
            transport as Arc<dyn crate::client::process::LspTransport>,
        ));

        // Drop sender to close channel
        drop(tx);

        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), handle).await;
    }
}
