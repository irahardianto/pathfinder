//! Production search implementation using the `grep-*` crate family.
//!
//! `RipgrepScout` drives files through the `grep-searcher` engine, collecting
//! matches with configurable context lines, then computes a SHA-256 version
//! hash for each matched file as a content fingerprint.
//!
//! Hashing is performed incrementally: bytes are fed to both the grep engine
//! and a SHA-256 hasher simultaneously, avoiding redundant file I/O.

use crate::searcher::{Scout, SearchError};
use crate::types::{SearchMatch, SearchParams, SearchResult};
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{Searcher, SearcherBuilder, Sink, SinkContext, SinkContextKind, SinkMatch};
use ignore::WalkBuilder;
use lru::LruCache;
use pathfinder_common::types::{VersionHash, ALWAYS_EXCLUDED_DIRS};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::{self, BufReader, Read};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::rc::Rc;

thread_local! {
    #[allow(clippy::expect_used)]
    static REGEX_CACHE: RefCell<LruCache<String, RegexMatcher>> =
        RefCell::new(LruCache::new(NonZeroUsize::new(32).expect("32 > 0")));
}

const BINARY_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "ico", "webp", "svg", "tiff", "tif", "mp3", "mp4", "wav",
    "avi", "mov", "mkv", "flv", "wmv", "webm", "ogg", "zip", "tar", "gz", "bz2", "xz", "7z", "rar",
    "tgz", "zst", "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "exe",
    "dll", "so", "dylib", "o", "a", "lib", "obj", "wasm", "class", "jar", "pyc", "pyo", "o",
    "woff", "woff2", "ttf", "otf", "eot", "sqlite", "db", "mdb", "node", "bin", "dat", "idx",
    "pack",
];

fn is_binary_extension(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| BINARY_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

struct TeeHasher<R> {
    reader: R,
    hasher: Sha256,
}

impl<R> TeeHasher<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            hasher: Sha256::new(),
        }
    }

    fn finish(self) -> [u8; 32] {
        self.hasher.finalize().into()
    }
}

impl<R: Read> Read for TeeHasher<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.reader.read(buf)?;
        if n > 0 {
            self.hasher.update(&buf[..n]);
        }
        Ok(n)
    }
}

/// Truncate a line to a maximum byte length, ensuring valid UTF-8 boundaries.
/// Appends `... [TRUNCATED]` if the line was shortened.
///
/// Retained for testing; production code uses `safe_truncate_bytes` + `decode_line`.
#[cfg(test)]
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

/// Find a safe UTF-8 truncation point at or before `limit` bytes.
///
/// Walks backward from `limit` to find a char boundary so we avoid
/// splitting a multi-byte UTF-8 sequence.
fn safe_truncate_bytes(bytes: &[u8], limit: usize) -> &[u8] {
    if bytes.len() <= limit {
        return bytes;
    }
    let mut split_at = limit;
    // UTF-8 continuation bytes (10xxxxxx) are not char boundaries.
    // Walk backward until we find a leading byte or ASCII char.
    while split_at > 0 && (bytes[split_at] & 0xC0) == 0x80 {
        split_at -= 1;
    }
    &bytes[..split_at]
}

#[inline]
fn decode_line(bytes: &[u8]) -> String {
    let trimmed = strip_line_endings(bytes);
    // Truncate raw bytes first to avoid UTF-8 validation on discarded tail.
    // 1000 ASCII chars = 1000 bytes max; non-ASCII may truncate shorter but
    // that's acceptable for display context.
    let truncated = safe_truncate_bytes(trimmed, 1000);
    if let Ok(s) = std::str::from_utf8(truncated) {
        if truncated.len() < trimmed.len() {
            format!("{s}... [TRUNCATED]")
        } else {
            s.to_owned()
        }
    } else {
        let s = String::from_utf8_lossy(truncated);
        format!("{s}... [TRUNCATED]")
    }
}

#[inline]
fn strip_line_endings(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    if end > 0 && bytes[end - 1] == b'\n' {
        end -= 1;
    }
    if end > 0 && bytes[end - 1] == b'\r' {
        end -= 1;
    }
    &bytes[..end]
}

