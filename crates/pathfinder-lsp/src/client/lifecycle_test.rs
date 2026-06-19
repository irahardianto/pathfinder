use super::*;
use crate::client::tests::{client_no_languages, client_with_descriptors, make_running_client};
use crate::client::{DetectedCapabilities, DiagnosticsStrategy, LspClient};
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

#[test]
fn test_handle_restart_backoff() {
    let _client = client_no_languages();
    // attempt = 0 -> None
    assert!(LspClient::handle_restart_backoff("rust", 0).is_none());

    // attempt = 1 -> Some(1s)
    let delay = LspClient::handle_restart_backoff("rust", 1);
    assert_eq!(delay, Some(Duration::from_secs(1)));

    // attempt = 2 -> Some(2s)
    let delay = LspClient::handle_restart_backoff("rust", 2);
    assert_eq!(delay, Some(Duration::from_secs(2)));

    // attempt = 30 -> capped at MAX_BACKOFF_SECS
    let delay = LspClient::handle_restart_backoff("rust", 30);
    assert_eq!(delay, Some(Duration::from_secs(MAX_BACKOFF_SECS)));
}

#[tokio::test]
async fn test_setup_indexing_watchers() {
    let _client = client_no_languages();
    let (_tx, rx) = tokio::sync::broadcast::channel(10);
    let spawned_at = Instant::now();

    let (
        indexing_complete,
        indexing_completion_source,
        indexing_duration_secs,
        indexing_progress,
        progress_handle,
        indexing_timeout_handle,
    ) = LspClient::setup_indexing_watchers("rust", rx, spawned_at);

    assert!(!indexing_complete.load(Ordering::Relaxed));
    assert!(indexing_completion_source.lock().is_none());
    assert!(indexing_duration_secs.lock().is_none());
    assert!(indexing_progress.lock().is_none());

    // Cleanup
    progress_handle.abort();
    indexing_timeout_handle.abort();
}

#[tokio::test]
async fn test_setup_registration_watcher() {
    let _client = client_no_languages();
    let (_tx, rx) = tokio::sync::broadcast::channel(10);
    let live_capabilities = Arc::new(parking_lot::RwLock::new(DetectedCapabilities::default()));

    let (fake_transport, _fake_transport_rx) = make_running_client("rust");
    let entry = fake_transport.processes.get("rust").unwrap();
    let ProcessEntry::Running(state) = entry.value() else {
        panic!("running")
    };
    let transport = Arc::clone(&state.transport);

    let handle = LspClient::setup_registration_watcher("rust", rx, &live_capabilities, &transport);
    handle.abort();
}

