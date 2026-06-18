use super::test_helpers::make_workspace;
use super::*;
use tempfile::TempDir;

fn params_for(workspace: &TempDir, query: &str) -> SearchParams {
    SearchParams {
        workspace_root: workspace.path().to_path_buf(),
        query: query.to_owned(),
        ..Default::default()
    }
}

// ── Red-Green: literal search ──────────────────────────────────

#[tokio::test]
async fn test_search_literal_pattern_found() {
    let ws = make_workspace(&[
        (
            "src/main.rs",
            "fn main() {\n    println!(\"hello world\");\n}\n",
        ),
        (
            "src/lib.rs",
            "// Library code\npub fn add(a: i32, b: i32) -> i32 { a + b }\n",
        ),
    ]);
    let scout = RipgrepScout;
    let result = scout
        .search(&params_for(&ws, "hello world"))
        .await
        .expect("search should succeed");

    assert_eq!(result.total_matches, 1);
    assert!(!result.truncated);
    assert_eq!(result.matches.len(), 1);
    assert_eq!(result.matches[0].file, "src/main.rs");
    assert_eq!(result.matches[0].line, 2);
}

#[tokio::test]
async fn test_search_literal_not_found() {
    let ws = make_workspace(&[("src/main.rs", "fn main() {}\n")]);
    let scout = RipgrepScout;
    let result = scout
        .search(&params_for(&ws, "NONEXISTENT_PATTERN_XYZ"))
        .await
        .expect("search should succeed");

    assert_eq!(result.total_matches, 0);
    assert!(!result.truncated);
    assert!(result.matches.is_empty());
}

// ── Red-Green: regex search ────────────────────────────────────

#[tokio::test]
async fn test_search_regex_pattern() {
    let ws = make_workspace(&[("src/auth.rs", "pub fn login() {}\npub fn logout() {}\n")]);
    let scout = RipgrepScout;
    let params = SearchParams {
        workspace_root: ws.path().to_path_buf(),
        query: r"pub fn log(in|out)\(\)".to_owned(),
        is_regex: true,
        ..Default::default()
    };
    let result = scout.search(&params).await.expect("search should succeed");

    assert_eq!(
        result.total_matches, 2,
        "should match both login and logout"
    );
}

#[tokio::test]
async fn test_search_invalid_regex_returns_error() {
    let ws = make_workspace(&[("src/main.rs", "fn main() {}\n")]);
    let scout = RipgrepScout;
    let params = SearchParams {
        workspace_root: ws.path().to_path_buf(),
        query: "[invalid regex".to_owned(),
        is_regex: true,
        ..Default::default()
    };
    let err = scout.search(&params).await;
    assert!(err.is_err());
    assert!(matches!(err, Err(SearchError::InvalidPattern(_))));
}

// ── Red-Green: path glob filter ────────────────────────────────

#[tokio::test]
async fn test_search_path_glob_restricts_files() {
    let ws = make_workspace(&[
        ("src/main.rs", "find_me\n"),
        ("docs/README.md", "find_me\n"),
        ("src/auth.rs", "find_me\n"),
    ]);
    let scout = RipgrepScout;
    let params = SearchParams {
        workspace_root: ws.path().to_path_buf(),
        query: "find_me".to_owned(),
        path_glob: "src/**/*.rs".to_owned(),
        ..Default::default()
    };
    let result = scout.search(&params).await.expect("search should succeed");

    // Only .rs files in src/ should match
    assert_eq!(result.total_matches, 2, "only src/*.rs should be searched");
    for m in &result.matches {
        assert!(
            m.file.starts_with("src/"),
            "file should be in src/: {}",
            m.file
        );
        assert!(
            std::path::Path::new(&m.file)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("rs")),
            "file should be .rs: {}",
            m.file
        );
    }
}

// ── Red-Green: max_results truncation ─────────────────────────

#[tokio::test]
async fn test_search_max_results_truncation() {
    // 5 files, each with "needle" — but we only want 3 results.
    let ws = make_workspace(&[
        ("a.rs", "needle\n"),
        ("b.rs", "needle\n"),
        ("c.rs", "needle\n"),
        ("d.rs", "needle\n"),
        ("e.rs", "needle\n"),
    ]);
    let scout = RipgrepScout;
    let params = SearchParams {
        workspace_root: ws.path().to_path_buf(),
        query: "needle".to_owned(),
        max_results: 3,
        ..Default::default()
    };
    let result = scout.search(&params).await.expect("search should succeed");

    assert_eq!(result.matches.len(), 3, "should cap at max_results");
    assert!(
        result.total_matches >= 3,
        "total_matches reflects matches found before truncation"
    );
    assert!(result.truncated, "should set truncated = true");
}

