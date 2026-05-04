//! JSON-RPC request/response correlation.
//!
//! `RequestDispatcher` maintains a map of in-flight requests keyed by their
//! `id`. When the background reader task receives a response, it
//! calls [`RequestDispatcher::dispatch_response`] to fire the matching
//! [`tokio::sync::oneshot`] and wake the caller.

use crate::LspError;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tokio::sync::{broadcast, oneshot};

/// Capacity of the server-notification broadcast channel.
/// Older notifications are dropped silently if all subscribers are slow.
const NOTIFICATION_CHANNEL_CAPACITY: usize = 64;

/// Correlates outgoing JSON-RPC requests with their responses.
///
/// All methods take `&self` (interior mutability via `Mutex`) so the
/// dispatcher can be shared across the writer and reader tasks via `Arc`.
pub(super) struct RequestDispatcher {
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, LspError>>>>,
    next_id: AtomicU64,
    /// Broadcast channel for unsolicited server notifications (no `id`).
    /// Subscribers receive a clone of each incoming notification `Value`.
    notification_tx: broadcast::Sender<Value>,
    /// MT-3: Broadcast channel for server-to-client *requests* (has both `id`
    /// and `method`, but the `id` is NOT in the pending map).
    ///
    /// Used by `registration_watcher_task` to receive `client/registerCapability`
    /// and `client/unregisterCapability` messages. The task must send a JSON-RPC
    /// response with the same `id` and `result: null` back to the server.
    server_request_tx: broadcast::Sender<Value>,
}

impl RequestDispatcher {
    pub(super) fn new() -> Self {
        let (notification_tx, _) = broadcast::channel(NOTIFICATION_CHANNEL_CAPACITY);
        let (server_request_tx, _) = broadcast::channel(NOTIFICATION_CHANNEL_CAPACITY);
        Self {
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            notification_tx,
            server_request_tx,
        }
    }

    /// Allocate a unique request id and register a oneshot receiver.
    ///
    /// Returns `(id, rx)`. The caller should write a JSON-RPC request with
    /// this `id` and then `.await rx` to receive the response.
    #[allow(clippy::expect_used)] // Mutex poisoning is unrecoverable
    pub(super) fn register(&self) -> (u64, oneshot::Receiver<Result<Value, LspError>>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().expect("dispatcher lock").insert(id, tx);
        (id, rx)
    }

