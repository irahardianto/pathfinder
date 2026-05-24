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

    /// Response would exceed `max_tokens`.
    #[error("token budget exceeded: {used} / {budget}")]
    TokenBudgetExceeded { used: usize, budget: usize },

    /// Path traversal detected in `resolve_strict`.
    #[error("path traversal rejected: {path} escapes workspace root {workspace_root}")]
    PathTraversal {
        path: PathBuf,
        workspace_root: PathBuf,
    },
}

impl PathfinderError {
    /// Returns the MCP-facing error code string.
    #[must_use]
    pub const fn error_code(&self) -> &'static str {
        match self {
            Self::FileNotFound { .. } => "FILE_NOT_FOUND",
            Self::SymbolNotFound { .. } => "SYMBOL_NOT_FOUND",
            Self::AmbiguousSymbol { .. } => "AMBIGUOUS_SYMBOL",
            Self::NoLspAvailable { .. } => "NO_LSP_AVAILABLE",
            Self::LspError { .. } => "LSP_ERROR",
            Self::LspTimeout { .. } => "LSP_TIMEOUT",
            Self::AccessDenied { .. } => "ACCESS_DENIED",
            Self::IoError { .. } => "INTERNAL_ERROR",
            Self::ParseError { .. } => "PARSE_ERROR",
            Self::UnsupportedLanguage { .. } => "UNSUPPORTED_LANGUAGE",
            Self::TokenBudgetExceeded { .. } => "TOKEN_BUDGET_EXCEEDED",
            Self::InvalidSemanticPath { .. } => "INVALID_SEMANTIC_PATH",
            Self::PathTraversal { .. } => "PATH_TRAVERSAL",
        }
    }

    /// Returns an actionable hint for the agent to self-correct without additional round-trips.
    ///
    /// `SYMBOL_NOT_FOUND` hints are dynamic and built from the `did_you_mean` suggestions.
    /// All other hints are static strings referencing specific Pathfinder tools.
    #[must_use]
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
                    // No suggestions — the symbol might be in a different file than what the agent guessed.
                    // Suggest search_codebase to find which file actually defines this symbol.
                    let symbol_name = semantic_path.split("::").last().unwrap_or(semantic_path);
                    Some(format!(
                        "Symbol not found in the specified file. Use search_codebase(query=\"{symbol_name}\") to find which file defines this symbol, then use the correct file path in the semantic path.{}",
                        separator_hint.unwrap_or("")
                    ))
                } else {
                    Some(format!(
                        "Did you mean: {}? Use search_codebase if the symbol is in a different file, or read_source_file to see available symbols in this file.{}",
                        did_you_mean.join(", "),
                        separator_hint.unwrap_or("")
                    ))
                }
            }
            Self::AccessDenied { .. } => {
                Some("File is outside workspace sandbox. Check .pathfinderignore rules.".to_owned())
            }
            Self::UnsupportedLanguage { .. } => Some(
                "No tree-sitter grammar for this file type. Use read_file for raw content."
                    .to_owned(),
            ),
            Self::FileNotFound { .. } => Some(
                "Verify the file path is relative to the workspace root and the file exists."
                    .to_owned(),
            ),
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
            Self::AccessDenied { tier, .. } => {
                serde_json::json!({ "tier": tier })
            }
            Self::TokenBudgetExceeded { used, budget } => {
                serde_json::json!({ "used": used, "budget": budget })
            }
            Self::InvalidSemanticPath { issue, .. } => {
                serde_json::json!({ "issue": issue })
            }
            Self::PathTraversal {
                path,
                workspace_root,
            } => {
                serde_json::json!({ "path": path, "workspace_root": workspace_root })
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
    fn test_hint_file_not_found() {
        let err = PathfinderError::FileNotFound { path: "a".into() };
        let hint = err.hint().expect("should have hint");
        assert!(hint.contains("relative"), "hint: {hint}");
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
    }

    #[test]
    fn test_all_error_codes_are_screaming_snake_case() {
        let errors: Vec<PathfinderError> = vec![
            PathfinderError::FileNotFound { path: "a".into() },
            PathfinderError::SymbolNotFound {
                semantic_path: "a".into(),
                did_you_mean: vec![],
            },
            PathfinderError::AmbiguousSymbol {
                semantic_path: "a".into(),
                matches: vec![],
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
            PathfinderError::TokenBudgetExceeded { used: 0, budget: 0 },
            PathfinderError::IoError {
                message: "disk full".into(),
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

    // ── E7.3: hint() method ─────────────────────────────────────────

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
        // When no suggestions, the symbol is likely in a different file.
        // Hint should suggest search_codebase to find the correct file.
        assert!(
            hint.contains("search_codebase"),
            "hint should suggest search_codebase to find the correct file: {hint}"
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
    fn test_unsupported_language_hint_mentions_read_file() {
        let err = PathfinderError::UnsupportedLanguage {
            path: "data.xyz".into(),
        };
        let hint = err.hint().expect("UNSUPPORTED_LANGUAGE should have a hint");
        assert!(
            hint.contains("read_file"),
            "hint should mention read_file: {hint}"
        );
    }

    #[test]
    fn test_hint_serialized_in_error_response() {
        let err = PathfinderError::AccessDenied {
            path: ".env".into(),
            tier: SandboxTier::HardcodedDeny,
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
}
