//! Pathfinder error taxonomy.
//!
//! All tools return errors in a standardized format:
//! `{ "error": "ERROR_CODE", "message": "...", "details": {} }`
//!
//! See PRD §5 for the full error taxonomy.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A standardized error type for Pathfinder operations.
#[derive(Debug, thiserror::Error)]
pub enum PathfinderError {
    /// File path doesn't exist.
    #[error("file not found: {path}")]
    FileNotFound {
        /// Path to the missing file.
        path: PathBuf,
    },

    /// File already exists (for `create_file`).
    #[error("file already exists: {path}")]
    FileAlreadyExists {
        /// Path to the existing file.
        path: PathBuf,
    },

    /// Semantic path doesn't resolve.
    #[error("symbol not found: {semantic_path}")]
    SymbolNotFound {
        /// The semantic path that wasn't found.
        semantic_path: String,
        /// Similar symbol names suggested by the system (Levenshtein distance).
        did_you_mean: Vec<String>,
    },

    /// Semantic path is malformed or missing required '::' separator.
    #[error("invalid semantic path: {input}")]
    InvalidSemanticPath {
        /// The invalid input string.
        input: String,
        /// Description of what makes it invalid.
        issue: String,
    },

    /// Multiple matches for a semantic path.
    #[error("ambiguous symbol: {semantic_path}")]
    AmbiguousSymbol {
        /// The ambiguous semantic path.
        semantic_path: String,
        /// All matching symbol paths.
        matches: Vec<String>,
    },

    /// File changed since last read. OCC violation.
    ///
    /// `lines_changed` is a lightweight `"+N/-M"` summary (O(N) line count comparison).
    /// It helps agents decide whether to retry without a full re-read.
    #[error("version mismatch for {path}")]
    VersionMismatch {
        path: PathBuf,
        current_version_hash: String,
        /// Optional `"+N/-M"` delta between the agent's version and the current version.
        lines_changed: Option<String>,
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
        /// For batch edits: index of the failed edit.
        edit_index: Option<usize>,
        /// For batch edits: valid options when `edit_type` is missing/invalid.
        valid_edit_types: Option<Vec<String>>,
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

    /// `replace_batch` text targeting: `old_text` not found within the
    /// ±25-line context window around `context_line`.
    ///
    /// The entire batch is rejected when any edit fails to resolve.
    #[error("text not found: '{old_text}' not found within ±25 lines of line {context_line} in {filepath}")]
    TextNotFound {
        filepath: PathBuf,
        old_text: String,
        context_line: u32,
        /// Snippet of actual content at `context_line` (for debugging)
        actual_content: Option<String>,
        /// Closest matching substring found in the window (for agent self-correction).
        /// Present when a near-match (>60% character overlap) exists but exact match fails.
        closest_match: Option<String>,
    },

    /// Path traversal detected in `resolve_strict`.
    #[error("path traversal rejected: {path} escapes workspace root {workspace_root}")]
    PathTraversal {
        path: PathBuf,
        workspace_root: PathBuf,
    },

    /// `replace_batch` post-apply structural validation detected that the
    /// combined edits introduced parse errors not present in the original source.
    /// This typically indicates nesting corruption from adjacent symbol replacements.
    #[error(
        "batch structural corruption in {filepath}: {new_errors} new parse error(s) introduced"
    )]
    BatchStructuralCorruption {
        filepath: String,
        original_errors: usize,
        new_errors: usize,
    },
}

