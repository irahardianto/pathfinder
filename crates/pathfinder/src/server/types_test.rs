use super::*;

#[test]
fn test_default_value_helpers() {
    assert_eq!(default_path_glob(), "**/*");
    assert_eq!(default_max_results(), 50);
    assert_eq!(default_context_lines(), 2);
    assert_eq!(default_repo_map_path(), ".");
    assert_eq!(default_max_tokens(), 16_000);
    assert_eq!(default_max_tokens_per_file(), 2_000);
    assert_eq!(default_max_depth(), 3);
    assert_eq!(default_max_references(), 50);
    assert_eq!(default_max_dependencies(), 50);
    assert_eq!(default_start_line(), 1);
    assert_eq!(default_max_lines(), 500);
    assert_eq!(default_detail_level(), "compact");
    assert!(default_group_by_file());
    assert_eq!(default_filter_mode(), FilterMode::CodeOnly);
}

#[test]
fn test_filepath_alias_deserialization() {
    let json_data = serde_json::json!({
        "path": "src/lib.rs",
        "start_line": 10,
    });
    let read_params: ReadParams = serde_json::from_value(json_data).unwrap();
    assert_eq!(read_params.filepath, Some("src/lib.rs".to_string()));
}

// ── Serde Roundtrip Tests ───────────────────────────────────────────

#[test]
fn test_degraded_reason_serde_roundtrip() {
    use pathfinder_common::types::DegradedReason;

    let variants = vec![
        (DegradedReason::NoLsp, "\"no_lsp\""),
        (DegradedReason::LspWarmupEmptyUnverified, "\"lsp_warmup_empty_unverified\""),
        (DegradedReason::LspWarmupGrepFallback, "\"lsp_warmup_grep_fallback\""),
        (DegradedReason::LspTimeoutGrepFallback, "\"lsp_timeout_grep_fallback\""),
        (DegradedReason::LspErrorGrepFallback, "\"lsp_error_grep_fallback\""),
        (DegradedReason::NoLspGrepFallback, "\"no_lsp_grep_fallback\""),
        (DegradedReason::GrepFallbackFileScoped, "\"grep_fallback_file_scoped\""),
        (DegradedReason::GrepFallbackImplScoped, "\"grep_fallback_impl_scoped\""),
        (DegradedReason::GrepFallbackGlobal, "\"grep_fallback_global\""),
        (DegradedReason::GrepFallbackDependencies, "\"grep_fallback_dependencies\""),
        (DegradedReason::UnsupportedLanguageFilterBypassed, "\"unsupported_language_filter_bypassed\""),
        (DegradedReason::UnsupportedLanguage, "\"unsupported_language\""),
        (DegradedReason::GitError, "\"git_error\""),
    ];

    for (variant, expected_json) in variants {
        let serialized = serde_json::to_string(&variant).expect("serialize DegradedReason");
        assert_eq!(serialized, expected_json, "serialize mismatch for {variant:?}");

        let deserialized: DegradedReason = serde_json::from_str(&serialized).expect("deserialize DegradedReason");
        assert_eq!(deserialized, variant, "roundtrip mismatch for {variant:?}");
    }
}

#[test]
fn test_detail_enum_serde() {
    let cases = vec![
        ("\"structure\"", "structure"),
        ("\"files\"", "files"),
        ("\"symbols\"", "symbols"),
    ];

    for (json_str, label) in cases {
        let detail: Detail = serde_json::from_str(json_str)
            .unwrap_or_else(|e| panic!("failed to deserialize Detail from {json_str}: {e}"));
        match (label, &detail) {
            ("structure", Detail::Structure) => {}
            ("files", Detail::Files) => {}
            ("symbols", Detail::Symbols) => {}
            _ => panic!("unexpected Detail variant for {label}: {detail:?}"),
        }
    }
}

#[test]
fn test_search_mode_serde() {
    let cases = vec![
        ("\"text\"", "text"),
        ("\"symbol\"", "symbol"),
        ("\"regex\"", "regex"),
    ];

    for (json_str, label) in cases {
        let mode: SearchMode = serde_json::from_str(json_str)
            .unwrap_or_else(|e| panic!("failed to deserialize SearchMode from {json_str}: {e}"));
        match (label, &mode) {
            ("text", SearchMode::Text) => {}
            ("symbol", SearchMode::Symbol) => {}
            ("regex", SearchMode::Regex) => {}
            _ => panic!("unexpected SearchMode variant for {label}: {mode:?}"),
        }
    }
}

