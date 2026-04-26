//! JSON-RPC message framing over stdin/stdout.
//!
//! The LSP wire protocol uses HTTP-style headers followed by a JSON body:
//!
//! ```text
//! Content-Length: <N>\r\n
//! \r\n
//! {"jsonrpc":"2.0","id":1,...}
//! ```
//!
//! Both functions operate on `&mut` I/O handles and are designed to be composed
//! in the background reader/writer tasks of [`LspClient`].

use crate::LspError;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

/// Read one JSON-RPC message from `reader`.
///
/// Parses the `Content-Length` header, reads that many bytes, and
/// deserialises the body as a `serde_json::Value`.
///
/// # Errors
/// Returns `LspError::Io` if the underlying read fails or EOF is reached
/// mid-message. Returns `LspError::Protocol` if the header is malformed
/// or the body is not valid JSON.
pub(super) async fn read_message<R>(reader: &mut BufReader<R>) -> Result<Value, LspError>
where
    R: AsyncReadExt + Unpin,
{
    const MAX_MESSAGE_SIZE: usize = 50 * 1024 * 1024; // 50MB

    // Parse headers until blank line
    let mut content_length: Option<usize> = None;

    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.map_err(|e| {
            LspError::Io(std::io::Error::new(
                e.kind(),
                format!("reading LSP header: {e}"),
            ))
        })?;
        if n == 0 {
            return Err(LspError::ConnectionLost);
        }

        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            // Blank line separates headers from body
            break;
        }

        if let Some(value) = trimmed
            .strip_prefix("Content-Length: ")
            .or_else(|| trimmed.strip_prefix("content-length: "))
        {
            content_length = Some(value.parse::<usize>().map_err(|_| {
                LspError::Protocol(format!("invalid Content-Length value: {value}"))
            })?);
        }
        // Ignore other headers (Content-Type etc.)
    }

    let length = content_length.ok_or_else(|| {
        LspError::Protocol("LSP message missing Content-Length header".to_owned())
    })?;

    if length > MAX_MESSAGE_SIZE {
        return Err(LspError::Protocol(format!(
            "LSP message size {length} bytes exceeds maximum allowed size of {MAX_MESSAGE_SIZE} bytes"
        )));
    }

    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await.map_err(|e| {
        LspError::Io(std::io::Error::new(
            e.kind(),
            format!("reading LSP body: {e}"),
        ))
    })?;

    let body_value: Value = serde_json::from_slice(&body)
        .map_err(|e| LspError::Protocol(format!("invalid JSON in LSP message: {e}")))?;

    tracing::debug!(message = %body_value, "LSP RECV");

    Ok(body_value)
}

