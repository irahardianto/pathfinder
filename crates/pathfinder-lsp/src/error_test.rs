use super::*;
use std::io;

#[test]
#[allow(clippy::unwrap_used)]
fn test_lsp_error_no_lsp_available_recovery_hint() {
    let err = LspError::NoLspAvailable;
    let hint = err.recovery_hint();

    assert!(hint.is_some());
    let hint_str = hint.unwrap();
    assert!(hint_str.contains("No LSP available"));
    assert!(hint_str.contains("lsp_health"));
}

#[test]
#[allow(clippy::unwrap_used)]
fn test_lsp_error_timeout_recovery_hint() {
    let err = LspError::Timeout {
        operation: "textDocument/definition".to_string(),
        timeout_ms: 10000,
    };
    let hint = err.recovery_hint();

    assert!(hint.is_some());
    let hint_str = hint.unwrap();
    assert!(hint_str.contains("textDocument/definition"));
    assert!(hint_str.contains("10000ms"));
    assert!(hint_str.contains("lsp_health"));
    assert!(hint_str.contains("retry in 30s"));
}

#[test]
#[allow(clippy::unwrap_used)]
fn test_lsp_error_protocol_recovery_hint() {
    let err = LspError::Protocol("malformed JSON-RPC".to_string());
    let hint = err.recovery_hint();

    assert!(hint.is_some());
    let hint_str = hint.unwrap();
    assert!(hint_str.contains("malformed JSON-RPC"));
    assert!(hint_str.contains("lsp_health"));
    assert!(hint_str.contains("search_codebase"));
}

#[test]
#[allow(clippy::unwrap_used)]
fn test_lsp_error_server_error_recovery_hint() {
    let err = LspError::ServerError {
        code: -32601,
        message: "Method not found".to_string(),
        data: None,
    };
    let hint = err.recovery_hint();

    assert!(hint.is_some());
    let hint_str = hint.unwrap();
    assert!(hint_str.contains("-32601"));
    assert!(hint_str.contains("Method not found"));
    assert!(hint_str.contains("lsp_health"));
}

#[test]
#[allow(clippy::unwrap_used)]
fn test_lsp_error_server_error_with_data_recovery_hint() {
    let err = LspError::ServerError {
        code: -32002,
        message: "ServerNotReady".to_string(),
        data: Some(serde_json::json!({"retry": true})),
    };
    let hint = err.recovery_hint();

    assert!(hint.is_some());
    let hint_str = hint.unwrap();
    assert!(hint_str.contains("-32002"));
    assert!(hint_str.contains("ServerNotReady"));
}

#[test]
fn test_lsp_error_display_server_error() {
    let err = LspError::ServerError {
        code: -32601,
        message: "Method not found".to_string(),
        data: None,
    };
    let display = format!("{err}");
    assert!(display.contains("-32601"));
    assert!(display.contains("Method not found"));
}

#[test]
fn test_lsp_error_display_server_error_with_data() {
    let err = LspError::ServerError {
        code: -32002,
        message: "ServerNotReady".to_string(),
        data: Some(serde_json::json!({"retry": true})),
    };
    let display = format!("{err}");
    assert!(display.contains("-32002"));
    assert!(display.contains("ServerNotReady"));
    assert!(display.contains("retry"));
}

#[test]
#[allow(clippy::unwrap_used)]
fn test_lsp_error_connection_lost_recovery_hint() {
    let err = LspError::ConnectionLost;
    let hint = err.recovery_hint();

    assert!(hint.is_some());
    let hint_str = hint.unwrap();
    assert!(hint_str.contains("connection lost"));
    assert!(hint_str.contains("lsp_health"));
    assert!(hint_str.contains("restart"));
    assert!(hint_str.contains("read_source_file"));
}

#[test]
#[allow(clippy::unwrap_used)]
fn test_lsp_error_unsupported_capability_recovery_hint() {
    let err = LspError::UnsupportedCapability {
        capability: "diagnosticProvider".to_string(),
    };
    let hint = err.recovery_hint();

    assert!(hint.is_some());
    let hint_str = hint.unwrap();
    assert!(hint_str.contains("diagnosticProvider"));
    assert!(hint_str.contains("does not support"));
    assert!(hint_str.contains("search_codebase"));
}