// ── Sink implementation ──────────────────────────────────────────────

/// A [`Sink`] that accumulates matches with surrounding context into a Vec.
///
/// One `MatchCollector` is created per file search run.
struct MatchCollector<'a> {
    /// File path relative to the workspace root (for display in results).
    /// Uses `Rc<str>` to share across matches without per-match allocation.
    relative_path: Rc<str>,
    /// SHA-256 hash of the file being searched.
    version_hash: String,
    /// Buffer of context lines that appear *before* the next match.
    context_before_buf: VecDeque<String>,
    /// Accumulated matches for this file.
    matches: &'a mut Vec<SearchMatch>,
    /// Running total of all matches seen (including those already capped).
    total_count: &'a mut usize,
    /// Maximum number of matches the caller wants.
    max_results: usize,
    /// Number of matches to skip before storing (for pagination).
    offset: usize,
    /// Global skip counter shared across files — tracks how many matches
    /// have been skipped so far across all files.
    skipped_count: &'a mut usize,
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
        relative_path: Rc<str>,
        matches: &'a mut Vec<SearchMatch>,
        total_count: &'a mut usize,
        max_results: usize,
        offset: usize,
        skipped_count: &'a mut usize,
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
    fn current_match_count(&self) -> usize {
        self.matches.len()
    }
}

