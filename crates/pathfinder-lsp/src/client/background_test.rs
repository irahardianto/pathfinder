use super::*;
use serde_json::json;
use std::time::Duration;

fn make_test_language_state(
    transport: Arc<dyn crate::client::process::LspTransport>,
    reader_handle: tokio::task::JoinHandle<()>,
    reader_alive: Arc<std::sync::atomic::AtomicBool>,
    watcher_handles: Vec<tokio::task::JoinHandle<()>>,
) -> crate::client::LanguageState {
    let (readiness_tx, readiness_rx) = tokio::sync::watch::channel(
        crate::client::ReadinessState::Ready(crate::types::CapabilityReadiness::all()),
    );
    crate::client::LanguageState {
        transport,
        lifecycle: None,
        reader_handle,
        reader_alive,
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
        watcher_handles,
        probe_handle: None,
        readiness_tx: Arc::new(readiness_tx),
        readiness_rx,
        workspace_root: std::path::PathBuf::from("/tmp"),
    }
}

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
        ProcessEntry::Running(Box::new(make_test_language_state(
            fake_transport as Arc<dyn crate::client::process::LspTransport>,
            tokio::spawn(async {}),
            Arc::clone(&reader_alive),
            vec![],
        ))),
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
            !caps.flags.definition_provider,
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
async fn test_reader_supervisor_crash_cancels_and_clears_docs() {
    let processes = Arc::new(DashMap::new());
    let dispatcher = Arc::new(RequestDispatcher::new());
    let doc_versions = Arc::new(DashMap::new());

    doc_versions.insert(
        "file:///foo.rs".to_string(),
        crate::client::DocumentEntry {
            language_id: "rust".to_string(),
            version: std::sync::atomic::AtomicI32::new(1),
            content_hash: std::sync::atomic::AtomicU64::new(0),
        },
    );
    doc_versions.insert(
        "file:///bar.go".to_string(),
        crate::client::DocumentEntry {
            language_id: "go".to_string(),
            version: std::sync::atomic::AtomicI32::new(1),
            content_hash: std::sync::atomic::AtomicU64::new(0),
        },
    );

    let reader_handle = tokio::spawn(async {
        panic!("simulated reader crash");
    });
    let reader_alive = Arc::new(std::sync::atomic::AtomicBool::new(true));

    let fake_transport = Arc::new(crate::client::fake_transport::FakeTransport::new());

    processes.insert(
        "rust".to_owned(),
        ProcessEntry::Running(Box::new(make_test_language_state(
            fake_transport as Arc<dyn crate::client::process::LspTransport>,
            tokio::spawn(async {}),
            Arc::clone(&reader_alive),
            vec![],
        ))),
    );

    reader_supervisor_task(
        "rust".to_owned(),
        reader_handle,
        reader_alive,
        Arc::clone(&processes),
        dispatcher,
        Arc::clone(&doc_versions),
    )
    .await;

    assert!(!doc_versions.contains_key("file:///foo.rs"));
    assert!(doc_versions.contains_key("file:///bar.go"));

    let entry = processes.get("rust");
    assert!(entry.is_some());
    assert!(matches!(
        entry.unwrap().value(),
        ProcessEntry::Unavailable(_)
    ));
}

#[tokio::test]
async fn test_progress_watcher_warns_on_late_end() {
    let (tx, rx) = broadcast::channel::<serde_json::Value>(5);
    let indexing_complete = Arc::new(std::sync::atomic::AtomicBool::new(true));
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

    let msg = json!({
        "jsonrpc": "2.0",
        "method": "$/progress",
        "params": {
            "token": "indexing",
            "value": {
                "kind": "end"
            }
        }
    });
    tx.send(msg).unwrap();

    drop(tx);
    let _ = handle.await;
}

