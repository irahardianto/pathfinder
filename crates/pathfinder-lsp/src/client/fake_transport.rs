use super::capabilities::DetectedCapabilities;
use super::process::LspTransport;
use super::protocol::RequestDispatcher;
use crate::LspError;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub(crate) struct FakeTransport {
    responses: Arc<Mutex<HashMap<String, VecDeque<Value>>>>,
    notifications_sent: Arc<Mutex<Vec<(String, Value)>>>,
    alive: Arc<AtomicBool>,
    last_used: Mutex<Instant>,
    in_flight: Arc<AtomicU32>,
    capabilities: Mutex<DetectedCapabilities>,
    dispatcher: Mutex<Option<Arc<RequestDispatcher>>>,
}

impl FakeTransport {
    pub fn new() -> Self {
        Self {
            responses: Arc::new(Mutex::new(HashMap::new())),
            notifications_sent: Arc::new(Mutex::new(Vec::new())),
            alive: Arc::new(AtomicBool::new(true)),
            last_used: Mutex::new(Instant::now()),
            in_flight: Arc::new(AtomicU32::new(0)),
            capabilities: Mutex::new(DetectedCapabilities::default()),
            dispatcher: Mutex::new(None),
        }
    }

    pub fn set_dispatcher(&self, dispatcher: Arc<RequestDispatcher>) {
        *self.dispatcher.lock().expect("dispatcher lock") = Some(dispatcher);
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
        *self.capabilities.lock().expect("capabilities lock") = caps;
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

            if let Some(mut response) = response {
                if let Some(obj) = response.as_object_mut() {
                    obj.insert("id".to_owned(), id.clone());
                }

                if let Some(ref dispatcher) = *self.dispatcher.lock().expect("dispatcher lock") {
                    dispatcher.dispatch_response(&response);
                }

                if response.get("error").is_some() {
                    let msg = response["error"]["message"]
                        .as_str()
                        .unwrap_or("fake error");
                    return Err(LspError::Protocol(msg.to_owned()));
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
        *self.last_used.lock().expect("last_used lock")
    }

    fn set_last_used(&self, when: Instant) {
        *self.last_used.lock().expect("last_used lock") = when;
    }

    fn in_flight(&self) -> &Arc<AtomicU32> {
        &self.in_flight
    }

    fn capabilities(&self) -> DetectedCapabilities {
        self.capabilities.lock().expect("capabilities lock").clone()
    }

    async fn shutdown(&self, _dispatcher: &RequestDispatcher) {
        // No-op for fake transport
    }
}
