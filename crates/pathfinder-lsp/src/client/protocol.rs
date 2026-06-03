//! JSON-RPC request/response correlation.
//!
//! `RequestDispatcher` maintains a map of in-flight requests keyed by their
//! `id`. When the background reader task receives a response, it
//! calls [`RequestDispatcher::dispatch_response`] to fire the matching
//! [`tokio::sync::oneshot`] and wake the caller.

use crate::LspError;
use dashmap::DashMap;
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
///
/// LSP-INIT-002: Per-language isolation prevents cross-language interference:
///
/// - Pending requests are tagged with `language_id` so `cancel_all` can be scoped
/// - Notifications and server requests are routed through per-language channels
type PendingRequest = (String, oneshot::Sender<Result<Value, LspError>>);

pub(crate) struct RequestDispatcher {
    pending: Mutex<HashMap<u64, PendingRequest>>,
    next_id: AtomicU64,
    /// Per-language broadcast channels for unsolicited server notifications (no `id`).
    /// Key: `language_id`, Value: `broadcast::Sender<Value>`
    ///
    /// FUTURE: cleanup when dynamic language support is added. Currently bounded to
    /// 5 languages (rust/go/typescript/python/java), so memory cost is negligible.
    notification_channels: DashMap<String, broadcast::Sender<Value>>,
    /// MT-3: Per-language broadcast channels for server-to-client *requests*
    /// (has both `id` and `method`, but the `id` is NOT in the pending map).
    ///
    /// FUTURE: cleanup when dynamic language support is added. Currently bounded to
    /// 5 languages (rust/go/typescript/python/java), so memory cost is negligible.
    server_request_channels: DashMap<String, broadcast::Sender<Value>>,
}