#[tokio::test]
async fn test_process_supervisor_task_shutdown() {
    let processes = Arc::new(DashMap::new());
    let dispatcher = Arc::new(RequestDispatcher::new());
    let doc_versions = Arc::new(DashMap::new());
    let (shutdown_tx, shutdown_rx) = broadcast::channel::<()>(1);

    let fake_transport = Arc::new(crate::client::fake_transport::FakeTransport::new());
    fake_transport.set_dispatcher(Arc::clone(&dispatcher));
    processes.insert(
        "rust".to_owned(),
        ProcessEntry::Running(Box::new(make_test_language_state(
            fake_transport as Arc<dyn crate::client::process::LspTransport>,
            tokio::spawn(async {}),
            Arc::new(std::sync::atomic::AtomicBool::new(true)),
            vec![],
        ))),
    );

    let handle = tokio::spawn(process_supervisor_task(
        Arc::clone(&processes),
        dispatcher,
        doc_versions,
        shutdown_rx,
    ));

    shutdown_tx.send(()).unwrap();
    let _ = tokio::time::timeout(Duration::from_millis(500), handle)
        .await
        .expect("task did not exit on shutdown signal");
    assert!(processes.is_empty());
}

#[tokio::test(start_paused = true)]
async fn test_process_supervisor_task_reaps_zombies() {
    let processes = Arc::new(DashMap::new());
    let dispatcher = Arc::new(RequestDispatcher::new());
    let doc_versions = Arc::new(DashMap::new());
    let (_shutdown_tx, shutdown_rx) = broadcast::channel::<()>(1);

    doc_versions.insert(
        "file:///foo.rs".to_string(),
        crate::client::DocumentEntry {
            language_id: "rust".to_string(),
            version: std::sync::atomic::AtomicI32::new(1),
            content_hash: std::sync::atomic::AtomicU64::new(0),
        },
    );

    let dead_transport = Arc::new(crate::client::fake_transport::FakeTransport::new());
    dead_transport.kill();

    processes.insert(
        "rust".to_owned(),
        ProcessEntry::Running(Box::new(make_test_language_state(
            dead_transport as Arc<dyn crate::client::process::LspTransport>,
            tokio::spawn(async {}),
            Arc::new(std::sync::atomic::AtomicBool::new(true)),
            vec![],
        ))),
    );

    let handle = tokio::spawn(process_supervisor_task(
        Arc::clone(&processes),
        dispatcher,
        Arc::clone(&doc_versions),
        shutdown_rx,
    ));

    tokio::time::sleep(Duration::from_secs(65)).await;

    let entry_rust = processes.get("rust");
    assert!(entry_rust.is_some());
    assert!(matches!(
        entry_rust.unwrap().value(),
        ProcessEntry::Unavailable(_)
    ));
    assert!(!doc_versions.contains_key("file:///foo.rs"));

    handle.abort();
}

use crate::client::process::LspTransport;
use crate::client::ProcessLifecycle;
use crate::client::UnavailableState;