// ── Red-Green: context lines ───────────────────────────────────

#[tokio::test]
async fn test_search_context_lines() {
    let ws = make_workspace(&[("src/main.rs", "line1\nline2\ntarget_line\nline4\nline5\n")]);
    let scout = RipgrepScout;
    let params = SearchParams {
        workspace_root: ws.path().to_path_buf(),
        query: "target_line".to_owned(),
        context_lines: 2,
        ..Default::default()
    };
    let result = scout.search(&params).await.expect("search should succeed");

    assert_eq!(result.matches.len(), 1);
    let m = &result.matches[0];
    assert_eq!(m.context_before.len(), 2, "should have 2 lines before");
    assert_eq!(m.context_after.len(), 2, "should have 2 lines after");
    assert!(m.context_before[0].contains("line1"));
    assert!(m.context_before[1].contains("line2"));
    assert!(m.context_after[0].contains("line4"));
    assert!(m.context_after[1].contains("line5"));
}

// ── Red-Green: version hash ────────────────────────────────────

#[tokio::test]
async fn test_search_match_has_version_hash() {
    let ws = make_workspace(&[("src/main.rs", "fn main() { /* marker */ }\n")]);
    let scout = RipgrepScout;
    let result = scout
        .search(&params_for(&ws, "marker"))
        .await
        .expect("search should succeed");

    assert_eq!(result.matches.len(), 1);
    let hash = &result.matches[0].version_hash;
    assert_eq!(hash.len(), 7, "hash should be a 7-char short hash: {hash}");
    assert!(
        hash.chars().all(|c| c.is_ascii_hexdigit()),
        "hash should be hex chars only: {hash}"
    );
}

// ── Red-Green: enclosing_semantic_path is null in Epic 2 ──────

#[tokio::test]
async fn test_search_enclosing_semantic_path_is_null() {
    let ws = make_workspace(&[("src/main.rs", "find_this\n")]);
    let scout = RipgrepScout;
    let result = scout
        .search(&params_for(&ws, "find_this"))
        .await
        .expect("search should succeed");

    assert_eq!(result.matches.len(), 1);
    assert!(
        result.matches[0].enclosing_semantic_path.is_none(),
        "should be None until Tree-sitter is implemented in Epic 3"
    );
}

// ── Red-Green: regex cache test ────────────────────────────────

#[tokio::test]
async fn test_regex_cache_same_pattern() {
    let ws = make_workspace(&[
        ("src/main.rs", "pub fn my_function() {}"),
        ("src/lib.rs", "fn my_function() {}"),
        ("src/auth.rs", "fn my_function() {}"),
    ]);

    let scout = RipgrepScout;
    let pattern = r"\bmy_function\b";

    let params1 = SearchParams {
        workspace_root: ws.path().to_path_buf(),
        query: pattern.to_owned(),
        is_regex: true,
        path_glob: "src/main.rs".to_owned(),
        ..Default::default()
    };

    let params2 = SearchParams {
        workspace_root: ws.path().to_path_buf(),
        query: pattern.to_owned(),
        is_regex: true,
        path_glob: "src/lib.rs".to_owned(),
        ..Default::default()
    };

    let params3 = SearchParams {
        workspace_root: ws.path().to_path_buf(),
        query: pattern.to_owned(),
        is_regex: true,
        path_glob: "src/auth.rs".to_owned(),
        ..Default::default()
    };

    let _ = scout.search(&params1).await.expect("search should succeed");
    let _ = scout.search(&params2).await.expect("search should succeed");
    let _ = scout.search(&params3).await.expect("search should succeed");
}

// ── Red-Green: exclude_glob ────────────────────────────────────

