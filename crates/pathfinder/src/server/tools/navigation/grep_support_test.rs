//! Unit and regression tests for `grep_support.rs`.
//!
//! Prior to this file, `grep_support.rs` (827 lines) had zero test coverage.
//! The most valuable test here is `test_resolve_search_extensions_includes_vue`
//! / `test_find_symbol_impl_inner_searches_vue_files`: they pin down a real
//! bug found while auditing this module — the old hand-rolled 8-extension
//! list (`rs, ts, tsx, js, jsx, py, go, java`) silently omitted `vue`, so
//! `locate`'s cross-file "did you mean" fallback could never suggest a
//! symbol defined in a `.vue` file even though `definition_patterns` has a
//! dedicated Vue arm. `resolve_search_extensions` is now driven from the
//! `LanguageGrammar` registry so this can't silently drift again.

use super::super::test_helpers::make_temp_workspace;
use super::*;
use crate::server::PathfinderServer;
use pathfinder_common::config::PathfinderConfig;
use pathfinder_common::sandbox::Sandbox;
use pathfinder_common::types::WorkspaceRoot;
use pathfinder_search::MockScout;
use pathfinder_treesitter::mock::MockSurgeon;
use std::path::Path;
use std::sync::Arc;

// ── resolve_search_extensions (WS-8 registry-driven extensions) ────

#[test]
fn test_resolve_search_extensions_includes_vue() {
    let exts = resolve_search_extensions();
    assert!(
        exts.iter()
            .any(|(ext, glob)| *ext == "vue" && glob == "**/*.vue"),
        "regression: cross-file symbol search must cover .vue files — the old \
         hand-rolled extension list omitted vue entirely, got: {exts:?}"
    );
}

#[test]
fn test_resolve_search_extensions_covers_every_searchable_language_extension() {
    let expected: usize = SEARCHABLE_LANGUAGES
        .iter()
        .filter_map(|&id| LanguageGrammar::find_by_id(id))
        .map(|g| g.extensions.len())
        .sum();
    let actual = resolve_search_extensions();
    assert_eq!(
        actual.len(),
        expected,
        "resolve_search_extensions must produce exactly one (ext, glob) pair per \
         extension declared in LanguageGrammar for each SEARCHABLE_LANGUAGES entry"
    );
}

#[test]
fn test_resolve_search_extensions_no_duplicate_extensions() {
    let exts = resolve_search_extensions();
    let mut seen = std::collections::HashSet::new();
    for (ext, _) in &exts {
        assert!(
            seen.insert(*ext),
            "duplicate extension {ext:?} — two languages in SEARCHABLE_LANGUAGES \
             claim the same file extension"
        );
    }
}

#[test]
fn test_resolve_search_extensions_glob_matches_extension() {
    for (ext, glob) in resolve_search_extensions() {
        assert_eq!(
            glob,
            format!("**/*.{ext}"),
            "glob for extension {ext:?} must be the plain **/*.<ext> pattern"
        );
    }
}

// ── find_symbol_impl_inner (integration, MockScout) ─────────────────

