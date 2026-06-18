use super::*;

#[test]
fn test_semantic_path_with_symbol() {
    let sp = SemanticPath::parse("src/auth.ts::AuthService.login").expect("should parse");
    assert_eq!(sp.file_path, PathBuf::from("src/auth.ts"));
    assert!(!sp.is_bare_file());

    let chain = sp.symbol_chain.as_ref().expect("should have symbol chain");
    assert_eq!(chain.segments.len(), 2);
    assert_eq!(chain.segments[0].name, "AuthService");
    assert_eq!(chain.segments[1].name, "login");
}

#[test]
fn test_semantic_path_bare_file() {
    let sp = SemanticPath::parse("src/utils.ts").expect("should parse");
    assert_eq!(sp.file_path, PathBuf::from("src/utils.ts"));
    assert!(sp.is_bare_file());
}

#[test]
fn test_semantic_path_with_overload() {
    let sp = SemanticPath::parse("src/auth.ts::AuthService.refreshToken#2").expect("should parse");
    let chain = sp.symbol_chain.as_ref().expect("should have symbol chain");
    let last = chain.segments.last().expect("should have segments");
    assert_eq!(last.name, "refreshToken");
    assert_eq!(last.overload_index, Some(2));
}

#[test]
fn test_semantic_path_display_roundtrip() {
    let input = "src/auth.ts::AuthService.login#2";
    let sp = SemanticPath::parse(input).expect("should parse");
    assert_eq!(sp.to_string(), input);
}

#[test]
fn test_semantic_path_empty_input() {
    assert!(SemanticPath::parse("").is_none());
}

#[test]
fn test_semantic_path_empty_file_part() {
    assert!(SemanticPath::parse("::AuthService").is_none());
}

#[test]
fn test_semantic_path_default_export() {
    let sp = SemanticPath::parse("src/auth.ts::default").expect("should parse");
    let chain = sp.symbol_chain.as_ref().expect("should have chain");
    assert_eq!(chain.segments.len(), 1);
    assert_eq!(chain.segments[0].name, "default");
}

#[test]
fn test_version_hash_compute() {
    let hash = VersionHash::compute(b"hello world");
    assert!(hash.as_str().starts_with("sha256:"));
    // SHA-256 of "hello world" is well-known
    assert!(hash.as_str().contains("b94d27b9934d3e08a52e52d7"));
}

#[test]
fn test_version_hash_equality() {
    let h1 = VersionHash::compute(b"same content");
    let h2 = VersionHash::compute(b"same content");
    assert_eq!(h1, h2);

    let h3 = VersionHash::compute(b"different content");
    assert_ne!(h1, h3);
}

// ── VersionHash::short() tests ────────────────────────────────────────────

/// `short()` must return exactly 7 hex characters with no prefix.
#[test]
fn test_version_hash_short_is_7_hex_chars() {
    let hash = VersionHash::compute(b"hello world");
    let s = hash.short();
    assert_eq!(s.len(), 7, "short() must be exactly 7 chars");
    assert!(
        s.chars().all(|c| c.is_ascii_hexdigit()),
        "short() must be hex chars only, got: {s}"
    );
}

/// `short()` must NOT contain the 'sha256:' prefix.
#[test]
fn test_version_hash_short_has_no_prefix() {
    let hash = VersionHash::compute(b"test content");
    assert!(
        !hash.short().starts_with("sha256:"),
        "short() must not start with 'sha256:'"
    );
}

/// `short()` must be the start of the hex portion of `as_str()`.
#[test]
fn test_version_hash_short_is_prefix_of_full_hex() {
    let hash = VersionHash::compute(b"hello world");
    let full = hash.as_str(); // "sha256:<64 hex>"
    assert!(
        full["sha256:".len()..].starts_with(hash.short()),
        "full hex must start with short()"
    );
}

/// `short()` must gracefully handle malformed hashes (too short).
#[test]
fn test_version_hash_short_handles_malformed_short() {
    let hash = VersionHash::from_raw("sha256:a1b2".to_string());
    assert_eq!(hash.short(), "a1b2", "short() returns all available chars");
}