impl RequestDispatcher {
    pub(crate) fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            notification_channels: DashMap::new(),
            server_request_channels: DashMap::new(),
        }
    }

    /// Allocate a unique request id and register a oneshot receiver.
    ///
    /// Returns `(id, rx)`. The caller should write a JSON-RPC request with
    /// this `id` and then `.await rx` to receive the response.
    ///
    /// LSP-INIT-002: `language_id` is stored alongside the sender so that
    /// `cancel_for_language()` can selectively cancel only requests from
    /// a crashed language server without affecting other languages.
    #[allow(clippy::expect_used)] // Mutex poisoning is unrecoverable
    pub(crate) fn register(
        &self,
        language_id: &str,
    ) -> (u64, oneshot::Receiver<Result<Value, LspError>>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("dispatcher lock")
            .insert(id, (language_id.to_owned(), tx));
        (id, rx)
    }

    /// Build a JSON-RPC request value for the given method and params.
    #[must_use]
    pub(crate) fn make_request(id: u64, method: &str, params: &Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        })
    }

    /// Build a JSON-RPC notification (no id, no response expected).
    #[must_use]
    pub(crate) fn make_notification(method: &str, params: &Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        })
    }

    /// Get or create a notification channel for the given language.
    ///
    /// LSP-INIT-002: Per-language channel isolation ensures notifications
    /// from one language don't bleed into other languages' progress watchers.
    pub(crate) fn subscribe_notifications_for_language(
        &self,
        language_id: &str,
    ) -> broadcast::Receiver<Value> {
        self.notification_channels
            .entry(language_id.to_owned())
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(NOTIFICATION_CHANNEL_CAPACITY);
                tx
            })
            .value()
            .subscribe()
    }

    /// Get or create a server request channel for the given language.
    ///
    /// LSP-INIT-002: Per-language channel isolation ensures capability registrations
    /// from one language don't pollute other languages' `live_capabilities`.
    pub(crate) fn subscribe_server_requests_for_language(
        &self,
        language_id: &str,
    ) -> broadcast::Receiver<Value> {
        self.server_request_channels
            .entry(language_id.to_owned())
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(NOTIFICATION_CHANNEL_CAPACITY);
                tx
            })
            .value()
            .subscribe()
    }

    /// Dispatch an incoming message to the waiting caller, with source language tagging.
    ///
    /// LSP-INIT-002: The `source_language_id` parameter ensures notifications and
    /// server requests are only routed to subscribers of that specific language.
    ///
    /// Dispatch priority (eliminates ID collision between client responses and
    /// server-to-client requests per JSON-RPC 2.0):
    /// 1. No `id` → notification → per-language notification channel
    /// 2. Has `id` + `method` → server-to-client request → per-language `server_request` channel
    /// 3. Has `id`, no `method`, in pending → response to our request → resolve oneshot
    /// 4. Has `id`, not in pending → unmatched response → silently dropped
    #[allow(clippy::expect_used)] // Mutex poisoning is unrecoverable
    pub(crate) fn dispatch_response_for_language(&self, source_language_id: &str, message: &Value) {
        let Some(id_val) = message.get("id") else {
            // Server notification (no id) — forward to per-language notification channel.
            let method = message
                .get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            tracing::debug!(
                language = %source_language_id,
                method = %method,
                "LSP: dispatching notification"
            );
            if let Some(tx) = self.notification_channels.get(source_language_id) {
                let _ = tx.send(message.clone());
            }
            return;
        };
        let Some(id) = id_val.as_u64() else {
            return;
        };

        // Check for method BEFORE checking pending to prevent ID collision
        // between server-to-client requests (e.g. client/registerCapability)
        // and our outgoing requests. A message with both `id` and `method`
        // is a server request, not a response to our request.
        if message.get("method").is_some() {
            // Server-to-client request (has id AND method).
            // Forward to per-language server_request channel.
            let method = message
                .get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            tracing::debug!(
                language = %source_language_id,
                id = %id,
                method = %method,
                "LSP: dispatching server-to-client request"
            );
            if let Some(tx) = self.server_request_channels.get(source_language_id) {
                let _ = tx.send(message.clone());
            }
            return;
        }

        // Normal response to a request we sent (has id, no method).
        if let Some((_lang, sender)) = self.pending.lock().expect("dispatcher lock").remove(&id) {
            let is_error = message.get("error").is_some();
            tracing::debug!(
                language = %source_language_id,
                id = %id,
                is_error = %is_error,
                "LSP: dispatching response to pending request"
            );
            let result = if is_error {
                let err_msg = message["error"]["message"]
                    .as_str()
                    .unwrap_or("LSP returned an error")
                    .to_owned();
                Err(LspError::Protocol(err_msg))
            } else {
                Ok(message["result"].clone())
            };
            let _ = sender.send(result);
        }
    }

    /// DEPRECATED: Use `subscribe_notifications_for_language` instead.
    ///
    /// Legacy method kept for backward compatibility with tests.
    /// Panics if no channel exists for "test" language.
    #[allow(dead_code)]
    pub(crate) fn subscribe_notifications(&self) -> broadcast::Receiver<Value> {
        self.subscribe_notifications_for_language("test")
    }

    /// DEPRECATED: Use `subscribe_server_requests_for_language` instead.
    ///
    /// Legacy method kept for backward compatibility with tests.
    /// Panics if no channel exists for "test" language.
    #[allow(dead_code)]
    pub(crate) fn subscribe_server_requests(&self) -> broadcast::Receiver<Value> {
        self.subscribe_server_requests_for_language("test")
    }

    /// DEPRECATED: Use `dispatch_response_for_language` instead.
    ///
    /// Legacy dispatch for tests. Uses "test" as the source language.
    #[allow(dead_code)]
    pub(crate) fn dispatch_response(&self, message: &Value) {
        self.dispatch_response_for_language("test", message);
    }

    /// Remove a pending request by ID (e.g. after a timeout).
    ///
    /// Prevents request IDs from leaking forever in the dispatcher when
    /// the caller gives up waiting for a response.
    #[allow(clippy::expect_used)] // Mutex poisoning is unrecoverable
    pub(crate) fn remove(&self, id: u64) {
        self.pending.lock().expect("dispatcher lock").remove(&id);
    }

    /// Cancel all pending requests with `LspError::ConnectionLost`.
    ///
    /// Called during shutdown to unblock all waiting callers across all languages.
    /// LSP-INIT-002: For per-language cancellation (when one language crashes),
    /// use `cancel_for_language()` instead.
    #[allow(dead_code)] // Kept for tests and future use
    #[allow(clippy::expect_used)] // Mutex poisoning is unrecoverable
    pub(crate) fn cancel_all(&self) {
        let drained: Vec<_> = self
            .pending
            .lock()
            .expect("dispatcher lock")
            .drain()
            .collect();
        for (_id, (_lang, tx)) in drained {
            let _ = tx.send(Err(LspError::ConnectionLost));
        }
    }

    /// Cancel only pending requests for a specific language with `LspError::ConnectionLost`.
    ///
    /// LSP-INIT-002: Called when a single language's LSP process crashes.
    /// This isolates the crash to only that language's pending requests,
    /// leaving other languages' requests unaffected.
    #[allow(clippy::expect_used)] // Mutex poisoning is unrecoverable
    pub(crate) fn cancel_for_language(&self, language_id: &str) {
        let mut pending = self.pending.lock().expect("dispatcher lock");

        let ids_to_cancel: Vec<u64> = pending
            .iter()
            .filter(|(_id, (lang, _tx))| lang == language_id)
            .map(|(&id, _)| id)
            .collect();

        let count = ids_to_cancel.len();
        if count > 0 {
            tracing::debug!(
                language = %language_id,
                count = %count,
                "LSP: cancel_for_language: cancelling pending requests"
            );
        }

        for id in ids_to_cancel {
            if let Some((_lang, tx)) = pending.remove(&id) {
                let _ = tx.send(Err(LspError::ConnectionLost));
            }
        }
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
        let (id, rx) = dispatcher.register("test");
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
        let (id, rx) = dispatcher.register("test");

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
        let (id, _rx) = dispatcher.register("test");

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
        let (_id1, _rx1) = dispatcher.register("test");
        let (_id2, _rx2) = dispatcher.register("test");
        assert_eq!(dispatcher.pending.lock().unwrap().len(), 2);

        dispatcher.cancel_all();
        assert!(dispatcher.pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_sequential_ids() {
        let dispatcher = RequestDispatcher::new();
        let (id1, _rx1) = dispatcher.register("test");
        let (id2, _rx2) = dispatcher.register("test");
        let (id3, _rx3) = dispatcher.register("test");
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
        let (_id, mut rx) = dispatcher.register("test");

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
        let (_id1, rx1) = dispatcher.register("test");
        let (_id2, rx2) = dispatcher.register("test");

        dispatcher.cancel_all();

        let result1 = rx1.await.expect("should receive");
        let result2 = rx2.await.expect("should receive");
        assert!(matches!(result1, Err(LspError::ConnectionLost)));
        assert!(matches!(result2, Err(LspError::ConnectionLost)));
    }

    #[tokio::test]
    async fn test_remove_drops_pending() {
        let dispatcher = RequestDispatcher::new();
        let (id, rx) = dispatcher.register("test");
        assert_eq!(dispatcher.pending.lock().unwrap().len(), 1);

        dispatcher.remove(id);
        assert!(dispatcher.pending.lock().unwrap().is_empty());

        // Receiver should error (sender dropped)
        assert!(rx.await.is_err());
    }

    #[tokio::test]
    async fn test_string_id_ignored() {
        let dispatcher = RequestDispatcher::new();
        let (_id, mut rx) = dispatcher.register("test");

        // LSP spec says IDs can be strings, but our implementation uses u64
        let response = json!({"jsonrpc": "2.0", "id": "string-id", "result": null});
        dispatcher.dispatch_response(&response);

        // Should not match — our ID is numeric
        assert_eq!(dispatcher.pending.lock().unwrap().len(), 1);
        assert!(rx.try_recv().is_err());
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
        let (id, rx) = dispatcher.register("test");
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

    // ── LSP-INIT-002: Cross-language isolation tests ─────────────────────────

    #[tokio::test]
    async fn test_cancel_for_language_only_cancels_matching_language() {
        // DEL-1.2: cancel_for_language("rust") only cancels rust requests,
        // leaves go requests intact. This is the core test for BUG-1 fix.
        let dispatcher = RequestDispatcher::new();

        // Register requests from two different languages
        let (_id_rust, rx_rust) = dispatcher.register("rust");
        let (_id_go, mut rx_go) = dispatcher.register("go");

        // Verify both are in pending
        assert_eq!(
            dispatcher.pending.lock().unwrap().len(),
            2,
            "should have 2 pending requests before cancel"
        );

        // Cancel only rust
        dispatcher.cancel_for_language("rust");

        // Rust request should receive ConnectionLost
        let result_rust = rx_rust.await.expect("should receive");
        assert!(
            matches!(result_rust, Err(LspError::ConnectionLost)),
            "rust request should be cancelled"
        );

        // Go request should still be pending (not cancelled)
        assert_eq!(
            dispatcher.pending.lock().unwrap().len(),
            1,
            "should have 1 pending request (go) after rust cancel"
        );

        // Go receiver should not have received anything yet
        assert!(
            rx_go.try_recv().is_err(),
            "go request should not be cancelled by cancel_for_language(\"rust\")"
        );
    }

    #[tokio::test]
    async fn test_cancel_for_language_no_matching_entries_is_noop() {
        // DEL-1.2: cancel_for_language with no matching entries is a no-op.
        let dispatcher = RequestDispatcher::new();

        // Register a rust request
        let (_id_rust, _rx_rust) = dispatcher.register("rust");

        // Cancel for a language that has no pending requests
        dispatcher.cancel_for_language("typescript");

        // Rust request should still be pending
        assert_eq!(
            dispatcher.pending.lock().unwrap().len(),
            1,
            "rust request should still be pending after no-op cancel"
        );
    }

    #[tokio::test]
    async fn test_cancel_for_language_multiple_languages_isolated() {
        // DEL-1.2: Verify that cancel_for_language correctly handles
        // multiple requests from multiple languages.
        let dispatcher = RequestDispatcher::new();

        // Register multiple requests per language
        let (_id_r1, mut rx_r1) = dispatcher.register("rust");
        let (_id_r2, mut rx_r2) = dispatcher.register("rust");
        let (_id_g1, rx_g1) = dispatcher.register("go");
        let (_id_t1, mut rx_t1) = dispatcher.register("typescript");
        let (_id_g2, rx_g2) = dispatcher.register("go");

        assert_eq!(dispatcher.pending.lock().unwrap().len(), 5);

        // Cancel all go requests
        dispatcher.cancel_for_language("go");

        // Check: rust and ts should still have pending requests
        // go receivers should get ConnectionLost
        let go_result_1 = rx_g1.await.expect("should receive");
        let go_result_2 = rx_g2.await.expect("should receive");
        assert!(matches!(go_result_1, Err(LspError::ConnectionLost)));
        assert!(matches!(go_result_2, Err(LspError::ConnectionLost)));

        // Rust and TS receivers should not have been cancelled
        assert_eq!(dispatcher.pending.lock().unwrap().len(), 3);
        assert!(rx_r1.try_recv().is_err());
        assert!(rx_r2.try_recv().is_err());
        assert!(rx_t1.try_recv().is_err());

        // Now cancel rust
        dispatcher.cancel_for_language("rust");
        let rust_result_1 = rx_r1.await.expect("should receive");
        let rust_result_2 = rx_r2.await.expect("should receive");
        assert!(matches!(rust_result_1, Err(LspError::ConnectionLost)));
        assert!(matches!(rust_result_2, Err(LspError::ConnectionLost)));

        // Only TS remains
        assert_eq!(dispatcher.pending.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_notification_routing_per_language() {
        // DEL-1.3: Notification from "rust" only reaches "rust" subscriber.
        // Notification from "go" does NOT reach "rust" subscriber.
        // This tests the BUG-2 fix (progress notification bleed).
        let dispatcher = RequestDispatcher::new();

        // Subscribe both languages to their notification channels
        let mut rx_rust = dispatcher.subscribe_notifications_for_language("rust");
        let mut rx_go = dispatcher.subscribe_notifications_for_language("go");

        // Send a rust progress notification
        let rust_notif = json!({
            "jsonrpc": "2.0",
            "method": "$/progress",
            "params": {
                "token": "indexing",
                "value": { "kind": "end" }
            }
        });
        dispatcher.dispatch_response_for_language("rust", &rust_notif);

        // Rust subscriber should receive it
        let received_rust =
            tokio::time::timeout(tokio::time::Duration::from_millis(100), rx_rust.recv())
                .await
                .expect("should not time out")
                .expect("should receive");

        assert_eq!(
            received_rust["method"].as_str().unwrap_or(""),
            "$/progress",
            "rust subscriber should receive rust notification"
        );

        // Go subscriber should NOT receive it (try_recv should fail)
        assert!(
            rx_go.try_recv().is_err(),
            "go subscriber should NOT receive rust notification"
        );

        // Now send a go notification
        let go_notif = json!({
            "jsonrpc": "2.0",
            "method": "window/logMessage",
            "params": { "message": "hello from go" }
        });
        dispatcher.dispatch_response_for_language("go", &go_notif);

        // Go subscriber should receive
        let received_go =
            tokio::time::timeout(tokio::time::Duration::from_millis(100), rx_go.recv())
                .await
                .expect("should not time out")
                .expect("should receive");

        assert_eq!(
            received_go["method"].as_str().unwrap_or(""),
            "window/logMessage",
            "go subscriber should receive go notification"
        );

        // Rust subscriber should NOT receive go's notification
        assert!(
            rx_rust.try_recv().is_err(),
            "rust subscriber should NOT receive go notification"
        );
    }

    #[tokio::test]
    async fn test_server_request_routing_per_language() {
        // DEL-1.4: client/registerCapability from "rust" only reaches
        // "rust" registration_watcher. This tests the BUG-3 fix.
        let dispatcher = RequestDispatcher::new();

        // Subscribe both languages
        let mut rx_rust = dispatcher.subscribe_server_requests_for_language("rust");
        let mut rx_ts = dispatcher.subscribe_server_requests_for_language("typescript");

        // Rust sends client/registerCapability for pull diagnostics
        let rust_reg = json!({
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
        dispatcher.dispatch_response_for_language("rust", &rust_reg);

        // Rust subscriber should receive
        let received_rust =
            tokio::time::timeout(tokio::time::Duration::from_millis(100), rx_rust.recv())
                .await
                .expect("should not time out")
                .expect("should receive");

        assert_eq!(
            received_rust["method"].as_str().unwrap_or(""),
            "client/registerCapability",
            "rust should receive its own registration"
        );
        assert_eq!(received_rust["id"], 1);

        // TypeScript subscriber should NOT receive rust's registration
        assert!(
            rx_ts.try_recv().is_err(),
            "typescript should NOT receive rust registration"
        );

        // Now TypeScript sends its own registration
        let ts_reg = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "client/registerCapability",
            "params": {
                "registrations": [{
                    "id": "reg-002",
                    "method": "workspace/executeCommand",
                    "registerOptions": {}
                }]
            }
        });
        dispatcher.dispatch_response_for_language("typescript", &ts_reg);

        // TypeScript subscriber should receive
        let received_ts =
            tokio::time::timeout(tokio::time::Duration::from_millis(100), rx_ts.recv())
                .await
                .expect("should not time out")
                .expect("should receive");

        assert_eq!(
            received_ts["method"].as_str().unwrap_or(""),
            "client/registerCapability"
        );
        assert_eq!(received_ts["id"], 2);

        // Rust subscriber should NOT receive TS's registration
        assert!(
            rx_rust.try_recv().is_err(),
            "rust should NOT receive typescript registration"
        );
    }

    #[tokio::test]
    async fn test_registration_watcher_response_sent_to_correct_transport_scenario() {
        // This tests the scenario for BUG-3:
        // Before fix: Rust's registration was applied to TypeScript's live_capabilities
        //             and response was sent to TypeScript's transport.
        // After fix: Registrations are routed per-language, responses go to correct watcher.
        //
        // This unit test verifies the dispatch routing is correct.
        // The full integration test would use FakeTransport with language_id.
        let dispatcher = RequestDispatcher::new();

        // Create two "watchers" subscribed to different languages
        let mut rust_rx = dispatcher.subscribe_server_requests_for_language("rust");
        let mut ts_rx = dispatcher.subscribe_server_requests_for_language("typescript");

        // Rust LSP sends registerCapability
        let rust_msg = json!({
            "jsonrpc": "2.0",
            "id": 100,
            "method": "client/registerCapability",
            "params": { "registrations": [] }
        });
        dispatcher.dispatch_response_for_language("rust", &rust_msg);

        // TypeScript LSP sends unregisterCapability
        let ts_msg = json!({
            "jsonrpc": "2.0",
            "id": 200,
            "method": "client/unregisterCapability",
            "params": { "unregisterations": [] }
        });
        dispatcher.dispatch_response_for_language("typescript", &ts_msg);

        // Verify routing: rust_rx gets id=100, ts_rx gets id=200
        let rust_received =
            tokio::time::timeout(tokio::time::Duration::from_millis(100), rust_rx.recv())
                .await
                .expect("should not time out")
                .expect("should receive");

        let ts_received =
            tokio::time::timeout(tokio::time::Duration::from_millis(100), ts_rx.recv())
                .await
                .expect("should not time out")
                .expect("should receive");

        // Critical assertions for BUG-3 fix:
        // Each language only receives its own server requests
        assert_eq!(rust_received["id"], 100, "rust gets its own registration");
        assert_eq!(
            rust_received["method"].as_str().unwrap_or(""),
            "client/registerCapability"
        );

        assert_eq!(ts_received["id"], 200, "typescript gets its own request");
        assert_eq!(
            ts_received["method"].as_str().unwrap_or(""),
            "client/unregisterCapability"
        );

        // And importantly: no cross-talk
        assert!(rust_rx.try_recv().is_err(), "rust queue is now empty");
        assert!(ts_rx.try_recv().is_err(), "typescript queue is now empty");
    }

    #[tokio::test]
    async fn test_server_request_with_colliding_id_not_treated_as_response() {
        // When a server sends client/registerCapability with an id that happens
        // to match one of our pending request ids, it must be dispatched as a
        // server request (has method), NOT as a response to our pending request.
        let dispatcher = RequestDispatcher::new();

        // Register a pending request — gets id=1
        let (id, mut rx) = dispatcher.register("rust");
        assert_eq!(id, 1);

        let mut server_rx = dispatcher.subscribe_server_requests_for_language("rust");

        // Server sends client/registerCapability with id=1 (same as our pending request)
        let server_req = json!({
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
        dispatcher.dispatch_response_for_language("rust", &server_req);

        // The server request should go to the server_request channel (has method)
        let received =
            tokio::time::timeout(tokio::time::Duration::from_millis(100), server_rx.recv())
                .await
                .expect("should not time out")
                .expect("should receive");

        assert_eq!(
            received["method"].as_str().unwrap_or(""),
            "client/registerCapability",
            "server request with colliding id should be dispatched to server_request channel"
        );

        // The pending request should NOT have been resolved
        assert!(
            rx.try_recv().is_err(),
            "pending request must NOT be resolved by a server request with colliding id"
        );

        // The pending entry should still exist
        assert_eq!(
            dispatcher.pending.lock().unwrap().len(),
            1,
            "pending request should still be registered"
        );
    }
}