#[tokio::test]
async fn test_search_exclude_glob_skips_matching_files() {
    let ws = make_workspace(&[
        ("src/main.rs", "needle\n"),
        ("src/main.test.rs", "needle\n"),
        ("src/auth.rs", "needle\n"),
    ]);
    let scout = RipgrepScout;
    let params = SearchParams {
        workspace_root: ws.path().to_path_buf(),
        query: "needle".to_owned(),
        exclude_glob: "**/*.test.*".to_owned(),
        ..Default::default()
    };
    let result = scout.search(&params).await.expect("search should succeed");

    // The .test.rs file should be excluded — only 2 matches should remain.
    assert_eq!(result.total_matches, 2, "test files should be excluded");
    for m in &result.matches {
        assert!(
            !m.file.contains(".test."),
            "excluded file showed up: {}",
            m.file
        );
    }
}

// ── Red-Green: .git/ directory exclusion ──────────────────────

#[tokio::test]
async fn test_search_excludes_git_directory() {
    // Issue: .git/index and other .git/ internals should be excluded from search
    // because they are binary files or git internal data, not source code.
    let ws = make_workspace(&[
        (".git/index", "needle_in_git_index\n"),
        (".git/config", "needle_in_git_config\n"),
        ("src/main.rs", "legitimate_needle\n"),
    ]);
    let scout = RipgrepScout;
    let params = SearchParams {
        workspace_root: ws.path().to_path_buf(),
        query: "needle".to_owned(),
        ..Default::default()
    };
    let result = scout.search(&params).await.expect("search should succeed");

    // Only src/main.rs should match, NOT .git/index or .git/config
    assert_eq!(
        result.total_matches,
        1,
        ".git/ files should be excluded, but got matches in: {:?}",
        result.matches.iter().map(|m| &m.file).collect::<Vec<_>>()
    );
    assert_eq!(
        result.matches[0].file, "src/main.rs",
        "only legitimate source file should match"
    );
}

#[tokio::test]
async fn test_search_excludes_node_modules_and_vendor() {
    // Dependencies directories should also be excluded by default
    let ws = make_workspace(&[
        ("node_modules/react/index.js", "needle_in_dep\n"),
        ("vendor/golang.org/x/net/http.go", "needle_in_vendor\n"),
        ("src/main.rs", "legitimate_needle\n"),
    ]);
    let scout = RipgrepScout;
    let params = SearchParams {
        workspace_root: ws.path().to_path_buf(),
        query: "needle".to_owned(),
        ..Default::default()
    };
    let result = scout.search(&params).await.expect("search should succeed");

    // Only src/main.rs should match
    assert_eq!(
        result.total_matches,
        1,
        "node_modules/ and vendor/ should be excluded, but got matches in: {:?}",
        result.matches.iter().map(|m| &m.file).collect::<Vec<_>>()
    );
    assert_eq!(result.matches[0].file, "src/main.rs");
}

// ── Red-Green: multiple matches per file ──────────────────────

#[tokio::test]
async fn test_search_multiple_matches_in_one_file() {
    let ws = make_workspace(&[("src/auth.rs", "token\nlogin\ntoken\nlogout\n")]);
    let scout = RipgrepScout;
    let result = scout
        .search(&params_for(&ws, "token"))
        .await
        .expect("search should succeed");

    assert_eq!(result.total_matches, 2);
    assert_eq!(result.matches[0].line, 1);
    assert_eq!(result.matches[1].line, 3);
}

#[tokio::test]
async fn test_search_invalid_glob_returns_error() {
    let ws = make_workspace(&[("src/main.rs", "fn main() {}\n")]);
    let scout = RipgrepScout;
    let params = SearchParams {
        workspace_root: ws.path().to_path_buf(),
        query: "main".to_owned(),
        path_glob: "[invalid glob".to_owned(),
        ..Default::default()
    };
    let err = scout.search(&params).await;
    assert!(err.is_err());
    assert!(matches!(err, Err(SearchError::InvalidPattern(_))));

    let params2 = SearchParams {
        workspace_root: ws.path().to_path_buf(),
        query: "main".to_owned(),
        exclude_glob: "[invalid glob".to_owned(),
        ..Default::default()
    };
    let err2 = scout.search(&params2).await;
    assert!(err2.is_err());
    assert!(matches!(err2, Err(SearchError::InvalidPattern(_))));
}

