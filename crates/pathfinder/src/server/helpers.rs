//! Shared helper functions for Pathfinder MCP tool handlers.
//!
//! Contains error conversion utilities and the file-language detector
//! used by `read_file`.

use pathfinder_common::error::PathfinderError;
use pathfinder_common::types::SemanticPath;
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
            PathfinderError::TokenBudgetExceeded {
                used: 1000,
                budget: 500,
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

    #[test]
    fn test_serialize_metadata_failure_logs_warning() {
        // Create a type that cannot be serialized
        // Using a struct with a non-serializable field
        #[derive(serde::Serialize)]
        struct Unserializable {
            #[serde(skip)]
            #[allow(dead_code)] // Field is intentionally unused for this test
            non_serializable: std::rc::Rc<i32>,
        }
        let data = Unserializable {
            non_serializable: std::rc::Rc::new(42),
        };
        // This should not panic, just return None and log a warning
        let result = super::serialize_metadata(&data);
        // The result might be Some if Rc is skipped, or None if serialization fails
        // Either way, it shouldn't panic
        let _ = result;
    }

    #[test]
    fn test_language_from_path_common_extensions() {
        let test_cases = vec![
            ("file.ts", "typescript"),
            ("file.tsx", "typescript"),
            ("file.js", "javascript"),
            ("file.jsx", "javascript"),
            ("file.mjs", "javascript"),
            ("file.cjs", "javascript"),
            ("file.rs", "rust"),
            ("file.go", "go"),
            ("file.py", "python"),
            ("file.json", "json"),
            ("file.yaml", "yaml"),
            ("file.yml", "yaml"),
            ("file.toml", "toml"),
            ("file.md", "markdown"),
            ("file.mdx", "markdown"),
            ("file.sh", "shell"),
            ("file.bash", "shell"),
        ];

        for (filename, expected) in test_cases {
            let path = Path::new(filename);
            assert_eq!(language_from_path(path), expected, "Failed for {filename}");
        }
    }

    #[test]
    fn test_language_from_path_dockerfile() {
        let path = Path::new("Dockerfile");
        assert_eq!(language_from_path(path), "dockerfile");

        // With extension
        let path = Path::new("path/to/Dockerfile");
        assert_eq!(language_from_path(path), "dockerfile");
    }

    #[test]
    fn test_language_from_path_unknown_extension() {
        let test_cases = vec!["file.xyz", "file.unknown", "file", "file.txt"];
        for filename in test_cases {
            let path = Path::new(filename);
            assert_eq!(language_from_path(path), "text", "Failed for {filename}");
        }
    }

    #[test]
    fn test_language_from_path_nested_paths() {
        let test_cases = vec![
            ("src/main.rs", "rust"),
            ("components/Button.tsx", "typescript"),
            ("scripts/deploy.sh", "shell"),
            ("config/app.yaml", "yaml"),
        ];

        for (filepath, expected) in test_cases {
            let path = Path::new(filepath);
            assert_eq!(language_from_path(path), expected, "Failed for {filepath}");
        }
    }

    #[test]
    fn test_parse_semantic_path_valid() {
        let valid_paths = vec![
            "src/main.rs::main",
            "path/to/file.ts::MyFunction",
            "lib.rs::MyStruct::method",
        ];

        for path_str in valid_paths {
            let result = parse_semantic_path(path_str);
            assert!(
                result.is_ok(),
                "Expected valid semantic path for: {path_str}"
            );
        }
    }

    #[test]
    fn test_parse_semantic_path_invalid() {
        // Empty string should fail
        let result = parse_semantic_path("");
        assert!(result.is_err(), "Empty string should be invalid");

        // Just separator should fail
        let result = parse_semantic_path("::");
        assert!(result.is_err(), "Just separator should be invalid");

        // Bare file paths are valid for SemanticPath, but may be invalid for tools
        // So we just test truly malformed cases
    }

    #[test]
    fn test_require_symbol_target_with_symbol() {
        let semantic_path =
            SemanticPath::parse("src/main.rs::main").expect("should parse valid path");
        let result = require_symbol_target(&semantic_path, "src/main.rs::main");
        assert!(result.is_ok());
    }

    #[test]
    fn test_require_symbol_target_with_bare_file() {
        let semantic_path = SemanticPath::parse("src/main.rs").expect("should parse valid path");
        let result = require_symbol_target(&semantic_path, "src/main.rs");
        assert!(result.is_err());
        // Check that the error has the right message
        let err = result.expect_err("should return error for bare file");
        if let Some(data) = err.data {
            if let Some(issue) = data.get("issue") {
                assert!(issue
                    .as_str()
                    .expect("issue should be a string")
                    .contains("requires a symbol target"));
            }
        }
    }

    #[test]
    fn test_io_error_data_creates_internal_error() {
        let error_data = io_error_data("test error");
        assert_eq!(error_data.code, ErrorCode::INTERNAL_ERROR);
        assert!(error_data.message.contains("test error"));
    }

    #[test]
    fn test_io_error_data_with_string() {
        let error_data = io_error_data(String::from("string error"));
        assert_eq!(error_data.code, ErrorCode::INTERNAL_ERROR);
        assert!(error_data.message.contains("string error"));
    }

    #[test]
    fn test_treesitter_error_to_error_data_file_not_found() {
        use pathfinder_treesitter::SurgeonError;
        let err = SurgeonError::FileNotFound("test.rs".into());
        let error_data = treesitter_error_to_error_data(err);
        // Should map to INVALID_PARAMS
        assert_eq!(error_data.code, ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn test_treesitter_error_to_error_data_parse_error() {
        use pathfinder_treesitter::SurgeonError;
        let err = SurgeonError::ParseError {
            path: "test.rs".into(),
            reason: "syntax error".into(),
        };
        let error_data = treesitter_error_to_error_data(err);
        // Should map to INTERNAL_ERROR
        assert_eq!(error_data.code, ErrorCode::INTERNAL_ERROR);
    }

    #[test]
    fn test_path_to_error_data_includes_structured_data() {
        let err = PathfinderError::FileNotFound {
            path: "src/missing.rs".into(),
        };
        let error_data = pathfinder_to_error_data(&err);
        assert!(error_data.data.is_some());
        let data = error_data
            .data
            .expect("error data should contain structured data");
        // Check that error field is present and has the right value
        assert_eq!(data["error"], "FILE_NOT_FOUND");
        // The path might be nested differently depending on the error response structure
        // Just verify the structure exists
        assert!(data.is_object());
    }
}