impl Sink for MatchCollector<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        // Increment total regardless of cap or offset.
        *self.total_count += 1;

        // Skip matches before the offset threshold.
        // Uses a global counter shared across all files so offset works correctly
        // across file boundaries.
        if *self.skipped_count < self.offset {
            *self.skipped_count += 1;
            // Still track line for context continuity
            let line = mat.line_number().unwrap_or(0);
            if line > self.last_seen_line + 1 {
                self.context_before_buf.clear();
            }
            self.last_seen_line = line;
            let content = decode_line(mat.bytes());
            if self.context_lines > 0 {
                if self.context_before_buf.len() >= self.context_lines {
                    self.context_before_buf.pop_front();
                }
                self.context_before_buf.push_back(content);
            }
            return Ok(true);
        }

        let current = self.current_match_count();
        if current >= self.max_results {
            self.truncated = true;
            // Stop searching this file to avoid useless work.
            return Ok(false);
        }

        // Flush the after-context buffer from the previous match into the last stored match.
        if !self.after_context_buf.is_empty() {
            if let Some(last) = self.matches.last_mut() {
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
        let content = decode_line(bytes);

        let mut column = 1_u64;
        if let Ok(Some(m)) = grep_matcher::Matcher::find(self.matcher, bytes) {
            if let Ok(prefix) = std::str::from_utf8(&bytes[..m.start()]) {
                column = prefix.chars().count() as u64 + 1;
            }
        }

        let content_for_context = if self.context_lines > 0 {
            Some(content.clone())
        } else {
            None
        };

        let search_match = SearchMatch {
            file: self.relative_path.to_string(),
            line,
            column,
            content,
            context_before: std::mem::take(&mut self.context_before_buf).into(),
            context_after: Vec::new(),
            enclosing_semantic_path: None,
            is_definition: None,
            version_hash: self.version_hash.clone(),
            known: None,
        };

        // This matching line itself acts as "before context" for a subsequent adjacent overlap match
        if let Some(ctx) = content_for_context {
            self.context_before_buf.push_back(ctx);
        }

        self.matches.push(search_match);

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

        let line = decode_line(ctx.bytes());

        if self.context_lines > 0 {
            if self.context_before_buf.len() >= self.context_lines {
                self.context_before_buf.pop_front();
            }
            self.context_before_buf.push_back(line.clone());
        }

        if *ctx.kind() == SinkContextKind::After && self.pending_after_context > 0 {
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
            if let Some(last) = self.matches.last_mut() {
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
    /// Uses thread-local cache to avoid re-compiling the same pattern.
    fn build_matcher(params: &SearchParams) -> Result<RegexMatcher, SearchError> {
        let pattern = if params.is_regex {
            params.query.clone()
        } else {
            regex::escape(&params.query)
        };

        let cache_key = format!("{pattern}\0ci=false");

        REGEX_CACHE.with_borrow_mut(|cache| {
            let matcher = cache.try_get_or_insert(cache_key, || {
                let mut builder = RegexMatcherBuilder::new();
                builder.case_insensitive(false);
                builder
                    .build(&pattern)
                    .map_err(|e| SearchError::InvalidPattern(e.to_string()))
            })?;
            Ok(matcher.clone())
        })
    }

    /// Walk workspace files filtered by the globs in `params`.
    ///
    /// Applies `path_glob` (include filter) and `exclude_glob` (exclude filter)
    /// before searching — so excluded files are never read at all.
    ///
    /// Returns `(files, binary_skipped, gitignored_skipped)` — tuples of `(absolute_path, relative_path_string)`
    /// plus the count of files skipped because they matched known binary extensions, and the count of
    /// gitignored files that matched the glob but were excluded by gitignore.
    #[allow(clippy::type_complexity)]
    fn walk_files(
        params: &SearchParams,
    ) -> Result<(Vec<(PathBuf, String)>, usize, usize), SearchError> {
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
            .hidden(false)
            .git_ignore(true)
            .git_global(false)
            .git_exclude(false)
            .build();

        let walker_no_ignore = WalkBuilder::new(&params.workspace_root)
            .hidden(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .build();

        let mut files = Vec::new();
        let mut binary_skipped: usize = 0;
        let mut total_files_without_gitignore: usize = 0;

        for entry in walker_no_ignore.flatten() {
            let path = entry.path().to_path_buf();
            if !path.is_file() {
                continue;
            }

            let relative = match path.strip_prefix(&params.workspace_root) {
                Ok(r) => r.to_string_lossy().to_string(),
                Err(_) => continue,
            };

            if ALWAYS_EXCLUDED_DIRS.iter().any(|dir| {
                let unix_dir = *dir;
                let win_dir = &unix_dir[..unix_dir.len() - 1];
                relative.starts_with(unix_dir) || relative.starts_with(win_dir)
            }) {
                continue;
            }

            let matches: bool = glob_matcher.is_match(&relative);
            if !matches && glob != "**/*" {
                continue;
            }

            if let Some(ref excl_set) = exclude_matcher {
                if excl_set.is_match(&relative) {
                    continue;
                }
            }

            if is_binary_extension(&path) {
                continue;
            }

            total_files_without_gitignore += 1;
        }

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

            // Always exclude git internals, dependency directories, and IDE configs.
            // These are never source code and cause false positives in grep fallback.
            // Use both Unix (/) and Windows (\) path separators for cross-platform support.
            if ALWAYS_EXCLUDED_DIRS.iter().any(|dir| {
                let unix_dir = *dir;
                // Convert "foo/" to "foo\" for Windows
                let win_dir = &unix_dir[..unix_dir.len() - 1];
                relative.starts_with(unix_dir) || relative.starts_with(win_dir)
            }) {
                continue;
            }

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

            // Skip known binary files before adding to search list.
            if is_binary_extension(&path) {
                binary_skipped += 1;
                continue;
            }

            files.push((path, relative));
        }

        files.sort_by(|a, b| a.1.cmp(&b.1));

        let gitignored_skipped = total_files_without_gitignore
            .saturating_sub(files.len())
            .saturating_sub(binary_skipped);
        Ok((files, binary_skipped, gitignored_skipped))
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

            let matcher = RipgrepScout::build_matcher(params)?;
            let (files, binary_skipped, gitignored_skipped) = Self::walk_files(params)?;
            let files_in_scope = files.len();

            let mut match_buf: Vec<SearchMatch> =
                Vec::with_capacity(params.max_results.min(256));
            let mut total_count: usize = 0;
            let mut skipped_count: usize = 0;
            let mut truncated = false;
            let mut files_searched: usize = 0;
            let mut other_skipped: usize = 0;

            let mut searcher = SearcherBuilder::new()
                .line_number(true)
                .before_context(params.context_lines)
                .after_context(params.context_lines)
                .build();

            for (abs_path, relative) in &files {
                let matches_before = match_buf.len();

                let file = match std::fs::File::open(abs_path) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!(file = %relative, error = %e, "Scout: failed to open file; skipping");
                        other_skipped += 1;
                        continue;
                    }
                };
                let mut tee = TeeHasher::new(BufReader::new(file));

                let mut sink = MatchCollector::new(
                    Rc::from(relative.as_str()),
                    &mut match_buf,
                    &mut total_count,
                    params.max_results,
                    params.offset,
                    &mut skipped_count,
                    params.context_lines,
                    &matcher,
                );

                if let Err(e) = searcher.search_reader(&matcher, &mut tee, &mut sink) {
                    tracing::warn!(file = %relative, error = %e, "Scout: failed to search file; skipping");
                    other_skipped += 1;
                    continue;
                }
                files_searched += 1;

                let matches_after = sink.current_match_count();
                let is_truncated = sink.truncated;
                // Release the mutable borrow on match_buf before indexing into it.
                drop(sink);

                if matches_after > matches_before {
                    let hash_bytes = tee.finish();
                    let hash = VersionHash::compute_from_raw(hash_bytes);
                    let short = hash.short().to_owned();
                    // Only update matches from this file (known range) instead of
                    // scanning all matches with a string comparison.
                    for m in &mut match_buf[matches_before..matches_after] {
                        m.version_hash.clone_from(&short);
                    }
                }

                if is_truncated {
                    truncated = true;
                    break;
                }
            }

            tracing::debug!(
                total_matches = total_count,
                returned = match_buf.len(),
                truncated,
                files_searched,
                files_in_scope,
                "Scout: search complete"
            );

            Ok(SearchResult {
                matches: match_buf,
                total_matches: total_count,
                truncated,
                files_searched,
                files_in_scope,
                binary_skipped,
                gitignored_skipped,
                other_skipped,
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
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod missing_coverage_tests {
    use super::*;
    use std::io::Write;

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

    #[test]
    fn test_is_binary_extension() {
        assert!(is_binary_extension(std::path::Path::new("image.png")));
        assert!(is_binary_extension(std::path::Path::new("archive.ZIP")));
        assert!(is_binary_extension(std::path::Path::new("lib/data.sqlite")));
        assert!(!is_binary_extension(std::path::Path::new("main.rs")));
        assert!(!is_binary_extension(std::path::Path::new("app.tsx")));
        assert!(!is_binary_extension(std::path::Path::new("config.toml")));
    }

    #[tokio::test]
    async fn test_search_skips_binary_files() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(dir.path().join("main.rs"), "fn main() { findme(); }").unwrap();
        std::fs::write(dir.path().join("image.png"), "PNG binary data").unwrap();
        std::fs::write(dir.path().join("data.pdf"), "PDF binary data").unwrap();

        let scout = RipgrepScout;
        let params = SearchParams {
            workspace_root: dir.path().to_path_buf(),
            query: "findme".to_owned(),
            ..Default::default()
        };
        let result = scout.search(&params).await.expect("search should succeed");

        assert_eq!(result.matches.len(), 1);
        assert_eq!(
            result.binary_skipped, 2,
            "png and pdf should be skipped as binary"
        );
        assert_eq!(result.files_searched, 1, "only main.rs should be searched");
        assert_eq!(
            result.files_in_scope, 1,
            "files_in_scope excludes binary files (only main.rs is searchable)"
        );
    }

    #[tokio::test]
    async fn test_search_counts_gitignored_files() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(dir.path())
            .output()
            .expect("git init should succeed");

        std::fs::write(dir.path().join("main.rs"), "fn main() { findme(); }").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.rs\n").unwrap();
        std::fs::write(dir.path().join("ignored.rs"), "fn ignored() { findme(); }").unwrap();

        let scout = RipgrepScout;
        let params = SearchParams {
            workspace_root: dir.path().to_path_buf(),
            query: "findme".to_owned(),
            ..Default::default()
        };
        let result = scout.search(&params).await.expect("search should succeed");

        assert_eq!(result.matches.len(), 1);
        assert_eq!(
            result.gitignored_skipped, 1,
            "ignored.rs should be counted as gitignored"
        );
        assert_eq!(result.files_searched, 1, "only main.rs should be searched");
        assert_eq!(
            result.files_in_scope, 1,
            "files_in_scope excludes gitignored (only main.rs is searchable)"
        );
    }
}

// ── BATCH-03c: Argument construction and coverage gap tests ──────────────────

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod batch03c_tests {
    use super::*;
    use std::io::Write;

    fn make_workspace(files: &[(&str, &str)]) -> tempfile::TempDir {
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

    // ── Multi-pattern query (regex alternation) ───────────────────────────

    #[tokio::test]
    async fn test_search_multi_pattern_regex_alternation() {
        let ws = make_workspace(&[
            ("src/alpha.rs", "foo_function here\n"),
            ("src/beta.rs", "bar_function here\n"),
            ("src/gamma.rs", "unrelated_content\n"),
        ]);
        let scout = RipgrepScout;
        let params = SearchParams {
            workspace_root: ws.path().to_path_buf(),
            query: "foo_function|bar_function".to_owned(),
            is_regex: true,
            ..Default::default()
        };
        let result = scout.search(&params).await.expect("search should succeed");
        assert_eq!(
            result.total_matches, 2,
            "multi-pattern regex should match both alternatives"
        );
        let files: std::collections::HashSet<_> =
            result.matches.iter().map(|m| m.file.clone()).collect();
        assert!(files.iter().any(|f| f.contains("alpha")));
        assert!(files.iter().any(|f| f.contains("beta")));
    }

    #[tokio::test]
    async fn test_search_literal_pipe_not_alternation() {
        let ws = make_workspace(&[
            ("pipe.rs", "cmd | grep pattern\n"),
            ("nopipe.rs", "foo bar\n"),
        ]);
        let scout = RipgrepScout;
        let params = SearchParams {
            workspace_root: ws.path().to_path_buf(),
            query: "cmd | grep".to_owned(),
            is_regex: false,
            ..Default::default()
        };
        let result = scout.search(&params).await.expect("search should succeed");
        assert_eq!(result.total_matches, 1, "literal | is not alternation");
        assert!(result.matches[0].file.contains("pipe"));
    }

    // ── File type filtering ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_search_file_type_glob_include_only_ts() {
        let ws = make_workspace(&[
            ("src/app.ts", "findme\n"),
            ("src/styles.css", "findme\n"),
            ("src/main.rs", "findme\n"),
        ]);
        let scout = RipgrepScout;
        let params = SearchParams {
            workspace_root: ws.path().to_path_buf(),
            query: "findme".to_owned(),
            path_glob: "**/*.ts".to_owned(),
            ..Default::default()
        };
        let result = scout.search(&params).await.expect("search should succeed");
        assert_eq!(result.total_matches, 1, "only .ts files should be searched");
        assert!(std::path::Path::new(&result.matches[0].file)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("ts")));
    }

    #[tokio::test]
    async fn test_search_file_type_glob_exclude_generated() {
        let ws = make_workspace(&[
            ("src/lib.rs", "findme\n"),
            ("src/generated.rs", "findme\n"),
            ("src/lib_test.rs", "findme\n"),
        ]);
        let scout = RipgrepScout;
        let params = SearchParams {
            workspace_root: ws.path().to_path_buf(),
            query: "findme".to_owned(),
            path_glob: "**/*.rs".to_owned(),
            exclude_glob: "**/generated*.rs".to_owned(),
            ..Default::default()
        };
        let result = scout.search(&params).await.expect("search should succeed");
        assert_eq!(result.total_matches, 2, "generated.rs should be excluded");
        assert!(result.matches.iter().all(|m| !m.file.contains("generated")));
    }

    // ── Context line configuration ────────────────────────────────────────

    #[tokio::test]
    async fn test_search_context_lines_one() {
        let ws = make_workspace(&[("src/main.rs", "before\ntarget\nafter\n")]);
        let scout = RipgrepScout;
        let params = SearchParams {
            workspace_root: ws.path().to_path_buf(),
            query: "target".to_owned(),
            context_lines: 1,
            ..Default::default()
        };
        let result = scout.search(&params).await.expect("search should succeed");
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].context_before.len(), 1);
        assert_eq!(result.matches[0].context_after.len(), 1);
        assert_eq!(result.matches[0].context_before[0], "before");
        assert_eq!(result.matches[0].context_after[0], "after");
    }

    #[tokio::test]
    async fn test_search_context_lines_three_at_file_boundary() {
        let ws = make_workspace(&[("src/main.rs", "target\nline2\nline3\n")]);
        let scout = RipgrepScout;
        let params = SearchParams {
            workspace_root: ws.path().to_path_buf(),
            query: "target".to_owned(),
            context_lines: 3,
            ..Default::default()
        };
        let result = scout.search(&params).await.expect("search should succeed");
        assert_eq!(result.matches.len(), 1);
        assert_eq!(
            result.matches[0].context_before.len(),
            0,
            "no before context at start of file"
        );
        assert_eq!(
            result.matches[0].context_after.len(),
            2,
            "only 2 lines exist after match"
        );
    }

    // ── Gitignore toggle ──────────────────────────────────────────────────

    #[tokio::test]
    async fn test_search_gitignore_respected_in_git_repo() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(dir.path())
            .output()
            .expect("git init");
        std::fs::write(dir.path().join("main.rs"), "findme\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.rs\n").unwrap();
        std::fs::write(dir.path().join("ignored.rs"), "findme\n").unwrap();

        let scout = RipgrepScout;
        let params = SearchParams {
            workspace_root: dir.path().to_path_buf(),
            query: "findme".to_owned(),
            ..Default::default()
        };
        let result = scout.search(&params).await.expect("search should succeed");
        assert_eq!(result.total_matches, 1, "gitignored file must be excluded");
        assert_eq!(result.matches[0].file, "main.rs");
        assert_eq!(
            result.gitignored_skipped, 1,
            "gitignored_skipped should count the ignored file"
        );
    }

    // ── Result deduplication ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_search_no_spurious_deduplication_different_lines() {
        let ws = make_workspace(&[("src/main.rs", "needle_a\nneedle_b\n")]);
        let scout = RipgrepScout;
        let params = SearchParams {
            workspace_root: ws.path().to_path_buf(),
            query: "needle_[ab]".to_owned(),
            is_regex: true,
            ..Default::default()
        };
        let result = scout.search(&params).await.expect("search should succeed");
        assert_eq!(
            result.total_matches, 2,
            "two different lines must not be deduplicated"
        );
    }

    #[tokio::test]
    async fn test_search_multiple_matches_same_file_no_dedup() {
        let ws = make_workspace(&[("src/main.rs", "needle\nneedle\nneedle\n")]);
        let scout = RipgrepScout;
        let params = SearchParams {
            workspace_root: ws.path().to_path_buf(),
            query: "needle".to_owned(),
            context_lines: 0,
            ..Default::default()
        };
        let result = scout.search(&params).await.expect("search should succeed");
        assert_eq!(result.total_matches, 3);
        assert_eq!(result.matches[0].line, 1);
        assert_eq!(result.matches[1].line, 2);
        assert_eq!(result.matches[2].line, 3);
    }

    // ── Large result set handling ─────────────────────────────────────────

    #[tokio::test]
    async fn test_search_large_result_set_total_matches_accurate() {
        let dir = tempfile::tempdir().expect("create tempdir");
        for i in 0..20_u32 {
            std::fs::write(dir.path().join(format!("file_{i:02}.rs")), "needle\n").unwrap();
        }
        let scout = RipgrepScout;
        let params = SearchParams {
            workspace_root: dir.path().to_path_buf(),
            query: "needle".to_owned(),
            max_results: 10,
            ..Default::default()
        };
        let result = scout.search(&params).await.expect("search should succeed");
        assert_eq!(result.matches.len(), 10, "should cap at max_results=10");
        assert!(
            result.total_matches >= 10,
            "total_matches must be at least 10: {}",
            result.total_matches
        );
        assert!(result.truncated, "must be marked truncated");
    }

    // ── Regex pattern escaping (literal search) ───────────────────────────

    #[tokio::test]
    async fn test_search_literal_regex_special_chars_escaped() {
        let ws = make_workspace(&[
            ("src/main.rs", "result.unwrap()\n"),
            ("src/other.rs", "result_unwrap\n"),
        ]);
        let scout = RipgrepScout;
        let params = SearchParams {
            workspace_root: ws.path().to_path_buf(),
            query: "result.unwrap()".to_owned(),
            is_regex: false,
            ..Default::default()
        };
        let result = scout.search(&params).await.expect("search should succeed");
        assert_eq!(result.total_matches, 1, "dot and parens must be literal");
        assert!(result.matches[0].file.contains("main"));
    }

    // ── files_searched / files_in_scope accounting ────────────────────────

    #[tokio::test]
    async fn test_search_files_in_scope_excludes_binary() {
        let dir = tempfile::tempdir().expect("create tempdir");
        std::fs::write(dir.path().join("app.rs"), "fn main() { findme(); }").unwrap();
        std::fs::write(dir.path().join("logo.png"), "PNG_DATA").unwrap();
        let scout = RipgrepScout;
        let params = SearchParams {
            workspace_root: dir.path().to_path_buf(),
            query: "findme".to_owned(),
            ..Default::default()
        };
        let result = scout.search(&params).await.expect("search should succeed");
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.files_searched, 1, "only app.rs is searched");
        assert_eq!(
            result.files_in_scope, 1,
            "files_in_scope excludes binary: {}",
            result.files_in_scope
        );
        assert!(
            result.binary_skipped >= 1,
            "logo.png must be counted as binary skipped"
        );
    }

    // ── safe_truncate_bytes unit tests ────────────────────────────────────

    #[test]
    fn test_safe_truncate_bytes_ascii() {
        assert_eq!(safe_truncate_bytes(b"hello world", 5), b"hello");
    }

    #[test]
    fn test_safe_truncate_bytes_within_limit() {
        assert_eq!(safe_truncate_bytes(b"hi", 100), b"hi");
    }

    #[test]
    fn test_safe_truncate_bytes_exact_limit() {
        assert_eq!(safe_truncate_bytes(b"hello", 5), b"hello");
    }

    #[test]
    fn test_safe_truncate_bytes_multibyte_boundary() {
        // "aéb" = [0x61, 0xC3, 0xA9, 0x62]; limit=2 hits 0xA9 (continuation byte 10xxxxxx)
        // → walk back to 0xC3 (leading byte 11xxxxxx, not continuation) → stop at split_at=1
        let bytes = "aéb".as_bytes();
        let truncated = safe_truncate_bytes(bytes, 2);
        assert_eq!(truncated, b"a");
        assert!(std::str::from_utf8(truncated).is_ok());
    }

    // ── strip_line_endings unit tests ─────────────────────────────────────

    #[test]
    fn test_strip_line_endings_crlf() {
        assert_eq!(strip_line_endings(b"hello\r\n"), b"hello");
    }

    #[test]
    fn test_strip_line_endings_lf_only() {
        assert_eq!(strip_line_endings(b"world\n"), b"world");
    }

    #[test]
    fn test_strip_line_endings_no_newline() {
        assert_eq!(strip_line_endings(b"plain"), b"plain");
    }

    #[test]
    fn test_strip_line_endings_empty() {
        assert_eq!(strip_line_endings(b""), b"");
    }

    // ── decode_line unit tests ────────────────────────────────────────────

    #[test]
    fn test_decode_line_short_ascii() {
        assert_eq!(decode_line(b"short line\n"), "short line");
    }

    #[test]
    fn test_decode_line_long_ascii_truncates() {
        let long = "a".repeat(1500);
        let long_bytes = format!("{long}\n");
        let result = decode_line(long_bytes.as_bytes());
        assert!(
            result.ends_with("... [TRUNCATED]"),
            "long line must be truncated"
        );
        assert!(
            result.len() < 1050,
            "truncated result must be under 1050 chars"
        );
    }

    #[test]
    fn test_decode_line_crlf_stripped() {
        assert_eq!(decode_line(b"line content\r\n"), "line content");
    }
}