#[test]
#[allow(clippy::unwrap_used)]
fn test_lsp_error_io_not_found_recovery_hint() {
    let io_err = io::Error::new(io::ErrorKind::NotFound, "binary not found");
    let err = LspError::Io(io_err);
    let hint = err.recovery_hint();

    assert!(hint.is_some());
    let hint_str = hint.unwrap();
    assert!(hint_str.contains("binary not found"));
    assert!(hint_str.contains("installed"));
    assert!(hint_str.contains("PATH"));
    assert!(hint_str.contains("lsp_health"));
}

#[test]
#[allow(clippy::unwrap_used)]
fn test_lsp_error_io_permission_denied_recovery_hint() {
    let io_err = io::Error::new(io::ErrorKind::PermissionDenied, "access denied");
    let err = LspError::Io(io_err);
    let hint = err.recovery_hint();

    assert!(hint.is_some());
    let hint_str = hint.unwrap();
    assert!(hint_str.contains("I/O error"));
    assert!(hint_str.contains("access denied"));
    assert!(hint_str.contains("lsp_health"));
}

#[test]
#[allow(clippy::unwrap_used)]
fn test_lsp_error_io_broken_pipe_recovery_hint() {
    let io_err = io::Error::new(io::ErrorKind::BrokenPipe, "pipe broken");
    let err = LspError::Io(io_err);
    let hint = err.recovery_hint();

    assert!(hint.is_some());
    let hint_str = hint.unwrap();
    assert!(hint_str.contains("I/O error"));
    assert!(hint_str.contains("pipe broken"));
    assert!(hint_str.contains("lsp_health"));
}

#[test]
fn test_lsp_error_display_no_lsp_available() {
    let err = LspError::NoLspAvailable;
    let display = format!("{err}");
    assert_eq!(display, "no LSP available for this file type");
}

#[test]
fn test_lsp_error_display_timeout() {
    let err = LspError::Timeout {
        operation: "initialize".to_string(),
        timeout_ms: 30000,
    };
    let display = format!("{err}");
    assert!(display.contains("initialize"));
    assert!(display.contains("30000ms"));
}

#[test]
fn test_lsp_error_display_protocol() {
    let err = LspError::Protocol("invalid response".to_string());
    let display = format!("{err}");
    assert_eq!(display, "LSP protocol error: invalid response");
}

// Display tests for ServerError are above (test_lsp_error_display_server_error*)

#[test]
fn test_lsp_error_display_connection_lost() {
    let err = LspError::ConnectionLost;
    let display = format!("{err}");
    assert_eq!(display, "LSP connection lost");
}

#[test]
fn test_lsp_error_display_unsupported_capability() {
    let err = LspError::UnsupportedCapability {
        capability: "callHierarchy".to_string(),
    };
    let display = format!("{err}");
    assert!(display.contains("callHierarchy"));
}

#[test]
fn test_lsp_error_display_io() {
    let io_err = io::Error::new(io::ErrorKind::NotFound, "test error");
    let err = LspError::Io(io_err);
    let display = format!("{err}");
    assert!(display.contains("I/O error"));
    assert!(display.contains("test error"));
}

#[test]
fn test_all_error_variants_have_recovery_hints() {
    // NoLspAvailable
    assert!(LspError::NoLspAvailable.recovery_hint().is_some());

    // Timeout
    assert!(LspError::Timeout {
        operation: "test".to_string(),
        timeout_ms: 5000,
    }
    .recovery_hint()
    .is_some());

    // Protocol
    assert!(LspError::Protocol("test".to_string())
        .recovery_hint()
        .is_some());

    // ConnectionLost
    assert!(LspError::ConnectionLost.recovery_hint().is_some());

    // UnsupportedCapability
    assert!(LspError::UnsupportedCapability {
        capability: "test".to_string(),
    }
    .recovery_hint()
    .is_some());

    // Io - NotFound
    assert!(
        LspError::Io(io::Error::new(io::ErrorKind::NotFound, "test"))
            .recovery_hint()
            .is_some()
    );

    // Io - PermissionDenied
    assert!(
        LspError::Io(io::Error::new(io::ErrorKind::PermissionDenied, "test"))
            .recovery_hint()
            .is_some()
    );

    // Io - Other
    assert!(LspError::Io(std::io::Error::other("test"))
        .recovery_hint()
        .is_some());

    // ServerError
    assert!(LspError::ServerError {
        code: -32600,
        message: "test".to_string(),
        data: None,
    }
    .recovery_hint()
    .is_some());

    // ServerError with data
    assert!(LspError::ServerError {
        code: -32600,
        message: "test".to_string(),
        data: Some(serde_json::json!("extra")),
    }
    .recovery_hint()
    .is_some());
}
