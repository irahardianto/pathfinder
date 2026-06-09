//! Fake LSP transport for testing.
//!
//! This is a test helper that allows configuring mock responses and
//! capturing notifications sent to the LSP server.

#![allow(
    clippy::expect_used,
    clippy::unchecked_time_subtraction,
    clippy::unwrap_or_default,
    dead_code
)]

use super::capabilities::DetectedCapabilities;
use super::process::LspTransport;
use super::protocol::RequestDispatcher;
use crate::LspError;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(crate) struct FailingBehavior {
    pub fail_after_n_requests: Option<usize>,
    pub fail_on_method: Option<String>,
    pub send_error_after_response: bool,
}

pub(crate) struct FakeTransport {
    responses: Arc<Mutex<HashMap<String, VecDeque<Value>>>>,
    notifications_sent: Arc<Mutex<Vec<(String, Value)>>>,
    alive: Arc<AtomicBool>,
    last_used: parking_lot::Mutex<Instant>,
    in_flight: Arc<AtomicU32>,
    capabilities: parking_lot::Mutex<DetectedCapabilities>,
    dispatcher: parking_lot::Mutex<Option<Arc<RequestDispatcher>>>,
    language_id: String,
    failing_behavior: parking_lot::Mutex<Option<FailingBehavior>>,
    request_count: Arc<AtomicU32>,
    response_delay: parking_lot::Mutex<Option<Duration>>,
}

impl FakeTransport {
    pub fn new() -> Self {
        Self {
            responses: Arc::new(Mutex::new(HashMap::new())),
            notifications_sent: Arc::new(Mutex::new(Vec::new())),
            alive: Arc::new(AtomicBool::new(true)),
            last_used: parking_lot::Mutex::new(Instant::now()),
            in_flight: Arc::new(AtomicU32::new(0)),
            capabilities: parking_lot::Mutex::new(DetectedCapabilities::default()),
            dispatcher: parking_lot::Mutex::new(None),
            language_id: "test".to_owned(),
            failing_behavior: parking_lot::Mutex::new(None),
            request_count: Arc::new(AtomicU32::new(0)),
            response_delay: parking_lot::Mutex::new(None),
        }
    }

    pub(super) fn set_dispatcher(&self, dispatcher: Arc<RequestDispatcher>) {
        *self.dispatcher.lock() = Some(dispatcher);
    }

    pub(super) fn set_language_id(&mut self, language_id: &str) {
        self.language_id = language_id.to_owned();
    }

    pub fn set_response(&self, method: &str, result: Value) {
        self.responses
            .lock()
            .expect("responses lock")
            .entry(method.to_owned())
            .or_insert_with(VecDeque::new)
            .push_back(result);
    }

    pub fn set_error(&self, method: &str, error_message: &str) {
        let error_response = serde_json::json!({
            "error": {
                "code": -1,
                "message": error_message
            }
        });
        self.set_response(method, error_response);
    }

    pub fn with_capabilities(&self, caps: DetectedCapabilities) {
        *self.capabilities.lock() = caps;
    }

    pub fn take_notifications(&self) -> Vec<(String, Value)> {
        self.notifications_sent
            .lock()
            .expect("notifications lock")
            .drain(..)
            .collect()
    }

    pub fn kill(&self) {
        self.alive.store(false, Ordering::SeqCst);
    }

    #[allow(dead_code)]
    pub fn is_killed(&self) -> bool {
        !self.alive.load(Ordering::SeqCst)
    }

    pub fn set_failing_behavior(&self, behavior: FailingBehavior) {
        *self.failing_behavior.lock() = Some(behavior);
    }

    pub fn request_count(&self) -> u32 {
        self.request_count.load(Ordering::SeqCst)
    }

    pub fn notification_count(&self) -> usize {
        self.notifications_sent
            .lock()
            .expect("notifications lock")
            .len()
    }

    pub fn set_response_delay(&self, delay: Duration) {
        *self.response_delay.lock() = Some(delay);
    }
}

#[async_trait]
impl LspTransport for FakeTransport {
    async fn send(&self, message: &Value) -> Result<(), LspError> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err(LspError::ConnectionLost);
        }

        let method = message
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();

        let is_request = message.get("id").is_some();
        if is_request {
            let count = self.request_count.fetch_add(1, Ordering::SeqCst) + 1;

            if let Some(ref behavior) = *self.failing_behavior.lock() {
                if let Some(fail_after) = behavior.fail_after_n_requests {
                    if count >= u32::try_from(fail_after).unwrap_or(u32::MAX) {
                        self.alive.store(false, Ordering::SeqCst);
                        return Err(LspError::ConnectionLost);
                    }
                }
                if let Some(ref fail_method) = behavior.fail_on_method {
                    if method == *fail_method {
                        self.alive.store(false, Ordering::SeqCst);
                        return Err(LspError::ConnectionLost);
                    }
                }
            }
        }

        if let Some(id) = message.get("id") {
            let response = {
                let mut responses = self.responses.lock().expect("responses lock");
                if let Some(queue) = responses.get_mut(&method) {
                    let resp = queue.pop_front();
                    if queue.is_empty() {
                        responses.remove(&method);
                    }
                    resp
                } else {
                    None
                }
            };

            let Some(mut response) = response else {
                // Fail fast: no response configured for this method.
                // Without this, the caller's oneshot receiver never fires
                // and the request hangs until timeout.
                return Err(LspError::Protocol(format!(
                    "FakeTransport: no response configured for method '{method}'"
                )));
            };

            if let Some(obj) = response.as_object_mut() {
                obj.insert("id".to_owned(), id.clone());
            }

            let delay = *self.response_delay.lock();
            let is_error = response.get("error").is_some();

            if is_error {
                if let Some(ref dispatcher) = *self.dispatcher.lock() {
                    dispatcher.dispatch_response_for_language(&self.language_id, &response);
                }

                let msg = response["error"]["message"]
                    .as_str()
                    .unwrap_or("fake error");
                return Err(LspError::Protocol(msg.to_owned()));
            }

            if let Some(delay) = delay {
                let dispatcher = self.dispatcher.lock().clone();
                let language_id = self.language_id.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    if let Some(dispatcher) = dispatcher {
                        dispatcher.dispatch_response_for_language(&language_id, &response);
                    }
                });
            } else {
                if let Some(ref dispatcher) = *self.dispatcher.lock() {
                    dispatcher.dispatch_response_for_language(&self.language_id, &response);
                }
            }
            Ok(())
        } else {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            self.notifications_sent
                .lock()
                .expect("notifications lock")
                .push((method, params));
            Ok(())
        }
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    fn last_used(&self) -> Instant {
        *self.last_used.lock()
    }

    fn set_last_used(&self, when: Instant) {
        *self.last_used.lock() = when;
    }

    fn in_flight(&self) -> &Arc<AtomicU32> {
        &self.in_flight
    }

    fn capabilities(&self) -> DetectedCapabilities {
        self.capabilities.lock().clone()
    }

    async fn shutdown(&self, _dispatcher: &RequestDispatcher, _language_id: &str) {
        // No-op for fake transport
    }
}