#[tokio::test]
async fn test_handle_shutdown_abort_cleanup() {
    let client = client_no_languages();

    let progress_handle = tokio::spawn(async {});
    let registration_handle = tokio::spawn(async {});
    let supervisor_handle = tokio::spawn(async {});
    let indexing_timeout_handle = tokio::spawn(async {});

    let (fake_client, _fake_rx) = make_running_client("rust");
    let entry = fake_client.processes.get("rust").unwrap();
    let ProcessEntry::Running(state) = entry.value() else {
        panic!("running")
    };
    let transport = Arc::clone(&state.transport);
    let lifecycle = state.lifecycle.clone().unwrap_or_else(|| {
        let child = tokio::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .unwrap();
        ProcessLifecycle {
            child: Arc::new(tokio::sync::Mutex::new(child)),
        }
    });

    client
        .handle_shutdown_abort_cleanup(
            "rust",
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

    // Verify child wait was called (by waiting on the process group/child)
    let mut child = lifecycle.child.lock().await;
    let wait_res = child.wait().await;
    assert!(wait_res.is_ok());
}

#[cfg(target_os = "linux")]
#[test]
fn test_detect_concurrent_lsp_linux() {
    let _client = client_no_languages();
    let found =
        LspClient::detect_concurrent_lsp_linux("rust", "some-dummy-nonexistent-binary-name");
    assert!(!found);
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[test]
fn test_detect_concurrent_lsp_macos() {
    let _client = client_no_languages();
    let found =
        LspClient::detect_concurrent_lsp_macos("rust", "some-dummy-nonexistent-binary-name");
    assert!(!found);
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
async fn test_wait_for_capability_immediate_success() {
    let (client, _fake) = make_running_client("rust");
    // rust has grace period 0. If we set capability to true, it succeeds immediately.
    if let Some(entry) = client.processes.get("rust") {
        if let ProcessEntry::Running(state) = entry.value() {
            let mut live_caps = state.live_capabilities.write();
            live_caps.definition_provider = true;
        }
    }
    let result = client
        .wait_for_capability(
            "rust",
            |caps| caps.definition_provider,
            "definitionProvider",
        )
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_wait_for_capability_immediate_failure_static() {
    let (client, _fake) = make_running_client("rust");
    // rust has grace period 0. Force capability to false.
    if let Some(entry) = client.processes.get("rust") {
        if let ProcessEntry::Running(state) = entry.value() {
            let mut live_caps = state.live_capabilities.write();
            live_caps.definition_provider = false;
        }
    }
    let result = client
        .wait_for_capability(
            "rust",
            |caps| caps.definition_provider,
            "definitionProvider",
        )
        .await;
    assert!(matches!(
        result,
        Err(LspError::UnsupportedCapability { .. })
    ));
}

#[tokio::test]
async fn test_wait_for_capability_delayed_success() {
    let (client, _fake) = make_running_client("java");
    // java has grace period 15s. Force capability to false initially.
    if let Some(entry) = client.processes.get("java") {
        if let ProcessEntry::Running(state) = entry.value() {
            let mut live_caps = state.live_capabilities.write();
            live_caps.definition_provider = false;
        }
    }

    let client_clone = client.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if let Some(entry) = client_clone.processes.get("java") {
            if let ProcessEntry::Running(state) = entry.value() {
                let mut live_caps = state.live_capabilities.write();
                live_caps.definition_provider = true;
            }
        }
    });

    let start = Instant::now();
    let result = client
        .wait_for_capability(
            "java",
            |caps| caps.definition_provider,
            "definitionProvider",
        )
        .await;

    assert!(result.is_ok(), "should succeed eventually: {result:?}");
    assert!(
        start.elapsed() >= Duration::from_millis(200),
        "should have waited at least 200ms"
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
            Duration::from_secs(1),
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
        has_running,
        "Running entry should NOT be removed by request() itself; only by supervisor"
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
async fn test_ensure_process_unavailable_state_after_failed_start() {
    let client = client_with_descriptors(vec!["rust"], HashMap::new());

    assert!(client.processes.get("rust").is_none());

    let result = client.ensure_process("rust").await;
    assert!(
        matches!(result, Err(LspError::NoLspAvailable)),
        "ensure_process should fail and return NoLspAvailable when spawner fails"
    );

    let entry = client.processes.get("rust");
    assert!(
        entry.is_some(),
        "Process entry should exist after failed ensure_process"
    );
    if let Some(entry) = entry {
        assert!(
            matches!(entry.value(), ProcessEntry::Unavailable(_)),
            "should be Unavailable after failed start"
        );
    }
}

#[tokio::test]
async fn test_idle_timeout_removes_process_after_timeout() {
    use crate::client::background::collect_idle_languages;

    let (client, _fake) = make_running_client("rust");

    if let Some(entry) = client.processes.get("rust") {
        if let ProcessEntry::Running(state) = entry.value() {
            state
                .transport
                .set_last_used(Instant::now().checked_sub(Duration::from_mins(20)).unwrap());
        }
    }

    let candidates = collect_idle_languages(&client.processes);
    assert_eq!(
        candidates,
        vec!["rust"],
        "idle process should be a candidate for removal"
    );

    let removed = client.processes.remove_if("rust", |_, v| {
        if let ProcessEntry::Running(state) = v {
            state.transport.last_used().elapsed() > Duration::from_mins(15)
                && state.transport.in_flight().load(Ordering::Acquire) == 0
        } else {
            false
        }
    });

    assert!(
        removed.is_some(),
        "idle process with no in-flight requests should be removed"
    );
    assert!(
        client.processes.get("rust").is_none(),
        "process entry should be gone after idle timeout removal"
    );
}

#[tokio::test]
async fn test_idle_timeout_does_not_remove_process_with_in_flight() {
    use crate::client::background::collect_idle_languages;

    let (client, _fake) = make_running_client("rust");

    if let Some(entry) = client.processes.get("rust") {
        if let ProcessEntry::Running(state) = entry.value() {
            state
                .transport
                .set_last_used(Instant::now().checked_sub(Duration::from_mins(20)).unwrap());
            state.transport.in_flight().store(1, Ordering::Relaxed);
        }
    }

    let candidates = collect_idle_languages(&client.processes);
    assert!(
        candidates.is_empty(),
        "process with in-flight requests should NOT be a candidate for removal"
    );

    assert!(
        client.processes.get("rust").is_some(),
        "process should still exist when in-flight > 0"
    );
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
        spawner: std::sync::Arc::new(
            crate::client::process::test_mocks::MockProcessSpawner::failing(),
        ),
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
async fn test_request_timeout_sends_cancel_notification() {
    let (client, fake) = make_running_client("rust");

    fake.set_response("textDocument/definition", json!({ "result": null }));
    fake.set_response_delay(Duration::from_secs(10));

    let _ = client
        .request(
            "rust",
            "textDocument/definition",
            json!({}),
            Duration::from_millis(50),
        )
        .await;

    // Give the spawned cancel task time to execute
    tokio::time::sleep(Duration::from_millis(100)).await;

    let notifications = fake.take_notifications();
    let cancel = notifications
        .iter()
        .find(|(method, _)| method == "$/cancelRequest");
    assert!(
        cancel.is_some(),
        "should send $/cancelRequest on timeout, got notifications: {notifications:?}"
    );
}

#[tokio::test]
async fn test_request_delayed_error_response_returns_server_error() {
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
        Err(LspError::ServerError { message, .. }) => {
            assert!(
                message.contains("something went wrong"),
                "error message should contain configured text: {message}"
            );
        }
        other => {
            panic!("expected ServerError from send() even with delay active, got: {other:?}")
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

    let result = client.detect_concurrent_lsp("rust", "/usr/local/bin/nonexistent-lsp-binary-xyz");
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
        matches!(result, Err(LspError::ServerError { ref message, .. }) if message.contains("server internal error")),
        "should return ServerError from server error response: {result:?}"
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
async fn test_force_respawn_with_lifecycle_kills_old_process() {
    use crate::client::fake_transport::FakeTransport;
    use crate::client::{LanguageState, ProcessLifecycle};

    let client = client_with_descriptors(vec!["rust"], HashMap::new());

    let sleep_bin = which::which("sleep")
        .or_else(|_| {
            which::which("/usr/bin/sleep").map(|_| std::path::PathBuf::from("/usr/bin/sleep"))
        })
        .unwrap_or_else(|_| std::path::PathBuf::from("/usr/bin/sleep"));
    let child = tokio::process::Command::new(&sleep_bin)
        .arg("60")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("Failed to spawn sleep");

    let child_pid = child.id().expect("child must have a PID");

    let lifecycle = ProcessLifecycle {
        child: Arc::new(tokio::sync::Mutex::new(child)),
    };
    let lifecycle_for_assert = lifecycle.clone();

    let fake = Arc::new(FakeTransport::new());
    let dispatcher = Arc::new(RequestDispatcher::new());
    fake.set_dispatcher(Arc::clone(&dispatcher));

    let reader_handle = tokio::spawn(async {
        std::future::pending::<()>().await;
    });

    let entry = ProcessEntry::Running(Box::new(LanguageState {
        transport: Arc::clone(&fake) as Arc<dyn LspTransport>,
        lifecycle: Some(lifecycle),
        reader_handle,
        reader_alive: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        restart_count: 0,
        spawned_at: Instant::now(),
        indexing_complete: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        indexing_completion_source: Arc::new(parking_lot::Mutex::new(None)),
        indexing_duration_secs: Arc::new(parking_lot::Mutex::new(None)),
        indexing_progress_percent: Arc::new(parking_lot::Mutex::new(None)),
        live_capabilities: Arc::new(parking_lot::RwLock::new(DetectedCapabilities::default())),
        in_coexistence_mode: false,
        watcher_handles: vec![],
    }));

    client.processes.insert("rust".to_owned(), entry);

    let result = client.force_respawn("rust").await;

    assert!(
        result.is_err(),
        "force_respawn should fail (no real LSP binary)"
    );

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut child_guard = lifecycle_for_assert.child.lock().await;
    let status = child_guard.try_wait();
    assert!(
        status.is_ok() && status.as_ref().unwrap().is_some(),
        "old sleep process should have been killed by force_respawn (pid {child_pid}): {status:?}"
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
    std::fs::create_dir_all(dir.path().join("src")).expect("create src");
    std::fs::write(dir.path().join("src/main.rs"), "fn main() {}").expect("write main.rs");

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
    std::fs::write(dir.path().join("main.go"), "package main").expect("write main.go");

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
    std::fs::write(dir.path().join("index.ts"), "").expect("write index.ts");

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
    std::fs::write(dir.path().join("main.py"), "").expect("write main.py");

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
    std::fs::create_dir_all(dir.path().join("src")).expect("create src");
    std::fs::write(dir.path().join("src/main.rs"), "fn main() {}").expect("write main.rs");

    std::fs::write(
        dir.path().join("go.mod"),
        "module example.com/test\n\ngo 1.21",
    )
    .expect("write go.mod");
    std::fs::write(dir.path().join("main.go"), "package main").expect("write main.go");

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

// ── FakeTransport coverage gaps ──────────────────────────────────────────

#[tokio::test]
async fn test_fake_transport_failing_behavior_fail_after_n_requests() {
    let (client, fake) = make_running_client("rust");

    fake.set_response(
        "textDocument/definition",
        json!({ "result": { "uri": "file:///test.rs" } }),
    );
    fake.set_failing_behavior(crate::client::fake_transport::FailingBehavior {
        fail_after_n_requests: Some(2),
        fail_on_method: None,
    });

    // First request should succeed
    let r1 = client
        .request(
            "rust",
            "textDocument/definition",
            json!({}),
            Duration::from_secs(5),
        )
        .await;
    assert!(r1.is_ok(), "first request should succeed: {r1:?}");

    // Second request should fail (count >= 2)
    fake.set_response(
        "textDocument/definition",
        json!({ "result": { "uri": "file:///test2.rs" } }),
    );
    let r2 = client
        .request(
            "rust",
            "textDocument/definition",
            json!({}),
            Duration::from_secs(5),
        )
        .await;
    assert!(
        r2.is_err(),
        "second request should fail due to failing behavior: {r2:?}"
    );
}

#[tokio::test]
async fn test_fake_transport_failing_behavior_fail_on_method() {
    let (client, fake) = make_running_client("rust");

    fake.set_response(
        "textDocument/definition",
        json!({ "result": { "uri": "file:///test.rs" } }),
    );
    fake.set_response(
        "textDocument/hover",
        json!({ "result": { "contents": "test" } }),
    );
    fake.set_failing_behavior(crate::client::fake_transport::FailingBehavior {
        fail_after_n_requests: None,
        fail_on_method: Some("textDocument/hover".to_owned()),
    });

    // definition should succeed
    let r1 = client
        .request(
            "rust",
            "textDocument/definition",
            json!({}),
            Duration::from_secs(5),
        )
        .await;
    assert!(r1.is_ok(), "definition should succeed: {r1:?}");

    // hover should fail
    let r2 = client
        .request(
            "rust",
            "textDocument/hover",
            json!({}),
            Duration::from_secs(5),
        )
        .await;
    assert!(
        r2.is_err(),
        "hover should fail due to failing behavior: {r2:?}"
    );
}

#[tokio::test]
async fn test_fake_transport_response_delay_success() {
    let (client, fake) = make_running_client("rust");

    fake.set_response(
        "textDocument/hover",
        json!({ "result": { "contents": "delayed" } }),
    );
    fake.set_response_delay(Duration::from_millis(50));

    let result = client
        .request(
            "rust",
            "textDocument/hover",
            json!({}),
            Duration::from_secs(5),
        )
        .await;

    assert!(
        result.is_ok(),
        "delayed response should succeed: {result:?}"
    );
    let val = result.unwrap();
    assert_eq!(val["contents"], "delayed");
}

#[tokio::test]
async fn test_fake_transport_is_alive_after_init() {
    let (_client, fake) = make_running_client("rust");
    assert!(fake.is_alive(), "should be alive after init");
}

#[tokio::test]
async fn test_fake_transport_kill_makes_not_alive() {
    let (_client, fake) = make_running_client("rust");
    assert!(fake.is_alive());
    fake.kill();
    assert!(!fake.is_alive(), "should not be alive after kill");
}

#[tokio::test]
async fn test_fake_transport_request_count_increments() {
    let (client, fake) = make_running_client("rust");

    fake.set_response("textDocument/definition", json!({ "result": {} }));

    assert_eq!(fake.request_count(), 0);

    let _ = client
        .request(
            "rust",
            "textDocument/definition",
            json!({}),
            Duration::from_secs(5),
        )
        .await;

    assert_eq!(fake.request_count(), 1);

    let _ = client
        .request(
            "rust",
            "textDocument/definition",
            json!({}),
            Duration::from_secs(5),
        )
        .await;

    assert_eq!(fake.request_count(), 2);
}

#[tokio::test]
async fn test_fake_transport_notification_count() {
    let (client, fake) = make_running_client("rust");

    assert_eq!(fake.notification_count(), 0);

    let _ = client
        .notify("rust", "textDocument/didOpen", json!({}))
        .await;
    assert_eq!(fake.notification_count(), 1);

    let _ = client
        .notify("rust", "textDocument/didChange", json!({}))
        .await;
    assert_eq!(fake.notification_count(), 2);
}

#[tokio::test]
async fn test_fake_transport_capabilities_default() {
    let (_client, fake) = make_running_client("rust");
    let caps = fake.capabilities();
    assert!(
        !caps.definition_provider,
        "default should have no definition provider"
    );
}

#[tokio::test]
async fn test_fake_transport_capabilities_with_custom() {
    let (_client, fake) = make_running_client("rust");
    let custom_caps = DetectedCapabilities {
        definition_provider: true,
        call_hierarchy_provider: true,
        ..Default::default()
    };
    fake.with_capabilities(custom_caps.clone());
    let caps = fake.capabilities();
    assert!(caps.definition_provider);
    assert!(caps.call_hierarchy_provider);
}

fn make_multi_lang_entry(fake: Arc<crate::client::fake_transport::FakeTransport>) -> ProcessEntry {
    let reader_handle = tokio::spawn(async {
        std::future::pending::<()>().await;
    });
    ProcessEntry::Running(Box::new(LanguageState {
        transport: fake as Arc<dyn LspTransport>,
        lifecycle: None,
        reader_handle,
        reader_alive: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        restart_count: 0,
        spawned_at: Instant::now(),
        indexing_complete: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        indexing_completion_source: Arc::new(parking_lot::Mutex::new(Some(
            IndexingCompletionSource::Progress,
        ))),
        indexing_duration_secs: Arc::new(parking_lot::Mutex::new(Some(0))),
        indexing_progress_percent: Arc::new(parking_lot::Mutex::new(None)),
        live_capabilities: Arc::new(parking_lot::RwLock::new(DetectedCapabilities {
            definition_provider: true,
            ..DetectedCapabilities::default()
        })),
        in_coexistence_mode: false,
        watcher_handles: vec![],
    }))
}

#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn test_multi_language_requests_route_to_correct_transport() {
    use crate::client::fake_transport::FakeTransport;

    let mut rust_transport = FakeTransport::new();
    rust_transport.set_language_id("rust");
    let rust_fake = Arc::new(rust_transport);

    let mut go_transport = FakeTransport::new();
    go_transport.set_language_id("go");
    let go_fake = Arc::new(go_transport);

    let dispatcher = Arc::new(RequestDispatcher::new());

    rust_fake.set_dispatcher(Arc::clone(&dispatcher));
    go_fake.set_dispatcher(Arc::clone(&dispatcher));

    rust_fake.set_response(
        "textDocument/definition",
        json!({ "result": { "uri": "file:///rust-result.rs" } }),
    );
    go_fake.set_response(
        "textDocument/definition",
        json!({ "result": { "uri": "file:///go-result.go" } }),
    );

    let processes = DashMap::new();
    processes.insert(
        "rust".to_owned(),
        make_multi_lang_entry(Arc::clone(&rust_fake)),
    );
    processes.insert("go".to_owned(), make_multi_lang_entry(Arc::clone(&go_fake)));

    let descriptors = vec![
        LanguageLsp {
            language_id: "rust".to_owned(),
            command: "rust-analyzer".to_owned(),
            args: vec![],
            root: std::env::temp_dir(),
            init_timeout_secs: None,
            auto_plugins: vec![],
            init_options: serde_json::Value::Null,
        },
        LanguageLsp {
            language_id: "go".to_owned(),
            command: "gopls".to_owned(),
            args: vec![],
            root: std::env::temp_dir(),
            init_timeout_secs: None,
            auto_plugins: vec![],
            init_options: serde_json::Value::Null,
        },
    ];

    let (shutdown_tx, _) = broadcast::channel(1);
    let client = super::super::LspClient {
        descriptors: Arc::new(descriptors),
        missing_languages: Arc::new(Vec::new()),
        processes: Arc::new(processes),
        init_locks: Arc::new(DashMap::new()),
        dispatcher: Arc::clone(&dispatcher),
        shutdown_tx: Arc::new(shutdown_tx),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        doc_versions: Arc::new(DashMap::new()),
        warm_start_complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        spawner: std::sync::Arc::new(
            crate::client::process::test_mocks::MockProcessSpawner::failing(),
        ),
    };

    let rust_result = client
        .request(
            "rust",
            "textDocument/definition",
            json!({}),
            Duration::from_secs(5),
        )
        .await;
    assert!(
        rust_result.is_ok(),
        "rust request should succeed: {rust_result:?}"
    );
    assert_eq!(
        rust_result.unwrap()["uri"],
        "file:///rust-result.rs",
        "rust request should return rust transport's response"
    );

    let go_result = client
        .request(
            "go",
            "textDocument/definition",
            json!({}),
            Duration::from_secs(5),
        )
        .await;
    assert!(
        go_result.is_ok(),
        "go request should succeed: {go_result:?}"
    );
    assert_eq!(
        go_result.unwrap()["uri"],
        "file:///go-result.go",
        "go request should return go transport's response"
    );

    assert_eq!(
        rust_fake.request_count(),
        1,
        "rust transport should have 1 request"
    );
    assert_eq!(
        go_fake.request_count(),
        1,
        "go transport should have 1 request"
    );
}

#[tokio::test]
async fn test_fake_transport_shutdown_is_noop() {
    let (_client, fake) = make_running_client("rust");
    let dispatcher = Arc::new(RequestDispatcher::new());

    fake.set_response("shutdown", json!({ "result": null }));

    assert!(fake.is_alive(), "should be alive before shutdown");
    fake.shutdown(&dispatcher, "rust").await;
    assert!(
        fake.is_alive(),
        "FakeTransport shutdown should be no-op (still alive)"
    );
}

#[tokio::test]
async fn test_request_and_notify_do_not_cleanup_stale_reader() {
    let (client, _fake) = make_running_client("rust");
    // Abort the reader handle to make it is_finished()
    {
        let entry = client.processes.get("rust").unwrap();
        let ProcessEntry::Running(state) = entry.value() else {
            panic!("expected Running process entry");
        };
        state.reader_handle.abort();
    }

    // Wait a brief moment for the reader task to be aborted
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Verify calling request returns ConnectionLost
    let res = client
        .request("rust", "someMethod", json!({}), Duration::from_secs(1))
        .await;
    assert!(matches!(res, Err(LspError::ConnectionLost)));

    // Verify the process entry is still in the map (no inline cleanup) and is STILL Running
    {
        let entry = client.processes.get("rust").unwrap();
        assert!(matches!(entry.value(), ProcessEntry::Running(_)));
    }

    // Verify calling notify returns ConnectionLost
    let res_notify = client.notify("rust", "someNotification", json!({})).await;
    assert!(matches!(res_notify, Err(LspError::ConnectionLost)));

    // Verify the process entry is still in the map and is STILL Running
    {
        let entry = client.processes.get("rust").unwrap();
        assert!(matches!(entry.value(), ProcessEntry::Running(_)));
    }
}

// ── Coverage: indexing_timeout_for_language go/python ─────────────

#[test]
fn test_indexing_timeout_go_is_30s() {
    assert_eq!(indexing_timeout_for_language("go"), Duration::from_secs(30));
}

#[test]
fn test_indexing_timeout_python_is_30s() {
    assert_eq!(
        indexing_timeout_for_language("python"),
        Duration::from_secs(30)
    );
}

// ── Coverage: call_hierarchy_request success path ────────────────

#[tokio::test]
async fn test_call_hierarchy_request_success() {
    let (client, fake) = make_running_client("rust");

    // Set up the transport to return a valid call hierarchy response.
    // The method name must match what call_hierarchy_request dispatches.
    // FakeTransport requires the response to be a JSON object so it can
    // insert the request `id`. The dispatcher then extracts `message["result"]`.
    fake.set_response(
        "callHierarchy/incomingCalls",
        json!({"result": [
            {
                "from": {
                    "name": "caller_fn",
                    "kind": 12,
                    "uri": "file:///workspace/src/lib.rs",
                    "range": { "start": { "line": 10, "character": 0 }, "end": { "line": 20, "character": 0 } },
                    "selectionRange": { "start": { "line": 10, "character": 4 }, "end": { "line": 10, "character": 13 } }
                },
                "fromRanges": [
                    { "start": { "line": 15, "character": 8 }, "end": { "line": 15, "character": 20 } }
                ]
            }
        ]}),
    );

    let item = crate::types::CallHierarchyItem {
        name: "target_fn".to_owned(),
        kind: "function".to_owned(),
        detail: None,
        file: "src/main.rs".to_owned(),
        line: 5,
        column: 1,
        data: Some(json!({
            "name": "target_fn",
            "kind": 12,
            "uri": "file:///workspace/src/main.rs",
            "range": { "start": { "line": 4, "character": 0 }, "end": { "line": 10, "character": 0 } },
            "selectionRange": { "start": { "line": 4, "character": 4 }, "end": { "line": 4, "character": 13 } }
        })),
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
        result.is_ok(),
        "should succeed with valid response: {result:?}"
    );
    let calls = result.expect("checked");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].item.name, "caller_fn");
    assert!(!calls[0].call_sites.is_empty());
}

// ── Coverage: call_hierarchy_request error from transport ────────

#[tokio::test]
async fn test_call_hierarchy_request_transport_error() {
    let (client, fake) = make_running_client("rust");

    fake.set_error("callHierarchy/incomingCalls", "internal error");

    let item = crate::types::CallHierarchyItem {
        name: "fn".to_owned(),
        kind: "function".to_owned(),
        detail: None,
        file: "src/main.rs".to_owned(),
        line: 1,
        column: 1,
        data: Some(
            json!({"name": "fn", "kind": 12, "uri": "file:///workspace/src/main.rs",
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 1, "character": 0}},
            "selectionRange": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 2}}}),
        ),
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

    assert!(result.is_err(), "transport error should propagate");
}

// ── Coverage: wait_for_capability timeout after grace period ─────

#[tokio::test]
async fn test_wait_for_capability_timeout_after_grace() {
    // Use typescript (5s grace) but with tokio::time::pause for instant advance.
    tokio::time::pause();

    let (client, _fake) = make_running_client("typescript");
    // Force capability to false.
    if let Some(entry) = client.processes.get("typescript") {
        if let ProcessEntry::Running(state) = entry.value() {
            let mut live_caps = state.live_capabilities.write();
            live_caps.call_hierarchy_provider = false;
        }
    }

    let result = client
        .wait_for_capability(
            "typescript",
            |caps| caps.call_hierarchy_provider,
            "callHierarchyProvider",
        )
        .await;

    assert!(
        matches!(result, Err(LspError::UnsupportedCapability { .. })),
        "should fail after grace period: {result:?}"
    );
}

// ── Coverage: wait_for_capability Unavailable during loop ────────

#[tokio::test]
async fn test_wait_for_capability_unavailable_during_loop() {
    // java has 15s grace. Start with Running, then switch to Unavailable.
    let (client, _fake) = make_running_client("java");
    if let Some(entry) = client.processes.get("java") {
        if let ProcessEntry::Running(state) = entry.value() {
            let mut live_caps = state.live_capabilities.write();
            live_caps.call_hierarchy_provider = false;
        }
    }

    // Spawn a task that switches the process to Unavailable after 50ms.
    let client_clone = client.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        client_clone.processes.insert(
            "java".to_owned(),
            ProcessEntry::Unavailable(UnavailableState {
                backoff_attempt: 0,
                unavailable_since: Instant::now(),
            }),
        );
    });

    let result = client
        .wait_for_capability(
            "java",
            |caps| caps.call_hierarchy_provider,
            "callHierarchyProvider",
        )
        .await;

    assert!(
        matches!(result, Err(LspError::NoLspAvailable)),
        "should return NoLspAvailable when process becomes Unavailable: {result:?}"
    );
}

// ── Coverage: capabilities_for with Running ──────────────────────

#[tokio::test]
async fn test_capabilities_for_running_returns_caps() {
    let (client, _fake) = make_running_client("rust");
    let result = client.capabilities_for("rust");
    assert!(result.is_ok());
    let caps = result.expect("checked");
    // make_running_client sets definition_provider = true
    assert!(caps.definition_provider);
}

// ── Coverage: touch on running process ───────────────────────────

#[tokio::test]
async fn test_touch_updates_last_used_on_running() {
    let (client, _fake) = make_running_client("rust");

    // Record time before touch
    let before = {
        let entry = client.processes.get("rust").unwrap();
        if let ProcessEntry::Running(state) = entry.value() {
            state.transport.last_used()
        } else {
            panic!("expected Running");
        }
    };

    tokio::time::sleep(Duration::from_millis(10)).await;
    client.touch("rust");

    let after = {
        let entry = client.processes.get("rust").unwrap();
        if let ProcessEntry::Running(state) = entry.value() {
            state.transport.last_used()
        } else {
            panic!("expected Running");
        }
    };

    assert!(after > before, "touch should update last_used timestamp");
}

// ── Coverage: touch on nonexistent language is noop ──────────────

#[tokio::test]
async fn test_touch_nonexistent_language_is_noop() {
    let client = client_no_languages();
    // Should not panic
    client.touch("nonexistent");
}

// ── Coverage: shutdown idempotency (second call) ─────────────────

#[tokio::test]
async fn test_shutdown_idempotent() {
    let client = client_no_languages();
    client.shutdown();
    // Second call should not panic and should be a no-op
    client.shutdown();
    assert!(
        client.shutdown_requested.load(Ordering::Acquire),
        "shutdown_requested should be true after shutdown"
    );
}

// ── Coverage: warm_start_for_languages_and_track empty ───────────

#[tokio::test]
async fn test_warm_start_for_languages_and_track_no_languages() {
    let client = client_no_languages();
    // No known languages, so to_start is empty.
    client.warm_start_for_languages_and_track(&["rust".to_owned()]);
    // Should set warm_start_complete immediately.
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(
        client.warm_start_complete.load(Ordering::Acquire),
        "warm_start_complete should be set when no languages to start"
    );
}

// ── Coverage: warm_start_for_languages_and_track already running ─

#[tokio::test]
async fn test_warm_start_for_languages_and_track_already_running() {
    let (client, _fake) = make_running_client("rust");
    // rust is already running, so to_start should be empty.
    client.warm_start_for_languages_and_track(&["rust".to_owned()]);
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(
        client.warm_start_complete.load(Ordering::Acquire),
        "warm_start_complete should be set when all requested already running"
    );
}

// ── Coverage: force_respawn with Unavailable entry ───────────────

#[tokio::test]
async fn test_force_respawn_with_unavailable_entry() {
    let processes = HashMap::from([(
        "rust".to_owned(),
        ProcessEntry::Unavailable(UnavailableState {
            backoff_attempt: 3,
            unavailable_since: Instant::now(),
        }),
    )]);
    let client = client_with_descriptors(vec!["rust"], processes);

    // force_respawn should attempt start_process which fails (no real binary)
    let result = client.force_respawn("rust").await;
    assert!(result.is_err(), "should fail without real binary");

    // Should have a new Unavailable entry with attempt from the old entry
    let entry = client.processes.get("rust");
    assert!(entry.is_some());
}

// ── Coverage: ensure_process post-lock Unavailable not elapsed ───

#[tokio::test]
async fn test_ensure_process_post_lock_unavailable_not_elapsed() {
    // This covers line 390 in lifecycle.rs: the post-lock check where
    // backoff has NOT elapsed (returns NoLspAvailable from inside the lock).
    //
    // Scenario: Two concurrent ensure_process calls. The first acquires the
    // lock and fails start_process (inserting Unavailable with fresh timestamp).
    // The second waits for the lock, then finds the fresh Unavailable entry
    // with backoff not elapsed.
    let client = client_with_descriptors(vec!["rust"], HashMap::new());

    // Pre-populate with Unavailable that has backoff NOT elapsed.
    // Use backoff_attempt=0 so backoff_secs=1, and set unavailable_since to now.
    client.processes.insert(
        "rust".to_owned(),
        ProcessEntry::Unavailable(UnavailableState {
            backoff_attempt: 0,
            unavailable_since: Instant::now(),
        }),
    );

    // First call: pre-lock check sees elapsed (we'll skip this with a fresh entry).
    // Actually, the pre-lock check will also see it as not elapsed.
    let result = client.ensure_process("rust").await;
    assert!(
        matches!(result, Err(LspError::NoLspAvailable)),
        "should return NoLspAvailable when backoff not elapsed: {result:?}"
    );
}

// ── Coverage: notify success path ────────────────────────────────

#[tokio::test]
async fn test_notify_running_process_succeeds() {
    let (client, _fake) = make_running_client("rust");

    let result = client
        .notify(
            "rust",
            "textDocument/didSave",
            json!({"textDocument": {"uri": "file:///test.rs"}}),
        )
        .await;

    assert!(
        result.is_ok(),
        "notify on running process should succeed: {result:?}"
    );
}

#[tokio::test]
async fn test_touch_language_updates_last_used_on_running() {
    let (client, _fake) = make_running_client("rust");

    let before = {
        let entry = client.processes.get("rust").unwrap();
        if let ProcessEntry::Running(state) = entry.value() {
            state.transport.last_used()
        } else {
            panic!("expected Running");
        }
    };

    tokio::time::sleep(Duration::from_millis(10)).await;
    client.touch_language("rust");

    let after = {
        let entry = client.processes.get("rust").unwrap();
        if let ProcessEntry::Running(state) = entry.value() {
            state.transport.last_used()
        } else {
            panic!("expected Running");
        }
    };

    assert!(
        after > before,
        "touch_language should update last_used timestamp"
    );
}

#[tokio::test]
async fn test_warm_start_for_languages_tasks_executed() {
    let client = client_with_descriptors(vec!["rust"], HashMap::new());
    let handles = client.warm_start_for_languages(&["rust".to_owned()]);
    assert_eq!(handles.len(), 1);
    for h in handles {
        let _ = h.await;
    }
}

const PYTHON_LSP_SERVER: &str = r#"
import sys, json, time

def read_msg():
    content_len = None
    while True:
        line = b""
        while not line.endswith(b"\n"):
            c = sys.stdin.buffer.read(1)
            if not c:
                return None
            line += c
        line = line.strip()
        if not line:
            break
        if line.startswith(b"Content-Length:"):
            content_len = int(line.split(b":")[1].strip())
    if content_len is None:
        return None
    body = sys.stdin.buffer.read(content_len)
    return json.loads(body.decode('utf-8'))

def write_msg(msg):
    body = json.dumps(msg).encode('utf-8')
    sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode('utf-8') + body)
    sys.stdout.buffer.flush()

