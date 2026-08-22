//! JSON-RPC / LSP message framing over stdin/stdout.
//!
//! The LSP wire protocol is:
//!   `Content-Length: <N>\r\n`
//!   `\r\n`
//!   `<N bytes of UTF-8 JSON>`
//!
//! This module provides read/write helpers and framing-level fault injection
//! (`--malform`) for testability (e.g. `LSP-A4`).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{self, BufRead, Write};
use std::str::FromStr;

/// Malformed output injection modes at the framing layer (`LSP-A4`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MalformMode {
    /// Standard well-formed LSP response framing.
    #[default]
    None,
    /// Send an incorrect `Content-Length` header byte count.
    BadContentLength,
    /// Send invalid / truncated JSON in the body with valid Content-Length framing.
    InvalidJson,
    /// Send a JSON-RPC response with `id` but neither `result` nor `error` (`protocol.rs:230`).
    NoResultNoError,
    /// Send a response with `id` encoded as a JSON string (`protocol.rs:197-205`).
    StringId,
    /// Send a response echoing the requested `method` alongside `id` and `result` (`protocol.rs:179-194`).
    EchoMethodInResponse,
}

impl FromStr for MalformMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().replace('_', "-").as_str() {
            "none" => Ok(Self::None),
            "bad-content-length" => Ok(Self::BadContentLength),
            "invalid-json" => Ok(Self::InvalidJson),
            "no-result-no-error" => Ok(Self::NoResultNoError),
            "string-id" => Ok(Self::StringId),
            "echo-method-in-response" => Ok(Self::EchoMethodInResponse),
            other => Err(format!("unknown malform mode: {other:?}")),
        }
    }
}

/// A minimal JSON-RPC 2.0 message (request, notification, or response).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JsonRpcMessage {
    /// The JSON-RPC version (always "2.0").
    #[serde(default = "default_jsonrpc")]
    pub jsonrpc: String,
    /// Request identifier (omitted for notifications).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// Method name to invoke (e.g. "textDocument/hover" or "window/workDoneProgress/create").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// Method parameters (structure varies by method).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    /// Result payload (present on responses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Error details (present on error responses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

fn default_jsonrpc() -> String {
    "2.0".to_string()
}

/// A JSON-RPC 2.0 response (success or error).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    /// The JSON-RPC version (always "2.0").
    pub jsonrpc: &'static str,
    /// Request identifier (matches the request's `id`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// Success result (present only for successful responses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Error details (present only for error responses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// A JSON-RPC 2.0 error object included in error responses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JsonRpcError {
    /// The JSON-RPC error code (e.g. `-32601` for method-not-found, `-32801` for content-modified).
    pub code: i64,
    /// A human-readable description of the error.
    pub message: String,
    /// Optional structured error data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcResponse {
    /// Construct a successful JSON-RPC 2.0 response.
    #[must_use]
    pub const fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Construct a JSON-RPC 2.0 error response.
    pub fn error(id: Option<Value>, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }

    /// Construct a JSON-RPC 2.0 error response with additional data.
    pub fn error_with_data(
        id: Option<Value>,
        code: i64,
        message: impl Into<String>,
        data: Option<Value>,
    ) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data,
            }),
        }
    }
}

/// Read one LSP message from `reader`.
///
/// Returns `None` on clean EOF (client closed the pipe).
///
/// # Errors
/// Returns `Err` on malformed headers or invalid UTF-8 body.
pub fn read_message<R: BufRead>(reader: &mut R) -> io::Result<Option<JsonRpcMessage>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            // Clean EOF
            return Ok(None);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            // End of headers
            break;
        }
        if let Some(rest) = line.strip_prefix("Content-Length: ") {
            content_length = rest.trim().parse().ok();
        }
        // Ignore unknown headers (Content-Type etc.)
    }

    let len = content_length.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length header")
    })?;

    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;

    let msg = serde_json::from_slice(&body).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid JSON-RPC body: {e}"),
        )
    })?;

    Ok(Some(msg))
}

/// Write raw byte body with standard LSP `Content-Length` framing.
pub fn write_framed_bytes<W: Write>(writer: &mut W, body: &[u8]) -> io::Result<()> {
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes())?;
    writer.write_all(body)?;
    writer.flush()?;
    Ok(())
}

/// Write one JSON-RPC response to `writer` with proper LSP framing.
///
/// # Errors
/// Returns `Err` on serialization or write failure.
pub fn write_response<W: Write>(writer: &mut W, response: &JsonRpcResponse) -> io::Result<()> {
    let body = serde_json::to_string(response)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("serialize error: {e}")))?;
    write_framed_bytes(writer, body.as_bytes())
}

/// Write a server-to-client JSON-RPC request.
pub fn write_request<W: Write>(
    writer: &mut W,
    id: Value,
    method: &str,
    params: Option<Value>,
) -> io::Result<()> {
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params.unwrap_or(serde_json::Value::Null)
    });
    let body = req.to_string();
    write_framed_bytes(writer, body.as_bytes())
}

