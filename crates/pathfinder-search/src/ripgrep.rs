//! Production search implementation using the `grep-*` crate family.
//!
//! `RipgrepScout` drives files through the `grep-searcher` engine, collecting
//! matches with configurable context lines, then computes a SHA-256 version
//! hash for each matched file to enable immediate OCC-based editing.
//!
//! Hashing is performed lazily: a file is only read for hashing when it
//! contains at least one match, avoiding redundant I/O for non-matching files.

use crate::searcher::{Scout, SearchError};
use crate::types::{SearchMatch, SearchParams, SearchResult};
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{Searcher, SearcherBuilder, Sink, SinkContext, SinkContextKind, SinkMatch};
use ignore::WalkBuilder;
use pathfinder_common::types::VersionHash;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

// ── Helper Functions ─────────────────────────────────────────────────────

/// Recover from mutex poisoning with a warning log.
///
/// Mutex poisoning indicates a panic occurred while holding the lock;
/// the data MAY be in an inconsistent state, but for search result caches
/// this is acceptable since results will simply be regenerated.
fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(e) => {
            tracing::warn!(
                "mutex poisoned, recovering (possible data inconsistency in search cache)"
            );
            e.into_inner()
        }
    }
}

/// Truncate a line to a maximum byte length, ensuring valid UTF-8 boundaries.
/// Appends `... [TRUNCATED]` if the line was shortened.
fn truncate_line(line: &str, limit: usize) -> String {
    if line.len() <= limit {
        return line.to_owned();
    }
    let mut split_at = limit;
    while !line.is_char_boundary(split_at) && split_at > 0 {
        split_at -= 1;
    }
    format!("{}... [TRUNCATED]", &line[..split_at])
}

// ── Sink implementation ──────────────────────────────────────────────

/// A [`Sink`] that accumulates matches with surrounding context into a Vec.
///
/// One `MatchCollector` is created per file search run.
struct MatchCollector<'a> {
    /// File path relative to the workspace root (for display in results).
    relative_path: String,
    /// SHA-256 hash of the file being searched.
    version_hash: String,
    /// Buffer of context lines that appear *before* the next match.
    context_before_buf: VecDeque<String>,
    /// Accumulated matches for this file.
    matches: &'a Mutex<Vec<SearchMatch>>,
    /// Running total of all matches seen (including those already capped).
    total_count: &'a Mutex<usize>,
    /// Maximum number of matches the caller wants.
    max_results: usize,
    /// Number of matches to skip before storing (for pagination).
    offset: usize,
    /// Global skip counter shared across files — tracks how many matches
    /// have been skipped so far across all files.
    skipped_count: &'a Mutex<usize>,
    /// Whether we have already hit the cap.
    truncated: bool,
    /// Context lines to collect *after* the current match.
    context_lines: usize,
    /// Lines of "after context" still to collect for the most recent match.
    pending_after_context: usize,
    /// After-context lines accumulated for the most recent match.
    after_context_buf: Vec<String>,
    /// Matcher used to compute exact column offset.
    matcher: &'a grep_regex::RegexMatcher,
    /// Keep track of the last seen line number to detect gaps.
    last_seen_line: u64,
}

impl<'a> MatchCollector<'a> {
    #[allow(clippy::similar_names, clippy::too_many_arguments)]
    fn new(
        relative_path: String,
        matches: &'a Mutex<Vec<SearchMatch>>,
        total_count: &'a Mutex<usize>,
        max_results: usize,
        offset: usize,
        skipped_count: &'a Mutex<usize>,
        context_lines: usize,
        matcher: &'a grep_regex::RegexMatcher,
    ) -> Self {
        Self {
            relative_path,
            version_hash: String::default(), // filled lazily after search
            context_before_buf: VecDeque::default(),
            matches,
            total_count,
            max_results,
            offset,
            skipped_count,
            truncated: false,
            context_lines,
            pending_after_context: 0,
            after_context_buf: Vec::default(),
            matcher,
            last_seen_line: 0,
        }
    }

    /// Backfill the version hash into all collected matches for this file.
    ///
    /// Called after the search completes and only if the file had matches,
    /// so we only pay the cost of reading + hashing files that actually matched.
    fn backfill_hash(&self, hash: &str) {
        let mut guard = lock_or_recover(self.matches);
        for m in guard.iter_mut() {
            if m.file == self.relative_path && m.version_hash.is_empty() {
                m.version_hash = hash.to_string();
            }
        }
    }

    fn current_match_count(&self) -> usize {
        lock_or_recover(self.matches).len()
    }
}