#[tokio::test]
async fn test_search_context_lines_overlap() {
    let ws = make_workspace(&[(
        "src/main.rs",
        "line1\nline2\nmatch\nline4\nmatch\nline6\nline7\nmatch\n",
    )]);
    let scout = RipgrepScout;
    let params = SearchParams {
        workspace_root: ws.path().to_path_buf(),
        query: "match".to_owned(),
        context_lines: 2,
        ..Default::default()
    };
    let result = scout.search(&params).await.expect("search should succeed");

    assert_eq!(result.matches.len(), 3);

    // Match 1 at line 3
    assert_eq!(result.matches[0].line, 3);
    assert_eq!(result.matches[0].context_before, vec!["line1", "line2"]);
    assert_eq!(result.matches[0].context_after, vec!["line4"]); // Only line4 before the next match at line 5

    // Match 2 at line 5
    assert_eq!(result.matches[1].line, 5);
    assert_eq!(result.matches[1].context_before, vec!["match", "line4"]); // "match" from line 3, "line4" from line 4
    assert_eq!(result.matches[1].context_after, vec!["line6", "line7"]);

    // Match 3 at line 8
    assert_eq!(result.matches[2].line, 8);
    assert_eq!(result.matches[2].context_before, vec!["line6", "line7"]);
    assert_eq!(result.matches[2].context_after, Vec::<String>::default());
}

#[tokio::test]
async fn test_search_line_truncation() {
    let long_line = "a".repeat(2000);
    let ws = make_workspace(&[(
        "src/main.rs",
        &format!("{long_line}\nmatch {long_line}\n{long_line}\n"),
    )]);
    let scout = RipgrepScout;
    let params = SearchParams {
        workspace_root: ws.path().to_path_buf(),
        query: "match".to_owned(),
        context_lines: 1,
        ..Default::default()
    };
    let result = scout.search(&params).await.expect("search should succeed");

    assert_eq!(result.matches.len(), 1);
    let match_content = &result.matches[0].content;
    let context_before = &result.matches[0].context_before[0];
    let context_after = &result.matches[0].context_after[0];

    assert!(match_content.ends_with("... [TRUNCATED]"));
    assert!(match_content.len() < 1050); // 1000 + length of suffix

    assert!(context_before.ends_with("... [TRUNCATED]"));
    assert!(context_before.len() < 1050);

    assert!(context_after.ends_with("... [TRUNCATED]"));
    assert!(context_after.len() < 1050);
}

// ── Additional edge-case tests for coverage gaps ─────────────

#[tokio::test]
async fn test_search_column_offset() {
    // Verify that the column field reflects the match position, not just 1
    let ws = make_workspace(&[("src/main.rs", "    pub fn hello() -> i32 { 42 }\n")]);
    let scout = RipgrepScout;
    let result = scout
        .search(&params_for(&ws, "hello"))
        .await
        .expect("search should succeed");

    assert_eq!(result.matches.len(), 1);
    let m = &result.matches[0];
    // "    pub fn hello" — 'hello' starts at column 12 (1-indexed)
    assert_eq!(m.column, 12, "column should be 1-indexed position of match");
}

#[tokio::test]
async fn test_search_default_impl() {
    // RipgrepScout should behave identically to new()
    let scout = RipgrepScout;
    let ws = make_workspace(&[("src/main.rs", "find_default\n")]);
    let result = scout
        .search(&params_for(&ws, "find_default"))
        .await
        .expect("search should succeed");
    assert_eq!(result.total_matches, 1);
}

#[tokio::test]
async fn test_search_match_column_for_first_char() {
    // Match at the very beginning of a line
    let ws = make_workspace(&[("src/lib.rs", "fn main() {}\n")]);
    let scout = RipgrepScout;
    let result = scout
        .search(&params_for(&ws, "fn main"))
        .await
        .expect("search should succeed");

    assert_eq!(result.matches[0].column, 1);
}

#[tokio::test]
async fn test_search_zero_context_lines() {
    let ws = make_workspace(&[("src/main.rs", "line1\ntarget\nline3\n")]);
    let scout = RipgrepScout;
    let params = SearchParams {
        workspace_root: ws.path().to_path_buf(),
        query: "target".to_owned(),
        context_lines: 0,
        ..Default::default()
    };
    let result = scout.search(&params).await.expect("search should succeed");

    assert_eq!(result.matches.len(), 1);
    assert!(result.matches[0].context_before.is_empty());
    assert!(result.matches[0].context_after.is_empty());
}

