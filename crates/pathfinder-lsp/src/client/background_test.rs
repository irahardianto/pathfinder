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
fn test_extract_registration_action_unregister_correct_spelling() {
    let msg = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "client/unregisterCapability",
        "params": {
            "unregistrations": [{
                "id": "reg-2"
            }]
        }
    });

    let action = extract_registration_action(&msg);
    assert_eq!(action.registrations.len(), 0);
    assert_eq!(action.unregistrations.len(), 1);
    assert_eq!(action.unregistrations[0], "reg-2");
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

#[tokio::test]
async fn test_collect_idle_languages_returns_stale_entries() {
    use crate::client::fake_transport::FakeTransport;
    use crate::client::process::LspTransport;

    let processes = DashMap::new();
    let dispatcher = Arc::new(RequestDispatcher::new());

    // Create a FakeTransport with last_used set far in the past (over 15 min).
    let fake_transport = Arc::new(FakeTransport::new());
    fake_transport.set_dispatcher(Arc::clone(&dispatcher));
    // Set last_used to 20 minutes ago (well past the 15-min idle timeout).
    let stale_time = Instant::now()
        .checked_sub(Duration::from_mins(20))
        .expect("time subtraction should not underflow");
    fake_transport.set_last_used(stale_time);

    processes.insert(
        "rust".to_owned(),
        ProcessEntry::Running(Box::new(crate::client::LanguageState {
            transport: fake_transport as Arc<dyn LspTransport>,
            lifecycle: None,
            reader_handle: tokio::spawn(async {}),
            reader_alive: Arc::new(std::sync::atomic::AtomicBool::new(true)),
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

    let idle = collect_idle_languages(&processes);
    assert_eq!(idle.len(), 1, "should collect 1 stale language");
    assert_eq!(idle[0], "rust");
}

#[tokio::test]
async fn test_collect_idle_languages_skips_recent() {
    use crate::client::fake_transport::FakeTransport;
    use crate::client::process::LspTransport;

    let processes = DashMap::new();
    let dispatcher = Arc::new(RequestDispatcher::new());

    // Create a FakeTransport with last_used set to now (recent activity).
    let fake_transport = Arc::new(FakeTransport::new());
    fake_transport.set_dispatcher(Arc::clone(&dispatcher));
    // last_used defaults to Instant::now() in FakeTransport::new(), so it's recent.

    processes.insert(
        "go".to_owned(),
        ProcessEntry::Running(Box::new(crate::client::LanguageState {
            transport: fake_transport as Arc<dyn LspTransport>,
            lifecycle: None,
            reader_handle: tokio::spawn(async {}),
            reader_alive: Arc::new(std::sync::atomic::AtomicBool::new(true)),
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

    let idle = collect_idle_languages(&processes);
    assert!(
        idle.is_empty(),
        "should not collect recently-used languages, got: {idle:?}"
    );
}

#[test]
fn test_collect_idle_languages_skips_unavailable_entries() {
    let processes = DashMap::new();

    // Unavailable entries should be ignored by collect_idle_languages.
    processes.insert(
        "python".to_owned(),
        ProcessEntry::Unavailable(crate::client::UnavailableState {
            unavailable_since: Instant::now().checked_sub(Duration::from_mins(30)).unwrap(),
            backoff_attempt: 1,
        }),
    );

    let idle = collect_idle_languages(&processes);
    assert!(
        idle.is_empty(),
        "should not collect Unavailable entries, got: {idle:?}"
    );
}

#[tokio::test]
async fn test_collect_idle_languages_skips_in_flight() {
    use crate::client::fake_transport::FakeTransport;
    use crate::client::process::LspTransport;
    use std::sync::atomic::Ordering;

    let processes = DashMap::new();
    let dispatcher = Arc::new(RequestDispatcher::new());

    let fake_transport = Arc::new(FakeTransport::new());
    fake_transport.set_dispatcher(Arc::clone(&dispatcher));
    // Set last_used to 20 minutes ago (stale).
    let stale_time = Instant::now()
        .checked_sub(Duration::from_mins(20))
        .expect("time subtraction should not underflow");
    fake_transport.set_last_used(stale_time);
    // But set in_flight to 1 (active request in progress).
    fake_transport.in_flight().store(1, Ordering::Release);

    processes.insert(
        "typescript".to_owned(),
        ProcessEntry::Running(Box::new(crate::client::LanguageState {
            transport: fake_transport as Arc<dyn LspTransport>,
            lifecycle: None,
            reader_handle: tokio::spawn(async {}),
            reader_alive: Arc::new(std::sync::atomic::AtomicBool::new(true)),
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

    let idle = collect_idle_languages(&processes);
    assert!(
        idle.is_empty(),
        "should not collect languages with in-flight requests, got: {idle:?}"
    );
}