    /// Build a JSON-RPC request value for the given method and params.
    #[must_use]
    pub(super) fn make_request(id: u64, method: &str, params: &Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        })
    }

    /// Build a JSON-RPC notification (no id, no response expected).
    #[must_use]
    pub(super) fn make_notification(method: &str, params: &Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        })
    }

    /// Dispatch an incoming message to the waiting caller.
    ///
    /// If the message has an `id` that matches a pending request, fires its
    /// oneshot. Notifications (no `id`) are forwarded to the notification
    /// broadcast channel (subscribers include the `progress_watcher_task`).
    /// Unmatched responses are silently ignored.
    #[allow(clippy::expect_used)] // Mutex poisoning is unrecoverable
    pub(super) fn dispatch_response(&self, message: &Value) {
        let Some(id_val) = message.get("id") else {
            // Server notification (no id) — forward to notification broadcast channel.
            // Ignore send errors (no active subscribers is fine).
            let _ = self.notification_tx.send(message.clone());
            return;
        };
        let Some(id) = id_val.as_u64() else {
            return;
        };

        let tx = self.pending.lock().expect("dispatcher lock").remove(&id);

        if let Some(sender) = tx {
            // Normal response to a request we sent: resolve the waiting oneshot.
            let result = if message.get("error").is_some() {
                let err_msg = message["error"]["message"]
                    .as_str()
                    .unwrap_or("LSP returned an error")
                    .to_owned();
                Err(LspError::Protocol(err_msg))
            } else {
                Ok(message["result"].clone())
            };
            // Ignore send error — the caller may have timed out and dropped the rx
            let _ = sender.send(result);
        } else if message.get("method").is_some() {
            // MT-3: Server-to-client request (has id AND method, NOT in pending).
            // Examples: client/registerCapability, client/unregisterCapability.
            // Forward to the server_request channel for registration_watcher_task.
            let _ = self.server_request_tx.send(message.clone());
        }
        // Else: unmatched response id with no method — silently ignore.
    }

    /// Subscribe to unsolicited server notifications.
    ///
    /// Returns a `broadcast::Receiver` that yields each incoming notification
    /// value (messages without a JSON-RPC `id`). Used by `progress_watcher_task`
    /// to detect `$/progress` events for LSP indexing completion.
    pub(super) fn subscribe_notifications(&self) -> broadcast::Receiver<Value> {
        self.notification_tx.subscribe()
    }
    /// MT-3: Subscribe to server-to-client requests.
    ///
    /// Returns a `broadcast::Receiver` for messages that have both `id` and
    /// `method` but were NOT initiated by us (i.e., not in the pending map).
    /// Used by `registration_watcher_task` to handle `client/registerCapability`
    /// and `client/unregisterCapability` from the LSP server.
    pub(super) fn subscribe_server_requests(&self) -> broadcast::Receiver<Value> {
        self.server_request_tx.subscribe()
    }

    /// Remove a pending request by ID (e.g. after a timeout).
    ///
    /// Prevents request IDs from leaking forever in the dispatcher when
    /// the caller gives up waiting for a response.
    #[allow(clippy::expect_used)] // Mutex poisoning is unrecoverable
    pub(super) fn remove(&self, id: u64) {
        self.pending.lock().expect("dispatcher lock").remove(&id);
    }

    /// Cancel all pending requests with `LspError::ConnectionLost`.
    ///
    /// Called when the LSP process exits to unblock all waiting callers.
    #[allow(clippy::expect_used)] // Mutex poisoning is unrecoverable
    pub(super) fn cancel_all(&self) {
        let drained: Vec<_> = self
            .pending
            .lock()
            .expect("dispatcher lock")
            .drain()
            .collect();
        for (_, tx) in drained {
            let _ = tx.send(Err(LspError::ConnectionLost));
        }
    }

    /// Collect `textDocument/publishDiagnostics` notifications for a file.
    ///
    /// MT-2: `grace_period` is now caller-supplied (per-server config) instead of
    /// a hardcoded constant. Pass `caps.push_collection_config().grace_ms` to adapt
    /// the window per LSP server identity (gopls and tsserver need longer windows).
    ///
    /// Two-phase adaptive collection:
    /// - Phase 1: wait up to `timeout` for the first matching notification.
    /// - Phase 2: collect follow-up batches for `grace_period`, then return.
    pub(super) async fn collect_push_diagnostics(
        &self,
        file_uri: &str,
        timeout: tokio::time::Duration,
        grace_period: tokio::time::Duration,
    ) -> Vec<serde_json::Value> {
        let mut rx = self.subscribe_notifications();
        let mut collected = Vec::new();
        let max_deadline = tokio::time::Instant::now() + timeout;
        let mut grace_deadline: Option<tokio::time::Instant> = None;

        loop {
            let now = tokio::time::Instant::now();
            let effective_deadline = grace_deadline.unwrap_or(max_deadline);
            let remaining = effective_deadline.saturating_duration_since(now);
            if remaining.is_zero() {
                break;
            }

            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Ok(msg)) => {
                    let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
                    if method != "textDocument/publishDiagnostics" {
                        continue;
                    }
                    let uri = msg
                        .pointer("/params/uri")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if uri == file_uri {
                        if grace_deadline.is_none() {
                            let grace_end = tokio::time::Instant::now() + grace_period;
                            grace_deadline = Some(grace_end.min(max_deadline));
                        }
                        collected.push(msg);
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) | Err(_) => break,
            }
        }

        collected
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_request_response_roundtrip() {
        let dispatcher = RequestDispatcher::new();
        let (id, rx) = dispatcher.register();
        assert_eq!(id, 1);

        // Simulate a response arriving
        let response = json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "uri": "file:///foo.rs", "range": {} }
        });
        dispatcher.dispatch_response(&response);

        let result = rx.await.expect("oneshot receive");
        assert!(result.is_ok());
        assert_eq!(result.unwrap()["uri"], "file:///foo.rs");
    }

    #[tokio::test]
    async fn test_error_response_propagated() {
        let dispatcher = RequestDispatcher::new();
        let (id, rx) = dispatcher.register();

        let response = json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": "Method not found" }
        });
        dispatcher.dispatch_response(&response);

        let result = rx.await.expect("oneshot receive");
        assert!(result.is_err());
        match result {
            Err(LspError::Protocol(msg)) => assert!(msg.contains("Method not found")),
            _ => panic!("expected Protocol error"),
        }
    }

    #[tokio::test]
    async fn test_notification_ignored() {
        let dispatcher = RequestDispatcher::new();
        let (id, _rx) = dispatcher.register();

        // A notification (no id) — should not panic or affect pending map
        let notif = json!({ "jsonrpc": "2.0", "method": "window/logMessage", "params": {} });
        dispatcher.dispatch_response(&notif);

        // The pending entry for `id` should still be there
        assert_eq!(
            dispatcher.pending.lock().unwrap().len(),
            1,
            "notification must not remove pending request"
        );
        let _ = id; // suppress unused warning
    }

    #[test]
    fn test_cancel_all_drains_pending() {
        let dispatcher = RequestDispatcher::new();
        let (_id1, _rx1) = dispatcher.register();
        let (_id2, _rx2) = dispatcher.register();
        assert_eq!(dispatcher.pending.lock().unwrap().len(), 2);

        dispatcher.cancel_all();
        assert!(dispatcher.pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_sequential_ids() {
        let dispatcher = RequestDispatcher::new();
        let (id1, _rx1) = dispatcher.register();
        let (id2, _rx2) = dispatcher.register();
        let (id3, _rx3) = dispatcher.register();
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
    }

    #[test]
    fn test_make_request_structure() {
        let msg =
            RequestDispatcher::make_request(42, "textDocument/definition", &json!({"key": "val"}));
        assert_eq!(msg["jsonrpc"], "2.0");
        assert_eq!(msg["id"], 42);
        assert_eq!(msg["method"], "textDocument/definition");
        assert_eq!(msg["params"]["key"], "val");
    }

    #[test]
    fn test_make_notification_structure() {
        let msg = RequestDispatcher::make_notification("initialized", &json!({}));
        assert_eq!(msg["jsonrpc"], "2.0");
        assert!(msg.get("id").is_none(), "notifications must not have id");
        assert_eq!(msg["method"], "initialized");
    }

    #[tokio::test]
    async fn test_dispatch_unmatched_id_ignored() {
        let dispatcher = RequestDispatcher::new();
        let (_id, mut rx) = dispatcher.register();

        // Dispatch a response with a different ID
        let response = json!({"jsonrpc": "2.0", "id": 999, "result": "wrong"});
        dispatcher.dispatch_response(&response);

        // The original pending entry should still be there
        assert_eq!(dispatcher.pending.lock().unwrap().len(), 1);
        // The receiver should not have been fulfilled
        assert!(rx.try_recv().is_err(), "unmatched id should not deliver");
    }

    #[tokio::test]
    async fn test_cancel_all_sends_connection_lost() {
        let dispatcher = RequestDispatcher::new();
        let (_id1, rx1) = dispatcher.register();
        let (_id2, rx2) = dispatcher.register();

        dispatcher.cancel_all();

        let result1 = rx1.await.expect("should receive");
        let result2 = rx2.await.expect("should receive");
        assert!(matches!(result1, Err(LspError::ConnectionLost)));
        assert!(matches!(result2, Err(LspError::ConnectionLost)));
    }

    #[tokio::test]
    async fn test_remove_drops_pending() {
        let dispatcher = RequestDispatcher::new();
        let (id, rx) = dispatcher.register();
        assert_eq!(dispatcher.pending.lock().unwrap().len(), 1);

        dispatcher.remove(id);
        assert!(dispatcher.pending.lock().unwrap().is_empty());

        // Receiver should error (sender dropped)
        assert!(rx.await.is_err());
    }

    #[tokio::test]
    async fn test_string_id_ignored() {
        let dispatcher = RequestDispatcher::new();
        let (_id, mut rx) = dispatcher.register();

        // LSP spec says IDs can be strings, but our implementation uses u64
        let response = json!({"jsonrpc": "2.0", "id": "string-id", "result": null});
        dispatcher.dispatch_response(&response);

        // Should not match — our ID is numeric
        assert_eq!(dispatcher.pending.lock().unwrap().len(), 1);
        assert!(rx.try_recv().is_err());
    }

    // Tests for collect_push_diagnostics
    //
    // Note: broadcast channels only deliver messages sent AFTER subscription.
    // So tests spawn a task that sends after a delay, then start collecting.
    #[tokio::test]
    async fn test_collect_push_diagnostics_timeout_returns_empty() {
        let dispatcher = RequestDispatcher::new();
        // Short timeout — no notifications sent
        let result = dispatcher
            .collect_push_diagnostics(
                "file:///test.rs",
                tokio::time::Duration::from_millis(10),
                tokio::time::Duration::from_millis(50),
            )
            .await;
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_collect_push_diagnostics_collects_matching_uri() {
        let dispatcher = std::sync::Arc::new(RequestDispatcher::new());
        let test_uri = "file:///test.rs";

        let notif = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": test_uri,
                "diagnostics": []
            }
        });

        // Spawn task that dispatches after we start collecting
        let dispatcher_clone = dispatcher.clone();
        let notif_clone = notif.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            dispatcher_clone.dispatch_response(&notif_clone);
        });

        // Start collecting with timeout longer than the dispatch delay
        let result = dispatcher
            .collect_push_diagnostics(
                test_uri,
                tokio::time::Duration::from_millis(150),
                tokio::time::Duration::from_millis(50),
            )
            .await;

        let _ = handle.await;

        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["params"]["uri"], test_uri);
    }

    // This test doesn't need to receive anything — it verifies that notifications
    // for OTHER files are filtered out. The current test structure works because:
    // 1. We dispatch for other_uri BEFORE subscribing (so we never see it anyway)
    // 2. We collect for test_uri with a short timeout
    // 3. Result should be empty because no one sends test_uri notifications
    #[tokio::test]
    async fn test_collect_push_diagnostics_ignores_other_files() {
        let dispatcher = std::sync::Arc::new(RequestDispatcher::new());
        let test_uri = "file:///test.rs";
        let other_uri = "file:///other.rs";

        // Send notification for a DIFFERENT file after a delay
        let notif = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": other_uri,
                "diagnostics": []
            }
        });

        let dispatcher_clone = dispatcher.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            dispatcher_clone.dispatch_response(&notif);
        });

        // Collect for test_uri — should NOT include the other_uri notification
        let result = dispatcher
            .collect_push_diagnostics(
                test_uri,
                tokio::time::Duration::from_millis(150),
                tokio::time::Duration::from_millis(50),
            )
            .await;

        let _ = handle.await;

        assert!(
            result.is_empty(),
            "should ignore notifications for different URIs"
        );
    }

    #[tokio::test]
    async fn test_collect_push_diagnostics_ignores_non_diagnostics_notifications() {
        let dispatcher = std::sync::Arc::new(RequestDispatcher::new());
        let test_uri = "file:///test.rs";

        // Send a log message notification (not diagnostics)
        let log_notif = json!({
            "jsonrpc": "2.0",
            "method": "window/logMessage",
            "params": { "message": "hello" }
        });

        // Send a diagnostics notification for the target file
        let diag_notif = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": test_uri,
                "diagnostics": []
            }
        });

        let dispatcher_clone = dispatcher.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            dispatcher_clone.dispatch_response(&log_notif);
            dispatcher_clone.dispatch_response(&diag_notif);
        });

        let result = dispatcher
            .collect_push_diagnostics(
                test_uri,
                tokio::time::Duration::from_millis(150),
                tokio::time::Duration::from_millis(50),
            )
            .await;

        let _ = handle.await;

        assert_eq!(
            result.len(),
            1,
            "should only collect diagnostics notifications"
        );
        assert_eq!(result[0]["method"], "textDocument/publishDiagnostics");
    }

    // ── MT-3: server-to-client request channel ────────────────────────────────

    #[tokio::test]
    async fn test_server_request_client_register_capability_is_forwarded() {
        // When the server sends client/registerCapability (has id AND method),
        // dispatch_response must emit it on the server_request channel rather than
        // silently dropping it or treating it as a normal notification.
        let dispatcher = RequestDispatcher::new();
        let mut rx = dispatcher.subscribe_server_requests();

        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "client/registerCapability",
            "params": {
                "registrations": [{
                    "id": "reg-001",
                    "method": "textDocument/diagnostic",
                    "registerOptions": {}
                }]
            }
        });
        dispatcher.dispatch_response(&req);

        let msg = tokio::time::timeout(tokio::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("should not time out")
            .expect("should receive");

        assert_eq!(
            msg["method"].as_str().unwrap_or(""),
            "client/registerCapability"
        );
        assert_eq!(msg["id"], 1);
    }

    #[tokio::test]
    async fn test_server_request_not_delivered_as_notification() {
        // client/registerCapability must NOT be broadcast on the notification channel.
        let dispatcher = RequestDispatcher::new();
        let mut notif_rx = dispatcher.subscribe_notifications();

        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "client/registerCapability",
            "params": { "registrations": [] }
        });
        dispatcher.dispatch_response(&req);

        // The notification channel should remain empty
        let received = notif_rx.try_recv();
        assert!(
            received.is_err(),
            "client/registerCapability must not be broadcast to notification subscribers"
        );
    }

    #[tokio::test]
    async fn test_server_request_unregister_capability_forwarded() {
        let dispatcher = RequestDispatcher::new();
        let mut rx = dispatcher.subscribe_server_requests();

        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "client/unregisterCapability",
            "params": {
                "unregisterations": [{ "id": "reg-001", "method": "textDocument/diagnostic" }]
            }
        });
        dispatcher.dispatch_response(&req);

        let msg = tokio::time::timeout(tokio::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("should not time out")
            .expect("should receive");

        assert_eq!(
            msg["method"].as_str().unwrap_or(""),
            "client/unregisterCapability"
        );
    }

    #[test]
    fn test_normal_response_not_forwarded_to_server_request_channel() {
        // A normal response (no method field) should not leak to the server_request channel.
        let dispatcher = RequestDispatcher::new();
        let (id, rx) = dispatcher.register();
        let mut server_rx = dispatcher.subscribe_server_requests();

        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {}
        });
        dispatcher.dispatch_response(&response);

        // The request was fulfilled normally
        std::mem::drop(rx); // receiver still valid — drop explicitly to avoid let_underscore_future lint
                            // Server request channel remains empty
        assert!(
            server_rx.try_recv().is_err(),
            "normal response must not appear on server_request channel"
        );
    }
}