/// `short()` must gracefully handle malformed hashes (no prefix).
#[test]
fn test_version_hash_short_handles_malformed_no_prefix() {
    let hash = VersionHash::from_raw("abcdef".to_string());
    assert_eq!(hash.short(), "abcdef", "short() returns entire string");
}

/// `short()` must gracefully handle malformed hashes (just prefix).
#[test]
fn test_version_hash_short_handles_malformed_only_prefix() {
    let hash = VersionHash::from_raw("sha256:".to_string());
    assert_eq!(hash.short(), "", "short() returns empty string");
}

// ── VersionHash::matches() tests ──────────────────────────────────────────

/// The preferred format: 7 hex chars, no prefix — what `short()` emits.
#[test]
fn test_matches_short_no_prefix() {
    let hash = VersionHash::compute(b"hello world");
    assert!(
        hash.matches(hash.short()),
        "hash.matches(hash.short()) must be true — roundtrip test"
    );
}

/// Short hash with the legacy sha256: prefix.
#[test]
fn test_matches_short_with_legacy_prefix() {
    let hash = VersionHash::compute(b"hello world");
    let with_prefix = format!("sha256:{}", hash.short());
    assert!(
        hash.matches(&with_prefix),
        "7-char hash with sha256: prefix must match"
    );
}

/// Full 71-char hash with prefix (backward compatibility).
#[test]
fn test_matches_full_hash_with_prefix() {
    let hash = VersionHash::compute(b"hello world");
    assert!(
        hash.matches(hash.as_str()),
        "full hash as_str() must match itself"
    );
}

/// 8-char prefix should also be accepted (> minimum).
#[test]
fn test_matches_8_char_prefix_accepted() {
    let hash = VersionHash::compute(b"hello world");
    let eight = &hash.as_str()["sha256:".len().."sha256:".len() + 8];
    assert!(hash.matches(eight), "8-char prefix must be accepted");
}

/// Inputs shorter than 7 hex chars must be rejected.
#[test]
fn test_matches_too_short_rejected() {
    let hash = VersionHash::compute(b"hello world");
    assert!(!hash.matches("e3dc7f"), "6 hex chars must be rejected");
    assert!(
        !hash.matches("sha256:abc"),
        "3 hex chars with prefix rejected"
    );
    assert!(!hash.matches(""), "empty string must be rejected");
}

/// Wrong prefix must not match.
#[test]
fn test_matches_wrong_hex_fails() {
    let hash = VersionHash::compute(b"hello world");
    assert!(!hash.matches("0000000"), "wrong 7-char hex must not match");
    assert!(
        !hash.matches("sha256:0000000"),
        "wrong prefixed hex must not match"
    );
}

/// Hashes of different content must not match each other.
#[test]
fn test_matches_different_content_fails() {
    let hash_a = VersionHash::compute(b"content A");
    let hash_b = VersionHash::compute(b"content B");
    assert!(
        !hash_a.matches(hash_b.short()),
        "short hash from different content must not match"
    );
}

#[test]
fn test_filter_mode_default() {
    assert_eq!(FilterMode::default(), FilterMode::CodeOnly);
}

#[test]
fn test_resolve_path_traversal_is_detected() {
    // WorkspaceRoot::resolve must still return the joined path (so the
    // Sandbox can do its job), but the traversal-detection branch must
    // fire without panicking.
    let dir = tempfile::tempdir().expect("create tempdir");
    let root = WorkspaceRoot::new(dir.path()).expect("create workspace root");

    let traversal = std::path::Path::new("../../etc/passwd");
    // Should not panic; the sandbox is the primary enforcement layer.
    let resolved = root.resolve(traversal);
    // The resolved path escapes the workspace — that is expected here.
    // The Sandbox (not resolve) is responsible for rejection.
    assert!(resolved.to_string_lossy().contains("etc/passwd"));
}

#[test]
fn test_resolve_strict_rejects_traversal() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let root = WorkspaceRoot::new(dir.path()).expect("create workspace root");

    let traversal = std::path::Path::new("../../etc/passwd");
    let result = root.resolve_strict(traversal);

    assert!(result.is_err());
    assert!(matches!(result, Err(PathfinderError::PathTraversal { .. })));
}