/// Write one JSON-RPC message to `writer`.
///
/// Serialises `message` to JSON, prepends the `Content-Length` header,
/// and flushes the writer.
///
/// # Errors
/// Returns `LspError::Io` if serialisation or the underlying write fails.
pub(super) async fn write_message<W>(writer: &mut W, message: &Value) -> Result<(), LspError>
where
    W: AsyncWriteExt + Unpin,
{
    tracing::debug!(message = %message, "LSP SEND");

    let body = serde_json::to_vec(message)
        .map_err(|e| LspError::Protocol(format!("serialising JSON-RPC message: {e}")))?;

    let header = format!("Content-Length: {}\r\n\r\n", body.len());

    writer.write_all(header.as_bytes()).await.map_err(|e| {
        LspError::Io(std::io::Error::new(
            e.kind(),
            format!("writing LSP header: {e}"),
        ))
    })?;
    writer.write_all(&body).await.map_err(|e| {
        LspError::Io(std::io::Error::new(
            e.kind(),
            format!("writing LSP body: {e}"),
        ))
    })?;
    writer.flush().await.map_err(|e| {
        LspError::Io(std::io::Error::new(
            e.kind(),
            format!("flushing LSP writer: {e}"),
        ))
    })?;

    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::BufReader;

    async fn roundtrip(msg: Value) -> Value {
        // Write into an in-memory buffer
        let mut buf: Vec<u8> = Vec::new();
        write_message(&mut buf, &msg).await.expect("write");

        // Read back from the buffer
        let mut reader = BufReader::new(buf.as_slice());
        read_message(&mut reader).await.expect("read")
    }

    #[tokio::test]
    async fn test_roundtrip_request() {
        let msg = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "textDocument/definition",
            "params": { "textDocument": { "uri": "file:///foo.rs" } }
        });
        let result = roundtrip(msg.clone()).await;
        assert_eq!(result, msg);
    }

    #[tokio::test]
    async fn test_roundtrip_notification() {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {}
        });
        let result = roundtrip(msg.clone()).await;
        assert_eq!(result, msg);
    }

    #[tokio::test]
    async fn test_missing_content_length_is_error() {
        // Manually crafted message without header
        let bad = b"\r\n{\"jsonrpc\":\"2.0\"}";
        let mut reader = BufReader::new(bad.as_slice());
        let result = read_message(&mut reader).await;
        assert!(result.is_err());
        match result {
            Err(LspError::Protocol(msg)) => assert!(msg.contains("Content-Length")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_eof_returns_connection_lost() {
        let empty: &[u8] = b"";
        let mut reader = BufReader::new(empty);
        let result = read_message(&mut reader).await;
        assert!(matches!(result, Err(LspError::ConnectionLost)));
    }

    #[tokio::test]
    async fn test_oversized_content_length_is_rejected() {
        // 50MB + 1 byte — just over the limit
        let oversized = b"Content-Length: 52428801\r\n\r\n";
        let mut reader = BufReader::new(oversized.as_slice());
        let result = read_message(&mut reader).await;
        match result {
            Err(LspError::Protocol(msg)) => assert!(
                msg.contains("exceeds"),
                "expected 'exceeds' in error message, got: {msg}"
            ),
            other => panic!("expected Protocol error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_case_insensitive_content_length() {
        // lowercase "content-length" header should also work
        let msg = json!({"jsonrpc": "2.0", "id": 1, "result": null});
        let body = serde_json::to_vec(&msg).unwrap();
        let framed = format!("content-length: {}\r\n\r\n", body.len());
        let mut buf: Vec<u8> = framed.as_bytes().to_vec();
        buf.extend_from_slice(&body);

        let mut reader = BufReader::new(buf.as_slice());
        let result = read_message(&mut reader).await;
        assert!(result.is_ok(), "lowercase header should parse: {result:?}");
    }

    #[tokio::test]
    async fn test_extra_headers_ignored() {
        // Content-Type and other headers should be silently ignored
        let msg = json!({"jsonrpc": "2.0", "id": 1, "result": 42});
        let body = serde_json::to_vec(&msg).unwrap();
        let framed = format!(
            "Content-Length: {}\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n",
            body.len()
        );
        let mut buf: Vec<u8> = framed.as_bytes().to_vec();
        buf.extend_from_slice(&body);

        let mut reader = BufReader::new(buf.as_slice());
        let result = read_message(&mut reader).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap()["result"], 42);
    }

    #[tokio::test]
    async fn test_invalid_content_length_value() {
        let bad = b"Content-Length: not_a_number\r\n\r\n";
        let mut reader = BufReader::new(bad.as_slice());
        let result = read_message(&mut reader).await;
        assert!(result.is_err());
        match result {
            Err(LspError::Protocol(msg)) => {
                assert!(msg.contains("invalid Content-Length"), "got: {msg}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_invalid_json_body() {
        let body = b"{not valid json}";
        let framed = format!("Content-Length: {}\r\n\r\n", body.len());
        let mut buf: Vec<u8> = framed.as_bytes().to_vec();
        buf.extend_from_slice(body);

        let mut reader = BufReader::new(buf.as_slice());
        let result = read_message(&mut reader).await;
        assert!(result.is_err());
        match result {
            Err(LspError::Protocol(msg)) => assert!(msg.contains("invalid JSON")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_write_message_format() {
        // Verify the wire format produced by write_message
        let mut writer: Vec<u8> = Vec::new();
        let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "test"});
        write_message(&mut writer, &msg)
            .await
            .expect("vec write should succeed");

        // Should start with Content-Length header
        let written = String::from_utf8(writer.clone()).expect("valid utf8");
        assert!(written.starts_with("Content-Length: "));
        // Should contain the double CRLF separator
        assert!(written.contains("\r\n\r\n"));
        // After the separator, should be valid JSON
        let body_start = written.find("\r\n\r\n").unwrap() + 4;
        let body: serde_json::Value =
            serde_json::from_str(&written[body_start..]).expect("valid json body");
        assert_eq!(body["id"], 1);
    }
}
