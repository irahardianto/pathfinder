//! JSON-RPC request/response correlation.
//!
//! `RequestDispatcher` maintains a map of in-flight requests keyed by their
//! `id`. When the background reader task receives a response, it
//! calls [`RequestDispatcher::dispatch_response`] to fire the matching
//! [`tokio::sync::oneshot`] and wake the caller.

use crate::LspError;
use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{broadcast, oneshot};

/// Capacity of the server-notification broadcast channel.
/// Older notifications are dropped silently if all subscribers are slow.
const NOTIFICATION_CHANNEL_CAPACITY: usize = 64;

/// Correlates outgoing JSON-RPC requests with their responses.
///
/// All methods take `&self` (interior mutability via `DashMap`) so the
/// dispatcher can be shared across the writer and reader tasks via `Arc`.
///
/// LSP-INIT-002: Per-language isolation prevents cross-language interference:
///
/// - Pending requests are tagged with `language_id` so `cancel_all` can be scoped
/// - Notifications and server requests are routed through per-language channels
type PendingRequest = (String, Option<oneshot::Sender<Result<Value, LspError>>>);

pub(crate) struct RequestDispatcher {
    /// `DashMap` for concurrent request registration, dispatch, and cancellation.
    /// ID-based sharding gives natural parallelism across languages.
    pending: DashMap<u64, PendingRequest>,
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
            pending: DashMap::new(),
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
    pub(crate) fn register(
        &self,
        language_id: &str,
    ) -> (u64, oneshot::Receiver<Result<Value, LspError>>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, (language_id.to_owned(), Some(tx)));
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
    pub(crate) fn dispatch_response_for_language(&self, source_language_id: &str, message: &Value) {
        // M-2: Handle JSON-RPC batch arrays — dispatch each element individually.
        if let Some(batch) = message.as_array() {
            for item in batch {
                self.dispatch_response_for_language(source_language_id, item);
            }
            return;
        }

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

        // M-1: Check for method BEFORE ID type to handle string-ID server requests.
        // A message with both `id` and `method` is a server-to-client request
        // (e.g. client/registerCapability), not a response to our request.
        if message.get("method").is_some() {
            let method = message
                .get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            tracing::debug!(
                language = %source_language_id,
                id = %id_val,
                method = %method,
                "LSP: dispatching server-to-client request"
            );
            if let Some(tx) = self.server_request_channels.get(source_language_id) {
                let _ = tx.send(message.clone());
            }
            return;
        }

        // M-1: Now check ID type. Only u64 IDs can be correlated with pending requests.
        let Some(id) = id_val.as_u64() else {
            // String ID on a non-method message — likely a malformed response.
            tracing::warn!(
                language = %source_language_id,
                id = %id_val,
                "LSP: received non-u64 response ID (string IDs are spec-compliant but uncommon)"
            );
            return;
        };

        // Normal response to a request we sent (has id, no method).
        if let Some((_id, (_lang, Some(sender)))) = self.pending.remove(&id) {
            let is_error = message.get("error").is_some();
            tracing::debug!(
                language = %source_language_id,
                id = %id,
                is_error = %is_error,
                "LSP: dispatching response to pending request"
            );
            let result = if is_error {
                let error_obj = &message["error"];
                let code = error_obj["code"].as_i64().unwrap_or(0);
                let err_msg = error_obj["message"]
                    .as_str()
                    .unwrap_or("LSP returned an error")
                    .to_owned();
                let data = error_obj.get("data").cloned();
                Err(LspError::ServerError {
                    code,
                    message: err_msg,
                    data,
                })
            } else {
                Ok(message["result"].clone())
            };
            let _ = sender.send(result);
        } else {
            // L-8: Log unmatched responses with categorization.
            let has_error = message.get("error").is_some();
            let error_code = message
                .get("error")
                .and_then(|e| e.get("code"))
                .and_then(serde_json::Value::as_i64);
            tracing::warn!(
                language = %source_language_id,
                id = %id,
                has_error = has_error,
                error_code = error_code,
                "LSP: unmatched response — request timed out or ID never registered"
            );
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
    pub(crate) fn remove(&self, id: u64) {
        self.pending.remove(&id);
    }

    #[cfg(test)]
    pub(crate) fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Cancel all pending requests with `LspError::ConnectionLost`.
    ///
    /// Called during shutdown to unblock all waiting callers across all languages.
    /// LSP-INIT-002: For per-language cancellation (when one language crashes),
    /// use `cancel_for_language()` instead.
    #[allow(dead_code)] // Kept for tests and future use
    pub(crate) fn cancel_all(&self) {
        let mut senders = Vec::new();
        self.pending.retain(|_id, (_lang, tx_opt)| {
            if let Some(tx) = tx_opt.take() {
                senders.push(tx);
            }
            false
        });
        for tx in senders {
            let _ = tx.send(Err(LspError::ConnectionLost));
        }
    }

    /// Cancel only pending requests for a specific language with `LspError::ConnectionLost`.
    ///
    /// LSP-INIT-002: Called when a single language's LSP process crashes.
    /// This isolates the crash to only that language's pending requests,
    /// leaving other languages' requests unaffected.
    pub(crate) fn cancel_for_language(&self, language_id: &str) {
        let mut senders = Vec::new();
        self.pending.retain(|_id, (lang, tx_opt)| {
            if lang == language_id {
                if let Some(tx) = tx_opt.take() {
                    senders.push(tx);
                }
                false
            } else {
                true
            }
        });

        let count = senders.len();
        if count > 0 {
            tracing::debug!(
                language = %language_id,
                count = %count,
                "LSP: cancel_for_language: cancelling pending requests"
            );
        }

        for tx in senders {
            let _ = tx.send(Err(LspError::ConnectionLost));
        }
    }
}

#[cfg(test)]
#[path = "protocol_test.rs"]
mod tests;