#[tokio::test]
async fn test_progress_watcher_processes_normal_messages() {
    let (tx, rx) = broadcast::channel::<serde_json::Value>(5);
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

    // Send a report progress message
    let report_msg = json!({
        "jsonrpc": "2.0",
        "method": "$/progress",
        "params": {
            "token": "indexing",
            "value": {
                "kind": "report",
                "percentage": 50
            }
        }
    });
    tx.send(report_msg).unwrap();

    // Send an end progress message
    let end_msg = json!({
        "jsonrpc": "2.0",
        "method": "$/progress",
        "params": {
            "token": "indexing",
            "value": {
                "kind": "end"
            }
        }
    });
    tx.send(end_msg).unwrap();

    drop(tx);
    let _ = handle.await;

    // Verify it was marked as complete and duration is set
    assert!(indexing_complete.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(
        *indexing_source.lock(),
        Some(IndexingCompletionSource::Progress)
    );
}

#[tokio::test]
async fn test_progress_watcher_handles_lagged() {
    let (tx, rx) = broadcast::channel::<serde_json::Value>(1);
    let indexing_complete = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let indexing_source = Arc::new(parking_lot::Mutex::new(None));
    let indexing_duration = Arc::new(parking_lot::Mutex::new(None));
    let indexing_progress = Arc::new(parking_lot::Mutex::new(None));

    // Send two messages to a channel of capacity 1 to force lag
    let report_msg = json!({
        "jsonrpc": "2.0",
        "method": "$/progress",
        "params": {
            "token": "indexing",
            "value": {
                "kind": "report",
                "percentage": 50
            }
        }
    });
    tx.send(report_msg.clone()).unwrap();
    tx.send(report_msg).unwrap(); // This will overflow the queue

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
    let _ = handle.await;
}

#[tokio::test]
async fn test_registration_watcher_applies_capability_updates() {
    let dispatcher = Arc::new(RequestDispatcher::new());
    let rx = dispatcher.subscribe_server_requests_for_language("rust");
    let live_capabilities = Arc::new(parking_lot::RwLock::new(
        crate::client::DetectedCapabilities::default(),
    ));
    let transport = Arc::new(crate::client::fake_transport::FakeTransport::new());
    transport.set_response("", json!({}));

    let handle = tokio::spawn(registration_watcher_task(
        "rust".to_owned(),
        rx,
        Arc::clone(&live_capabilities),
        transport as Arc<dyn LspTransport>,
    ));

    let register_msg = json!({
        "jsonrpc": "2.0",
        "id": 123,
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

    // Give watcher time to process
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Verify capability was updated
    assert!(live_capabilities.read().flags.definition_provider);

    handle.abort();
}

#[tokio::test]
async fn test_registration_watcher_applies_unregistrations() {
    let dispatcher = Arc::new(RequestDispatcher::new());
    let rx = dispatcher.subscribe_server_requests_for_language("rust");
    let live_capabilities = Arc::new(parking_lot::RwLock::new(
        crate::client::DetectedCapabilities::default(),
    ));
    let transport = Arc::new(crate::client::fake_transport::FakeTransport::new());
    transport.set_response("", json!({}));

    // Pre-register capability
    {
        let mut caps = live_capabilities.write();
        caps.apply_registration("textDocument/definition", "reg-1", &json!({}));
        assert!(caps.flags.definition_provider);
    }

    let handle = tokio::spawn(registration_watcher_task(
        "rust".to_owned(),
        rx,
        Arc::clone(&live_capabilities),
        transport as Arc<dyn LspTransport>,
    ));

    let unregister_msg = json!({
        "jsonrpc": "2.0",
        "id": 124,
        "method": "client/unregisterCapability",
        "params": {
            "unregistrations": [{
                "id": "reg-1"
            }]
        }
    });
    dispatcher.dispatch_response_for_language("rust", &unregister_msg);

    // Give watcher time to process
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Verify capability was unregistered
    assert!(!live_capabilities.read().flags.definition_provider);

    handle.abort();
}

#[tokio::test]
async fn test_registration_watcher_handles_lagged() {
    let (tx, rx) = broadcast::channel::<serde_json::Value>(1);
    let live_capabilities = Arc::new(parking_lot::RwLock::new(
        crate::client::DetectedCapabilities::default(),
    ));
    let transport = Arc::new(crate::client::fake_transport::FakeTransport::new());

    // Send two messages to force lagged
    let msg = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "client/registerCapability",
        "params": {
            "registrations": []
        }
    });
    tx.send(msg.clone()).unwrap();
    tx.send(msg).unwrap();

    let handle = tokio::spawn(registration_watcher_task(
        "rust".to_owned(),
        rx,
        Arc::clone(&live_capabilities),
        transport as Arc<dyn LspTransport>,
    ));

    drop(tx);
    let _ = handle.await;
}

#[tokio::test]
async fn test_registration_watcher_empty_unregister_payload() {
    let dispatcher = Arc::new(RequestDispatcher::new());
    let rx = dispatcher.subscribe_server_requests_for_language("rust");
    let live_capabilities = Arc::new(parking_lot::RwLock::new(
        crate::client::DetectedCapabilities::default(),
    ));
    let transport = Arc::new(crate::client::fake_transport::FakeTransport::new());

    let handle = tokio::spawn(registration_watcher_task(
        "rust".to_owned(),
        rx,
        Arc::clone(&live_capabilities),
        transport as Arc<dyn LspTransport>,
    ));

    // Send unregisterCapability without unregistrations array (covers line 576)
    let unregister_msg = json!({
        "jsonrpc": "2.0",
        "id": 125,
        "method": "client/unregisterCapability",
        "params": {}
    });
    dispatcher.dispatch_response_for_language("rust", &unregister_msg);

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    handle.abort();
}

#[tokio::test]
async fn test_reader_supervisor_clears_doc_versions_with_zero_cleared() {
    let processes = Arc::new(DashMap::new());
    let dispatcher = Arc::new(RequestDispatcher::new());
    let doc_versions = Arc::new(DashMap::new());

    // Do NOT insert any document versions for "rust" to keep cleared = 0 (covers line 89 branch)
    doc_versions.insert(
        "file:///bar.go".to_string(),
        crate::client::DocumentEntry {
            language_id: "go".to_string(),
            version: std::sync::atomic::AtomicI32::new(1),
            content_hash: std::sync::atomic::AtomicU64::new(0),
        },
    );

    let reader_handle = tokio::spawn(async {
        panic!("simulated reader crash");
    });
    let reader_alive = Arc::new(std::sync::atomic::AtomicBool::new(true));

    let fake_transport = Arc::new(crate::client::fake_transport::FakeTransport::new());

    processes.insert(
        "rust".to_owned(),
        ProcessEntry::Running(Box::new(make_test_language_state(
            fake_transport as Arc<dyn crate::client::process::LspTransport>,
            tokio::spawn(async {}),
            Arc::clone(&reader_alive),
            vec![],
        ))),
    );

    reader_supervisor_task(
        "rust".to_owned(),
        reader_handle,
        reader_alive,
        Arc::clone(&processes),
        dispatcher,
        Arc::clone(&doc_versions),
    )
    .await;
}

#[tokio::test]
async fn test_reader_supervisor_raced_or_already_removed() {
    let processes = Arc::new(DashMap::new());
    let dispatcher = Arc::new(RequestDispatcher::new());
    let doc_versions = Arc::new(DashMap::new());

    let reader_handle = tokio::spawn(async {});
    let reader_alive = Arc::new(std::sync::atomic::AtomicBool::new(true));

    // Run supervisor when process is NOT in map (covers line 113)
    reader_supervisor_task(
        "rust".to_owned(),
        reader_handle,
        reader_alive,
        Arc::clone(&processes),
        dispatcher,
        Arc::clone(&doc_versions),
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn test_process_supervisor_task_reap_race_and_remove_if_false() {
    use crate::client::process::LspTransport;

    let processes = Arc::new(DashMap::new());
    let dispatcher = Arc::new(RequestDispatcher::new());
    let doc_versions = Arc::new(DashMap::new());
    let (shutdown_tx, shutdown_rx) = broadcast::channel::<()>(1);

    // We insert two dead processes: "rust" and "go".
    // "rust" has a lifecycle child process.
    let child_rust = tokio::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));

    let dead_transport_rust = Arc::new(crate::client::fake_transport::FakeTransport::new());
    dead_transport_rust.kill();

    let mut state_rust = make_test_language_state(
        dead_transport_rust as Arc<dyn LspTransport>,
        tokio::spawn(async {}),
        Arc::new(std::sync::atomic::AtomicBool::new(true)),
        vec![],
    );
    state_rust.lifecycle = Some(ProcessLifecycle {
        child: Arc::new(tokio::sync::Mutex::new(child_rust)),
    });

    processes.insert(
        "rust".to_owned(),
        ProcessEntry::Running(Box::new(state_rust)),
    );

    let dead_transport_go = Arc::new(crate::client::fake_transport::FakeTransport::new());
    dead_transport_go.kill();

    processes.insert(
        "go".to_owned(),
        ProcessEntry::Running(Box::new(make_test_language_state(
            dead_transport_go as Arc<dyn LspTransport>,
            tokio::spawn(async {}),
            Arc::new(std::sync::atomic::AtomicBool::new(true)),
            vec![],
        ))),
    );

    let processes_clone = Arc::clone(&processes);
    let handle = tokio::spawn(process_supervisor_task(
        processes_clone,
        dispatcher,
        doc_versions,
        shutdown_rx,
    ));

    let processes_for_race = Arc::clone(&processes);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(61)).await;
        processes_for_race.insert(
            "go".to_owned(),
            ProcessEntry::Unavailable(UnavailableState {
                unavailable_since: Instant::now(),
                backoff_attempt: 2,
                reason: None,
            }),
        );
    });

    tokio::time::sleep(Duration::from_secs(65)).await;

    let entry_rust = processes.get("rust");
    assert!(entry_rust.is_some());
    assert!(matches!(
        entry_rust.unwrap().value(),
        ProcessEntry::Unavailable(_)
    ));

    let entry_go = processes.get("go");
    assert!(entry_go.is_some());
    if let Some(entry) = entry_go {
        if let ProcessEntry::Unavailable(ref state) = entry.value() {
            assert_eq!(state.backoff_attempt, 2);
        } else {
            panic!("expected Unavailable for go");
        }
    }

    shutdown_tx.send(()).unwrap();
    let _ = handle.await;
}

