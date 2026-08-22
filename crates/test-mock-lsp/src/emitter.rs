//! Server-initiated message emitter for `test-mock-lsp` (`LSP-Z2`).
//!
//! Emits server-to-client requests and notifications:
//! - Dynamic capability registration via `client/registerCapability` (AC-3.5)
//! - Work done progress via `window/workDoneProgress/create` and `$/progress` (`LSP-A10`)
//! - Configuration queries via `workspace/configuration` (`LSP-A11`)

use crate::config::MockConfig;
use crate::protocol::{write_notification, write_request};
use serde_json::{json, Value};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

static NEXT_REQ_ID: AtomicU64 = AtomicU64::new(1000);

/// Server-initiated message emitter.
#[derive(Debug, Clone)]
pub struct MessageEmitter {
    config: MockConfig,
    started: Arc<AtomicBool>,
}

impl MessageEmitter {
    /// Create a new `MessageEmitter` with `config`.
    #[must_use]
    pub fn new(config: MockConfig) -> Self {
        Self {
            config,
            started: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Start server-initiated background tasks if configured.
    ///
    /// The `writer` is shared behind a mutex to ensure LSP framing atomicity.
    pub fn start_background_emitter<W: Write + Send + 'static>(&self, writer: Arc<Mutex<W>>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }

        let config = self.config.clone();

        thread::spawn(move || {
            // 1. Handle dynamic capability registration
            if let Some(delay_ms) = config.register_after_ms {
                let writer_clone = Arc::clone(&writer);
                let methods = config.register_methods.clone();
                let reg_id_base = config
                    .register_id
                    .clone()
                    .unwrap_or_else(|| "reg-mock".to_string());

                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(delay_ms));
                    emit_dynamic_registration(&writer_clone, &methods, &reg_id_base);
                });
            }

            // 2. Handle workspace configuration request
            if config.config_request || config.config_request_after_ms.is_some() {
                let delay = config.config_request_after_ms.unwrap_or(50);
                let writer_clone = Arc::clone(&writer);
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(delay));
                    emit_config_request(&writer_clone);
                });
            }

            // 3. Handle progress script / burst emission
            if config.progress_script.is_some() || config.progress_count.is_some() {
                let delay = config.progress_after_ms.unwrap_or(50);
                let writer_clone = Arc::clone(&writer);
                let script = config.progress_script.clone();
                let count = config.progress_count;
                thread::spawn(move || {
                    thread::sleep(Duration::from_millis(delay));
                    emit_progress_sequence(&writer_clone, script.as_ref(), count);
                });
            }
        });
    }
}

/// Emit `client/registerCapability` request.
pub fn emit_dynamic_registration<W: Write>(
    writer: &Arc<Mutex<W>>,
    methods: &[String],
    reg_id_base: &str,
) {
    let req_id = NEXT_REQ_ID.fetch_add(1, Ordering::SeqCst);
    let effective_methods = if methods.is_empty() {
        vec!["textDocument/definition".to_string()]
    } else {
        methods.to_vec()
    };

    let registrations: Vec<Value> = effective_methods
        .iter()
        .enumerate()
        .map(|(idx, m)| {
            json!({
                "id": format!("{reg_id_base}-{idx}"),
                "method": m,
                "registerOptions": {}
            })
        })
        .collect();

    let params = json!({
        "registrations": registrations
    });

    if let Ok(mut guard) = writer.lock() {
        let _ = write_request(
            &mut *guard,
            json!(req_id),
            "client/registerCapability",
            Some(params),
        );
    }
}

/// Emit `workspace/configuration` request (`LSP-A11`).
pub fn emit_config_request<W: Write>(writer: &Arc<Mutex<W>>) {
    let req_id = NEXT_REQ_ID.fetch_add(1, Ordering::SeqCst);
    let params = json!({
        "items": [
            { "section": "pathfinder" }
        ]
    });

    if let Ok(mut guard) = writer.lock() {
        let _ = write_request(
            &mut *guard,
            json!(req_id),
            "workspace/configuration",
            Some(params),
        );
    }
}

/// Emit `window/workDoneProgress/create` and `$/progress` sequence (`LSP-A10`).
pub fn emit_progress_sequence<W: Write>(
    writer: &Arc<Mutex<W>>,
    script: Option<&Value>,
    burst_count: Option<usize>,
) {
    let create_id = NEXT_REQ_ID.fetch_add(1, Ordering::SeqCst);
    let token = "mock-progress-token";

    // 1. Send workDoneProgress/create request
    if let Ok(mut guard) = writer.lock() {
        let _ = write_request(
            &mut *guard,
            json!(create_id),
            "window/workDoneProgress/create",
            Some(json!({ "token": token })),
        );
    }

    // Small yield to allow create to dispatch
    thread::sleep(Duration::from_millis(10));

    // 2. Handle burst count (e.g. 100 notifications to test 64-slot channel capacity)
    if let Some(count) = burst_count {
        if let Ok(mut guard) = writer.lock() {
            let _ = write_notification(
                &mut *guard,
                "$/progress",
                Some(json!({
                    "token": token,
                    "value": {
                        "kind": "begin",
                        "title": "Indexing",
                        "percentage": 0
                    }
                })),
            );

            for i in 1..=count {
                let pct = (i * 100) / count;
                let _ = write_notification(
                    &mut *guard,
                    "$/progress",
                    Some(json!({
                        "token": token,
                        "value": {
                            "kind": "report",
                            "percentage": pct,
                            "message": format!("{pct}%")
                        }
                    })),
                );
            }

            let _ = write_notification(
                &mut *guard,
                "$/progress",
                Some(json!({
                    "token": token,
                    "value": {
                        "kind": "end",
                        "message": "Complete"
                    }
                })),
            );
        }
        return;
    }

    // 3. Handle scripted progress
    if let Some(val) = script {
        let steps = match val {
            Value::Array(arr) => arr.clone(),
            Value::String(s) => vec![Value::String(s.clone())],
            _ => vec![],
        };

        if let Ok(mut guard) = writer.lock() {
            for step in steps {
                let payload = match step {
                    Value::String(s) if s == "begin" => json!({
                        "token": token,
                        "value": { "kind": "begin", "title": "Indexing" }
                    }),
                    Value::String(s) if s == "end" => json!({
                        "token": token,
                        "value": { "kind": "end", "message": "Done" }
                    }),
                    Value::String(s) if s.starts_with("report:") => {
                        let pct: u32 = s["report:".len()..].trim().parse().unwrap_or(50);
                        json!({
                            "token": token,
                            "value": { "kind": "report", "percentage": pct }
                        })
                    }
                    Value::Object(obj) => json!({
                        "token": token,
                        "value": obj
                    }),
                    _ => continue,
                };

                let _ = write_notification(&mut *guard, "$/progress", Some(payload));
            }
        }
    }
}