#[test]
fn test_resolve_strict_rejects_absolute_path() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let root = WorkspaceRoot::new(dir.path()).expect("create workspace root");

    let absolute = std::path::Path::new("/etc/passwd");
    let result = root.resolve_strict(absolute);

    assert!(result.is_err());
    assert!(matches!(result, Err(PathfinderError::PathTraversal { .. })));
}

#[test]
fn test_resolve_strict_accepts_relative_path() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let root = WorkspaceRoot::new(dir.path()).expect("create workspace root");

    let relative = std::path::Path::new("src/main.rs");
    let result = root.resolve_strict(relative);

    assert!(result.is_ok());
    let resolved = result.expect("should be Ok");
    assert!(resolved.to_string_lossy().contains("src/main.rs"));
}

// ── DegradedReason tests ────────────────────────────────────────────────

#[test]
fn test_degraded_reason_serde_snake_case() {
    // Verify serde serialization produces snake_case strings (backward compatible)
    use super::DegradedReason;

    assert_eq!(
        serde_json::to_string(&DegradedReason::NoLsp).expect("NoLsp should serialize to JSON"),
        "\"no_lsp\""
    );
    assert_eq!(
        serde_json::to_string(&DegradedReason::LspWarmupGrepFallback)
            .expect("LspWarmupGrepFallback should serialize to JSON"),
        "\"lsp_warmup_grep_fallback\""
    );
    assert_eq!(
        serde_json::to_string(&DegradedReason::GitError)
            .expect("GitError should serialize to JSON"),
        "\"git_error\""
    );
}

#[test]
fn test_degraded_reason_display() {
    use super::DegradedReason;

    assert_eq!(DegradedReason::NoLsp.to_string(), "no_lsp");
    assert_eq!(
        DegradedReason::LspWarmupEmptyUnverified.to_string(),
        "lsp_warmup_empty_unverified"
    );
    assert_eq!(
        DegradedReason::GrepFallbackGlobal.to_string(),
        "grep_fallback_global"
    );
}

#[test]
fn test_degraded_reason_guidance_no_lsp() {
    use super::DegradedReason;
    let g = DegradedReason::NoLsp.guidance();
    assert!(!g.retry_recommended);
    assert!(g.permanent);
    assert_eq!(g.fallback_tool, Some(FallbackTool::Search));
    assert_eq!(g.trust_level, TrustLevel::Partial);
}

#[test]
fn test_degraded_reason_guidance_warmup_retry() {
    use super::DegradedReason;
    let g = DegradedReason::LspWarmupEmptyUnverified.guidance();
    assert!(g.retry_recommended);
    assert_eq!(g.retry_after_seconds, Some(15));
    assert!(!g.permanent);
}

#[test]
fn test_degraded_reason_guidance_grep_fallback_permanent() {
    use super::DegradedReason;
    let g = DegradedReason::LspErrorGrepFallback.guidance();
    assert!(!g.retry_recommended);
    assert!(g.permanent);
    assert_eq!(g.fallback_tool, Some(FallbackTool::Search));
    assert_eq!(g.trust_level, TrustLevel::Heuristic);
}

#[test]
fn test_visibility_display_and_from_str() {
    let v_pub = Visibility::Public;
    assert_eq!(v_pub.to_string(), "public");
    assert_eq!(
        "public".parse::<Visibility>().expect("valid"),
        Visibility::Public
    );

    let v_all = Visibility::All;
    assert_eq!(v_all.to_string(), "all");
    assert_eq!("all".parse::<Visibility>().expect("valid"), Visibility::All);

    let err = "invalid".parse::<Visibility>();
    assert!(err.is_err());
    assert_eq!(
        err.expect_err("invalid visibility").to_string(),
        "invalid visibility: 'invalid' (expected 'public' or 'all')"
    );
}

