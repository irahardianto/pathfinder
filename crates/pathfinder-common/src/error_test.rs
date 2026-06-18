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
        retry_after_seconds: None,
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
    assert!(hint.contains("not a valid semantic path"), "hint: {hint}");
}

#[test]
fn test_hint_bare_file_suggests_alternatives() {
    let err = PathfinderError::InvalidSemanticPath {
        input: "src/main.rs".into(),
        issue: "this tool requires a symbol target — use 'file.rs::symbol' format".into(),
    };
    let hint = err.hint().expect("should have hint");
    assert!(hint.contains("read"), "hint should suggest read: {hint}");
    assert!(hint.contains("read"), "hint should suggest read: {hint}");
}

// ── GAP-008: LSP error hints ────────────────────────────────────

#[test]
fn test_lsp_error_hint_timeout_includes_workaround() {
    let err = PathfinderError::LspError {
        message: "LSP timed out on 'textDocument/definition' after 10000ms".to_owned(),
    };
    let hint = err.hint().expect("LspError should have a hint");
    assert!(
        hint.contains("search"),
        "hint should mention search: {hint}"
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
        hint.contains("read"),
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
        hint.contains("search"),
        "hint should mention search: {hint}"
    );
    assert!(
        hint.contains("health"),
        "hint should mention health: {hint}"
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
        hint.contains("search"),
        "hint should mention search: {hint}"
    );
    assert!(
        hint.contains("tree-sitter"),
        "hint should mention tree-sitter: {hint}"
    );
    assert!(
        hint.contains("health"),
        "hint should mention health: {hint}"
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
        hint.contains("inspect"),
        "hint should mention inspect: {hint}"
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
            retry_after_seconds: None,
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
        retry_after_seconds: None,
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
        retry_after_seconds: None,
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
        retry_after_seconds: None,
    };
    let hint = err
        .hint()
        .expect("should have hint even without suggestions");
    // When no suggestions, the symbol is likely in a different file.
    // Hint should suggest search to find the correct file.
    assert!(
        hint.contains("search"),
        "hint should suggest search to find the correct file: {hint}"
    );
}

#[test]
fn test_symbol_not_found_retry_hint_during_warmup() {
    let err = PathfinderError::SymbolNotFound {
        semantic_path: "src/main.rs::foo".into(),
        did_you_mean: vec![],
        retry_after_seconds: Some(10),
    };
    let hint = err.hint().expect("should have hint during warmup");
    assert!(
        hint.contains("Retry in 10 seconds"),
        "hint should mention retry delay: {hint}"
    );
    let response = err.to_error_response();
    assert_eq!(response.details["retry_after_seconds"], 10);
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
    assert!(hint.contains("read"), "hint should mention read: {hint}");
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

#[test]
fn test_hint_returns_some_for_all_error_variants() {
    let errors = vec![
        PathfinderError::FileNotFound { path: "a".into() },
        PathfinderError::SymbolNotFound {
            semantic_path: "a".into(),
            did_you_mean: vec![],
            retry_after_seconds: None,
        },
        PathfinderError::InvalidSemanticPath {
            input: "a".into(),
            issue: "b".into(),
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
        PathfinderError::IoError {
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
        PathfinderError::PathTraversal {
            path: "a".into(),
            workspace_root: "b".into(),
        },
    ];

    for err in errors {
        match err {
            PathfinderError::AmbiguousSymbol { .. }
            | PathfinderError::IoError { .. }
            | PathfinderError::ParseError { .. }
            | PathfinderError::TokenBudgetExceeded { .. } => {
                assert!(err.hint().is_none(), "expected None hint for {err:?}");
            }
            _ => {
                assert!(err.hint().is_some(), "expected Some hint for {err:?}");
            }
        }
    }
}

#[test]
fn test_hint_symbol_not_found_no_separator() {
    let err = PathfinderError::SymbolNotFound {
        semantic_path: "AuthService.login".into(),
        did_you_mean: vec![],
        retry_after_seconds: None,
    };
    let hint = err.hint().expect("should have hint");
    assert!(
        hint.contains("require '::'"),
        "hint should mention requiring '::' separator: {hint}"
    );
}

#[test]
fn test_hint_symbol_not_found_multiple_separators() {
    let err = PathfinderError::SymbolNotFound {
        semantic_path: "src/lib.rs::Outer::Inner".into(),
        did_you_mean: vec![],
        retry_after_seconds: None,
    };
    let hint = err.hint().expect("should have hint");
    assert!(
        hint.contains("only one '::'"),
        "hint should mention only one '::' allowed: {hint}"
    );
}

#[test]
fn test_to_details_symbol_not_found_no_retry() {
    let err = PathfinderError::SymbolNotFound {
        semantic_path: "src/auth.ts::login".into(),
        did_you_mean: vec!["logout".into()],
        retry_after_seconds: None,
    };
    let details = err.to_error_response().details;
    assert!(
        details.get("retry_after_seconds").is_none(),
        "retry_after_seconds should be absent when None"
    );
    // did_you_mean should still be present
    assert!(details.get("did_you_mean").is_some());
}