impl Sink for MatchCollector<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        // Increment total regardless of cap or offset.
        {
            let mut count = lock_or_recover(self.total_count);
            *count += 1;
        }

        // Skip matches before the offset threshold.
        // Uses a global counter shared across all files so offset works correctly
        // across file boundaries.
        {
            let mut skipped = lock_or_recover(self.skipped_count);
            if *skipped < self.offset {
                *skipped += 1;
                drop(skipped);
                // Still track line for context continuity
                let line = mat.line_number().unwrap_or(0);
                if line > self.last_seen_line + 1 {
                    self.context_before_buf.clear();
                }
                self.last_seen_line = line;
                let bytes = mat.bytes();
                let content = String::from_utf8_lossy(bytes)
                    .trim_end_matches('\n')
                    .trim_end_matches('\r')
                    .to_owned();
                let content = truncate_line(&content, 1000);
                if self.context_lines > 0 {
                    if self.context_before_buf.len() >= self.context_lines {
                        self.context_before_buf.pop_front();
                    }
                    self.context_before_buf.push_back(content);
                }
                return Ok(true);
            }
        }

        let current = self.current_match_count();
        if current >= self.max_results {
            self.truncated = true;
            // Stop searching this file to avoid useless work.
            return Ok(false);
        }

        // Flush the after-context buffer from the previous match into the last stored match.
        if !self.after_context_buf.is_empty() {
            let mut guard = lock_or_recover(self.matches);
            if let Some(last) = guard.last_mut() {
                last.context_after = std::mem::take(&mut self.after_context_buf);
            }
        }
        self.pending_after_context = self.context_lines;

        // Check for gap in line numbers to clear context before buf
        let line = mat.line_number().unwrap_or(0);
        if line > self.last_seen_line + 1 {
            self.context_before_buf.clear();
        }
        self.last_seen_line = line;

        let bytes = mat.bytes();
        // PERF: Avoid allocating a full String just to trim and truncate.
        // String::from_utf8_lossy returns a Cow, we borrow its slice, trim, and then pass to truncate_line
        // which allocates the final String just once.
        let raw_content = String::from_utf8_lossy(bytes);
        let trimmed_content = raw_content.trim_end_matches('\n').trim_end_matches('\r');
        let content = truncate_line(trimmed_content, 1000);

        // Column is 1-indexed per PRD §3.1.
        let mut column = 1_u64;
        if let Ok(Some(m)) = grep_matcher::Matcher::find(self.matcher, bytes) {
            if let Ok(prefix) = std::str::from_utf8(&bytes[..m.start()]) {
                column = prefix.chars().count() as u64 + 1;
            }
        }

        let search_match = SearchMatch {
            file: self.relative_path.clone(),
            line,
            column,
            content: content.clone(),
            context_before: std::mem::take(&mut self.context_before_buf).into(),
            context_after: Vec::new(), // filled later by `context()`
            enclosing_semantic_path: None,
            version_hash: self.version_hash.clone(),
            known: None, // set to Some(true) by search_codebase_impl for known_files
        };

        // This matching line itself acts as "before context" for a subsequent adjacent overlap match
        if self.context_lines > 0 {
            self.context_before_buf.push_back(content);
        }

        {
            let mut guard = lock_or_recover(self.matches);
            guard.push(search_match);
        }

        Ok(true)
    }

    fn context(
        &mut self,
        _searcher: &Searcher,
        ctx: &SinkContext<'_>,
    ) -> Result<bool, Self::Error> {
        let line_num = ctx.line_number().unwrap_or(0);
        if line_num > self.last_seen_line + 1 {
            self.context_before_buf.clear();
        }
        self.last_seen_line = line_num;

        // PERF: Avoid double-allocation by trimming the borrowed Cow before passing to truncate_line.
        let raw_line = String::from_utf8_lossy(ctx.bytes());
        let trimmed_line = raw_line.trim_end_matches('\n').trim_end_matches('\r');
        let line = truncate_line(trimmed_line, 1000);

        let is_after = *ctx.kind() == SinkContextKind::After;
        let needs_after = is_after && self.pending_after_context > 0;
        let needs_before = self.context_lines > 0;

        if needs_before {
            if self.context_before_buf.len() >= self.context_lines {
                self.context_before_buf.pop_front();
            }
            // PERF: Only clone if we also need it for after_context_buf
            if needs_after {
                self.context_before_buf.push_back(line.clone());
            } else {
                self.context_before_buf.push_back(line);
                return Ok(true);
            }
        }

        if needs_after {
            self.after_context_buf.push(line);
            self.pending_after_context -= 1;
        }

        Ok(true)
    }

    fn finish(
        &mut self,
        _searcher: &Searcher,
        _: &grep_searcher::SinkFinish,
    ) -> Result<(), Self::Error> {
        // Flush any remaining after-context into the last match.
        if !self.after_context_buf.is_empty() {
            let mut guard = lock_or_recover(self.matches);
            if let Some(last) = guard.last_mut() {
                last.context_after = std::mem::take(&mut self.after_context_buf);
            }
        }
        Ok(())
    }
}