#[tokio::test]
async fn test_find_symbol_impl_inner_searches_vue_files() {
    let scout = Arc::new(MockScout::default());
    let surgeon = Arc::new(MockSurgeon::default());
    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        scout.clone(),
        surgeon,
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = FindSymbolParams {
        query: "login".to_string(),
        kind: None,
        max_results: 20,
    };
    let _ = server.find_symbol_impl_inner(params).await;

    let calls = scout.calls();
    assert!(
        calls.iter().any(|c| c.path_glob == "**/*.vue"),
        "regression: locate's cross-file did-you-mean fallback must search .vue \
         files; calls were: {:?}",
        calls.iter().map(|c| &c.path_glob).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn test_find_symbol_impl_inner_rejects_path_separators_in_query() {
    let scout = Arc::new(MockScout::default());
    let surgeon = Arc::new(MockSurgeon::default());
    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        scout,
        surgeon,
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = FindSymbolParams {
        query: "../etc/passwd".to_string(),
        kind: None,
        max_results: 20,
    };
    let result = server.find_symbol_impl_inner(params).await;
    assert!(
        result.is_err(),
        "query with path separators must be rejected"
    );
}

// ── validate_find_symbol_params ──────────────────────────────────────

#[test]
fn test_validate_find_symbol_params_rejects_empty_query() {
    let params = FindSymbolParams {
        query: "   ".to_string(),
        kind: None,
        max_results: 20,
    };
    assert!(validate_find_symbol_params(&params).is_err());
}

#[test]
fn test_validate_find_symbol_params_rejects_unknown_kind() {
    let params = FindSymbolParams {
        query: "login".to_string(),
        kind: Some("bogus".to_string()),
        max_results: 20,
    };
    assert!(validate_find_symbol_params(&params).is_err());
}

#[test]
fn test_validate_find_symbol_params_accepts_valid_input() {
    let params = FindSymbolParams {
        query: "login".to_string(),
        kind: Some("function".to_string()),
        max_results: 20,
    };
    assert!(validate_find_symbol_params(&params).is_ok());
}

// ── is_valid_kind_filter / kind_matches_filter ───────────────────────

#[test]
fn test_is_valid_kind_filter_accepts_canonical_and_aliases() {
    for k in [
        "function",
        "method",
        "fn",
        "class",
        "struct",
        "interface",
        "trait",
        "enum",
        "type",
        "constant",
        "const",
        "static",
        "let",
        "module",
        "mod",
        "namespace",
        "impl",
    ] {
        assert!(is_valid_kind_filter(k), "{k} should be a valid kind filter");
    }
    assert!(!is_valid_kind_filter("bogus"));
}

#[test]
fn test_kind_matches_filter_type_matches_broadly() {
    assert!(kind_matches_filter("enum", "type"));
    assert!(kind_matches_filter("struct", "type"));
    assert!(!kind_matches_filter("constant", "type"));
}

#[test]
fn test_kind_matches_filter_class_excludes_enum() {
    // Per validate_find_symbol_params's own error message: "class also matches
    // struct and interface" — enum is deliberately excluded.
    assert!(kind_matches_filter("struct", "class"));
    assert!(kind_matches_filter("interface", "class"));
    assert!(!kind_matches_filter("enum", "class"));
}

// ── relevance_score ───────────────────────────────────────────────────

#[test]
fn test_relevance_score_exact_beats_prefix_beats_contains_beats_none() {
    assert_eq!(relevance_score("login", "login"), 3);
    assert_eq!(relevance_score("log", "login"), 2);
    assert_eq!(relevance_score("gin", "login"), 1);
    assert_eq!(relevance_score("logout", "login"), 0);
}

// ── extract_identifier_prefix ─────────────────────────────────────────

#[test]
fn test_extract_identifier_prefix_valid_and_invalid() {
    assert_eq!(extract_identifier_prefix("myFunc("), Some("myFunc"));
    assert_eq!(
        extract_identifier_prefix("_private123("),
        Some("_private123")
    );
    assert_eq!(extract_identifier_prefix("123abc"), None);
    assert_eq!(extract_identifier_prefix(""), None);
}

// ── truncate_preview ────────────────────────────────────────────────

#[test]
fn test_truncate_preview_short_string_unchanged() {
    assert_eq!(truncate_preview("short", 100), "short");
    assert_eq!(truncate_preview("", 3), String::new());
}

#[test]
fn test_truncate_preview_ascii_truncation() {
    assert_eq!(truncate_preview("abcdef", 3), "abc...");
}

#[test]
fn test_truncate_preview_multibyte_boundary_safe() {
    let s = "héllo wörld";
    let truncated = truncate_preview(s, 3);
    // Must not panic on a non-ASCII byte boundary, and must end in "...".
    assert!(truncated.ends_with("..."));
}

// ── infer_kind_from_line / extract_name_from_line ───────────────────

#[test]
fn test_infer_kind_from_line_common_keywords() {
    assert_eq!(infer_kind_from_line("pub fn login() {"), "function");
    assert_eq!(infer_kind_from_line("struct User {"), "struct");
    assert_eq!(infer_kind_from_line("class Foo {"), "class");
    assert_eq!(infer_kind_from_line("enum Color {"), "enum");
    assert_eq!(infer_kind_from_line("trait Shape {"), "interface");
    assert_eq!(infer_kind_from_line("const MAX: u32 = 5;"), "constant");
    assert_eq!(infer_kind_from_line("mod foo {"), "module");
    assert_eq!(infer_kind_from_line("impl Foo {"), "impl");
    assert_eq!(infer_kind_from_line("garbage line"), "unknown");
}

#[test]
fn test_extract_name_from_line_after_keyword() {
    assert_eq!(extract_name_from_line("pub fn login() -> bool {"), "login");
    assert_eq!(
        extract_name_from_line("struct User { name: String }"),
        "User"
    );
    assert_eq!(extract_name_from_line("def validate(x):"), "validate");
}

// ── is_workspace_file_path ────────────────────────────────────────────

#[test]
fn test_is_workspace_file_path_rejects_vendor_directories() {
    let root = Path::new("/workspace");
    assert!(!is_workspace_file_path(
        Path::new("src/vendor/lib.rs"),
        root,
        root
    ));
    assert!(!is_workspace_file_path(
        Path::new("frontend/node_modules/pkg/index.js"),
        root,
        root
    ));
}

#[test]
fn test_is_workspace_file_path_accepts_plain_relative_path() {
    let root = Path::new("/workspace");
    assert!(is_workspace_file_path(Path::new("src/auth.rs"), root, root));
}
