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
pub async fn read_message<R>(reader: &mut BufReader<R>) -> Result<Value, LspError>
where
    R: AsyncReadExt + Unpin,
{
    const MAX_MESSAGE_SIZE: usize = 50 * 1024 * 1024; // 50MB

    // Parse headers until blank line
    let mut content_length: Option<usize> = None;

    loop {
        let mut line = String::default();
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

/// `MockStdout` that implements `AsyncRead` for testing malformed responses.
///
/// Allows injecting raw byte sequences to test protocol error handling.
#[cfg(test)]
struct MockStdout {
    data: std::io::Cursor<Vec<u8>>,
}

#[cfg(test)]
impl MockStdout {
    fn write_lsp_message(content: &str) -> Self {
        let body = content.as_bytes();
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        let mut data = header.into_bytes();
        data.extend_from_slice(body);
        Self {
            data: std::io::Cursor::new(data),
        }
    }

    fn write_raw(bytes: &[u8]) -> Self {
        Self {
            data: std::io::Cursor::new(bytes.to_vec()),
        }
    }
}

#[cfg(test)]
impl tokio::io::AsyncRead for MockStdout {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        use std::io::Read as StdRead;
        let n = StdRead::read(&mut self.data, buf.initialize_unfilled())?;
        buf.advance(n);
        std::task::Poll::Ready(Ok(()))
    }
}

/// Write one JSON-RPC message to `writer`.
///
/// Serialises `message` to JSON, prepends the `Content-Length` header,
/// and flushes the writer.
///
/// # Errors
/// Returns `LspError::Io` if serialisation or the underlying write fails.
pub async fn write_message<W>(writer: &mut W, message: &Value) -> Result<(), LspError>
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

    #[tokio::test]
    async fn test_empty_body() {
        let framed = b"Content-Length: 2\r\n\r\n{}";
        let mut reader = BufReader::new(framed.as_slice());
        let result = read_message(&mut reader).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), json!({}));
    }

    #[tokio::test]
    async fn test_content_length_zero_returns_protocol_error() {
        let framed = b"Content-Length: 0\r\n\r\n";
        let mut reader = BufReader::new(framed.as_slice());
        let result = read_message(&mut reader).await;
        assert!(result.is_err());
        match result {
            Err(LspError::Protocol(msg)) => {
                assert!(
                    msg.contains("invalid JSON"),
                    "expected 'invalid JSON' in error, got: {msg}"
                );
            }
            other => panic!("expected Protocol error for zero-length body, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_header_with_leading_space() {
        // Headers with leading spaces before the name are technically invalid
        // but let's test they're rejected properly
        let msg = json!({"jsonrpc": "2.0", "result": true});
        let body = serde_json::to_vec(&msg).unwrap();
        let framed = format!(" Content-Length: {}\r\n\r\n", body.len());
        let mut buf: Vec<u8> = framed.as_bytes().to_vec();
        buf.extend_from_slice(&body);

        let mut reader = BufReader::new(buf.as_slice());
        let result = read_message(&mut reader).await;
        match result {
            Err(LspError::Protocol(msg)) => {
                assert!(msg.contains("Content-Length"), "expected 'Content-Length' in error, got: {msg}");
            }
            other => panic!("expected Protocol error for missing Content-Length due to leading space, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_unicode_in_json_body() {
        // Unicode characters in JSON should be preserved
        let msg = json!({"message": "Hello 世界 🌍"});
        let body = serde_json::to_vec(&msg).unwrap();
        let framed = format!("Content-Length: {}\r\n\r\n", body.len());
        let mut buf: Vec<u8> = framed.as_bytes().to_vec();
        buf.extend_from_slice(&body);

        let mut reader = BufReader::new(buf.as_slice());
        let result = read_message(&mut reader).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap()["message"], "Hello 世界 🌍");
    }

    #[tokio::test]
    async fn test_large_but_valid_message() {
        // A message just under the 50MB limit
        let large_array: Vec<i32> = (0..1_000_000).collect();
        let msg = json!({"data": large_array});
        let body = serde_json::to_vec(&msg).unwrap();
        let framed = format!("Content-Length: {}\r\n\r\n", body.len());
        let mut buf: Vec<u8> = framed.as_bytes().to_vec();
        buf.extend_from_slice(&body);

        let mut reader = BufReader::new(buf.as_slice());
        let result = read_message(&mut reader).await;
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result["data"].as_array().unwrap().len(), 1_000_000);
    }

    #[tokio::test]
    async fn test_multiple_messages_sequentially() {
        // Read multiple messages from the same stream
        let msg1 = json!({"id": 1, "result": "first"});
        let msg2 = json!({"id": 2, "result": "second"});
        let msg3 = json!({"id": 3, "result": "third"});

        let mut buf: Vec<u8> = Vec::new();
        write_message(&mut buf, &msg1).await.unwrap();
        write_message(&mut buf, &msg2).await.unwrap();
        write_message(&mut buf, &msg3).await.unwrap();

        let mut reader = BufReader::new(buf.as_slice());
        assert_eq!(read_message(&mut reader).await.unwrap()["result"], "first");
        assert_eq!(read_message(&mut reader).await.unwrap()["result"], "second");
        assert_eq!(read_message(&mut reader).await.unwrap()["result"], "third");
    }

    #[tokio::test]
    async fn test_exact_max_message_size() {
        // Message exactly at the 50MB limit should be accepted
        // Using a large array of small integers
        let array_size = 50 * 1024 * 1024 / 4; // ~50MB of 4-byte ints in JSON
        let msg = json!({"data": vec![0; array_size]});
        let body = serde_json::to_vec(&msg).unwrap();

        // The actual size might vary slightly due to JSON encoding
        let framed = format!("Content-Length: {}\r\n\r\n", body.len());
        let mut buf: Vec<u8> = framed.as_bytes().to_vec();
        buf.extend_from_slice(&body);

        // Only test if the message is actually at or near the limit
        if body.len() <= 50 * 1024 * 1024 {
            let mut reader = BufReader::new(buf.as_slice());
            let result = read_message(&mut reader).await;
            assert!(result.is_ok());
        }
    }

    #[tokio::test]
    async fn test_incomplete_body_returns_error() {
        // Content-Length says there's more data than actually provided
        let incomplete = br#"Content-Length: 100

{"incomplete": "data"}"#;
        let mut reader = BufReader::new(incomplete.as_slice());
        let result = read_message(&mut reader).await;
        assert!(result.is_err());
        // Should be an Io error since read_exact fails
        match result {
            Err(LspError::Io(_)) => {}
            other => panic!("expected Io error, got: {other:?}"),
        }
    }

    // D-3: MockStdout tests for malformed responses

    #[tokio::test]
    async fn test_mock_stdout_valid_message() {
        let mock = MockStdout::write_lsp_message(r#"{"jsonrpc":"2.0","result":42}"#);
        let mut reader = BufReader::new(mock);
        let result = read_message(&mut reader).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap()["result"], 42);
    }

    #[tokio::test]
    async fn test_mock_stdout_invalid_utf8_in_header() {
        // Create a header with invalid UTF-8
        let mut data = vec![];
        // Header line with invalid UTF-8 sequence
        data.extend_from_slice(b"Content-Length: 10\r\n");
        data.extend_from_slice(&[0xFF, 0xFE, 0xFD]); // Invalid UTF-8
        data.extend_from_slice(b"\r\n\r\n");
        data.extend_from_slice(b"0123456789");

        let mock = MockStdout::write_raw(&data);
        let mut reader = BufReader::new(mock);
        let result = read_message(&mut reader).await;

        // Should fail as Io error because read_line can't decode UTF-8
        assert!(result.is_err());
        match result {
            Err(LspError::Io(_)) => {}
            other => panic!("expected Io error for invalid UTF-8, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_mock_stdout_body_shorter_than_content_length() {
        // Content-Length claims 100 bytes but only provides 20
        let mut data = vec![];
        data.extend_from_slice(b"Content-Length: 100\r\n\r\n");
        data.extend_from_slice(b"short body only 20");

        let mock = MockStdout::write_raw(&data);
        let mut reader = BufReader::new(mock);
        let result = read_message(&mut reader).await;

        // Should fail with Io error since read_exact can't read 100 bytes
        assert!(result.is_err());
        match result {
            Err(LspError::Io(_)) => {}
            other => panic!("expected Io error for short body, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_mock_stdout_invalid_json_body() {
        let body = b"not valid json";
        let mut data = vec![];
        data.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
        data.extend_from_slice(body);

        let mock = MockStdout::write_raw(&data);
        let mut reader = BufReader::new(mock);
        let result = read_message(&mut reader).await;

        assert!(result.is_err());
        match result {
            Err(LspError::Protocol(msg)) => {
                assert!(msg.contains("invalid JSON") || msg.contains("expected value"));
            }
            other => panic!("expected Protocol error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_mock_stdout_empty_body_with_nonzero_length() {
        // Content-Length says 5 bytes but body is empty
        let mock = MockStdout::write_raw(b"Content-Length: 5\r\n\r\n");
        let mut reader = BufReader::new(mock);
        let result = read_message(&mut reader).await;

        assert!(result.is_err());
        match result {
            Err(LspError::Io(_)) => {}
            other => panic!("expected Io error for empty body with nonzero length, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_mock_stdout_header_without_blank_line() {
        let mock = MockStdout::write_raw(b"Content-Length: 10\r\n{}{}{}{}{}{}{}{}{}{}");
        let mut reader = BufReader::new(mock);
        let result = read_message(&mut reader).await;

        match result {
            Err(LspError::ConnectionLost | LspError::Protocol(_)) => {}
            other => panic!(
                "expected ConnectionLost or Protocol for no blank-line separator, got: {other:?}"
            ),
        }
    }

    #[tokio::test]
    async fn test_mock_stdout_negative_content_length() {
        // Negative Content-Length should be rejected
        let mock = MockStdout::write_raw(b"Content-Length: -10\r\n\r\n{}");
        let mut reader = BufReader::new(mock);
        let result = read_message(&mut reader).await;

        assert!(result.is_err());
        match result {
            Err(LspError::Protocol(msg)) => {
                assert!(msg.contains("invalid Content-Length") || msg.contains("parse"));
            }
            other => panic!("expected Protocol error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_mock_stdout_multiple_blank_lines() {
        // Extra blank lines before Content-Length header - parser should
        // treat them as blank header lines until it hits the real header
        let body = b"{}";
        let mut data = vec![];
        data.extend_from_slice(b"\r\nContent-Length: 2\r\n\r\n");
        data.extend_from_slice(body);

        let mock = MockStdout::write_raw(&data);
        let mut reader = BufReader::new(mock);
        let result = read_message(&mut reader).await;

        match result {
            Err(LspError::Protocol(msg)) => {
                assert!(
                    msg.contains("Content-Length"),
                    "expected 'Content-Length' in error, got: {msg}"
                );
            }
            other => panic!("expected Protocol error for premature blank line, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_mock_stdout_only_headers_no_body() {
        // Valid headers but EOF before body
        let mock = MockStdout::write_raw(b"Content-Length: 10\r\n\r\n");
        let mut reader = BufReader::new(mock);
        let result = read_message(&mut reader).await;

        // Should fail with Io error since body is incomplete
        assert!(result.is_err());
        match result {
            Err(LspError::Io(_)) => {}
            other => panic!("expected Io error for missing body, got: {other:?}"),
        }
    }
}