// ── Scout implementation ─────────────────────────────────────────────

/// Production search engine backed by the `grep-*` crate family.
///
/// Walks the workspace with [`ignore::WalkBuilder`] (glob + `.gitignore`-aware),
/// and runs each file through `grep-searcher`.
#[derive(Default)]
pub struct RipgrepScout;

impl RipgrepScout {
    /// Build a `RegexMatcher` from `params`, respecting `is_regex`.
    fn build_matcher(params: &SearchParams) -> Result<RegexMatcher, SearchError> {
        let mut builder = RegexMatcherBuilder::new();
        builder.case_insensitive(false);

        let pattern = if params.is_regex {
            params.query.clone()
        } else {
            // Escape special regex characters for literal search.
            regex::escape(&params.query)
        };

        builder
            .build(&pattern)
            .map_err(|e| SearchError::InvalidPattern(e.to_string()))
    }

    /// Walk workspace files filtered by the globs in `params`.
    ///
    /// Applies `path_glob` (include filter) and `exclude_glob` (exclude filter)
    /// before searching — so excluded files are never read at all.
    ///
    /// Returns tuples of `(absolute_path, relative_path_string)`.
    fn walk_files(params: &SearchParams) -> Result<Vec<(PathBuf, String)>, SearchError> {
        let glob = &params.path_glob;
        let exclude_glob = &params.exclude_glob;

        // Build a globset for the user's path_glob (include) pattern.
        let glob_matcher = globset::GlobBuilder::new(glob)
            .literal_separator(false)
            .build()
            .and_then(|g| globset::GlobSet::builder().add(g).build())
            .map_err(|e| SearchError::InvalidPattern(format!("invalid path_glob: {e}")))?;

        // Build a globset for the exclude_glob pattern (optional).
        let exclude_matcher = if exclude_glob.is_empty() {
            None
        } else {
            Some(
                globset::GlobBuilder::new(exclude_glob)
                    .literal_separator(false)
                    .build()
                    .and_then(|g| globset::GlobSet::builder().add(g).build())
                    .map_err(|e| {
                        SearchError::InvalidPattern(format!("invalid exclude_glob: {e}"))
                    })?,
            )
        };

        let walker = WalkBuilder::new(&params.workspace_root)
            .hidden(false) // include dot-files unless .gitignore excludes them
            .git_ignore(true)
            .git_global(false)
            .git_exclude(false)
            .build();

        let mut files = Vec::new();

        for entry in walker.flatten() {
            let path = entry.path().to_path_buf();
            if !path.is_file() {
                continue;
            }

            // Compute relative path string for glob matching and output.
            let relative = match path.strip_prefix(&params.workspace_root) {
                Ok(r) => r.to_string_lossy().to_string(),
                Err(_) => continue,
            };

            // Apply path_glob filter if one was specified.
            let matches: bool = glob_matcher.is_match(&relative);
            if !matches && glob != "**/*" {
                continue;
            }

            // Apply exclude_glob filter — skip files matching the exclusion pattern.
            // This runs before any file I/O so excluded files are never read.
            if let Some(ref excl_set) = exclude_matcher {
                if excl_set.is_match(&relative) {
                    continue;
                }
            }

            files.push((path, relative));
        }

        files.sort_by(|a, b| a.1.cmp(&b.1));
        Ok(files)
    }
}