/// Write a server-to-client JSON-RPC notification.
pub fn write_notification<W: Write>(
    writer: &mut W,
    method: &str,
    params: Option<Value>,
) -> io::Result<()> {
    let notif = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params.unwrap_or(serde_json::Value::Null)
    });
    let body = notif.to_string();
    write_framed_bytes(writer, body.as_bytes())
}

/// Write a response with intentional framing or structure malformations (`LSP-A4`).
pub fn write_malformed_response<W: Write>(
    writer: &mut W,
    response: &JsonRpcResponse,
    method: Option<&str>,
    mode: MalformMode,
) -> io::Result<()> {
    match mode {
        MalformMode::None => write_response(writer, response),
        MalformMode::BadContentLength => {
            let body = serde_json::to_string(response).map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("serialize error: {e}"))
            })?;
            // Intentionally corrupt Content-Length by +42 bytes
            let bad_len = body.len() + 42;
            let header = format!("Content-Length: {bad_len}\r\n\r\n");
            writer.write_all(header.as_bytes())?;
            writer.write_all(body.as_bytes())?;
            writer.flush()
        }
        MalformMode::InvalidJson => {
            let corrupted_body =
                "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{corrupted_unclosed_json";
            write_framed_bytes(writer, corrupted_body.as_bytes())
        }
        MalformMode::NoResultNoError => {
            let raw = serde_json::json!({
                "jsonrpc": "2.0",
                "id": response.id
            });
            let body = raw.to_string();
            write_framed_bytes(writer, body.as_bytes())
        }
        MalformMode::StringId => {
            let mut val = serde_json::to_value(response).map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("serialize error: {e}"))
            })?;
            let str_id = match &response.id {
                Some(Value::Number(n)) => Value::String(n.to_string()),
                Some(Value::String(s)) => Value::String(s.clone()),
                _ => Value::String("1".to_string()),
            };
            val["id"] = str_id;
            let body = val.to_string();
            write_framed_bytes(writer, body.as_bytes())
        }
        MalformMode::EchoMethodInResponse => {
            let mut val = serde_json::to_value(response).map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("serialize error: {e}"))
            })?;
            let method_name = method.unwrap_or("textDocument/definition");
            val["method"] = Value::String(method_name.to_string());
            let body = val.to_string();
            write_framed_bytes(writer, body.as_bytes())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_read_valid_message() {
        let body = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}";
        let raw = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        let mut cursor = Cursor::new(raw.as_bytes());
        let msg = read_message(&mut cursor).unwrap().unwrap();

        assert_eq!(msg.id, Some(serde_json::json!(1)));
        assert_eq!(msg.method.as_deref(), Some("initialize"));
    }

    #[test]
    fn test_malform_modes_from_str() {
        assert_eq!("none".parse::<MalformMode>().unwrap(), MalformMode::None);
        assert_eq!(
            "bad-content-length".parse::<MalformMode>().unwrap(),
            MalformMode::BadContentLength
        );
        assert_eq!(
            "invalid-json".parse::<MalformMode>().unwrap(),
            MalformMode::InvalidJson
        );
        assert_eq!(
            "no-result-no-error".parse::<MalformMode>().unwrap(),
            MalformMode::NoResultNoError
        );
        assert_eq!(
            "string-id".parse::<MalformMode>().unwrap(),
            MalformMode::StringId
        );
        assert_eq!(
            "echo-method-in-response".parse::<MalformMode>().unwrap(),
            MalformMode::EchoMethodInResponse
        );
    }

    #[test]
    fn test_write_malformed_string_id() {
        let mut buf = Vec::new();
        let resp = JsonRpcResponse::success(Some(serde_json::json!(42)), serde_json::json!({}));
        write_malformed_response(&mut buf, &resp, None, MalformMode::StringId).unwrap();

        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("\"id\":\"42\""));
    }

    #[test]
    fn test_write_malformed_echo_method() {
        let mut buf = Vec::new();
        let resp = JsonRpcResponse::success(Some(serde_json::json!(1)), serde_json::json!({}));
        write_malformed_response(
            &mut buf,
            &resp,
            Some("textDocument/definition"),
            MalformMode::EchoMethodInResponse,
        )
        .unwrap();

        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("\"method\":\"textDocument/definition\""));
        assert!(output.contains("\"id\":1"));
    }

    #[test]
    fn test_write_malformed_no_result_no_error() {
        let mut buf = Vec::new();
        let resp = JsonRpcResponse::success(Some(serde_json::json!(1)), serde_json::json!({}));
        write_malformed_response(&mut buf, &resp, None, MalformMode::NoResultNoError).unwrap();

        let output = String::from_utf8(buf).unwrap();
        assert!(!output.contains("\"result\""));
        assert!(!output.contains("\"error\""));
        assert!(output.contains("\"id\":1"));
    }
}