#[tokio::test]
async fn test_search_match_count_exceeds_max_across_files() {
    // total_matches should reflect ALL matches found before truncation
    let ws = make_workspace(&[
        ("a.rs", "needle\nneedle\n"),
        ("b.rs", "needle\nneedle\n"),
        ("c.rs", "needle\nneedle\n"),
    ]);
    let scout = RipgrepScout;
    let params = SearchParams {
        workspace_root: ws.path().to_path_buf(),
        query: "needle".to_owned(),
        max_results: 2,
        ..Default::default()
    };
    let result = scout.search(&params).await.expect("search should succeed");

    assert_eq!(result.matches.len(), 2, "should cap at max_results");
    assert!(
        result.total_matches >= 2,
        "total_matches should reflect all found: {}",
        result.total_matches
    );
    assert!(result.truncated);
}

#[test]
fn test_truncate_line_short() {
    // Short lines should not be truncated
    let result = truncate_line("hello", 1000);
    assert_eq!(result, "hello");
}

#[test]
fn test_truncate_line_exact_boundary() {
    // Line exactly at limit should not be truncated
    let line = "a".repeat(1000);
    let result = truncate_line(&line, 1000);
    assert_eq!(result, line);
}

#[test]
fn test_truncate_line_over_boundary() {
    let line = "a".repeat(1001);
    let result = truncate_line(&line, 1000);
    assert!(result.ends_with("... [TRUNCATED]"));
    // The truncated portion should be at most 1000 chars + suffix
    assert!(result.len() < line.len() + 20);
    // The original content portion should be at most 1000 chars
    let without_suffix = result.trim_end_matches("... [TRUNCATED]");
    assert!(without_suffix.len() <= 1000);
}

#[test]
fn test_truncate_line_multibyte_char_boundary() {
    // Ensure truncation respects UTF-8 char boundaries
    let line = format!("{}{}", "x".repeat(999), "Ä".repeat(10)); // Ä is 2 bytes
    let result = truncate_line(&line, 1000);
    assert!(result.ends_with("... [TRUNCATED]"));
    // Should not panic on char boundary
    assert!(std::str::from_utf8(result.as_bytes()).is_ok());
}

#[tokio::test]
async fn test_search_known_field_default_false() {
    let ws = make_workspace(&[("src/main.rs", "find_known\n")]);
    let scout = RipgrepScout;
    let result = scout
        .search(&params_for(&ws, "find_known"))
        .await
        .expect("search should succeed");

    assert_eq!(result.matches[0].known, None);
}

// ── GAP-007: Offset pagination tests ──────────────────────────────

#[tokio::test]
async fn test_search_offset_pagination() {
    let ws = make_workspace(&[
        ("a.rs", "needle\n"),
        ("b.rs", "needle\n"),
        ("c.rs", "needle\n"),
        ("d.rs", "needle\n"),
        ("e.rs", "needle\n"),
        ("f.rs", "needle\n"),
        ("g.rs", "needle\n"),
        ("h.rs", "needle\n"),
        ("i.rs", "needle\n"),
        ("j.rs", "needle\n"),
    ]);
    let scout = RipgrepScout;

    // Page 1: offset=0, max_results=3
    let p1 = scout
        .search(&SearchParams {
            workspace_root: ws.path().to_path_buf(),
            query: "needle".to_owned(),
            max_results: 3,
            offset: 0,
            ..Default::default()
        })
        .await
        .expect("search should succeed");
    assert_eq!(p1.matches.len(), 3, "page 1 should have 3 matches");
    assert!(p1.truncated, "page 1 should be truncated");

    // Page 2: offset=3, max_results=3
    let p2 = scout
        .search(&SearchParams {
            workspace_root: ws.path().to_path_buf(),
            query: "needle".to_owned(),
            max_results: 3,
            offset: 3,
            ..Default::default()
        })
        .await
        .expect("search should succeed");
    assert_eq!(p2.matches.len(), 3, "page 2 should have 3 matches");
    assert!(p2.truncated, "page 2 should be truncated");

    // Verify pages don't overlap (different files)
    let p1_files: std::collections::HashSet<_> =
        p1.matches.iter().map(|m| m.file.clone()).collect();
    let p2_files: std::collections::HashSet<_> =
        p2.matches.iter().map(|m| m.file.clone()).collect();
    assert_eq!(
        p1_files.intersection(&p2_files).count(),
        0,
        "pages should not overlap"
    );

    // Page 3: offset=6, max_results=3
    let p3 = scout
        .search(&SearchParams {
            workspace_root: ws.path().to_path_buf(),
            query: "needle".to_owned(),
            max_results: 3,
            offset: 6,
            ..Default::default()
        })
        .await
        .expect("search should succeed");
    assert_eq!(p3.matches.len(), 3, "page 3 should have 3 matches");
    assert!(p3.truncated, "page 3 should be truncated");

    // Page 4: offset=9, max_results=3
    let p4 = scout
        .search(&SearchParams {
            workspace_root: ws.path().to_path_buf(),
            query: "needle".to_owned(),
            max_results: 3,
            offset: 9,
            ..Default::default()
        })
        .await
        .expect("search should succeed");
    assert_eq!(p4.matches.len(), 1, "page 4 should have 1 match");
    assert!(!p4.truncated, "page 4 should not be truncated");

    // Verify all 10 files were covered across pages
    let all_files: std::collections::HashSet<_> = p1
        .matches
        .iter()
        .chain(p2.matches.iter())
        .chain(p3.matches.iter())
        .chain(p4.matches.iter())
        .map(|m| m.file.clone())
        .collect();
    assert_eq!(
        all_files.len(),
        10,
        "all 10 files should be covered by pagination"
    );
}