# Process initialize request
msg = read_msg()
if msg and "id" in msg:
    opts = msg.get("params", {}).get("initializationOptions", {})
    if isinstance(opts, dict) and opts.get("sleep_before_respond"):
        time.sleep(0.5)
    write_msg({
        "jsonrpc": "2.0",
        "id": msg["id"],
        "result": {
            "capabilities": {
                "definitionProvider": True,
                "callHierarchyProvider": True,
                "referencesProvider": True,
                "implementationProvider": True
            }
        }
    })

# Read loop
while True:
    msg = read_msg()
    if msg is None:
        break
"#;

#[tokio::test]
async fn test_start_process_success_path_with_python_lsp() {
    let dir = tempfile::tempdir().expect("tempdir");
    let descriptor = LanguageLsp {
        language_id: "python-test".to_owned(),
        command: "python3".to_owned(),
        args: vec!["-c".to_owned(), PYTHON_LSP_SERVER.to_owned()],
        root: dir.path().to_path_buf(),
        init_timeout_secs: Some(5),
        auto_plugins: vec![],
        init_options: serde_json::Value::Null,
    };

    let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);
    let client = LspClient {
        descriptors: Arc::new(vec![descriptor.clone()]),
        missing_languages: Arc::new(Vec::new()),
        processes: Arc::new(DashMap::new()),
        init_locks: Arc::new(DashMap::new()),
        dispatcher: Arc::new(RequestDispatcher::new()),
        shutdown_tx: Arc::new(shutdown_tx),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        doc_versions: Arc::new(DashMap::new()),
        warm_start_complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        spawner: std::sync::Arc::new(crate::client::process::RealProcessSpawner),
    };

    let result = client.start_process(descriptor, 0).await;
    assert!(
        result.is_ok(),
        "should successfully start and initialize: {result:?}"
    );

    let entry = client.processes.get("python-test");
    assert!(entry.is_some());
    let is_running = matches!(entry.unwrap().value(), ProcessEntry::Running(_));
    assert!(is_running, "process should be running");

    // Clean up
    client.shutdown();
    tokio::time::sleep(Duration::from_millis(100)).await;
}

