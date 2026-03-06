//! Pathfinder error taxonomy.
//!
//! All tools return errors in a standardized format:
//! `{ "error": "ERROR_CODE", "message": "...", "details": {} }`
//!
//! See PRD §5 for the full error taxonomy.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// All error codes in the Pathfinder error taxonomy (PRD §5).
///
/// Each variant carries contextual data relevant to the specific error,
/// enabling agents to self-correct without additional round-trips.
#[derive(Debug, thiserror::Error)]
pub enum PathfinderError {
    /// File path doesn't exist.
    #[error("file not found: {path}")]
    FileNotFound { path: PathBuf },

    /// File already exists (for `create_file`).
    #[error("file already exists: {path}")]
    FileAlreadyExists { path: PathBuf },

    /// Semantic path doesn't resolve.
    #[error("symbol not found: {semantic_path}")]
    SymbolNotFound {
        semantic_path: String,
        did_you_mean: Vec<String>,
    },

    /// Multiple matches for a semantic path.
    #[error("ambiguous symbol: {semantic_path}")]
    AmbiguousSymbol {
        semantic_path: String,
        matches: Vec<String>,
    },

    /// File changed since last read. OCC violation.
    #[error("version mismatch for {path}")]
    VersionMismatch {
        path: PathBuf,
        current_version_hash: String,
    },

    /// Edit introduced new errors.
    #[error("validation failed: {count} new errors introduced")]
    ValidationFailed {
        count: usize,
        introduced_errors: Vec<DiagnosticError>,
    },

    /// No language server available for this file type.
    #[error("no LSP available for language: {language}")]
    NoLspAvailable { language: String },

    /// Language server crashed or returned an error.
    #[error("LSP error: {message}")]
    LspError { message: String },

    /// A generic I/O error occurred.
    #[error("I/O error: {message}")]
    IoError { message: String },

    /// LSP didn't respond within timeout.
    #[error("LSP timeout after {timeout_ms}ms")]
    LspTimeout { timeout_ms: u64 },

    /// File is in the sandbox deny-list.
    #[error("access denied: {path}")]
    AccessDenied { path: PathBuf, tier: SandboxTier },

    /// Tree-sitter couldn't parse the file.
    #[error("parse error in {path}: {reason}")]
    ParseError { path: PathBuf, reason: String },

    /// The language of the semantic path's file is not supported.
    #[error("unsupported language for target file: {path}")]
    UnsupportedLanguage { path: PathBuf },

    /// Target symbol is incompatible with the edit type.
    #[error("invalid target: {reason}")]
    InvalidTarget {
        semantic_path: String,
        reason: String,
    },

    /// Response would exceed `max_tokens`.
    #[error("token budget exceeded: {used} / {budget}")]
    TokenBudgetExceeded { used: usize, budget: usize },

    /// `write_file` replacements: `old_text` not found in file content.
    #[error("match not found: old_text not present in file")]
    MatchNotFound { filepath: PathBuf },

    /// `write_file` replacements: `old_text` found multiple times.
    #[error("ambiguous match: old_text found {occurrences} times")]
    AmbiguousMatch {
        filepath: PathBuf,
        occurrences: usize,
    },
}

impl PathfinderError {
    /// Returns the MCP-facing error code string.
    #[must_use]
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::FileNotFound { .. } => "FILE_NOT_FOUND",
            Self::FileAlreadyExists { .. } => "FILE_ALREADY_EXISTS",
            Self::SymbolNotFound { .. } => "SYMBOL_NOT_FOUND",
            Self::AmbiguousSymbol { .. } => "AMBIGUOUS_SYMBOL",
            Self::VersionMismatch { .. } => "VERSION_MISMATCH",
            Self::ValidationFailed { .. } => "VALIDATION_FAILED",
            Self::NoLspAvailable { .. } => "NO_LSP_AVAILABLE",
            Self::LspError { .. } => "LSP_ERROR",
            Self::LspTimeout { .. } => "LSP_TIMEOUT",
            Self::AccessDenied { .. } => "ACCESS_DENIED",
            Self::IoError { .. } => "INTERNAL_ERROR",
            Self::ParseError { .. } => "PARSE_ERROR",
            Self::UnsupportedLanguage { .. } => "UNSUPPORTED_LANGUAGE",
            Self::InvalidTarget { .. } => "INVALID_TARGET",
            Self::TokenBudgetExceeded { .. } => "TOKEN_BUDGET_EXCEEDED",
            Self::MatchNotFound { .. } => "MATCH_NOT_FOUND",
            Self::AmbiguousMatch { .. } => "AMBIGUOUS_MATCH",
        }
    }

    /// Serialize to the standard MCP error JSON format.
    #[must_use]
    pub fn to_error_response(&self) -> ErrorResponse {
        ErrorResponse {
            error: self.error_code().to_owned(),
            message: self.to_string(),
            details: self.to_details(),
        }
    }

    fn to_details(&self) -> serde_json::Value {
        match self {
            Self::SymbolNotFound { did_you_mean, .. } => {
                serde_json::json!({ "did_you_mean": did_you_mean })
            }
            Self::AmbiguousSymbol { matches, .. } => {
                serde_json::json!({ "matches": matches })
            }
            Self::VersionMismatch {
                current_version_hash,
                ..
            } => {
                serde_json::json!({ "current_version_hash": current_version_hash })
            }
            Self::ValidationFailed {
                introduced_errors, ..
            } => {
                serde_json::json!({ "introduced_errors": introduced_errors })
            }
            Self::AmbiguousMatch { occurrences, .. } => {
                serde_json::json!({ "occurrences": occurrences })
            }
            Self::AccessDenied { tier, .. } => {
                serde_json::json!({ "tier": tier })
            }
            Self::TokenBudgetExceeded { used, budget } => {
                serde_json::json!({ "used": used, "budget": budget })
            }
            _ => serde_json::Value::Object(serde_json::Map::new()),
        }
    }
}