impl PathfinderError {
    /// Returns the MCP-facing error code string.
    #[must_use]
    pub const fn error_code(&self) -> &'static str {
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
            Self::TextNotFound { .. } => "TEXT_NOT_FOUND",
            Self::InvalidSemanticPath { .. } => "INVALID_SEMANTIC_PATH",
            Self::PathTraversal { .. } => "PATH_TRAVERSAL",
            Self::BatchStructuralCorruption { .. } => "BATCH_STRUCTURAL_CORRUPTION",
        }
    }

    /// Returns an actionable hint for the agent to self-correct without additional round-trips.
    ///
    /// `SYMBOL_NOT_FOUND` hints are dynamic and built from the `did_you_mean` suggestions.
    /// All other hints are static strings referencing specific Pathfinder tools.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn hint(&self) -> Option<String> {
        match self {
            Self::SymbolNotFound {
                semantic_path,
                did_you_mean,
            } => {
                // Detect path separator confusion: agent may have used `.` instead of `::`
                // (e.g., `src/lib.rs.MyStruct.method` instead of `src/lib.rs::MyStruct.method`)
                // or used `::` between nested symbols where `.` is expected.
                let separator_hint = if !semantic_path.contains("::") {
                    Some(
                        " Note: semantic paths require '::' between the file and symbol \
                         (e.g., 'src/lib.rs::MyStruct.method'). \
                         Nested symbols within the same file use '.' (e.g., 'MyStruct.method')."
                    )
                } else if semantic_path.matches("::").count() > 1 {
                    Some(
                        " Note: only one '::' is allowed — between the file path and the symbol. \
                         Nested symbols within the file use '.' (e.g., 'src/lib.rs::Outer.Inner.method')."
                    )
                } else {
                    None
                };

                if did_you_mean.is_empty() {
                    Some(format!(
                        "Use read_source_file to see available symbols in this file.{}",
                        separator_hint.unwrap_or("")
                    ))
                } else {
                    Some(format!(
                        "Did you mean: {}? Use read_source_file to see available symbols.{}",
                        did_you_mean.join(", "),
                        separator_hint.unwrap_or("")
                    ))
                }
            }
            Self::InvalidTarget { valid_edit_types, .. } => {
                if valid_edit_types.is_some() {
                    Some("Set edit_type to one of: 'replace_body', 'replace_full', 'insert_before', 'insert_after', 'delete'. Or set old_text + context_line for text-based targeting.".to_owned())
                } else {
                    Some("replace_body requires a block-bodied construct. For constants, use replace_full.".to_owned())
                }
            }
            Self::VersionMismatch { .. } => Some(
                "The file was modified. Use the new hash to retry your edit if the changes \
                 do not overlap with your target."
                    .to_owned(),
            ),
            Self::AccessDenied { .. } => {
                Some("File is outside workspace sandbox. Check .pathfinderignore rules.".to_owned())
            }
            Self::UnsupportedLanguage { .. } => Some(
                "No tree-sitter grammar for this file type. Use read_file and write_file instead."
                    .to_owned(),
            ),
            Self::FileNotFound { .. } => Some(
                "Verify the file path is relative to the workspace root and the file exists."
                    .to_owned(),
            ),
            Self::ValidationFailed {
                count,
                introduced_errors,
                ..
            } => {
                let first_error = introduced_errors.first().map(|e| {
                    format!(
                        " First error: \"{}\" in {} (line ~{}).",
                        e.message.chars().take(120).collect::<String>(),
                        e.file,
                        // DiagnosticError doesn't carry a line number directly;
                        // the message often contains it — surface what we have.
                        e.code
                    )
                });
                Some(format!(
                    "Validation failed: {count} new error(s) introduced.{} \
                     Set ignore_validation_failures=true to write despite errors, \
                     or fix the introduced errors before retrying.",
                    first_error.as_deref().unwrap_or("")
                ))
            }
            Self::MatchNotFound { .. } => Some(
                "The old_text was not found in the file. Use read_file to verify the exact text \
                 before retrying."
                    .to_owned(),
            ),
            Self::AmbiguousMatch { occurrences, .. } => Some(format!(
                "old_text matched {occurrences} times. Make it more specific or use \
                 replace_batch with a semantic_path to target a single symbol."
            )),
            Self::TextNotFound { context_line, closest_match, .. } => {
                let base = format!(
                    "The old_text was not found within ±25 lines of line {context_line}. \
                     Use read_source_file to verify the exact text and adjust context_line."
                );
                if let Some(candidate) = closest_match {
                    Some(format!("{base} Closest match found: '{candidate}'."))
                } else {
                    Some(base)
                }
            }
            Self::InvalidSemanticPath { input, .. } => Some(format!(
                "'{input}' is missing the file path — did you mean 'crates/.../file.rs::{input}'? \
                 Semantic paths must include the file path and '::' separator (e.g., 'src/auth.ts::AuthService.login')."
            )),
            Self::PathTraversal { .. } => Some(
                "Path traversal is not allowed. Use a relative path without '..' components or absolute paths."
                    .to_owned(),
            ),
            Self::LspError { message } => {
                let hint = if message.contains("timed out") || message.contains("timeout") {
                    format!(
                        "LSP timed out. The language server may still be indexing, under memory pressure, or deadlocked. \
                         Workaround: use search_codebase + read_symbol_scope (tree-sitter) instead of \
                         LSP-dependent tools (get_definition, analyze_impact, read_with_deep_context). \
                         Original error: {message}"
                    )
                } else if message.contains("connection lost") || message.contains("crashed") {
                    format!(
                        "LSP process crashed or disconnected. Pathfinder will attempt to restart it. \
                         Workaround: use tree-sitter-based tools (search_codebase, read_symbol_scope, read_source_file). \
                         Original error: {message}"
                    )
                } else {
                    format!(
                        "LSP error: {message}. Workaround: use search_codebase for text-based navigation \
                         or check lsp_health for current status."
                    )
                };
                Some(hint)
            }
            Self::LspTimeout { timeout_ms } => Some(format!(
                "LSP timed out after {timeout_ms}ms. The language server may still be indexing, under memory pressure, or deadlocked. \
                 Workaround: use search_codebase + read_symbol_scope (tree-sitter) instead of \
                 LSP-dependent tools (get_definition, analyze_impact, read_with_deep_context). \
                 Check lsp_health for current status."
            )),
            Self::NoLspAvailable { language } => Some(format!(
                "No LSP available for {language}. Install a language server to enable LSP-dependent features. \
                 Tree-sitter tools (read_symbol_scope, search_codebase, read_source_file) still work without LSP."
            )),
            Self::BatchStructuralCorruption { .. } => Some(
                "The combined batch edits produced structurally invalid code. \
                 This usually happens when adjacent symbol replacements interact (e.g., \
                 consuming each other's closing braces). Use sequential individual edit \
                 tools (replace_full, replace_body) instead of replace_batch for these edits."
                    .to_string(),
            ),
            _ => None,
        }
    }

    /// Serialize to the standard MCP error JSON format.
    #[must_use]
    pub fn to_error_response(&self) -> ErrorResponse {
        ErrorResponse {
            error: self.error_code().to_owned(),
            message: self.to_string(),
            details: self.to_details(),
            hint: self.hint(),
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
                lines_changed,
                ..
            } => {
                serde_json::json!({
                    "current_version_hash": current_version_hash,
                    "lines_changed": lines_changed,
                })
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
            Self::InvalidSemanticPath { issue, .. } => {
                serde_json::json!({ "issue": issue })
            }
            Self::InvalidTarget {
                edit_index,
                valid_edit_types,
                ..
            } => {
                let mut map = serde_json::Map::new();
                if let Some(idx) = edit_index {
                    map.insert("edit_index".to_string(), serde_json::json!(idx));
                }
                if let Some(types) = valid_edit_types {
                    map.insert("valid_edit_types".to_string(), serde_json::json!(types));
                }
                serde_json::Value::Object(map)
            }
            Self::TextNotFound {
                filepath,
                old_text,
                context_line,
                actual_content,
                closest_match,
            } => {
                let mut map = serde_json::Map::new();
                map.insert("filepath".to_string(), serde_json::json!(filepath));
                map.insert("old_text".to_string(), serde_json::json!(old_text));
                map.insert("context_line".to_string(), serde_json::json!(context_line));
                if let Some(content) = actual_content {
                    map.insert("actual_content".to_string(), serde_json::json!(content));
                }
                if let Some(candidate) = closest_match {
                    map.insert("closest_match".to_string(), serde_json::json!(candidate));
                }
                serde_json::Value::Object(map)
            }
            Self::PathTraversal {
                path,
                workspace_root,
            } => {
                serde_json::json!({ "path": path, "workspace_root": workspace_root })
            }
            Self::BatchStructuralCorruption {
                filepath,
                original_errors,
                new_errors,
            } => {
                serde_json::json!({
                    "filepath": filepath,
                    "original_errors": original_errors,
                    "new_errors": new_errors,
                })
            }
            _ => serde_json::Value::Object(serde_json::Map::new()),
        }
    }
}