#[test]
fn test_semantic_path_parse_edge_cases() {
    // Line 106: symbol_part is empty
    assert!(SemanticPath::parse("src/auth.ts::").is_none());

    // Line 118: dot-only symbol part (segments is empty)
    assert!(SemanticPath::parse("src/auth.ts::.").is_none());

    // Line 155: empty segment in symbol chain is skipped, but chain can still parse if other segments exist
    let sp = SemanticPath::parse("src/auth.ts::a..b")
        .expect("should parse a..b by skipping empty segment");
    let chain = sp.symbol_chain.expect("should have symbol chain");
    assert_eq!(chain.segments.len(), 2);
    assert_eq!(chain.segments[0].name, "a");
    assert_eq!(chain.segments[1].name, "b");
}

#[test]
fn test_version_hash_display() {
    let h = VersionHash::compute(b"hello");
    assert_eq!(h.to_string(), h.as_str());
}

#[test]
fn test_resolve_absolute_path_unstrict() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let root = WorkspaceRoot::new(dir.path()).expect("create workspace root");
    let resolved = root.resolve(std::path::Path::new("/etc/passwd"));
    assert!(resolved.to_string_lossy().contains("etc/passwd"));
    assert!(!resolved.to_string_lossy().starts_with("/etc/passwd"));
}

#[test]
fn test_resolve_warning_logs() {
    let _ = tracing_subscriber::fmt::try_init();
    let dir = tempfile::tempdir().expect("create tempdir");
    let root = WorkspaceRoot::new(dir.path()).expect("create workspace root");
    let _resolved = root.resolve(std::path::Path::new("../../etc/passwd"));
}

#[test]
fn test_degraded_reason_all_variants() {
    use super::DegradedReason;

    let variants = [
        DegradedReason::NoLsp,
        DegradedReason::LspWarmupEmptyUnverified,
        DegradedReason::LspWarmupGrepFallback,
        DegradedReason::LspTimeoutGrepFallback,
        DegradedReason::LspErrorGrepFallback,
        DegradedReason::NoLspGrepFallback,
        DegradedReason::GrepFallbackFileScoped,
        DegradedReason::GrepFallbackImplScoped,
        DegradedReason::GrepFallbackGlobal,
        DegradedReason::GrepFallbackDependencies,
        DegradedReason::UnsupportedLanguageFilterBypassed,
        DegradedReason::UnsupportedLanguage,
        DegradedReason::GitError,
    ];

    for &variant in &variants {
        // Verify Display impl (line 494)
        let s = variant.to_string();
        assert!(!s.is_empty());

        // Verify guidance logic
        let guidance = variant.guidance();
        assert_eq!(
                guidance.retry_recommended,
                guidance.retry_after_seconds.is_some(),
                "retry_recommended must match the presence of retry_after_seconds for variant {variant:?}"
            );
    }
}

// ── FallbackTool serde roundtrip ───────────────────────────────────────

#[test]
fn test_fallback_tool_serde_roundtrip() {
    let variants = [
        (FallbackTool::Search, "\"search\""),
        (FallbackTool::Read, "\"read\""),
    ];
    for (variant, expected_json) in variants {
        let serialized = serde_json::to_string(&variant).expect("FallbackTool should serialize");
        assert_eq!(serialized, expected_json);
        let deserialized: FallbackTool =
            serde_json::from_str(&serialized).expect("FallbackTool should deserialize");
        assert_eq!(deserialized, variant);
    }
}

// ── TrustLevel serde roundtrip ────────────────────────────────────────

#[test]
fn test_trust_level_serde_roundtrip() {
    let variants = [
        (TrustLevel::Unreliable, "\"unreliable\""),
        (TrustLevel::Heuristic, "\"heuristic\""),
        (TrustLevel::Partial, "\"partial\""),
        (TrustLevel::None, "\"none\""),
    ];
    for (variant, expected_json) in variants {
        let serialized = serde_json::to_string(&variant).expect("TrustLevel should serialize");
        assert_eq!(serialized, expected_json);
        let deserialized: TrustLevel =
            serde_json::from_str(&serialized).expect("TrustLevel should deserialize");
        assert_eq!(deserialized, variant);
    }
}

// ── ActionableGuidance serde roundtrip ─────────────────────────────────