#[tokio::test]
async fn test_search_offset_beyond_results() {
    let ws = make_workspace(&[
        ("a.rs", "needle\n"),
        ("b.rs", "needle\n"),
        ("c.rs", "needle\n"),
        ("d.rs", "needle\n"),
        ("e.rs", "needle\n"),
    ]);
    let scout = RipgrepScout;
    let result = scout
        .search(&SearchParams {
            workspace_root: ws.path().to_path_buf(),
            query: "needle".to_owned(),
            max_results: 50,
            offset: 10,
            ..Default::default()
        })
        .await
        .expect("search should succeed");
    assert_eq!(
        result.matches.len(),
        0,
        "offset beyond results should be empty"
    );
    assert_eq!(
        result.total_matches, 5,
        "total_matches should still report 5"
    );
    assert!(!result.truncated, "should not be truncated");
}

#[tokio::test]
async fn test_search_offset_with_truncation() {
    let ws = make_workspace(&[
        ("a.rs", "needle\n"),
        ("b.rs", "needle\n"),
        ("c.rs", "needle\n"),
        ("d.rs", "needle\n"),
        ("e.rs", "needle\n"),
        ("f.rs", "needle\n"),
        ("g.rs", "needle\n"),
        ("h.rs", "needle\n"),
    ]);
    let scout = RipgrepScout;
    let result = scout
        .search(&SearchParams {
            workspace_root: ws.path().to_path_buf(),
            query: "needle".to_owned(),
            max_results: 3,
            offset: 0,
            ..Default::default()
        })
        .await
        .expect("search should succeed");
    assert_eq!(result.matches.len(), 3);
    assert!(result.truncated);
    // total_matches includes the match that triggered truncation but not remaining files
    assert!(
        result.total_matches >= 4,
        "total_matches should include at least 4: {}",
        result.total_matches
    );
}

// ── GAP: strip_line_endings tests ─────────────────────────────────

#[test]
fn test_strip_line_endings_crlf() {
    let input = b"hello\r\n";
    let result = strip_line_endings(input);
    assert_eq!(result, b"hello");
}

#[test]
fn test_strip_line_endings_bare_cr() {
    let input = b"hello\r";
    let result = strip_line_endings(input);
    assert_eq!(result, b"hello");
}

#[test]
fn test_strip_line_endings_lf_only() {
    let input = b"hello\n";
    let result = strip_line_endings(input);
    assert_eq!(result, b"hello");
}

#[test]
fn test_strip_line_endings_no_ending() {
    let input = b"hello";
    let result = strip_line_endings(input);
    assert_eq!(result, b"hello");
}

// ── GAP: safe_truncate_bytes at multibyte boundary ────────────────

