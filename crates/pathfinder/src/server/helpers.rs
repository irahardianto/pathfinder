//! Shared helper functions for Pathfinder MCP tool handlers.
//!
//! Contains error conversion utilities and the file-language detector
//! used by `read_file`.

use pathfinder_common::error::PathfinderError;
use pathfinder_common::sandbox::Sandbox;
use pathfinder_common::types::{SemanticPath, VersionHash};
use rmcp::model::{ErrorCode, ErrorData};
use std::path::{Path, PathBuf};

// ── Error Helpers ─────────────────────────────────────────────────

/// Convert a [`PathfinderError`] to an [`ErrorData`] that MCP callers can
/// inspect. The structured error JSON is embedded in the `data` field so
/// agents can parse `error` (code) and `message` without extra round-trips.
pub fn pathfinder_to_error_data(err: &PathfinderError) -> ErrorData {
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

    // JSON-RPC error code allocation (-320xx range, implementation-defined):
    // -32602  INVALID_PARAMS     Client errors (file not found, bad paths, etc.)
    // -32001  ACCESS_DENIED      Sandbox violation
    // -32002  [reserved by rmcp] RESOURCE_NOT_FOUND
    // -32003  VERSION_MISMATCH   OCC conflict
    // -32004  VALIDATION_FAILED  LSP errors introduced by edit
    // -32603  INTERNAL_ERROR     Genuine server failures (I/O, parse, LSP crash)
    let error_code = match err {
        // Client errors (invalid parameters) -> INVALID_PARAMS (-32602)
        pathfinder_common::error::PathfinderError::FileNotFound { .. }
        | pathfinder_common::error::PathfinderError::FileAlreadyExists { .. }
        | pathfinder_common::error::PathfinderError::SymbolNotFound { .. }
        | pathfinder_common::error::PathfinderError::AmbiguousSymbol { .. }
        | pathfinder_common::error::PathfinderError::InvalidSemanticPath { .. }
        | pathfinder_common::error::PathfinderError::UnsupportedLanguage { .. }
        | pathfinder_common::error::PathfinderError::InvalidTarget { .. }
        | pathfinder_common::error::PathfinderError::TokenBudgetExceeded { .. }
        | pathfinder_common::error::PathfinderError::MatchNotFound { .. }
        | pathfinder_common::error::PathfinderError::AmbiguousMatch { .. }
        | pathfinder_common::error::PathfinderError::TextNotFound { .. } => {
            ErrorCode::INVALID_PARAMS
        }

        // Access control -> custom error -32001
        pathfinder_common::error::PathfinderError::AccessDenied { .. } => ErrorCode(-32001),

        // OCC/conflict -> custom error -32003 (avoid -32002, used by rmcp for RESOURCE_NOT_FOUND)
        pathfinder_common::error::PathfinderError::VersionMismatch { .. } => ErrorCode(-32003),

        // Validation -> custom error -32004
        pathfinder_common::error::PathfinderError::ValidationFailed { .. } => ErrorCode(-32004),

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

    ErrorData::new(error_code, err.error_code(), data)
}

/// Convert a `SurgeonError` into a `PathfinderError` and then to an [`ErrorData`].
/// This centralizes the exhaustive matching of AST errors to our standard error taxonomy.
pub fn treesitter_error_to_error_data(e: pathfinder_treesitter::SurgeonError) -> ErrorData {
    pathfinder_to_error_data(&e.into())
}

/// Wrap a plain IO / infrastructure message in an [`ErrorData`].
pub fn io_error_data(msg: impl Into<std::borrow::Cow<'static, str>>) -> ErrorData {
    ErrorData::internal_error(msg, None)
}

/// OCC guard: verify the agent's `base_version` matches the current file hash.
///
/// Delegates to [`VersionHash::matches`] which accepts all formats:
/// - `"e3dc7f9"` — 7-char short form (no prefix, preferred)
/// - `"sha256:e3dc7f9"` — short form with legacy prefix
/// - `"sha256:<64 hex>"` — full hash (backward compatible)
///
/// Returns `VERSION_MISMATCH` (`-32003`) if the hash does not match or the
/// supplied prefix is shorter than 7 hex chars.
pub fn check_occ(
    base_version: &str,
    current_hash: &VersionHash,
    path: PathBuf,
) -> Result<(), ErrorData> {
    if !current_hash.matches(base_version) {
        return Err(pathfinder_to_error_data(
            &PathfinderError::VersionMismatch {
                path,
                current_version_hash: current_hash.as_str().to_owned(),
                lines_changed: None,
            },
        ));
    }
    Ok(())
}

/// Sandbox access guard with structured logging.
///
/// Checks whether `relative_path` is accessible per the sandbox rules.
/// On denial, logs a structured warning and returns `Err(ACCESS_DENIED)`.
/// Centralises the 7-line sandbox-check preamble duplicated across edit handlers.
pub fn check_sandbox_access(
    sandbox: &Sandbox,
    relative_path: &Path,
    tool_name: &str,
    raw_semantic_path: &str,
) -> Result<(), ErrorData> {
    if let Err(e) = sandbox.check(relative_path) {
        tracing::warn!(
            tool = tool_name,
            semantic_path = raw_semantic_path,
            error = %e,
            "{tool_name}: access denied"
        );
        return Err(pathfinder_to_error_data(&e));
    }
    Ok(())
}

// ── Language Detection ──────────────────────────────────────────────

/// Detect the language of a file from its extension.
/// Used by `read_file` to populate the `language` field in the response.
pub fn language_from_path(path: &Path) -> String {
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

// ── Semantic-Path Helpers ───────────────────────────────────────────

/// Parse `raw` into a [`SemanticPath`], returning a structured [`ErrorData`] on failure.
///
/// This centralises the `let Some(semantic_path) = SemanticPath::parse(...)` preamble
/// that previously appeared in every tool handler.
pub fn parse_semantic_path(raw: &str) -> Result<SemanticPath, ErrorData> {
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
pub fn require_symbol_target(
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
pub fn serialize_metadata<T: serde::Serialize>(metadata: &T) -> Option<serde_json::Value> {
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
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use pathfinder_common::error::{PathfinderError, SandboxTier};
    use rmcp::model::ErrorCode;

    #[test]
    fn test_error_code_mapping_client_errors_to_invalid_params() {
        // Client errors should map to INVALID_PARAMS (-32602)
        let client_errors = vec![
            PathfinderError::FileNotFound {
                path: "src/main.rs".into(),
            },
            PathfinderError::FileAlreadyExists {
                path: "src/main.rs".into(),
            },
            PathfinderError::SymbolNotFound {
                semantic_path: "src/auth.ts::login".into(),
                did_you_mean: vec![],
            },
            PathfinderError::AmbiguousSymbol {
                semantic_path: "src/auth.ts::login".into(),
                matches: vec![],
            },
            PathfinderError::InvalidSemanticPath {
                input: "invalid".into(),
                issue: "missing ::".into(),
            },
            PathfinderError::UnsupportedLanguage {
                path: "data.xyz".into(),
            },
            PathfinderError::InvalidTarget {
                semantic_path: "src/lib.rs::CONST".into(),
                reason: "not a block construct".into(),
                edit_index: None,
                valid_edit_types: None,
            },
            PathfinderError::TokenBudgetExceeded {
                used: 1000,
                budget: 500,
            },
            PathfinderError::MatchNotFound {
                filepath: "config.yaml".into(),
            },
            PathfinderError::AmbiguousMatch {
                filepath: "config.yaml".into(),
                occurrences: 2,
            },
            PathfinderError::TextNotFound {
                filepath: "src/main.rs".into(),
                old_text: "fn main()".into(),
                context_line: 10,
                actual_content: None,
                closest_match: None,
            },
        ];

        for err in client_errors {
            let error_data = pathfinder_to_error_data(&err);
            assert_eq!(
                error_data.code,
                ErrorCode::INVALID_PARAMS,
                "Expected INVALID_PARAMS for error: {}",
                err.error_code()
            );
        }
    }

    #[test]
    fn test_error_code_mapping_access_denied_to_custom_code() {
        let err = PathfinderError::AccessDenied {
            path: ".env".into(),
            tier: SandboxTier::HardcodedDeny,
        };

        let error_data = pathfinder_to_error_data(&err);
        assert_eq!(error_data.code, ErrorCode(-32001));
    }

    #[test]
    fn test_error_code_mapping_version_mismatch_to_custom_code() {
        let err = PathfinderError::VersionMismatch {
            path: "src/main.rs".into(),
            current_version_hash: "sha256:abc123".into(),
            lines_changed: None,
        };

        let error_data = pathfinder_to_error_data(&err);
        assert_eq!(error_data.code, ErrorCode(-32003));
    }

    #[test]
    fn test_error_code_mapping_validation_failed_to_custom_code() {
        let err = PathfinderError::ValidationFailed {
            count: 2,
            introduced_errors: vec![],
        };

        let error_data = pathfinder_to_error_data(&err);
        assert_eq!(error_data.code, ErrorCode(-32004));
    }

    #[test]
    fn test_error_code_mapping_internal_errors_to_internal_error() {
        let internal_errors = vec![
            PathfinderError::IoError {
                message: "disk full".into(),
            },
            PathfinderError::ParseError {
                path: "src/main.rs".into(),
                reason: "unexpected token".into(),
            },
            PathfinderError::LspError {
                message: "LSP crashed".into(),
            },
            PathfinderError::LspTimeout { timeout_ms: 5000 },
            PathfinderError::NoLspAvailable {
                language: "ruby".into(),
            },
        ];

        for err in internal_errors {
            let error_data = pathfinder_to_error_data(&err);
            assert_eq!(
                error_data.code,
                ErrorCode::INTERNAL_ERROR,
                "Expected INTERNAL_ERROR for error: {}",
                err.error_code()
            );
        }
    }

    #[test]
    fn test_check_occ_full_hash_match() {
        let hash = VersionHash::compute(b"hello world");
        let result = check_occ(hash.as_str(), &hash, PathBuf::from("test.rs"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_check_occ_short_7_char_prefix_matches() {
        let hash = VersionHash::compute(b"hello world");
        // Legacy format: "sha256:" + 7 hex chars
        let short_with_prefix = &hash.as_str()[..14]; // 7 + 7 = 14 chars
        let result = check_occ(short_with_prefix, &hash, PathBuf::from("test.rs"));
        assert!(
            result.is_ok(),
            "7-char prefix with sha256: should be accepted"
        );

        // Preferred format: just 7 hex chars, no prefix — what short() emits
        let short_no_prefix = hash.short();
        let result2 = check_occ(short_no_prefix, &hash, PathBuf::from("test.rs"));
        assert!(
            result2.is_ok(),
            "7-char no-prefix short hash must be accepted"
        );
    }

    #[test]
    fn test_check_occ_wrong_prefix_fails() {
        let hash = VersionHash::compute(b"hello world");
        let result = check_occ("sha256:0000000", &hash, PathBuf::from("test.rs"));
        assert!(result.is_err(), "wrong prefix must fail");
    }

    #[test]
    fn test_check_occ_prefix_too_short_is_rejected() {
        let hash = VersionHash::compute(b"hello world");
        let result = check_occ("sha256:4ec", &hash, PathBuf::from("test.rs")); // only 3 hex chars
        assert!(result.is_err(), "prefix < 7 hex chars must be rejected");
    }

    #[test]
    fn test_check_occ_full_hash_mismatch_fails() {
        let hash_a = VersionHash::compute(b"hello world");
        let hash_b = VersionHash::compute(b"different content");
        let result = check_occ(hash_a.as_str(), &hash_b, PathBuf::from("test.rs"));
        assert!(result.is_err());
    }

    #[test]
    fn test_check_occ_8_char_prefix_matches() {
        let hash = VersionHash::compute(b"hello world");
        let prefix_8 = &hash.as_str()[..15]; // "sha256:" + 8 hex chars
        let result = check_occ(prefix_8, &hash, PathBuf::from("test.rs"));
        assert!(result.is_ok(), "8-char prefix should also be accepted");
    }

    /// Regression test: `SurgeonError::FileNotFound` must surface as
    /// `INVALID_PARAMS (-32602)`, not `INTERNAL_ERROR (-32603)`.
    ///
    /// Before the fix, a missing file in `cached_parse` propagated through
    /// `SurgeonError::Io` → `PathfinderError::IoError` → `-32603`, misleading
    /// agents into thinking the server had crashed.
    #[test]
    fn test_surgeon_file_not_found_maps_to_invalid_params() {
        use pathfinder_treesitter::SurgeonError;

        let surgeon_err = SurgeonError::FileNotFound("src/does_not_exist.rs".into());
        let pf_err: pathfinder_common::error::PathfinderError = surgeon_err.into();
        let error_data = pathfinder_to_error_data(&pf_err);

        assert_eq!(
            error_data.code,
            ErrorCode::INVALID_PARAMS,
            "missing file must be INVALID_PARAMS, not INTERNAL_ERROR"
        );
        // Verify the structured error code string
        let code_str = error_data
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code_str, "FILE_NOT_FOUND");
    }

    #[test]
    fn test_serialize_metadata_success() {
        use std::collections::HashMap;
        let mut map = HashMap::new();
        map.insert("key", "value");
        let result = super::serialize_metadata(&map);
        assert!(result.is_some());
    }
}
