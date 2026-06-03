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
    processes: Arc<DashMap<String, ProcessEntry>>,
    dispatcher: Arc<RequestDispatcher>,
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

    // P1-2 fix: Use remove_if to only remove if reader_handle is still finished.
    // This prevents killing a healthy replacement process that was spawned by
    // crash recovery between reader_handle.await() and this remove operation.
    let removed = processes.remove_if(
        &language_id,
        |_, v| matches!(v, ProcessEntry::Running(s) if s.reader_handle.is_finished()),
    );

    if let Some((_lang, ProcessEntry::Running(state))) = removed {
        tracing::debug!(
            language = %language_id,
            "LSP: supervisor reaping child process to free PID slot"
        );
        state.reader_handle.abort();

        // MEDIUM-1 fix: Cancel pending requests for this language when reader crashes.
        // Normal EOF path has reader_task itself calling cancel_for_language before
        // exit. But panic/abort bypasses that, so supervisor must do it.
        if crashed {
            dispatcher.cancel_for_language(&language_id);
        }

        if let Some(ref lifecycle) = state.lifecycle {
            let _ = lifecycle.child.lock().await.wait().await;
        }

        if crashed {
            tracing::warn!(
                language = %language_id,
                "LSP: inserting Unavailable entry after crash for backoff protection"
            );
            processes.insert(
                language_id,
                ProcessEntry::Unavailable(super::UnavailableState {
                    unavailable_since: std::time::Instant::now(),
                    backoff_attempt: 1,
                }),
            );
        }
    } else {
        tracing::debug!(
            language = %language_id,
            "LSP: supervisor found entry already removed (raced with idle-loop or force_respawn) or replaced by recovery"
        );
    }
}

pub async fn progress_watcher_task(
    language_id: String,
    dispatcher: Arc<RequestDispatcher>,
    indexing_complete: Arc<std::sync::atomic::AtomicBool>,
    indexing_completion_source: Arc<std::sync::Mutex<Option<IndexingCompletionSource>>>,
    indexing_duration_secs: Arc<std::sync::Mutex<Option<u64>>>,
    indexing_progress_percent: Arc<std::sync::Mutex<Option<u8>>>,
    spawned_at: Instant,
) {
    // LSP-INIT-002: Subscribe only to this language's notifications.
    // This prevents progress notifications from other languages (e.g., rust's
    // WorkDoneProgressEnd) from falsely marking this language as indexing-complete.
    let mut rx = dispatcher.subscribe_notifications_for_language(&language_id);
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
    dispatcher: Arc<RequestDispatcher>,
    live_capabilities: Arc<std::sync::RwLock<crate::client::DetectedCapabilities>>,
    transport: Arc<dyn crate::client::process::LspTransport>,
) {
    // LSP-INIT-002: Subscribe only to this language's server requests.
    // This prevents capability registrations from other languages (e.g., rust's
    // pull diagnostics registration) from polluting this language's live_capabilities.
    let mut rx = dispatcher.subscribe_server_requests_for_language(&language_id);
    tracing::debug!(language = %language_id, "registration_watcher_task: started");

    loop {
        match rx.recv().await {
            Ok(msg) => {
                let action = extract_registration_action(&msg);

                if !action.registrations.is_empty() {
                    #[allow(clippy::expect_used)]
                    let mut caps = live_capabilities
                        .write()
                        .expect("live_capabilities write lock");
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
                    #[allow(clippy::expect_used)]
                    let mut caps = live_capabilities
                        .write()
                        .expect("live_capabilities write lock");
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

                if let Some(ref id_val) = action.response_id {
                    let response = build_registration_response(id_val);
                    if let Err(e) = transport.send(&response).await {
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

pub async fn idle_timeout_task(
    processes: Arc<DashMap<String, ProcessEntry>>,
    dispatcher: Arc<RequestDispatcher>,
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
                        state.reader_handle.abort();
                        // BUG-4 fix: reader is aborted so it won't call cancel_for_language.
                        // We must cancel pending requests for this language explicitly.
                        dispatcher.cancel_for_language(&lang);
                        state.transport.shutdown(&dispatcher, &lang).await;
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
                    if let Some((_lang, ProcessEntry::Running(state))) = processes.remove(&lang) {
                        tracing::error!(
                            language = %lang,
                            "LSP: zombie reap — process died outside reader task, \
                             removing entry so recovery can proceed"
                        );
                        state.reader_handle.abort();
                        // Zombie reap: reader may or may not have called cancel_for_language.
                        // Call it again to ensure pending requests are unblocked.
                        dispatcher.cancel_for_language(&lang);
                        if let Some(ref lifecycle) = state.lifecycle {
                            let _ = lifecycle.child.lock().await.wait().await;
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
                        state.reader_handle.abort();
                        // Idle timeout: reader is aborted, so call cancel_for_language explicitly.
                        dispatcher.cancel_for_language(&lang);
                        state.transport.shutdown(&dispatcher, &lang).await;
                        if let Some(ref lifecycle) = state.lifecycle {
                            let _ = lifecycle.child.lock().await.wait().await;
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
    indexing_completion_source: &std::sync::Mutex<Option<IndexingCompletionSource>>,
    indexing_duration_secs: &std::sync::Mutex<Option<u64>>,
    indexing_progress_percent: &std::sync::Mutex<Option<u8>>,
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

            if let Ok(mut source) = indexing_completion_source.lock() {
                *source = Some(IndexingCompletionSource::Progress);
            }
            if let Ok(mut dur) = indexing_duration_secs.lock() {
                *dur = Some(duration);
            }
            if let Ok(mut progress) = indexing_progress_percent.lock() {
                *progress = None;
            }
        }
        ProgressAction::Report { percentage } => {
            if let Ok(mut progress) = indexing_progress_percent.lock() {
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
        let indexing_completion_source = std::sync::Mutex::new(None);
        let indexing_duration_secs = std::sync::Mutex::new(None);
        let indexing_progress_percent = std::sync::Mutex::new(Some(50));
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
            *indexing_completion_source.lock().unwrap(),
            Some(IndexingCompletionSource::Progress)
        );
        assert!(indexing_duration_secs.lock().unwrap().is_some());
        assert_eq!(*indexing_progress_percent.lock().unwrap(), None);
    }

    #[test]
    fn test_apply_progress_action_end_already_complete() {
        let indexing_complete = std::sync::atomic::AtomicBool::new(true);
        let indexing_completion_source =
            std::sync::Mutex::new(Some(IndexingCompletionSource::TimeoutFallback));
        let indexing_duration_secs = std::sync::Mutex::new(Some(100));
        let indexing_progress_percent = std::sync::Mutex::new(None);
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
            *indexing_completion_source.lock().unwrap(),
            Some(IndexingCompletionSource::TimeoutFallback)
        );
        assert_eq!(*indexing_duration_secs.lock().unwrap(), Some(100));
    }

    #[test]
    fn test_apply_progress_action_report() {
        let indexing_complete = std::sync::atomic::AtomicBool::new(false);
        let indexing_completion_source = std::sync::Mutex::new(None);
        let indexing_duration_secs = std::sync::Mutex::new(None);
        let indexing_progress_percent = std::sync::Mutex::new(None);
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
        assert_eq!(*indexing_progress_percent.lock().unwrap(), Some(75));
        assert!(indexing_duration_secs.lock().unwrap().is_none());
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
}
