//! Shared helper functions for Pathfinder MCP tool handlers.
//!
//! Contains error conversion utilities, the stub-response builder, and
//! the file-language detector used by `read_file`.

use super::types::StubResponse;
use pathfinder_common::error::PathfinderError;
use rmcp::handler::server::wrapper::Json;
use rmcp::model::{ErrorCode, ErrorData};
use std::path::Path;

// ── Error Helpers ─────────────────────────────────────────────────

/// Convert a [`PathfinderError`] to an [`ErrorData`] that MCP callers can
/// inspect. The structured error JSON is embedded in the `data` field so
/// agents can parse `error` (code) and `message` without extra round-trips.
pub(crate) fn pathfinder_to_error_data(err: &PathfinderError) -> ErrorData {
    let data = match serde_json::to_value(err.to_error_response()) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(
                error = %e,
                error_type = std::any::type_name::<PathfinderError>(),
                "pathfinder_to_error_data: serialization failed, error context will be lost"
            );
            None
        }
    };
    ErrorData::new(ErrorCode::INTERNAL_ERROR, err.error_code(), data)
}

/// Wrap a plain IO / infrastructure message in an [`ErrorData`].
pub(crate) fn io_error_data(msg: impl Into<std::borrow::Cow<'static, str>>) -> ErrorData {
    ErrorData::internal_error(msg, None)
}

// ── Stub Response Helper ────────────────────────────────────────────

/// Return a standard `NOT_IMPLEMENTED` response for unbuilt tool stubs.
pub(crate) fn stub_response(tool_name: &str) -> Json<StubResponse> {
    tracing::info!(tool = tool_name, "{tool_name}: start");
    let response = Json(StubResponse {
        error: "NOT_IMPLEMENTED".to_owned(),
        message: format!("Tool '{tool_name}' is not yet implemented. Coming in a future epic."),
        details: std::collections::HashMap::new(),
    });
    tracing::info!(tool = tool_name, "{tool_name}: complete (stub)");
    response
}

// ── Language Detection ──────────────────────────────────────────────

/// Detect the language of a file from its extension.
/// Used by `read_file` to populate the `language` field in the response.
pub(crate) fn language_from_path(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some("ts" | "tsx") => "typescript",
        Some("js" | "jsx" | "mjs" | "cjs") => "javascript",
        Some("rs") => "rust",
        Some("go") => "go",
        Some("py") => "python",
        Some("json") => "json",
        Some("yaml" | "yml") => "yaml",
        Some("toml") => "toml",
        Some("md" | "mdx") => "markdown",
        Some("sh" | "bash") => "shell",
        Some("dockerfile") | None
            if path.file_name().and_then(|n| n.to_str()) == Some("Dockerfile") =>
        {
            "dockerfile"
        }
        _ => "text",
    }
    .to_owned()
}
