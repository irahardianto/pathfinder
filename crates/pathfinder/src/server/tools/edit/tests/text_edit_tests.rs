#![allow(clippy::unwrap_used, clippy::expect_used, clippy::needless_return)]
use crate::server::PathfinderServer;

use std::path::Path;

use super::super::text_edit::*;
use super::super::*;
#[test]
fn test_apply_sorted_edits_overlap() {
    // Edit 1: bytes 0..5, Edit 2: bytes 2..7 (overlap)
    let edits = vec![
        (
            0,
            "edit0".to_string(),
            ResolvedEdit {
                start_byte: 0,
                end_byte: 5,
                replacement: vec![],
            },
        ),
        (
            1,
            "edit1".to_string(),
            ResolvedEdit {
                start_byte: 2,
                end_byte: 7,
                replacement: vec![],
            },
        ),
    ];

    let result = PathfinderServer::apply_sorted_edits(b"0123456789", edits);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.code.0, -32602); // INVALID_PARAMS mapped from InvalidTarget
    let data = err.data.expect("should have data");
    assert_eq!(data["details"]["edit_index"], 0);
    assert_eq!(data["details"]["valid_edit_types"], serde_json::Value::Null);
    // not applicable here
}

fn src(lines: &[&str]) -> Vec<u8> {
    lines.join("\n").into_bytes()
}

// ── success cases ───────────────────────────────────────────────────

#[test]
fn test_exact_match_replaces_correctly() {
    let source = src(&[
        "line 1",
        "line 2",
        "line 3",
        "line 4",
        "<button>Click me</button>",
        "line 6",
        "line 7",
    ]);
    let r = resolve_text_edit(
        &source,
        "<button>Click me</button>",
        5,
        "<button>Submit</button>",
        false,
        Path::new("app.vue"),
    )
    .expect("exact match should succeed");

    let mut out = source.clone();
    out.splice(r.start_byte..r.end_byte, r.replacement);
    let out_str = String::from_utf8(out).unwrap();
    assert!(
        out_str.contains("<button>Submit</button>"),
        "replacement present: {out_str}"
    );
    assert!(
        !out_str.contains("<button>Click me</button>"),
        "old text gone: {out_str}"
    );
}

#[test]
fn test_window_boundary_at_plus_25_is_included() {
    // Target on line 35, context_line 10 → window = [1, 35] (±25 from line 10).
    let mut lines = vec!["filler"; 40];
    lines[34] = "special text"; // line 35 (1-indexed)
    let source = src(&lines);
    resolve_text_edit(
        &source,
        "special text",
        10,
        "replaced",
        false,
        Path::new("a.rs"),
    )
    .expect("±25 edge should be included");
}

#[test]
fn test_context_line_zero_clamped_safely() {
    let source = src(&["only line"]);
    let r = resolve_text_edit(
        &source,
        "only line",
        0,
        "replaced",
        false,
        Path::new("a.rs"),
    )
    .expect("context_line=0 clamped to center=0 → window ok");
    assert_eq!(&r.replacement, b"replaced");
}

#[test]
fn test_multiline_old_text() {
    let source = src(&[
        "fn foo() {",
        "    let x = 1;",
        "    let y = 2;",
        "}",
        "fn bar() {}",
    ]);
    let old = "    let x = 1;\n    let y = 2;";
    let r = resolve_text_edit(
        &source,
        old,
        2,
        "    let z = 42;",
        false,
        Path::new("lib.rs"),
    )
    .expect("multi-line match should succeed");
    let mut out = source.clone();
    out.splice(r.start_byte..r.end_byte, r.replacement);
    let s = String::from_utf8(out).unwrap();
    assert!(s.contains("let z = 42;"), "replacement present: {s}");
    assert!(!s.contains("let x = 1;"), "old text removed: {s}");
}

// ── normalize_whitespace ────────────────────────────────────────────

#[test]
fn test_normalize_whitespace_matches_with_collapsed_spaces() {
    let source = src(&[
        "<div>",
        "  <button   class=\"btn\"   >Click</button>",
        "</div>",
    ]);
    let r = resolve_text_edit(
        &source,
        "<button class=\"btn\" >Click</button>",
        2,
        "<button class=\"btn\">Submit</button>",
        true,
        Path::new("comp.vue"),
    )
    .expect("normalized whitespace should match");
    let mut out = source.clone();
    out.splice(r.start_byte..r.end_byte, r.replacement);
    let s = String::from_utf8(out).unwrap();
    assert!(s.contains("Submit"), "replacement present: {s}");
}

