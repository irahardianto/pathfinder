//! JSON-RPC / LSP message framing over stdin/stdout.
//!
//! The LSP wire protocol is:
//!   Content-Length: <N>\r\n
//!   \r\n
//!   <N bytes of UTF-8 JSON>
//!
//! This module provides read/write helpers that are synchronous (no async runtime
//! needed for the mock server — it processes one message at a time).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{self, BufRead, Write};

/// A minimal JSON-RPC 2.0 message.
///
/// The mock server uses a single type for both requests and notifications.
/// Responses are represented separately via [`JsonRpcResponse`].
#[derive(Debug, Deserialize)]
pub struct JsonRpcMessage {
    /// The JSON-RPC version (always "2.0").
    #[allow(dead_code)]
    pub jsonrpc: String,
    /// Request identifier (omitted for notifications).
    pub id: Option<Value>,
    /// Method name to invoke (e.g., "textDocument/hover").
    pub method: Option<String>,
    /// Method parameters (structure varies by method).
    pub params: Option<Value>,
}

/// A JSON-RPC 2.0 response (success or error).
#[derive(Debug, Serialize)]
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
#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    /// The JSON-RPC error code (e.g., `-32601` for method-not-found).
    pub code: i64,
    /// A human-readable description of the error.
    pub message: String,
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
    // Read headers until a blank line
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::default();
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
            content_length = rest.parse().ok();
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

/// Write one JSON-RPC response to `writer` with proper LSP framing.
///
/// # Errors
/// Returns `Err` on serialization or write failure.
pub fn write_response<W: Write>(writer: &mut W, response: &JsonRpcResponse) -> io::Result<()> {
    let body = serde_json::to_string(response)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("serialize error: {e}")))?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    writer.write_all(header.as_bytes())?;
    writer.write_all(body.as_bytes())?;
    writer.flush()?;
    Ok(())
}
