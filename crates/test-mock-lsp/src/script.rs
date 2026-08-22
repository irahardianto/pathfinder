//! Scriptable response timeline engine for `test-mock-lsp` (`LSP-Z2`).
//!
//! Enables deterministic simulation of:
//! - Slow indexing (e.g. empty responses for 20s then populated)
//! - Empty-then-populated sequences (after N requests)
//! - Request timeouts / hangs
//! - Specific error codes (e.g. `-32801` ContentModified, `-32802` RequestCancelled)
//! - Mid-session crashes on specific methods
//! - Framing malformations per method

use crate::protocol::MalformMode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::str::FromStr;
use std::time::{Duration, Instant};

/// An action or response produced by a script rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptAction {
    /// Return a successful JSON-RPC response with the given payload.
    Respond(Value),
    /// Return a JSON-RPC error response with code and message.
    Error {
        /// JSON-RPC error code.
        code: i64,
        /// Human-readable error message.
        message: String,
        /// Optional structured error data.
        data: Option<Value>,
    },
    /// Hang the handler (sleep indefinitely) to test client request timeouts.
    Hang,
    /// Terminate the mock server process immediately (`std::process::exit(1)`).
    Crash,
    /// Emit a malformed response frame per [`MalformMode`].
    Malformed {
        /// Malform mode to apply.
        mode: MalformMode,
        /// Response payload.
        payload: Value,
    },
}

/// A single rule in a response timeline script.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptRule {
    /// Method pattern to match (e.g. `"textDocument/prepareCallHierarchy"`, `"initialize"`, `"*"`).
    pub method: String,
    /// Minimum elapsed time in milliseconds from server start before this rule activates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_ms: Option<u64>,
    /// Minimum number of requests (either for this method or globally) before this rule activates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_n_requests: Option<usize>,
    /// Raw response payload or action keyword (`"hang"`, `"crash"`, `"error:<code>"`, `"error:<code:msg>"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Value>,
    /// Explicit action keyword (`"hang"`, `"crash"`, `"error:<code>"`, `"malform:<mode>"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Explicit error code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<i64>,
    /// Explicit error message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Malformed framing mode for this rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub malform: Option<MalformMode>,
    /// Milliseconds to sleep before executing this rule's action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<u64>,
}

impl ScriptRule {
    /// Parse this rule's configured action into a [`ScriptAction`].
    #[must_use]
    pub fn resolved_action(&self) -> ScriptAction {
        // 1. Check explicit error code / message
        if let Some(code) = self.error_code {
            return ScriptAction::Error {
                code,
                message: self
                    .error_message
                    .clone()
                    .unwrap_or_else(|| default_error_message(code)),
                data: None,
            };
        }

        // 2. Check explicit malform mode
        if let Some(mode) = self.malform {
            let payload = self.response.clone().unwrap_or(Value::Null);
            return ScriptAction::Malformed { mode, payload };
        }

        // 3. Check explicit action string
        if let Some(action_str) = &self.action {
            if let Some(action) = parse_action_string(action_str, self.response.as_ref()) {
                return action;
            }
        }

        // 4. Check string response for action keywords
        if let Some(Value::String(s)) = &self.response {
            if let Some(action) = parse_action_string(s, None) {
                return action;
            }
        }

        // 5. Standard success response
        ScriptAction::Respond(self.response.clone().unwrap_or(Value::Null))
    }
}

fn parse_action_string(s: &str, fallback_payload: Option<&Value>) -> Option<ScriptAction> {
    let trimmed = s.trim();
    let lower = trimmed.to_lowercase();
    if lower == "hang" {
        return Some(ScriptAction::Hang);
    }
    if lower == "crash" {
        return Some(ScriptAction::Crash);
    }
    if lower.starts_with("error:") {
        let rest = &trimmed["error:".len()..];
        let parts: Vec<&str> = rest.splitn(2, ':').collect();
        if let Ok(code) = parts[0].trim().parse::<i64>() {
            let message = if parts.len() > 1 && !parts[1].trim().is_empty() {
                parts[1].trim().to_string()
            } else {
                default_error_message(code)
            };
            return Some(ScriptAction::Error {
                code,
                message,
                data: None,
            });
        }
    }
    if let Some(rest) = lower.strip_prefix("malform:") {
        if let Ok(mode) = MalformMode::from_str(rest.trim()) {
            let payload = fallback_payload.cloned().unwrap_or(Value::Null);
            return Some(ScriptAction::Malformed { mode, payload });
        }
    }
    None
}

fn default_error_message(code: i64) -> String {
    match code {
        -32700 => "Parse error".to_string(),
        -32600 => "Invalid Request".to_string(),
        -32601 => "Method not found".to_string(),
        -32602 => "Invalid params".to_string(),
        -32603 => "Internal error".to_string(),
        -32800 => "Request cancelled".to_string(),
        -32801 => "Content modified".to_string(),
        -32802 => "Server cancelled".to_string(),
        -32803 => "Request failed".to_string(),
        _ => format!("LSP error {code}"),
    }
}

/// The script evaluation result for an incoming request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptEvaluation {
    /// The action to execute.
    pub action: ScriptAction,
    /// Optional pre-execution delay.
    pub delay: Option<Duration>,
}

/// Active script execution engine.
#[derive(Debug, Clone)]
pub struct ScriptEngine {
    rules: Vec<ScriptRule>,
    start_time: Instant,
    global_request_count: usize,
    method_request_counts: HashMap<String, usize>,
}

impl ScriptEngine {
    /// Create a new `ScriptEngine` with the given rules.
    #[must_use]
    pub fn new(rules: Vec<ScriptRule>) -> Self {
        Self {
            rules,
            start_time: Instant::now(),
            global_request_count: 0,
            method_request_counts: HashMap::new(),
        }
    }

