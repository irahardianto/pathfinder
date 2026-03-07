//! JSON-RPC request/response correlation.
//!
//! `RequestDispatcher` maintains a map of in-flight requests keyed by their
//! JSON-RPC `id`. When the background reader task receives a response, it
//! calls [`RequestDispatcher::dispatch_response`] to fire the matching
//! [`tokio::sync::oneshot`] and wake the caller.

use crate::LspError;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tokio::sync::oneshot;

/// Correlates outgoing JSON-RPC requests with their responses.
///
/// All methods take `&self` (interior mutability via `Mutex`) so the
/// dispatcher can be shared across the writer and reader tasks via `Arc`.
pub(super) struct RequestDispatcher {
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, LspError>>>>,
    next_id: AtomicU64,
}

impl RequestDispatcher {
    pub(super) fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
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
    /// oneshot. Notifications (no `id`) and unmatched responses are silently
    /// ignored (they are handled elsewhere or are unsolicited server messages).
    #[allow(clippy::expect_used)] // Mutex poisoning is unrecoverable
    pub(super) fn dispatch_response(&self, message: &Value) {
        let Some(id_val) = message.get("id") else {
            // Notification — no correlation needed
            return;
        };
        let Some(id) = id_val.as_u64() else {
            return;
        };

        let tx = self
            .pending
            .lock()
            .expect("dispatcher lock")
            .remove(&id);

        if let Some(sender) = tx {
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
        }
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
}