#[test]
fn test_no_normalize_fails_on_spacing_mismatch() {
    // With fuzzy fallback, spacing mismatches are now handled automatically
    let source = src(&["<button   class=\"btn\">Click</button>"]);
    let r = resolve_text_edit(
        &source,
        "<button class=\"btn\">Click</button>",
        1,
        "<button>Submit</button>",
        false,
        Path::new("a.vue"),
    )
    .expect("fuzzy fallback should handle spacing mismatch");

    let mut out = source.clone();
    out.splice(r.start_byte..r.end_byte, r.replacement);
    let s = String::from_utf8(out).unwrap();
    assert!(s.contains("Submit"), "replacement present");
    assert!(!s.contains("Click"), "old text removed");
}

// ── failure cases ───────────────────────────────────────────────────

#[test]
fn test_text_not_in_window_returns_text_not_found() {
    let mut lines = vec!["line"; 60];
    lines[50] = "target text"; // line 51
    let source = src(&lines);
    // Window for context_line=5 covers lines 1–30 (±25); line 51 is outside.
    let err = resolve_text_edit(&source, "target text", 5, "r", false, Path::new("a.rs"))
        .expect_err("out-of-window match should fail");
    let pathfinder_common::error::PathfinderError::TextNotFound { context_line, .. } = err else {
        panic!("expected TextNotFound, got: {err:?}");
    };
    assert_eq!(context_line, 5);
}

#[test]
fn test_text_not_found_at_all_returns_error() {
    let source = src(&["hello world"]);
    let err = resolve_text_edit(&source, "not present", 1, "", false, Path::new("f.rs"))
        .expect_err("missing text returns TextNotFound");
    assert!(matches!(
        err,
        pathfinder_common::error::PathfinderError::TextNotFound { .. }
    ));
}

// ── Fix 3: Text Edit Improvements ─────────────────────────────────────

#[test]
fn test_window_25_lines() {
    // Test that the search window is ±25 lines (not ±10)
    let source = src(&[
        "line 1", "line 2", "line 3", "line 4", "line 5", "line 6", "line 7", "line 8", "line 9",
        "line 10", "line 11", "line 12", "line 13", "line 14", "line 15", "line 16", "line 17",
        "line 18", "line 19", "line 20", "line 21", "line 22", "line 23", "line 24", "line 25",
        "line 26", "line 27", "line 28", "line 29", "line 30",
    ]);

    // context_line=15, target text at line 30 (15+15=30, within ±25 window)
    let r = resolve_text_edit(
        &source,
        "line 30",
        15,
        "replaced",
        false,
        Path::new("test.rs"),
    )
    .expect("match at line 30 (±25 from line 15) should succeed");

    let mut out = source.clone();
    out.splice(r.start_byte..r.end_byte, r.replacement);
    let s = String::from_utf8(out).unwrap();
    assert!(s.contains("replaced"), "replacement present");
    assert!(!s.contains("line 30"), "old text removed");
}

#[test]
fn test_fuzzy_whitespace_fallback() {
    // Test that whitespace-normalized fuzzy matching is attempted when exact match fails
    let source = src(&[
        "<div>",
        "  <button   class=\"btn\">Click</button>",
        "</div>",
    ]);

    // Exact match should fail (wrong spacing), but fuzzy fallback should succeed
    let r = resolve_text_edit(
        &source,
        "<button class=\"btn\">Click</button>",
        2,
        "<button>Submit</button>",
        false,
        Path::new("comp.vue"),
    )
    .expect("fuzzy whitespace fallback should succeed");

    let mut out = source.clone();
    out.splice(r.start_byte..r.end_byte, r.replacement);
    let s = String::from_utf8(out).unwrap();
    assert!(s.contains("Submit"), "replacement present");
    assert!(!s.contains("Click"), "old text removed");
}

#[test]
fn test_text_not_found_includes_actual_content() {
    let source = src(&["line 1", "line 2", "line 3"]);

    let err = resolve_text_edit(&source, "not present", 2, "", false, Path::new("f.rs"))
        .expect_err("missing text returns TextNotFound");

    let pathfinder_common::error::PathfinderError::TextNotFound {
        filepath,
        old_text,
        context_line,
        actual_content,
        ..
    } = err
    else {
        panic!("expected TextNotFound, got: {err:?}");
    };

    assert_eq!(filepath, Path::new("f.rs"));
    assert_eq!(old_text, "not present");
    assert_eq!(context_line, 2);
    assert!(
        actual_content.is_some(),
        "actual_content should be populated with window text"
    );

    let content = actual_content.unwrap();
    assert!(
        content.contains("line 1") || content.contains("line 2") || content.contains("line 3"),
        "actual_content should contain context from the source file"
    );
}

// ── is_whitespace_significant_file (L353-357) ────────────────────────────

#[test]
fn test_is_whitespace_significant_file_known_extensions() {
    assert!(is_whitespace_significant_file(Path::new("main.py")));
    assert!(is_whitespace_significant_file(Path::new("config.yaml")));
    assert!(is_whitespace_significant_file(Path::new("config.yml")));
    assert!(is_whitespace_significant_file(Path::new("Cargo.toml")));
}