/// Standard MCP error response format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    /// Error code identifying the type of error.
    pub error: String,
    /// Human-readable message describing the error.
    pub message: String,
    /// Additional details about the error in JSON format.
    pub details: serde_json::Value,
    /// Actionable recovery hint for the agent. Present on most error variants.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// Compute a lightweight `"+N/-M"` lines-changed summary.
///
/// Compares line counts between `old_content` and `new_content` in O(N) time
/// (no diff algorithm — just counts newlines in each string).
///
/// Used by `VERSION_MISMATCH` errors so agents can gauge how much the file
/// changed and decide whether to retry without a full re-read.
#[must_use]
pub fn compute_lines_changed(old_content: &str, new_content: &str) -> String {
    let old_lines = old_content.lines().count();
    let new_lines = new_content.lines().count();
    let added = new_lines.saturating_sub(old_lines);
    let removed = old_lines.saturating_sub(new_lines);
    format!("+{added}/-{removed}")
}

/// A diagnostic error reported by the LSP.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DiagnosticError {
    /// The severity level of the diagnostic error.
    pub severity: u8,
    /// The diagnostic error code.
    pub code: String,
    /// The error message describing the diagnostic.
    pub message: String,
    /// The file path related to the diagnostic.
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
            lines_changed: Some("+5/-2".into()),
        };
        let response = err.to_error_response();

        assert_eq!(response.error, "VERSION_MISMATCH");
        assert_eq!(response.details["current_version_hash"], "sha256:abc123");
        assert_eq!(response.details["lines_changed"], "+5/-2");
        assert!(
            response.hint.is_some(),
            "VERSION_MISMATCH should carry a hint"
        );

        // Verify it round-trips through JSON
        let json = serde_json::to_string(&response).expect("serialization should succeed");
        let deserialized: ErrorResponse =
            serde_json::from_str(&json).expect("deserialization should succeed");
        assert_eq!(deserialized.error, "VERSION_MISMATCH");
    }

    #[test]
    fn test_hint_file_not_found() {
        let err = PathfinderError::FileNotFound { path: "a".into() };
        let hint = err.hint().expect("should have hint");
        assert!(hint.contains("relative"), "hint: {hint}");
    }

    #[test]
    fn test_hint_validation_failed() {
        let err = PathfinderError::ValidationFailed {
            count: 0,
            introduced_errors: vec![],
        };
        let hint = err.hint().expect("should have hint");
        assert!(hint.contains("ignore_validation_failures"), "hint: {hint}");
    }

    #[test]
    fn test_hint_ambiguous_match() {
        let err = PathfinderError::AmbiguousMatch {
            filepath: "a".into(),
            occurrences: 2,
        };
        let hint = err.hint().expect("should have hint");
        assert!(hint.contains("2 times"), "hint: {hint}");
    }

    #[test]
    fn test_hint_invalid_semantic_path() {
        let err = PathfinderError::InvalidSemanticPath {
            input: "x".into(),
            issue: "y".into(),
        };
        let hint = err.hint().expect("should have hint");
        assert!(hint.contains("'x' is missing"), "hint: {hint}");
    }

    #[test]
    fn test_hint_file_already_exists() {
        let err = PathfinderError::FileAlreadyExists { path: "a".into() };
        assert!(err.hint().is_none());
    }

    // ── GAP-008: LSP error hints ────────────────────────────────────

    #[test]
    fn test_lsp_error_hint_timeout_includes_workaround() {
        let err = PathfinderError::LspError {
            message: "LSP timed out on 'textDocument/definition' after 10000ms".to_owned(),
        };
        let hint = err.hint().expect("LspError should have a hint");
        assert!(
            hint.contains("search_codebase"),
            "hint should mention search_codebase: {hint}"
        );
        assert!(
            hint.contains("tree-sitter"),
            "hint should mention tree-sitter: {hint}"
        );
    }

    #[test]
    fn test_lsp_error_hint_connection_lost() {
        let err = PathfinderError::LspError {
            message: "connection lost to language server".to_owned(),
        };
        let hint = err.hint().expect("LspError should have a hint");
        assert!(
            hint.contains("crashed or disconnected"),
            "hint should mention crash: {hint}"
        );
        assert!(
            hint.contains("read_source_file"),
            "hint should mention tree-sitter tools: {hint}"
        );
    }

    #[test]
    fn test_lsp_error_hint_generic() {
        let err = PathfinderError::LspError {
            message: "unexpected internal error".to_owned(),
        };
        let hint = err.hint().expect("LspError should have a hint");
        assert!(
            hint.contains("search_codebase"),
            "hint should mention search_codebase: {hint}"
        );
        assert!(
            hint.contains("lsp_health"),
            "hint should mention lsp_health: {hint}"
        );
    }

    #[test]
    fn test_lsp_timeout_hint_includes_workaround() {
        let err = PathfinderError::LspTimeout { timeout_ms: 10000 };
        let hint = err.hint().expect("LspTimeout should have a hint");
        assert!(
            hint.contains("10000ms"),
            "hint should include timeout duration: {hint}"
        );
        assert!(
            hint.contains("search_codebase"),
            "hint should mention search_codebase: {hint}"
        );
        assert!(
            hint.contains("tree-sitter"),
            "hint should mention tree-sitter: {hint}"
        );
        assert!(
            hint.contains("lsp_health"),
            "hint should mention lsp_health: {hint}"
        );
    }

    #[test]
    fn test_no_lsp_hint_mentions_tree_sitter() {
        let err = PathfinderError::NoLspAvailable {
            language: "go".to_owned(),
        };
        let hint = err.hint().expect("NoLspAvailable should have a hint");
        assert!(hint.contains("go"), "hint should mention language: {hint}");
        assert!(
            hint.to_lowercase().contains("tree-sitter"),
            "hint should mention tree-sitter: {hint}"
        );
        assert!(
            hint.contains("read_symbol_scope"),
            "hint should mention read_symbol_scope: {hint}"
        );
    }

    #[test]
    fn test_details_serialization_extra() {
        let err = PathfinderError::AmbiguousSymbol {
            semantic_path: "a".into(),
            matches: vec!["b".into()],
        };
        assert_eq!(err.to_details()["matches"][0], "b");

        let err = PathfinderError::ValidationFailed {
            count: 0,
            introduced_errors: vec![],
        };
        assert!(err.to_details().get("introduced_errors").is_some());

        let err = PathfinderError::AmbiguousMatch {
            filepath: "a".into(),
            occurrences: 3,
        };
        assert_eq!(err.to_details()["occurrences"], 3);

        let err = PathfinderError::AccessDenied {
            path: "a".into(),
            tier: SandboxTier::UserDefined,
        };
        assert_eq!(err.to_details()["tier"], "UserDefined");

        let err = PathfinderError::TokenBudgetExceeded {
            used: 10,
            budget: 5,
        };
        assert_eq!(err.to_details()["used"], 10);
        assert_eq!(err.to_details()["budget"], 5);

        let err = PathfinderError::InvalidSemanticPath {
            input: "a".into(),
            issue: "b".into(),
        };
        assert_eq!(err.to_details()["issue"], "b");

        let err = PathfinderError::FileNotFound { path: "a".into() };
        assert!(err
            .to_details()
            .as_object()
            .expect("should be an object")
            .is_empty());

        let err = PathfinderError::TextNotFound {
            filepath: "a.vue".into(),
            old_text: "<button>Check</button>".into(),
            context_line: 42,
            actual_content: Some("content".into()),
            closest_match: None,
        };
        assert_eq!(err.to_details()["actual_content"], "content");
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
                lines_changed: None,
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
                edit_index: None,
                valid_edit_types: None,
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
            PathfinderError::TextNotFound {
                filepath: "a.vue".into(),
                old_text: "<button>Check</button>".into(),
                context_line: 42,
                actual_content: None,
                closest_match: None,
            },
            PathfinderError::InvalidSemanticPath {
                input: "send".into(),
                issue: "missing ::".into(),
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

    // ── E7.2: compute_lines_changed ─────────────────────────────────

    #[test]
    fn test_compute_lines_changed_lines_added() {
        // old: 2 lines, new: 5 lines → +3/-0
        let old = "line1\nline2";
        let new = "line1\nline2\nline3\nline4\nline5";
        assert_eq!(compute_lines_changed(old, new), "+3/-0");
    }

    #[test]
    fn test_compute_lines_changed_lines_removed() {
        // old: 4 lines, new: 2 lines → +0/-2
        let old = "a\nb\nc\nd";
        let new = "a\nb";
        assert_eq!(compute_lines_changed(old, new), "+0/-2");
    }

    #[test]
    fn test_compute_lines_changed_mixed() {
        // old: 3 lines, new: 4 lines → +1/-0
        let old = "a\nb\nc";
        let new = "a\nb\nc\nd";
        assert_eq!(compute_lines_changed(old, new), "+1/-0");
    }

    #[test]
    fn test_compute_lines_changed_identical() {
        let content = "same\ncontent\nhere";
        assert_eq!(compute_lines_changed(content, content), "+0/-0");
    }

    #[test]
    fn test_compute_lines_changed_empty_to_nonempty() {
        assert_eq!(compute_lines_changed("", "a\nb\nc"), "+3/-0");
    }

    // ── E7.3: hint() method ─────────────────────────────────────────

    #[test]
    fn test_version_mismatch_hint_is_present() {
        let err = PathfinderError::VersionMismatch {
            path: "src/lib.rs".into(),
            current_version_hash: "sha256:new".into(),
            lines_changed: Some("+2/-1".into()),
        };
        let hint = err.hint().expect("VERSION_MISMATCH should have a hint");
        assert!(
            hint.contains("new hash"),
            "hint should mention re-reading: {hint}"
        );
    }

    #[test]
    fn test_symbol_not_found_hint_with_suggestions() {
        let err = PathfinderError::SymbolNotFound {
            semantic_path: "src/auth.ts::login".into(),
            did_you_mean: vec!["logout".into(), "logIn".into()],
        };
        let hint = err.hint().expect("should have hint");
        assert!(
            hint.contains("logout"),
            "hint should include suggestions: {hint}"
        );
        assert!(
            hint.contains("logIn"),
            "hint should include all suggestions: {hint}"
        );
    }

    #[test]
    fn test_symbol_not_found_hint_without_suggestions() {
        let err = PathfinderError::SymbolNotFound {
            semantic_path: "src/auth.ts::unknown".into(),
            did_you_mean: vec![],
        };
        let hint = err
            .hint()
            .expect("should have hint even without suggestions");
        assert!(
            hint.contains("read_source_file"),
            "hint should point to read_source_file: {hint}"
        );
    }

    #[test]
    fn test_access_denied_hint_mentions_sandbox() {
        let err = PathfinderError::AccessDenied {
            path: ".env".into(),
            tier: SandboxTier::HardcodedDeny,
        };
        let hint = err.hint().expect("ACCESS_DENIED should have a hint");
        assert!(
            hint.contains("sandbox"),
            "hint should mention sandbox: {hint}"
        );
    }

    #[test]
    fn test_unsupported_language_hint_mentions_write_file() {
        let err = PathfinderError::UnsupportedLanguage {
            path: "data.xyz".into(),
        };
        let hint = err.hint().expect("UNSUPPORTED_LANGUAGE should have a hint");
        assert!(
            hint.contains("write_file"),
            "hint should mention write_file: {hint}"
        );
    }

    #[test]
    fn test_validation_failed_hint_mentions_ignore_flag() {
        let err = PathfinderError::ValidationFailed {
            count: 2,
            introduced_errors: vec![],
        };
        let hint = err.hint().expect("VALIDATION_FAILED should have a hint");
        assert!(
            hint.contains("ignore_validation_failures"),
            "hint should mention the flag: {hint}"
        );
    }

    #[test]
    fn test_match_not_found_hint_mentions_read_file() {
        let err = PathfinderError::MatchNotFound {
            filepath: "config.yaml".into(),
        };
        let hint = err.hint().expect("MATCH_NOT_FOUND should have a hint");
        assert!(
            hint.contains("read_file"),
            "hint should mention read_file: {hint}"
        );
    }

    #[test]
    fn test_hint_serialized_in_error_response() {
        let err = PathfinderError::InvalidTarget {
            semantic_path: "src/lib.rs::CONST".into(),
            reason: "not a block construct".into(),
            edit_index: None,
            valid_edit_types: None,
        };
        let resp = err.to_error_response();
        assert!(
            resp.hint.is_some(),
            "hint must be serialized in ErrorResponse"
        );
        let json = serde_json::to_value(&resp).expect("serialize");
        assert!(
            json.get("hint").is_some(),
            "hint must appear in JSON output"
        );
    }

    #[test]
    fn test_text_not_found_hint_mentions_context_line() {
        let err = PathfinderError::TextNotFound {
            filepath: "src/component.vue".into(),
            old_text: "<button>Check</button>".to_owned(),
            context_line: 42,
            actual_content: None,
            closest_match: None,
        };
        assert_eq!(err.error_code(), "TEXT_NOT_FOUND");
        let hint = err.hint().expect("TEXT_NOT_FOUND should have a hint");
        assert!(
            hint.contains("42"),
            "hint should mention context_line: {hint}"
        );
        assert!(
            hint.contains("read_source_file"),
            "hint should reference read_source_file: {hint}"
        );
    }

    #[test]
    fn test_text_not_found_hint_with_closest_match() {
        let err = PathfinderError::TextNotFound {
            filepath: "src/auth.ts".into(),
            old_text: "const x = 1;".to_owned(),
            context_line: 10,
            actual_content: None,
            closest_match: Some("const x = 2;".to_owned()),
        };
        let hint = err
            .hint()
            .expect("TEXT_NOT_FOUND with closest_match should have a hint");
        assert!(
            hint.contains("const x = 2;"),
            "hint should include the closest match candidate: {hint}"
        );
        let response = err.to_error_response();
        assert_eq!(response.details["closest_match"], "const x = 2;");
    }

    #[test]
    fn test_path_traversal_error() {
        let err = PathfinderError::PathTraversal {
            path: "../../etc/passwd".into(),
            workspace_root: "/workspace".into(),
        };

        assert_eq!(err.error_code(), "PATH_TRAVERSAL");
        let hint = err.hint().expect("PATH_TRAVERSAL should have a hint");
        assert!(
            hint.contains("not allowed"),
            "hint should explain traversal is not allowed: {hint}"
        );

        let response = err.to_error_response();
        assert_eq!(response.error, "PATH_TRAVERSAL");
        assert_eq!(response.details["path"], "../../etc/passwd");
        assert_eq!(response.details["workspace_root"], "/workspace");
    }

    // ── InvalidTarget coverage ──────────────────────────────────────

    #[test]
    fn test_invalid_target_hint_with_valid_edit_types() {
        let err = PathfinderError::InvalidTarget {
            semantic_path: "src/lib.rs::MY_CONST".into(),
            reason: "missing edit_type".into(),
            edit_index: Some(0),
            valid_edit_types: Some(vec!["replace_full".into(), "delete".into()]),
        };
        let hint = err
            .hint()
            .expect("should have hint when valid_edit_types is Some");
        assert!(
            hint.contains("replace_body"),
            "hint should list valid edit types: {hint}"
        );
        assert!(
            hint.contains("old_text"),
            "hint should mention text-based targeting: {hint}"
        );
    }

    #[test]
    fn test_invalid_target_hint_without_valid_edit_types() {
        let err = PathfinderError::InvalidTarget {
            semantic_path: "src/lib.rs::MY_CONST".into(),
            reason: "not a block construct".into(),
            edit_index: None,
            valid_edit_types: None,
        };
        let hint = err
            .hint()
            .expect("should have hint when valid_edit_types is None");
        assert!(
            hint.contains("replace_body requires"),
            "hint should explain replace_body limitation: {hint}"
        );
        assert!(
            hint.contains("replace_full"),
            "hint should suggest replace_full: {hint}"
        );
    }

    #[test]
    fn test_invalid_target_details_with_edit_index_and_types() {
        let err = PathfinderError::InvalidTarget {
            semantic_path: "src/lib.rs::func".into(),
            reason: "bad edit_type".into(),
            edit_index: Some(2),
            valid_edit_types: Some(vec!["replace_full".into(), "delete".into()]),
        };
        let details = err.to_details();
        assert_eq!(details["edit_index"], 2, "edit_index should be present");
        let types = details["valid_edit_types"]
            .as_array()
            .expect("valid_edit_types should be an array");
        assert_eq!(types.len(), 2);
        assert_eq!(types[0], "replace_full");
        assert_eq!(types[1], "delete");
    }
}