#[test]
fn test_safe_truncate_bytes_at_multibyte_boundary() {
    // 'é' is 2 bytes in UTF-8 (0xC3 0xA9). "héllo" = [h, 0xC3, 0xA9, l, l, o]
    let input = "héllo";
    let bytes = input.as_bytes();
    // Truncate at byte 2 — splits the 'é' char (byte 1 is 0xC3, byte 2 is 0xA9).
    let result = safe_truncate_bytes(bytes, 2);
    // Should back up to byte 1 (before the multibyte sequence).
    assert!(
        std::str::from_utf8(result).is_ok(),
        "result should be valid UTF-8"
    );
    assert_eq!(std::str::from_utf8(result).unwrap(), "h");
}

// ── GAP: decode_line tests ────────────────────────────────────────

#[test]
fn test_decode_line_valid_utf8() {
    let input = b"hello world\n";
    let result = decode_line(input);
    assert_eq!(result, "hello world");
}

#[test]
fn test_decode_line_invalid_utf8() {
    // 0xFF is never valid in UTF-8
    let input: Vec<u8> = vec![b'h', b'e', 0xFF, b'l', b'o', b'\n'];
    let result = decode_line(&input);
    // Lossy conversion replaces invalid bytes with U+FFFD
    assert!(
        result.contains('\u{FFFD}'),
        "should contain replacement char"
    );
    // Short enough that no truncation suffix is expected from length, but
    // decode_line adds "... [TRUNCATED]" on lossy conversion regardless.
    assert!(
        result.ends_with("... [TRUNCATED]"),
        "lossy conversion should add truncated suffix: {result}"
    );
}

// ── GAP: binary_skipped count in results ──────────────────────────

#[tokio::test]
async fn test_binary_skipped_count_in_results() {
    let ws = make_workspace(&[
        ("src/main.rs", "needle\n"),
        ("assets/image.png", "needle\n"),
        ("lib/module.dll", "needle\n"),
    ]);
    let scout = RipgrepScout;
    let result = scout
        .search(&params_for(&ws, "needle"))
        .await
        .expect("search should succeed");

    assert!(
        result.binary_skipped > 0,
        "binary_skipped should be > 0, got {}",
        result.binary_skipped
    );
    // Only the .rs file should produce a match
    assert_eq!(result.total_matches, 1);
    assert_eq!(result.matches[0].file, "src/main.rs");
}

// ── GAP: files_searched count ─────────────────────────────────────

#[tokio::test]
async fn test_files_searched_count() {
    let ws = make_workspace(&[
        ("a.rs", "needle\n"),
        ("b.rs", "no match here\n"),
        ("c.rs", "needle\n"),
    ]);
    let scout = RipgrepScout;
    let result = scout
        .search(&params_for(&ws, "needle"))
        .await
        .expect("search should succeed");

    // All 3 text files should be searched, even if not all contain matches
    assert_eq!(
        result.files_searched, 3,
        "all text files should be searched"
    );
    assert_eq!(result.total_matches, 2);
}

// ── GAP: search empty workspace ───────────────────────────────────

#[tokio::test]
async fn test_search_empty_workspace() {
    let ws = tempfile::tempdir().expect("create tempdir");
    let scout = RipgrepScout;
    let result = scout
        .search(&params_for(&ws, "anything"))
        .await
        .expect("search should succeed on empty workspace");

    assert_eq!(result.total_matches, 0);
    assert!(result.matches.is_empty());
    assert!(!result.truncated);
    assert_eq!(result.files_searched, 0);
}

// ── GAP: search with empty query ──────────────────────────────────

#[tokio::test]
async fn test_search_empty_query() {
    let ws = make_workspace(&[("src/main.rs", "fn main() {}\n")]);
    let scout = RipgrepScout;
    let result = scout.search(&params_for(&ws, "")).await;

    // Empty query may either error (invalid pattern) or return matches on every line.
    // Either is acceptable behavior — we just verify no panic.
    match result {
        Ok(r) => {
            // If it succeeds, the empty pattern matches every line
            assert!(r.files_searched > 0, "should search at least one file");
        }
        Err(e) => {
            // If it errors, it should be an InvalidPattern error
            assert!(
                matches!(e, SearchError::InvalidPattern(_)),
                "unexpected error type: {e:?}"
            );
        }
    }
}