/// Standard MCP error response format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
    pub message: String,
    pub details: serde_json::Value,
}

/// A diagnostic error reported by the LSP.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DiagnosticError {
    pub severity: u8,
    pub code: String,
    pub message: String,
    pub file: String,
}

/// Sandbox tier that denied access.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum SandboxTier {
    /// Always excluded, not configurable.
    HardcodedDeny,
    /// Excluded by default, overridable in config.
    DefaultDeny,
    /// User-defined via `.pathfinderignore`.
    UserDefined,
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_error_code_mapping() {
        let err = PathfinderError::FileNotFound {
            path: "src/main.rs".into(),
        };
        assert_eq!(err.error_code(), "FILE_NOT_FOUND");

        let err = PathfinderError::SymbolNotFound {
            semantic_path: "src/auth.ts::AuthService.login".into(),
            did_you_mean: vec!["AuthService.logout".into()],
        };
        assert_eq!(err.error_code(), "SYMBOL_NOT_FOUND");
    }

    #[test]
    fn test_error_response_serialization() {
        let err = PathfinderError::VersionMismatch {
            path: "src/main.rs".into(),
            current_version_hash: "sha256:abc123".into(),
        };
        let response = err.to_error_response();

        assert_eq!(response.error, "VERSION_MISMATCH");
        assert_eq!(response.details["current_version_hash"], "sha256:abc123");

        // Verify it round-trips through JSON
        let json = serde_json::to_string(&response).expect("serialization should succeed");
        let deserialized: ErrorResponse =
            serde_json::from_str(&json).expect("deserialization should succeed");
        assert_eq!(deserialized.error, "VERSION_MISMATCH");
    }

    #[test]
    fn test_all_error_codes_are_screaming_snake_case() {
        let errors: Vec<PathfinderError> = vec![
            PathfinderError::FileNotFound { path: "a".into() },
            PathfinderError::FileAlreadyExists { path: "a".into() },
            PathfinderError::SymbolNotFound {
                semantic_path: "a".into(),
                did_you_mean: vec![],
            },
            PathfinderError::AmbiguousSymbol {
                semantic_path: "a".into(),
                matches: vec![],
            },
            PathfinderError::VersionMismatch {
                path: "a".into(),
                current_version_hash: "x".into(),
            },
            PathfinderError::ValidationFailed {
                count: 0,
                introduced_errors: vec![],
            },
            PathfinderError::NoLspAvailable {
                language: "a".into(),
            },
            PathfinderError::LspError {
                message: "a".into(),
            },
            PathfinderError::LspTimeout { timeout_ms: 0 },
            PathfinderError::AccessDenied {
                path: "a".into(),
                tier: SandboxTier::HardcodedDeny,
            },
            PathfinderError::ParseError {
                path: "a".into(),
                reason: "a".into(),
            },
            PathfinderError::UnsupportedLanguage { path: "a".into() },
            PathfinderError::InvalidTarget {
                semantic_path: "a".into(),
                reason: "a".into(),
            },
            PathfinderError::TokenBudgetExceeded { used: 0, budget: 0 },
            PathfinderError::MatchNotFound {
                filepath: "a".into(),
            },
            PathfinderError::AmbiguousMatch {
                filepath: "a".into(),
                occurrences: 0,
            },
            PathfinderError::IoError {
                message: "disk full".into(),
            },
        ];

        for err in &errors {
            let code = err.error_code();
            assert!(
                code.chars().all(|c| c.is_ascii_uppercase() || c == '_'),
                "Error code '{code}' is not SCREAMING_SNAKE_CASE"
            );
        }
    }

    #[test]
    fn test_symbol_not_found_details_include_did_you_mean() {
        let err = PathfinderError::SymbolNotFound {
            semantic_path: "src/auth.ts::startServer".into(),
            did_you_mean: vec!["stopServer".into(), "startService".into()],
        };
        let response = err.to_error_response();
        let suggestions = response.details["did_you_mean"]
            .as_array()
            .expect("did_you_mean should be an array");
        assert_eq!(suggestions.len(), 2);
    }
}