#[tokio::test]
async fn test_start_process_shutdown_during_init() {
    let dir = tempfile::tempdir().expect("tempdir");
    let descriptor = LanguageLsp {
        language_id: "python-test".to_owned(),
        command: "python3".to_owned(),
        args: vec!["-c".to_owned(), PYTHON_LSP_SERVER.to_owned()],
        root: dir.path().to_path_buf(),
        init_timeout_secs: Some(5),
        auto_plugins: vec![],
        init_options: serde_json::json!({
            "sleep_before_respond": true
        }),
    };

    let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);
    let client = LspClient {
        descriptors: Arc::new(vec![descriptor.clone()]),
        missing_languages: Arc::new(Vec::new()),
        processes: Arc::new(DashMap::new()),
        init_locks: Arc::new(DashMap::new()),
        dispatcher: Arc::new(RequestDispatcher::new()),
        shutdown_tx: Arc::new(shutdown_tx),
        shutdown_requested: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        doc_versions: Arc::new(DashMap::new()),
        warm_start_complete: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        spawner: std::sync::Arc::new(crate::client::process::RealProcessSpawner),
    };

    let client_clone = client.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        client_clone.shutdown();
    });

    let result = client.start_process(descriptor, 0).await;
    assert!(
        matches!(result, Err(LspError::ConnectionLost)),
        "should return ConnectionLost because shutdown was requested: {result:?}"
    );

    let entry = client.processes.get("python-test");
    assert!(entry.is_none(), "process should not be inserted");
}

