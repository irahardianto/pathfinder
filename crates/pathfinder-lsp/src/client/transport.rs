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

        let parts: Vec<&str> = trimmed.splitn(2, ':').collect();
        if parts.len() == 2 && parts[0].trim_end().eq_ignore_ascii_case("content-length") {
            let value = parts[1].trim();
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
#[path = "transport_test.rs"]
mod tests;