#[test]
fn test_trace_scope_serde() {
    let cases = vec![
        ("\"callers\"", "callers"),
        ("\"references\"", "references"),
        ("\"overview\"", "overview"),
    ];

    for (json_str, label) in cases {
        let scope: TraceScope = serde_json::from_str(json_str)
            .unwrap_or_else(|e| panic!("failed to deserialize TraceScope from {json_str}: {e}"));
        match (label, &scope) {
            ("callers", TraceScope::Callers) => {}
            ("references", TraceScope::References) => {}
            ("overview", TraceScope::Overview) => {}
            _ => panic!("unexpected TraceScope variant for {label}: {scope:?}"),
        }
    }
}

// ── Default Impl Tests ──────────────────────────────────────────────

#[test]
fn test_search_params_default() {
    let params = SearchParams::default();
    assert_eq!(params.query, "");
    assert!(matches!(params.mode, SearchMode::Text));
    assert_eq!(params.path_glob, "**/*");
    assert_eq!(params.max_results, 50);
    assert_eq!(params.context_lines, 2);
    assert!(params.known_files.is_empty());
    assert_eq!(params.exclude_glob, "");
    assert_eq!(params.offset, 0);
    assert!(params.kind.is_none());
    assert!(params.group_by_file);
    assert!(matches!(params.filter_mode, FilterMode::CodeOnly));
}

#[test]
fn test_inspect_params_default() {
    let params = InspectParams::default();
    assert_eq!(params.semantic_path, "");
    assert!(!params.include_dependencies);
    assert_eq!(params.max_dependencies, 50);
    assert!(!params.include_imports);
}

#[test]
fn test_trace_params_default() {
    let params = TraceParams::default();
    assert_eq!(params.semantic_path, "");
    assert!(matches!(params.scope, TraceScope::Callers));
    assert_eq!(params.max_depth, 3);
    assert_eq!(params.max_references, 50);
    assert_eq!(params.offset, 0);
}

#[test]
fn test_explore_depth_default() {
    assert_eq!(default_explore_depth(), 3);
}

// ── is_false Helper Tests ───────────────────────────────────────────

#[test]
fn test_is_false_true_value() {
    // is_false returns !b, so is_false(true) == false
    assert!(!is_false(&true));
}

#[test]
fn test_is_false_false_value() {
    // is_false returns !b, so is_false(false) == true
    assert!(is_false(&false));
}

// ── skip_serializing_if Behavior Tests ──────────────────────────────

#[test]
fn test_symbol_overview_response_skip_empty_fields() {
    let resp = SymbolOverviewResponse {
        source: None,
        impact: None,
        references: None,
        files_referenced: 0,
        degraded: false,
        impact_degraded: false,
        references_degraded: false,
        degraded_reason: None,
        actionable_guidance: None,
        lsp_readiness: None,
        warm_start_in_progress: None,
    };

    let json = serde_json::to_value(&resp).expect("serialize SymbolOverviewResponse");
    let obj = json.as_object().expect("should be JSON object");

    // Fields with skip_serializing_if = Option::is_none should be absent
    assert!(!obj.contains_key("source"), "source should be absent when None");
    assert!(!obj.contains_key("impact"), "impact should be absent when None");
    assert!(!obj.contains_key("references"), "references should be absent when None");
    assert!(!obj.contains_key("degraded_reason"), "degraded_reason should be absent when None");
    assert!(!obj.contains_key("actionable_guidance"), "actionable_guidance should be absent when None");
    assert!(!obj.contains_key("lsp_readiness"), "lsp_readiness should be absent when None");
    assert!(!obj.contains_key("warm_start_in_progress"), "warm_start_in_progress should be absent when None");

    // Fields with skip_serializing_if = Not::not should be absent when false
    assert!(!obj.contains_key("impact_degraded"), "impact_degraded should be absent when false");
    assert!(!obj.contains_key("references_degraded"), "references_degraded should be absent when false");

    // Always-present fields should exist
    assert!(obj.contains_key("files_referenced"));
    assert!(obj.contains_key("degraded"));
}

#[test]
fn test_find_all_references_metadata_default_roundtrip() {
    let meta = FindAllReferencesMetadata::default();
    let json = serde_json::to_value(&meta).expect("serialize FindAllReferencesMetadata");
    let obj = json.as_object().expect("should be JSON object");

    // Always-present fields
    assert!(obj.contains_key("references"), "references should be present (defaults to None/null)");
    assert!(obj.contains_key("files_referenced"));
    assert!(obj.contains_key("degraded"));

    // Optional fields with skip_serializing_if should be absent when default
    assert!(!obj.contains_key("truncated"), "truncated should be absent when false (default)");
    assert!(!obj.contains_key("degraded_reason"), "degraded_reason should be absent when None");
    assert!(!obj.contains_key("actionable_guidance"));
    assert!(!obj.contains_key("lsp_readiness"));
    assert!(!obj.contains_key("duration_ms"));
    assert!(!obj.contains_key("resolution_strategy"));
    assert!(!obj.contains_key("hint"));
}
