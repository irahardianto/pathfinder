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
use pathfinder_common::types::{VersionHash, ALWAYS_EXCLUDED_DIRS, ALWAYS_EXCLUDED_DIR_NAMES};
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
    "dll", "so", "dylib", "o", "a", "lib", "obj", "wasm", "class", "jar", "pyc", "pyo", "woff",
    "woff2", "ttf", "otf", "eot", "sqlite", "db", "mdb", "node", "bin", "dat", "idx", "pack",
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

/// Per-entry filtering decision shared by both walkers in `walk_files`.
///
/// Returns `true` if the entry passes all filters and should be counted/collected.
/// Returns `false` if the entry should be skipped.
///
/// Applies, in order:
/// 1. Always-excluded directories (git internals, vendor dirs, IDE configs)
/// 2. Path-glob include filter
/// 3. Exclude-glob filter
/// 4. Binary-extension filter
///
/// Extracted as a free function so it can be called consistently from both the
/// pre-gitignore counting walk and the post-gitignore collection walk, preventing
/// the filtering logic from drifting between the two.
#[inline]
fn filter_entry(
    relative: &str,
    path: &std::path::Path,
    glob: &str,
    glob_matcher: &globset::GlobSet,
    exclude_matcher: Option<&globset::GlobSet>,
    skip_binary: bool,
) -> bool {
    // 1. Always-excluded dirs (git internals, dependency directories, IDE configs).
    //    Use both Unix (/) and Windows (\) path separators for cross-platform support.
    //    Each entry in ALWAYS_EXCLUDED_DIRS ends with '/', e.g. ".git/".
    //    We also check "<dir>\" so Windows-style paths are caught.
    //    IMPORTANT: we must NOT strip the separator — ".git" would false-match ".github/".
    if ALWAYS_EXCLUDED_DIRS.iter().any(|dir| {
        let unix_dir = *dir;
        let mut win_buf = String::with_capacity(unix_dir.len());
        win_buf.push_str(&unix_dir[..unix_dir.len() - 1]);
        win_buf.push('\\');
        relative.starts_with(unix_dir) || relative.starts_with(&win_buf)
    }) {
        return false;
    }

    // 2. Path-glob include filter.
    if !glob_matcher.is_match(relative) && glob != "**/*" {
        return false;
    }

    // 3. Exclude-glob filter — skip files matching the exclusion pattern.
    if let Some(excl_set) = exclude_matcher {
        if excl_set.is_match(relative) {
            return false;
        }
    }

    // 4. Binary-extension filter.
    if skip_binary && is_binary_extension(path) {
        return false;
    }

    true
}

impl RipgrepScout {
    /// Build a `RegexMatcher` from `params`, respecting `is_regex`.
    /// Uses thread-local cache to avoid re-compiling the same pattern.
    #[tracing::instrument(skip_all)]
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
    #[allow(clippy::type_complexity, clippy::too_many_lines)]
    #[tracing::instrument(skip_all)]
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

        // Build a globset for the exclude_glob patterns (optional).
        let exclude_matcher = if exclude_glob.is_empty() {
            None
        } else {
            let mut builder = globset::GlobSetBuilder::new();
            let mut has_patterns = false;
            for pattern in exclude_glob {
                if !pattern.is_empty() {
                    let g = globset::GlobBuilder::new(pattern)
                        .literal_separator(false)
                        .build()
                        .map_err(|e| {
                            SearchError::InvalidPattern(format!("invalid exclude_glob: {e}"))
                        })?;
                    builder.add(g);
                    has_patterns = true;
                }
            }
            if has_patterns {
                Some(builder.build().map_err(|e| {
                    SearchError::InvalidPattern(format!("invalid exclude_glob: {e}"))
                })?)
            } else {
                None
            }
        };

        // Walker-level directory pruning callback.
        //
        // Prevents `WalkBuilder` from descending into directories listed in
        // `ALWAYS_EXCLUDED_DIR_NAMES`. This is the performance-critical filter:
        // by returning `false` for a directory entry, the walker skips the
        // entire subtree (no syscalls for its children).
        fn prune_excluded_dirs(entry: &ignore::DirEntry) -> bool {
            if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                if let Some(name) = entry.file_name().to_str() {
                    if ALWAYS_EXCLUDED_DIR_NAMES.contains(&name) {
                        return false;
                    }
                }
            }
            true
        }

        let mut walker_builder = WalkBuilder::new(&params.workspace_root);
        walker_builder
            .hidden(false)
            .git_ignore(true)
            .git_global(false)
            .git_exclude(false)
            .filter_entry(prune_excluded_dirs);
        let walker = walker_builder.build();

        let mut walker_no_ignore_builder = WalkBuilder::new(&params.workspace_root);
        walker_no_ignore_builder
            .hidden(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .filter_entry(prune_excluded_dirs);
        let walker_no_ignore = walker_no_ignore_builder.build();

        let mut files = Vec::new();
        let mut binary_skipped: usize = 0;
        let mut total_files_without_gitignore: usize = 0;

        // First walk: gitignore disabled — count how many non-binary files pass the glob filter.
        // `total_files_without_gitignore` and `binary_skipped` are counted on the same population
        // so the gitignored_skipped arithmetic below is consistent.
        for entry in walker_no_ignore.flatten() {
            let path = entry.path().to_path_buf();
            if !path.is_file() {
                continue;
            }

            let relative = match path.strip_prefix(&params.workspace_root) {
                Ok(r) => r.to_string_lossy().to_string(),
                Err(_) => continue,
            };

            if filter_entry(
                &relative,
                &path,
                glob,
                &glob_matcher,
                exclude_matcher.as_ref(),
                false,
            ) {
                if is_binary_extension(&path) {
                    binary_skipped += 1;
                } else {
                    total_files_without_gitignore += 1;
                }
            }
        }

        // Second walk: gitignore enabled — collect files that actually pass all filters.
        for entry in walker.flatten() {
            let path = entry.path().to_path_buf();
            if !path.is_file() {
                continue;
            }

            let relative = match path.strip_prefix(&params.workspace_root) {
                Ok(r) => r.to_string_lossy().to_string(),
                Err(_) => continue,
            };

            if filter_entry(
                &relative,
                &path,
                glob,
                &glob_matcher,
                exclude_matcher.as_ref(),
                true,
            ) {
                files.push((path, relative));
            }
        }

        files.sort_by(|a, b| a.1.cmp(&b.1));

        // gitignored_skipped = files that passed all filters without gitignore
        //                      minus files that passed with gitignore.
        // Since total_files_without_gitignore and files.len() both exclude binary files
        // (total_files_without_gitignore filters out binaries in the first walk, and
        // files.len() filters them out in the second walk), this difference represents
        // exactly the non-binary files excluded by gitignore.
        let gitignored_skipped = total_files_without_gitignore.saturating_sub(files.len());
        Ok((files, binary_skipped, gitignored_skipped))
    }
}

#[async_trait::async_trait]
impl Scout for RipgrepScout {
    #[tracing::instrument(skip(self), fields(query = %params.query, workspace = %params.workspace_root.display()))]
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

// ── Shared test utilities ────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
pub(crate) mod test_helpers {
    use std::io::Write;
    use tempfile::TempDir;

    /// Create a temporary workspace with the given files (path → content).
    ///
    /// Shared by all test modules in this file to avoid duplication.
    pub(crate) fn make_workspace(files: &[(&str, &str)]) -> TempDir {
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
}

#[cfg(test)]
#[path = "ripgrep_test.rs"]
mod tests;
