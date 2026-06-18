//! Shared helper functions for Pathfinder MCP tool handlers.
//!
//! Contains error conversion utilities and the file-language detector
//! used by `read` (config file mode).

use pathfinder_common::error::PathfinderError;
use pathfinder_common::types::{SemanticPath, TrustLevel};
use rmcp::model::{ErrorCode, ErrorData};
use std::fmt::Write;
use std::path::Path;

/// Spec 5.1: Convert a `Duration::as_millis()` (u128) to u64 for JSON responses.
/// Millisecond timestamps will never overflow u64 in practice.
#[allow(clippy::cast_possible_truncation)]
#[must_use]
pub fn millis_to_u64(millis: u128) -> u64 {
    millis as u64
}

// ── Error Helpers ─────────────────────────────────────────────────

/// Convert a [`PathfinderError`] to an [`ErrorData`] that MCP callers can
/// inspect. The structured error JSON is embedded in the `data` field so
/// agents can parse `error` (code) and `message` without extra round-trips.
pub(crate) fn pathfinder_to_error_data(err: &PathfinderError) -> ErrorData {
    let err_resp = err.to_error_response();
    let data = match serde_json::to_value(&err_resp) {
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

    // JSON-RPC error code allocation (-320xx range, implementation-defined):
    // -32602  INVALID_PARAMS     Client errors (file not found, bad paths, etc.)
    // -32001  ACCESS_DENIED      Sandbox violation
    // -32603  INTERNAL_ERROR     Genuine server failures (I/O, parse, LSP crash)
    let error_code = match err {
        // Client errors (invalid parameters) -> INVALID_PARAMS (-32602)
        pathfinder_common::error::PathfinderError::FileNotFound { .. }
        | pathfinder_common::error::PathfinderError::SymbolNotFound { .. }
        | pathfinder_common::error::PathfinderError::AmbiguousSymbol { .. }
        | pathfinder_common::error::PathfinderError::InvalidSemanticPath { .. }
        | pathfinder_common::error::PathfinderError::UnsupportedLanguage { .. }
        | pathfinder_common::error::PathfinderError::TokenBudgetExceeded { .. } => {
            ErrorCode::INVALID_PARAMS
        }

        // Access control -> custom error -32001
        pathfinder_common::error::PathfinderError::AccessDenied { .. } => ErrorCode(-32001),

        // Genuine internal errors -> INTERNAL_ERROR (-32603)
        pathfinder_common::error::PathfinderError::IoError { .. }
        | pathfinder_common::error::PathfinderError::ParseError { .. }
        | pathfinder_common::error::PathfinderError::LspError { .. }
        | pathfinder_common::error::PathfinderError::LspTimeout { .. }
        | pathfinder_common::error::PathfinderError::NoLspAvailable { .. }
        | pathfinder_common::error::PathfinderError::PathTraversal { .. } => {
            ErrorCode::INTERNAL_ERROR
        }
    };

    let mut detailed_message = format!("{}: {}", err_resp.error, err_resp.message);
    if let Some(ref hint) = err_resp.hint {
        let _ = write!(detailed_message, " Hint: {hint}");
    }

    // Include matches if present (for AmbiguousSymbol).
    // NOTE: did_you_mean suggestions for SymbolNotFound are already included in
    // err_resp.hint (via PathfinderError::hint()) — do NOT add them here again
    // or they will appear twice in detailed_message.
    if let Some(matches) = err_resp.details.get("matches") {
        if let Some(arr) = matches.as_array() {
            if !arr.is_empty() {
                let match_list: Vec<String> = arr
                    .iter()
                    .filter_map(|v| v.as_str().map(ToOwned::to_owned))
                    .collect();
                if !match_list.is_empty() {
                    let _ = write!(
                        detailed_message,
                        " Matches found: {}",
                        match_list.join(", ")
                    );
                }
            }
        }
    }

    ErrorData::new(error_code, detailed_message, data)
}

/// Convert a `SurgeonError` into a `PathfinderError` and then to an [`ErrorData`].
/// This centralizes the exhaustive matching of AST errors to our standard error taxonomy.
pub(crate) fn treesitter_error_to_error_data(e: pathfinder_treesitter::SurgeonError) -> ErrorData {
    pathfinder_to_error_data(&e.into())
}

/// Wrap a plain IO / infrastructure message in an [`ErrorData`].
pub(crate) fn io_error_data(msg: impl Into<std::borrow::Cow<'static, str>>) -> ErrorData {
    ErrorData::internal_error(msg, None)
}

/// Wrap a parameter validation message in an [`ErrorData`] with code `INVALID_PARAMS`.
pub(crate) fn invalid_params_error(msg: impl Into<std::borrow::Cow<'static, str>>) -> ErrorData {
    ErrorData::invalid_params(msg, None)
}

// ── Degraded Mode Formatting ────────────────────────────────────────

/// Format a standardized degraded-mode text prefix for agent consumption.
///
/// Output format: `DEGRADED ({reason}) — {trust description} — {fallback} — {retry}`
/// Every tool uses this function so agents can parse degraded notices uniformly.
#[allow(clippy::trivially_copy_pass_by_ref)]
pub(crate) fn format_degraded_notice(reason: &pathfinder_common::types::DegradedReason) -> String {
    let guidance = reason.guidance();
    let mut parts = vec![format!("DEGRADED ({reason})")];

    // Match on the enum directly (not .as_str()) so the compiler enforces exhaustiveness:
    // adding a new TrustLevel variant will cause a compile error here, forcing an update.
    match guidance.trust_level {
        TrustLevel::Unreliable => {
            parts.push("results are UNRELIABLE, do not trust empty counts".into());
        }
        TrustLevel::Heuristic => {
            parts.push("results are heuristic (grep-based), verify manually".into());
        }
        TrustLevel::Partial => parts.push("results are PARTIAL, some features unavailable".into()),
        TrustLevel::None => parts.push("results are UNAVAILABLE for this language".into()),
    }

    if let Some(fallback) = &guidance.fallback_tool {
        parts.push(format!(
            "fallback: use {fallback} for authoritative results"
        ));
    }

    if let Some(secs) = guidance.retry_after_seconds {
        parts.push(format!("retry after ~{secs}s for LSP-backed results"));
    }

    parts.join(" — ")
}

// ── Language Detection ──────────────────────────────────────────────

/// Detect the language of a file from its extension.
/// Used by `read_file` to populate the `language` field in the response.
pub(crate) fn language_from_path(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("ts" | "tsx") => "typescript",
        Some("js" | "jsx" | "mjs" | "cjs") => "javascript",
        Some("rs") => "rust",
        Some("go") => "go",
        Some("py") => "python",
        Some("java") => "java",
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
}

// ── Semantic-Path Helpers ───────────────────────────────────────────

/// Parse `raw` into a [`SemanticPath`], returning a structured [`ErrorData`] on failure.
///
/// This centralises the `let Some(semantic_path) = SemanticPath::parse(...)` preamble
/// that previously appeared in every tool handler.
pub(crate) fn parse_semantic_path(raw: &str) -> Result<SemanticPath, ErrorData> {
    SemanticPath::parse(raw).ok_or_else(|| {
        pathfinder_to_error_data(&PathfinderError::InvalidSemanticPath {
            input: raw.to_owned(),
            issue: "Semantic path is malformed or missing '::' separator.".to_owned(),
        })
    })
}

/// Reject a bare file path for tool operations that require a symbol target.
///
/// Returns `Err` with a structured [`PathfinderError::InvalidSemanticPath`] when
/// `semantic_path.is_bare_file()` is `true`, otherwise `Ok(())`.
pub(crate) fn require_symbol_target(
    semantic_path: &SemanticPath,
    raw_path: &str,
) -> Result<(), ErrorData> {
    if semantic_path.is_bare_file() {
        return Err(pathfinder_to_error_data(
            &PathfinderError::InvalidSemanticPath {
                input: raw_path.to_owned(),
                issue: "this tool requires a symbol target — use 'file.rs::symbol' format"
                    .to_owned(),
            },
        ));
    }
    Ok(())
} // ── Serialization Helpers ───────────────────────────────────────────

/// Serialize metadata to JSON, logging a warning on failure instead of
/// silently degrading to `Value::Null` via `unwrap_or_default()`.
///
/// Returns `Some(Value)` on success, `None` on failure (with a warning log).
pub(crate) fn serialize_metadata<T: serde::Serialize>(metadata: &T) -> Option<serde_json::Value> {
    match serde_json::to_value(metadata) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(
                error = %e,
                type_name = std::any::type_name::<T>(),
                "structured metadata serialization failed; agent will receive null"
            );
            None
        }
    }
}

#[cfg(test)]
#[path = "helpers_test.rs"]
mod tests;