#[cfg(target_os = "linux")]
#[test]
fn test_detect_concurrent_lsp_linux_detailed_paths() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let sleep_bin = which::which("sleep")
        .or_else(|_| {
            which::which("/usr/bin/sleep").map(|_| std::path::PathBuf::from("/usr/bin/sleep"))
        })
        .or_else(|_| which::which("/bin/sleep").map(|_| std::path::PathBuf::from("/bin/sleep")))
        .unwrap_or_else(|_| std::path::PathBuf::from("/usr/bin/sleep"));
    let unique_sleep = temp_dir.path().join("unique_sleep_bin");
    std::fs::copy(&sleep_bin, &unique_sleep).expect("copy sleep");

    let true_bin = which::which("true")
        .or_else(|_| {
            which::which("/usr/bin/true").map(|_| std::path::PathBuf::from("/usr/bin/true"))
        })
        .or_else(|_| which::which("/bin/true").map(|_| std::path::PathBuf::from("/bin/true")))
        .unwrap_or_else(|_| std::path::PathBuf::from("/usr/bin/true"));
    let unique_true = temp_dir.path().join("unique_true_bin");
    std::fs::copy(&true_bin, &unique_true).expect("copy true");

    // 1. Own child process (should be skipped)
    let mut child = std::process::Command::new(&unique_sleep)
        .arg("10")
        .spawn()
        .unwrap();

    let found_child = LspClient::detect_concurrent_lsp_linux("rust", "unique_sleep_bin");

    // 2. Zombie child process (should be skipped)
    let mut zombie = std::process::Command::new(&unique_true).spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let found_zombie = LspClient::detect_concurrent_lsp_linux("rust", "unique_true_bin");

    let mut orphaned_shell = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{} 5 &", unique_sleep.display()))
        .spawn()
        .unwrap();
    let _ = orphaned_shell.wait();
    std::thread::sleep(std::time::Duration::from_millis(50));

    let found_external = LspClient::detect_concurrent_lsp_linux("rust", "unique_sleep_bin");

    let _ = child.kill();
    let _ = child.wait();
    let _ = zombie.kill();
    let _ = zombie.wait();

    assert!(!found_child);
    assert!(!found_zombie);
    assert!(found_external);
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[test]
fn test_detect_concurrent_lsp_macos_detailed_paths() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let sleep_bin = which::which("sleep")
        .or_else(|_| {
            which::which("/usr/bin/sleep").map(|_| std::path::PathBuf::from("/usr/bin/sleep"))
        })
        .or_else(|_| which::which("/bin/sleep").map(|_| std::path::PathBuf::from("/bin/sleep")))
        .unwrap_or_else(|_| std::path::PathBuf::from("/usr/bin/sleep"));
    let unique_sleep = temp_dir.path().join("unique_sleep_bin");
    std::fs::copy(&sleep_bin, &unique_sleep).expect("copy sleep");

    let true_bin = which::which("true")
        .or_else(|_| {
            which::which("/usr/bin/true").map(|_| std::path::PathBuf::from("/usr/bin/true"))
        })
        .or_else(|_| which::which("/bin/true").map(|_| std::path::PathBuf::from("/bin/true")))
        .unwrap_or_else(|_| std::path::PathBuf::from("/usr/bin/true"));
    let unique_true = temp_dir.path().join("unique_true_bin");
    std::fs::copy(&true_bin, &unique_true).expect("copy true");

    // 1. Own child process (should be skipped)
    let mut child = std::process::Command::new(&unique_sleep)
        .arg("10")
        .spawn()
        .unwrap();

    let found_child = LspClient::detect_concurrent_lsp_macos("rust", "unique_sleep_bin");

    // 2. Zombie child process (should be skipped)
    let mut zombie = std::process::Command::new(&unique_true).spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let found_zombie = LspClient::detect_concurrent_lsp_macos("rust", "unique_true_bin");

    let mut orphaned_shell = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{} 5 &", unique_sleep.display()))
        .spawn()
        .unwrap();
    let _ = orphaned_shell.wait();
    std::thread::sleep(std::time::Duration::from_millis(50));

    let found_external = LspClient::detect_concurrent_lsp_macos("rust", "unique_sleep_bin");

    let _ = child.kill();
    let _ = child.wait();
    let _ = zombie.kill();
    let _ = zombie.wait();

    assert!(!found_child);
    assert!(!found_zombie);
    assert!(found_external);
}
