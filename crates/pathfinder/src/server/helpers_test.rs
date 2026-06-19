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
            retry_after_seconds: None,
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
fn test_serialize_metadata_failure_returns_none() {
    // Custom type whose Serialize impl always fails.
    struct AlwaysFail;
    impl serde::Serialize for AlwaysFail {
        fn serialize<S: serde::Serializer>(&self, _s: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("intentional failure"))
        }
    }
    let result = super::serialize_metadata(&AlwaysFail);
    assert!(
        result.is_none(),
        "serialize_metadata should return None on serialization failure"
    );
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
        ("file.java", "java"),
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

/// AC-1.9: Java extension returns "java"
#[test]
fn test_language_from_path_java() {
    assert_eq!(language_from_path(Path::new("Main.java")), "java");
    assert_eq!(
        language_from_path(Path::new("src/com/example/UserService.java")),
        "java"
    );
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
    let semantic_path = SemanticPath::parse("src/main.rs::main").expect("should parse valid path");
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

#[test]
fn test_pathfinder_to_error_data_message_formatting() {
    // retry_after_seconds=None so hint contains "Did you mean: ..." text
    let err = PathfinderError::SymbolNotFound {
        semantic_path: "src/auth.ts::login".into(),
        did_you_mean: vec!["logout".to_owned(), "log_in".to_owned()],
        retry_after_seconds: None,
    };
    let error_data = pathfinder_to_error_data(&err);

    // Assert that the message has the detailed info
    assert!(error_data.message.contains("SYMBOL_NOT_FOUND"));
    assert!(error_data.message.contains("Did you mean: logout, log_in?"));
    assert!(error_data.message.contains("Hint:"));
}

/// Regression: "Did you mean" must appear exactly once in the `detailed_message`.
///
/// Previously, `pathfinder_to_error_data` appended `did_you_mean` BOTH from
/// `err_resp.hint` (which for `SymbolNotFound` already contains the suggestions)
/// AND from `err_resp.details.did_you_mean`, causing the string to appear twice.
#[test]
fn test_pathfinder_to_error_data_no_did_you_mean_duplication() {
    let err = PathfinderError::SymbolNotFound {
        semantic_path: "src/lib.rs::buildHealthHandler".into(),
        did_you_mean: vec!["buildHealthHandler".to_owned()],
        retry_after_seconds: None,
    };
    let error_data = pathfinder_to_error_data(&err);

    // "Did you mean" must appear exactly once
    let count = error_data.message.matches("Did you mean").count();
    assert_eq!(
        count, 1,
        "Expected 'Did you mean' exactly once in message, found {count}. Full message: {}",
        error_data.message
    );
}

// ── millis_to_u64 Tests ─────────────────────────────────────────────

#[test]
fn test_millis_to_u64_small_value() {
    assert_eq!(millis_to_u64(42), 42);
}

#[test]
fn test_millis_to_u64_zero() {
    assert_eq!(millis_to_u64(0), 0);
}

#[test]
fn test_millis_to_u64_max_u64() {
    assert_eq!(millis_to_u64(u128::from(u64::MAX)), u64::MAX);
}

// ── format_degraded_notice Tests ────────────────────────────────────

#[test]
fn test_format_degraded_notice_no_lsp() {
    use pathfinder_common::types::DegradedReason;

    let notice = format_degraded_notice(&DegradedReason::NoLsp);
    assert!(
        notice.contains("DEGRADED"),
        "notice should contain 'DEGRADED', got: {notice}"
    );
    assert!(
        notice.contains("no_lsp"),
        "notice should contain the reason 'no_lsp', got: {notice}"
    );
}

#[test]
fn test_format_degraded_notice_lsp_timeout() {
    use pathfinder_common::types::DegradedReason;

    let notice = format_degraded_notice(&DegradedReason::LspTimeoutGrepFallback);
    assert!(
        notice.contains("DEGRADED"),
        "notice should contain 'DEGRADED', got: {notice}"
    );
    assert!(
        notice.contains("retry"),
        "timeout notice should suggest retry, got: {notice}"
    );
}

#[test]
fn test_format_degraded_notice_lsp_error() {
    use pathfinder_common::types::DegradedReason;

    let notice = format_degraded_notice(&DegradedReason::LspErrorGrepFallback);
    assert!(
        notice.contains("DEGRADED"),
        "notice should contain 'DEGRADED', got: {notice}"
    );
}

// ── invalid_params_error Tests ──────────────────────────────────────

#[test]
fn test_invalid_params_error_creates_invalid_params_code() {
    let error_data = invalid_params_error("bad input");
    assert_eq!(
        error_data.code,
        ErrorCode::INVALID_PARAMS,
        "invalid_params_error should produce INVALID_PARAMS code"
    );
    assert!(
        error_data.message.contains("bad input"),
        "message should contain the input text, got: {}",
        error_data.message
    );
}

#[test]
fn test_pathfinder_to_error_data_ambiguous_symbol() {
    let err = PathfinderError::AmbiguousSymbol {
        semantic_path: "src/auth.ts::login".into(),
        matches: vec!["match1".to_owned(), "match2".to_owned()],
    };
    let error_data = pathfinder_to_error_data(&err);
    assert!(error_data.message.contains("Matches found: match1, match2"));
}

#[test]
fn test_format_degraded_notice_unsupported_language() {
    use pathfinder_common::types::DegradedReason;

    let notice = format_degraded_notice(&DegradedReason::UnsupportedLanguage);
    assert!(notice.contains("DEGRADED (unsupported_language)"));
    assert!(notice.contains("results are UNAVAILABLE for this language"));
}