#[tokio::test]
async fn test_reader_supervisor_clears_doc_versions_with_lifecycle() {
    let processes = Arc::new(DashMap::new());
    let dispatcher = Arc::new(RequestDispatcher::new());
    let doc_versions = Arc::new(DashMap::new());

    let child = tokio::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));

    let reader_handle = tokio::spawn(async {
        panic!("simulated reader crash");
    });
    let reader_alive = Arc::new(std::sync::atomic::AtomicBool::new(true));

    let fake_transport = Arc::new(crate::client::fake_transport::FakeTransport::new());

    let mut state = make_test_language_state(
        fake_transport as Arc<dyn crate::client::process::LspTransport>,
        tokio::spawn(async {}),
        Arc::clone(&reader_alive),
        vec![],
    );
    state.lifecycle = Some(ProcessLifecycle {
        child: Arc::new(tokio::sync::Mutex::new(child)),
    });

    processes.insert("rust".to_owned(), ProcessEntry::Running(Box::new(state)));

    reader_supervisor_task(
        "rust".to_owned(),
        reader_handle,
        reader_alive,
        Arc::clone(&processes),
        dispatcher,
        Arc::clone(&doc_versions),
    )
    .await;
}

#[tokio::test]
async fn test_watchers_aborted_on_crash() {
    let processes = Arc::new(DashMap::new());
    let dispatcher = Arc::new(RequestDispatcher::new());
    let doc_versions = Arc::new(DashMap::new());

    // 1. Create mock watcher tasks with oneshot channels to detect termination
    let (tx1, rx1) = tokio::sync::oneshot::channel::<()>();
    let progress_handle = tokio::spawn(async move {
        let _tx = tx1;
        std::future::pending::<()>().await;
    });

    let (tx2, rx2) = tokio::sync::oneshot::channel::<()>();
    let registration_handle = tokio::spawn(async move {
        let _tx = tx2;
        std::future::pending::<()>().await;
    });

    let reader_alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let reader_handle = tokio::spawn(async {
        panic!("simulated reader crash");
    });

    let fake_transport = Arc::new(crate::client::fake_transport::FakeTransport::new());

    processes.insert(
        "rust".to_owned(),
        ProcessEntry::Running(Box::new(make_test_language_state(
            fake_transport as Arc<dyn crate::client::process::LspTransport>,
            tokio::spawn(async {}),
            Arc::clone(&reader_alive),
            vec![progress_handle, registration_handle],
        ))),
    );

    // 2. Call the supervisor task
    reader_supervisor_task(
        "rust".to_owned(),
        reader_handle,
        reader_alive,
        Arc::clone(&processes),
        dispatcher,
        doc_versions,
    )
    .await;

    // 3. Verify that supervisor reaped the state and aborted the watchers
    let res_progress = rx1.await;
    let res_reg = rx2.await;

    assert!(
        res_progress.is_err(),
        "progress watcher must be aborted (sender dropped)"
    );
    assert!(
        res_reg.is_err(),
        "registration watcher must be aborted (sender dropped)"
    );
}