#[test]
fn test_is_whitespace_significant_file_non_significant_extensions() {
    assert!(!is_whitespace_significant_file(Path::new("src/main.rs")));
    assert!(!is_whitespace_significant_file(Path::new("comp.vue")));
    assert!(!is_whitespace_significant_file(Path::new("app.ts")));
    assert!(!is_whitespace_significant_file(Path::new("index.go")));
}

#[test]
fn test_is_whitespace_significant_file_no_extension() {
    assert!(!is_whitespace_significant_file(Path::new("Makefile")));
    assert!(!is_whitespace_significant_file(Path::new("Dockerfile")));
}

// ── normalize_blank_lines (L332-351) ─────────────────────────────────────

#[test]
fn test_normalize_blank_lines_collapses_triple_blank() {
    // Three consecutive newlines → must collapse to two (one blank line).
    let input = b"line1\n\n\n\nline2\n";
    let result = normalize_blank_lines(input);
    let s = String::from_utf8(result).unwrap();
    assert_eq!(
        s, "line1\n\nline2\n",
        "three consecutive \\n must collapse to two"
    );
}

#[test]
fn test_normalize_blank_lines_preserves_single_blank() {
    let input = b"line1\n\nline2\n";
    let result = normalize_blank_lines(input);
    assert_eq!(
        result, input,
        "a single blank line must be preserved unchanged"
    );
}

#[test]
fn test_normalize_blank_lines_no_blanks_unchanged() {
    let input = b"line1\nline2\nline3";
    let result = normalize_blank_lines(input);
    assert_eq!(result, input, "no blank lines must be preserved unchanged");
}

#[test]
fn test_normalize_blank_lines_empty_input() {
    let result = normalize_blank_lines(b"");
    assert!(result.is_empty(), "empty input must produce empty output");
}

// ── strip_orphaned_doc_comment (L359-395) ────────────────────────────────

#[test]
fn test_strip_orphaned_doc_comment_zero_before_end() {
    // before_end == 0: early return must be exercised.
    let result = strip_orphaned_doc_comment(b"anything", 0);
    assert_eq!(result, 0, "before_end=0 must return 0 unchanged");
}

#[test]
fn test_strip_orphaned_doc_comment_no_doc_comment() {
    // The last line before the cut is NOT a doc comment → before_end unchanged.
    let src = b"fn hello() {}\nfn world() {}\n";
    let before_end = src.len();
    let result = strip_orphaned_doc_comment(src, before_end);
    assert_eq!(
        result, before_end,
        "no doc comment → before_end must be unchanged"
    );
}

#[test]
fn test_strip_orphaned_doc_comment_strips_trailing_doc_comment() {
    // The line immediately before the cut point is `/// orphaned`
    // → the function should step back to exclude it.
    let src = b"fn foo() {}\n/// orphaned doc\n";
    let before_end = src.len();
    let result = strip_orphaned_doc_comment(src, before_end);
    // Result must be strictly less than before_end (comment stripped)
    assert!(
        result < before_end,
        "orphaned /// comment must be stripped: result={result} before_end={before_end}"
    );
    // Stripped position must still include fn foo() {}
    let kept = std::str::from_utf8(&src[..result]).unwrap();
    assert!(kept.contains("fn foo()"), "fn foo must still be present");
    assert!(!kept.contains("orphaned"), "orphaned doc must be removed");
}

#[test]
fn test_strip_orphaned_doc_comment_strips_inner_doc_comment() {
    // `//!` inner doc is also an orphaned comment and must be stripped.
    let src = b"mod foo {}\n//! inner doc\n";
    let before_end = src.len();
    let result = strip_orphaned_doc_comment(src, before_end);
    assert!(result < before_end, "orphaned //! comment must be stripped");
}

// ── whitespace-significant TextNotFound path (L183-190) ─────────────────

#[test]
fn test_text_not_found_in_python_file_no_fuzzy_fallback() {
    // Python files are whitespace-significant: the fuzzy fallback must NOT
    // be attempted when the exact match fails.  The error must be
    // TextNotFound with a `closest_match` field (not a recursive retry).
    let source = src(&["def hello():", "    pass"]);
    let err = resolve_text_edit(
        &source,
        "def hello( ):", // wrong spacing — different from source
        1,
        "def goodbye():",
        false,
        Path::new("app.py"),
    )
    .expect_err("spacing-mismatch in .py must fail without fuzzy fallback");
    assert!(
        matches!(
            err,
            pathfinder_common::error::PathfinderError::TextNotFound { .. }
        ),
        "expected TextNotFound, got: {err:?}"
    );
}