    /// Parse a JSON string into a `ScriptEngine`.
    ///
    /// Accepts either a JSON array of rules or a single rule object.
    ///
    /// # Errors
    /// Returns an error message if JSON parsing or rule validation fails.
    pub fn from_json_str(json_str: &str) -> Result<Self, String> {
        let val: Value =
            serde_json::from_str(json_str).map_err(|e| format!("invalid script JSON: {e}"))?;
        Self::from_value(&val)
    }

    /// Construct a `ScriptEngine` from a `serde_json::Value`.
    ///
    /// # Errors
    /// Returns an error message if the value cannot be parsed into rules.
    pub fn from_value(val: &Value) -> Result<Self, String> {
        let rules: Vec<ScriptRule> = match val {
            Value::Array(_) => serde_json::from_value(val.clone())
                .map_err(|e| format!("failed to parse script rules array: {e}"))?,
            Value::Object(_) => {
                let single: ScriptRule = serde_json::from_value(val.clone())
                    .map_err(|e| format!("failed to parse script rule object: {e}"))?;
                vec![single]
            }
            _ => return Err("script must be a JSON array or object".to_string()),
        };
        Ok(Self::new(rules))
    }

    /// Set an explicit start time (useful for deterministic tests).
    pub fn set_start_time(&mut self, time: Instant) {
        self.start_time = time;
    }

    /// Evaluate incoming request against the script timeline.
    ///
    /// Increments request counters and checks rules in declared order.
    /// Returns `Some(ScriptEvaluation)` if a rule matches, or `None` to use default handlers.
    pub fn evaluate(&mut self, method: &str) -> Option<ScriptEvaluation> {
        self.global_request_count += 1;
        let method_count = self
            .method_request_counts
            .entry(method.to_string())
            .or_insert(0);
        *method_count += 1;
        let cur_method_count = *method_count;
        let global_count = self.global_request_count;
        let elapsed_ms = self.start_time.elapsed().as_millis() as u64;

        for rule in &self.rules {
            // 1. Method match
            if rule.method != "*" && rule.method != method {
                continue;
            }

            // 2. Timeline delay gate (`after_ms`)
            if let Some(min_ms) = rule.after_ms {
                if elapsed_ms < min_ms {
                    continue;
                }
            }

            // 3. Request count gate (`after_n_requests`)
            if let Some(min_reqs) = rule.after_n_requests {
                // Rule activates if either method request count or global request count reaches threshold
                if cur_method_count < min_reqs && global_count < min_reqs {
                    continue;
                }
            }

            // Rule matches!
            let action = rule.resolved_action();
            let delay = rule.delay_ms.map(Duration::from_millis);
            return Some(ScriptEvaluation { action, delay });
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_script_timeline_slow_indexing() {
        let script_json = json!([
            {
                "method": "textDocument/prepareCallHierarchy",
                "after_ms": 20000,
                "response": [{ "name": "foo", "kind": 12 }]
            },
            {
                "method": "textDocument/prepareCallHierarchy",
                "response": []
            }
        ]);

        let mut engine = ScriptEngine::from_value(&script_json).unwrap();
        let t0 = Instant::now();
        engine.set_start_time(t0);

        // At t = 0ms, should match the second rule (empty array)
        let eval1 = engine
            .evaluate("textDocument/prepareCallHierarchy")
            .unwrap();
        assert_eq!(eval1.action, ScriptAction::Respond(json!([])));

        // Simulate advancing time past 20s
        engine.set_start_time(t0 - Duration::from_millis(20001));

        // At t > 20s, should match the first rule (non-empty array)
        let eval2 = engine
            .evaluate("textDocument/prepareCallHierarchy")
            .unwrap();
        assert_eq!(
            eval2.action,
            ScriptAction::Respond(json!([{ "name": "foo", "kind": 12 }]))
        );
    }

    #[test]
    fn test_script_after_n_requests() {
        let script_json = json!([
            {
                "method": "textDocument/definition",
                "after_n_requests": 2,
                "response": { "uri": "file:///src/lib.rs" }
            },
            {
                "method": "textDocument/definition",
                "response": null
            }
        ]);

        let mut engine = ScriptEngine::from_value(&script_json).unwrap();

        // 1st request -> null
        let eval1 = engine.evaluate("textDocument/definition").unwrap();
        assert_eq!(eval1.action, ScriptAction::Respond(json!(null)));

        // 2nd request -> non-empty
        let eval2 = engine.evaluate("textDocument/definition").unwrap();
        assert_eq!(
            eval2.action,
            ScriptAction::Respond(json!({ "uri": "file:///src/lib.rs" }))
        );
    }

    #[test]
    fn test_script_error_injection() {
        let script_json = json!([
            {
                "method": "textDocument/definition",
                "action": "error:-32801:Content modified"
            }
        ]);

        let mut engine = ScriptEngine::from_value(&script_json).unwrap();
        let eval = engine.evaluate("textDocument/definition").unwrap();

        assert_eq!(
            eval.action,
            ScriptAction::Error {
                code: -32801,
                message: "Content modified".to_string(),
                data: None,
            }
        );
    }

    #[test]
    fn test_script_hang_and_crash() {
        let script_json = json!([
            { "method": "textDocument/references", "response": "hang" },
            { "method": "textDocument/implementation", "action": "crash" }
        ]);

        let mut engine = ScriptEngine::from_value(&script_json).unwrap();

        let eval_hang = engine.evaluate("textDocument/references").unwrap();
        assert_eq!(eval_hang.action, ScriptAction::Hang);

        let eval_crash = engine.evaluate("textDocument/implementation").unwrap();
        assert_eq!(eval_crash.action, ScriptAction::Crash);
    }
}