#[test]
fn test_actionable_guidance_serde_roundtrip() {
    // Case 1: all Option fields populated
    let guidance = ActionableGuidance {
        retry_recommended: true,
        retry_after_seconds: Some(30),
        fallback_tool: Some(FallbackTool::Search),
        trust_level: TrustLevel::Heuristic,
        permanent: false,
    };
    let serialized = serde_json::to_string(&guidance).expect("ActionableGuidance should serialize");
    let deserialized: ActionableGuidance =
        serde_json::from_str(&serialized).expect("ActionableGuidance should deserialize");
    assert_eq!(deserialized, guidance);

    // Case 2: fallback_tool=None — must NOT appear as "null" in JSON
    // (validates skip_serializing_if = "Option::is_none" is present)
    let no_fallback = DegradedReason::GitError.guidance(); // fallback_tool: None, retry_after_seconds: Some(5)
    let serialized_no_fallback =
        serde_json::to_string(&no_fallback).expect("ActionableGuidance should serialize");
    assert!(
            !serialized_no_fallback.contains("\"fallback_tool\":null"),
            "fallback_tool=None should be omitted, not serialized as null. Got: {serialized_no_fallback}"
        );
    let deserialized_no_fallback: ActionableGuidance =
        serde_json::from_str(&serialized_no_fallback)
            .expect("ActionableGuidance should deserialize");
    assert_eq!(deserialized_no_fallback, no_fallback);

    // Case 3: retry_after_seconds=None — must NOT appear as "null" in JSON
    let no_retry = DegradedReason::NoLsp.guidance(); // retry_after_seconds: None, fallback_tool: None
    let serialized_no_retry =
        serde_json::to_string(&no_retry).expect("ActionableGuidance should serialize");
    assert!(
            !serialized_no_retry.contains("\"retry_after_seconds\":null"),
            "retry_after_seconds=None should be omitted, not serialized as null. Got: {serialized_no_retry}"
        );
    assert!(
        !serialized_no_retry.contains("\"fallback_tool\":null"),
        "fallback_tool=None should be omitted, not serialized as null. Got: {serialized_no_retry}"
    );
    let deserialized_no_retry: ActionableGuidance =
        serde_json::from_str(&serialized_no_retry).expect("ActionableGuidance should deserialize");
    assert_eq!(deserialized_no_retry, no_retry);
}

// ── guidance() field-level tests for uncovered arms ────────────────────

#[test]
fn test_guidance_lsp_warmup_grep_fallback() {
    let g = DegradedReason::LspWarmupGrepFallback.guidance();
    assert_eq!(g.fallback_tool, Some(FallbackTool::Search));
    assert_eq!(g.trust_level, TrustLevel::Heuristic);
    assert!(!g.permanent);
    assert!(g.retry_recommended);
}

#[test]
fn test_guidance_lsp_timeout_grep_fallback() {
    let g = DegradedReason::LspTimeoutGrepFallback.guidance();
    assert_eq!(g.fallback_tool, Some(FallbackTool::Search));
    assert_eq!(g.trust_level, TrustLevel::Heuristic);
    assert!(!g.permanent);
    assert!(g.retry_recommended);
}

#[test]
fn test_guidance_unsupported_language_filter_bypassed() {
    let g = DegradedReason::UnsupportedLanguageFilterBypassed.guidance();
    assert_eq!(g.fallback_tool, Some(FallbackTool::Read));
    assert_eq!(g.trust_level, TrustLevel::Partial);
    assert!(g.permanent);
}

#[test]
fn test_guidance_unsupported_language() {
    let g = DegradedReason::UnsupportedLanguage.guidance();
    assert_eq!(g.fallback_tool, Some(FallbackTool::Read));
    assert_eq!(g.trust_level, TrustLevel::None);
    assert!(g.permanent);
}

#[test]
fn test_guidance_git_error() {
    let g = DegradedReason::GitError.guidance();
    assert!(g.retry_recommended);
    assert_eq!(g.retry_after_seconds, Some(5));
    assert_eq!(g.fallback_tool, None);
    assert!(!g.permanent);
}