#[async_trait::async_trait]
impl Scout for RipgrepScout {
    async fn search(&self, params: &SearchParams) -> Result<SearchResult, SearchError> {
        let params_clone = params.clone();
        tokio::task::spawn_blocking(move || {
            let params = &params_clone;
            tracing::debug!(
                query = %params.query,
                is_regex = params.is_regex,
                path_glob = %params.path_glob,
                max_results = params.max_results,
                "Scout: starting search"
            );

            let matcher = Self::build_matcher(params)?;
            let files = Self::walk_files(params)?;

            let match_buf: Mutex<Vec<SearchMatch>> = Mutex::new(Vec::new());
            let total_count: Mutex<usize> = Mutex::new(0);
            let skipped_count: Mutex<usize> = Mutex::new(0);
            let mut truncated = false;

            let mut searcher = SearcherBuilder::new()
                .line_number(true)
                .before_context(params.context_lines)
                .after_context(params.context_lines)
                .build();

            for (abs_path, relative) in &files {
                let matches_before = { lock_or_recover(&match_buf).len() };

                let mut sink = MatchCollector::new(
                    relative.clone(),
                    &match_buf,
                    &total_count,
                    params.max_results,
                    params.offset,
                    &skipped_count,
                    params.context_lines,
                    &matcher,
                );

                if let Err(e) = searcher.search_path(&matcher, abs_path, &mut sink) {
                    tracing::warn!(file = %relative, error = %e, "Scout: failed to search file; skipping");
                    continue;
                }

                // Only hash the file when it produced at least one new match.
                // This avoids reading every file into memory just to compute a hash.
                let matches_after = { lock_or_recover(&match_buf).len() };
                if matches_after > matches_before {
                    let Ok(bytes) = std::fs::read(abs_path) else {
                        tracing::warn!(file = %relative, "Scout: failed to read file for hashing; skipping hash");
                        continue;
                    };
                    let hash = VersionHash::compute(&bytes).short().to_owned();
                    sink.backfill_hash(&hash);
                }

                if sink.truncated {
                    truncated = true;
                    // Stop searching remaining files to avoid useless work.
                    break;
                }
            }

            // ALLOW: lock is only poisoned on panic from within this function
            let collected = match_buf.into_inner().unwrap_or_default();
            let total = total_count.into_inner().unwrap_or_default();

            tracing::debug!(
                total_matches = total,
                returned = collected.len(),
                truncated,
                "Scout: search complete"
            );

            Ok(SearchResult {
                matches: collected,
                total_matches: total,
                truncated,
            })
        })
        .await
        .map_err(|e| SearchError::Engine(format!("spawn_blocking failed: {e}")))?
    }
}

// ── Unit Tests ───────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    /// Create a temporary workspace with the given files (path → content).
    fn make_workspace(files: &[(&str, &str)]) -> TempDir {
        let dir = tempfile::tempdir().expect("create tempdir");
        for (path, content) in files {
            let full = dir.path().join(path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).expect("create dirs");
            }
            let mut f = std::fs::File::create(&full).expect("create file");
            write!(f, "{content}").expect("write content");
        }
        dir
    }

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
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod missing_coverage_tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;

    #[test]
    fn test_lock_or_recover_poisoned() {
        let mutex = Mutex::new(vec![1, 2, 3]);

        let _ = std::panic::catch_unwind(|| {
            let _guard = mutex.lock().unwrap();
            panic!("poisoning");
        });

        let mut guard = lock_or_recover(&mutex);
        assert_eq!(*guard, vec![1, 2, 3]);
        guard.push(4);
    }

    #[tokio::test]
    async fn test_search_match_column_invalid_utf8_prefix() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let full = dir.path().join("main.rs");
        let mut f = std::fs::File::create(&full).expect("create file");
        f.write_all(b"x\xFFmatch\n").expect("write content");

        let scout = RipgrepScout;
        let params = SearchParams {
            workspace_root: dir.path().to_path_buf(),
            query: "match".to_owned(),
            ..Default::default()
        };
        let result = scout.search(&params).await.expect("search should succeed");

        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].column, 1);
    }

    #[tokio::test]
    async fn test_search_context_lines_gap() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let full = dir.path().join("main.rs");
        let mut f = std::fs::File::create(&full).expect("create file");
        f.write_all(b"line1\nline2\nmatch1\nline4\nline5\nline6\nline7\nline8\nmatch2\n")
            .expect("write content");

        let scout = RipgrepScout;
        let params = SearchParams {
            workspace_root: dir.path().to_path_buf(),
            query: "match".to_owned(),
            context_lines: 2,
            ..Default::default()
        };
        let result = scout.search(&params).await.expect("search should succeed");

        assert_eq!(result.matches.len(), 2);

        assert_eq!(result.matches[0].line, 3);
        assert_eq!(result.matches[0].context_before, vec!["line1", "line2"]);
        assert_eq!(result.matches[0].context_after, vec!["line4", "line5"]);

        assert_eq!(result.matches[1].line, 9);
        assert_eq!(result.matches[1].context_before, vec!["line7", "line8"]);
    }

    #[tokio::test]
    async fn test_search_context_lines_max_buffer() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let full = dir.path().join("main.rs");
        let mut f = std::fs::File::create(&full).expect("create file");
        f.write_all(b"line1\nline2\nline3\nmatch\n")
            .expect("write content");

        let scout = RipgrepScout;
        let params = SearchParams {
            workspace_root: dir.path().to_path_buf(),
            query: "match".to_owned(),
            context_lines: 2,
            ..Default::default()
        };
        let result = scout.search(&params).await.expect("search should succeed");

        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].context_before, vec!["line2", "line3"]);
    }
}
