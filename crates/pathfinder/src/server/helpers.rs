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
pub(crate) fn treesitter_error_to_error_data(e: pathfinder_treesitter::SurgeonError) -> ErrorData {
    pathfinder_to_error_data(&e.into())
}

/// Wrap a plain IO / infrastructure message in an [`ErrorData`].
pub(crate) fn io_error_data(msg: impl Into<std::borrow::Cow<'static, str>>) -> ErrorData {
    ErrorData::internal_error(msg, None)
}

/// OCC guard: verify the agent's `base_version` matches the current file hash.
///
/// Accepts two formats:
/// - Full hash: `sha256:<64 hex chars>` — exact match (legacy, always valid)
/// - Short prefix: `sha256:<N hex chars>` where N >= 7 — prefix match
///
/// The minimum prefix length of 7 characters matches Git's convention and
/// provides sufficient collision resistance for workspace-scale file sets.
/// A prefix shorter than 7 hex chars after `sha256:` is rejected as too short.
pub(crate) fn check_occ(
    base_version: &str,
    current_hash: &VersionHash,
    path: PathBuf,
) -> Result<(), ErrorData> {
    const SHA256_PREFIX: &str = "sha256:";
    const MIN_HEX_CHARS: usize = 7;

    let current = current_hash.as_str();

    let matches = match base_version.len().cmp(&current.len()) {
        std::cmp::Ordering::Equal => {
            // Fast path: same length → exact comparison
            base_version == current
        }
        std::cmp::Ordering::Greater => {
            // Claimed hash is longer than current (malformed or different algo)
            false
        }
        std::cmp::Ordering::Less => {
            // base_version is shorter — attempt prefix match
            let hex_part_len = base_version.strip_prefix(SHA256_PREFIX).map_or(0, str::len);

            if hex_part_len < MIN_HEX_CHARS {
                // Too short to be meaningful — treat as mismatch to be safe
                return Err(pathfinder_to_error_data(
                    &PathfinderError::VersionMismatch {
                        path,
                        current_version_hash: current.to_owned(),
                        lines_changed: None,
                    },
                ));
            }

            current.starts_with(base_version)
        }
    };

    if !matches {
        return Err(pathfinder_to_error_data(
            &PathfinderError::VersionMismatch {
                path,
                current_version_hash: current.to_owned(),
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
pub(crate) fn check_sandbox_access(
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
        // Take first 7 hex chars after "sha256:"
        let short = &hash.as_str()[..14]; // "sha256:" (7) + 7 hex chars = 14
        let result = check_occ(short, &hash, PathBuf::from("test.rs"));
        assert!(result.is_ok(), "7-char prefix should be accepted");
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
}
