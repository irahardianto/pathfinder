use crate::error::SurgeonError;
use crate::language::SupportedLanguage;
use crate::parser::AstParser;
use crate::vue_zones::{parse_vue_multizone, MultiZoneTree};
use lru::LruCache;
use parking_lot::Mutex;
use pathfinder_common::types::VersionHash;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::OnceCell;
use tracing::instrument;
use tree_sitter::Tree;

/// Map a filesystem `io::Error` to the appropriate `SurgeonError` variant.
///
/// `NotFound` becomes `FileNotFound` so the MCP layer can distinguish a
/// missing-file client error from a genuine server-side I/O failure.
#[inline]
fn io_err(e: std::io::Error, path: &Path) -> SurgeonError {
    if e.kind() == std::io::ErrorKind::NotFound {
        SurgeonError::FileNotFound(path.to_path_buf())
    } else {
        SurgeonError::Io(Arc::new(e))
    }
}

/// Type alias for in-flight parse operations.
/// Represents an `Arc` wrapping a `OnceCell` that will contain the parse result.
type InFlightParse = Arc<OnceCell<Result<(Tree, Arc<[u8]>), SurgeonError>>>;

/// Type alias for in-flight Vue parse operations.
type InFlightVueParse = Arc<OnceCell<Result<(MultiZoneTree, VersionHash), SurgeonError>>>;

/// Type alias for the in-flight parse map.
type InFlightParseMap = HashMap<PathBuf, InFlightParse>;

/// Type alias for the in-flight Vue parse map.
type InFlightVueParseMap = HashMap<PathBuf, InFlightVueParse>;

#[derive(Clone)]
enum ContentSource {
    Disk,
    Preloaded(Arc<[u8]>),
}

/// Contains the cached parsing result for a file.
#[derive(Debug)]
pub struct CacheEntry {
    /// The parsed AST tree.
    pub tree: Tree,
    /// The raw source code bytes.
    pub source: Arc<[u8]>,
    /// The content hash when parsed.
    pub content_hash: VersionHash,
    /// The language used for parsing.
    pub lang: SupportedLanguage,
    /// Last modification time of the file at the time of parsing (fast-path invalidation).
    pub mtime: SystemTime,
}

/// Cached parse result for a Vue SFC across all three zones.
///
/// Keyed by `PathBuf` in a separate LRU cache from non-Vue files so that
/// multi-zone trees don't pollute the single-zone eviction budget.
#[derive(Debug)]
pub struct MultiZoneEntry {
    pub multi: Arc<MultiZoneTree>,
    pub content_hash: VersionHash,
    pub mtime: SystemTime,
}

/// A thread-safe, parse-on-demand cache for ASTs.
///
/// Keeps parsed ASTs in memory up to a capacity limit. Evicts the
/// least-recently-used entry when full.
///
/// ## Fast-path invalidation
///
/// `get_or_parse` uses `tokio::fs::metadata` (a single `stat(2)` syscall) to
/// obtain the file's mtime before touching the cache. If the mtime is unchanged
/// the cached entry is returned immediately — no disk read, no hashing.
///
/// ## Singleflight deduplication
///
/// When multiple concurrent requests arrive for the same uncached file, only one
/// parse operation is performed. The other requests wait and receive the same
/// result, eliminating redundant I/O and CPU work.
#[derive(Debug)]
pub struct AstCache {
    entries: Mutex<LruCache<PathBuf, CacheEntry>>,
    /// Separate LRU cache for Vue SFC multi-zone parse results.
    vue_entries: Mutex<LruCache<PathBuf, MultiZoneEntry>>,
    /// Singleflight locks for in-flight parses to prevent redundant work.
    /// Maps path -> `OnceCell` containing the parse result.
    in_flight: Mutex<InFlightParseMap>,
    /// Singleflight locks for Vue parses.
    vue_in_flight: Mutex<InFlightVueParseMap>,
}

/// Inner singleflight helper for deduplicating in-flight parses.
#[expect(clippy::type_complexity)]
async fn get_or_parse_inner<T, F, Fut>(
    in_flight_map: &Mutex<HashMap<PathBuf, Arc<OnceCell<Result<T, SurgeonError>>>>>,
    path: &Path,
    parse_fn: F,
) -> Result<T, SurgeonError>
where
    T: Clone + Send + Sync + 'static,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, SurgeonError>> + Send,
{
    // --- Singleflight: check if another request is already parsing this file ---
    let cell = {
        let mut in_flight = in_flight_map.lock();
        // CLONE: path.to_path_buf() and cloning Arc
        in_flight
            .entry(path.to_path_buf())
            .or_insert_with(|| Arc::new(OnceCell::new()))
            .clone()
    };

    // Use get_or_init to ensure only one parse happens per file
    let result = cell.get_or_init(parse_fn).await;

    {
        let mut in_flight = in_flight_map.lock();
        in_flight.remove(path);
    }

    // CLONE: Clone the cached result or error
    result.as_ref().cloned().map_err(Clone::clone)
}

impl AstCache {
    /// # Panics
    /// Panics if `max_entries.max(1)` is somehow 0 (mathematically impossible).
    #[must_use]
    #[allow(clippy::missing_panics_doc, clippy::unwrap_used)]
    pub fn new(max_entries: usize) -> Self {
        let cap = NonZeroUsize::new(max_entries.max(1)).unwrap();
        Self {
            entries: Mutex::new(LruCache::new(cap)),
            vue_entries: Mutex::new(LruCache::new(cap)),
            in_flight: Mutex::new(HashMap::new()),
            vue_in_flight: Mutex::new(HashMap::new()),
        }
    }

    /// Retrieve the tree and source code for a file.
    ///
    /// Uses file mtime as a fast-path guard: if the cached entry's mtime
    /// matches the file's current mtime, the cached result is returned
    /// immediately (no disk read). A full read + hash + parse is only
    /// performed when the mtime has changed or the file is not yet cached.
    ///
    /// # Errors
    /// Returns `SurgeonError` if I/O fails or parsing fails.
    #[instrument(skip(self), fields(cache_hit = false))]
    pub async fn get_or_parse(
        &self,
        path: &Path,
        lang: SupportedLanguage,
    ) -> Result<(Tree, Arc<[u8]>), SurgeonError> {
        let meta = tokio::fs::metadata(path)
            .await
            .map_err(|e| io_err(e, path))?;
        let current_mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);

        self.get_or_parse_single_zone_impl(path, lang, current_mtime, ContentSource::Disk)
            .await
    }

    /// Retrieve the tree and source code using pre-loaded content and mtime.
    ///
    /// Identical to [`get_or_parse`] but accepts pre-read file bytes and mtime
    /// instead of reading from disk. Eliminates the double-read when the caller
    /// already has the file content (e.g., `generate_skeleton_text`).
    ///
    /// # Errors
    /// Returns `SurgeonError` if parsing fails.
    #[instrument(skip(self, content), fields(cache_hit = false))]
    pub async fn get_or_parse_preloaded(
        &self,
        path: &Path,
        lang: SupportedLanguage,
        content: Arc<[u8]>,
        mtime: SystemTime,
    ) -> Result<(Tree, Arc<[u8]>), SurgeonError> {
        self.get_or_parse_single_zone_impl(path, lang, mtime, ContentSource::Preloaded(content))
            .await
    }

    /// Retrieve the multi-zone parse result for a Vue SFC.
    ///
    /// Uses file mtime as a fast-path guard identical to [`get_or_parse`].
    /// On a cache miss, reads the SFC from disk and calls `parse_vue_multizone`.
    ///
    /// # Errors
    /// Returns `SurgeonError` if I/O fails or the script zone fails to parse.
    /// Template/style parse failures set `MultiZoneTree::degraded = true` but
    /// are non-fatal.
    #[instrument(skip(self), fields(cache_hit = false))]
    pub async fn get_or_parse_vue(
        &self,
        path: &Path,
    ) -> Result<(MultiZoneTree, VersionHash), SurgeonError> {
        let meta = tokio::fs::metadata(path)
            .await
            .map_err(|e| io_err(e, path))?;
        let current_mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);

        self.get_or_parse_vue_impl(path, current_mtime, ContentSource::Disk)
            .await
    }

    /// Retrieve the multi-zone parse result for a Vue SFC using pre-loaded content.
    ///
    /// Identical to [`get_or_parse_vue`] but accepts pre-read file bytes and mtime
    /// instead of reading from disk. Eliminates the double-read when the caller
    /// already has the file content (e.g., `generate_skeleton_text`).
    ///
    /// # Errors
    /// Returns `SurgeonError` if the script zone fails to parse.
    #[instrument(skip(self, content), fields(cache_hit = false))]
    pub async fn get_or_parse_vue_preloaded(
        &self,
        path: &Path,
        content: &[u8],
        mtime: SystemTime,
    ) -> Result<(MultiZoneTree, VersionHash), SurgeonError> {
        let content_arc = Arc::from(content);
        self.get_or_parse_vue_impl(path, mtime, ContentSource::Preloaded(content_arc))
            .await
    }

    async fn get_or_parse_single_zone_impl(
        &self,
        path: &Path,
        lang: SupportedLanguage,
        current_mtime: SystemTime,
        content_source: ContentSource,
    ) -> Result<(Tree, Arc<[u8]>), SurgeonError> {
        let needs_hash_check = {
            let mut lock = self.entries.lock();
            if let Some(entry) = lock.get(path) {
                if entry.mtime == current_mtime && entry.lang == lang {
                    tracing::Span::current().record("cache_hit", true);
                    return Ok((entry.tree.clone(), entry.source.clone()));
                }
                entry.lang == lang
            } else {
                false
            }
        };

        if needs_hash_check {
            let content = match &content_source {
                ContentSource::Disk => match tokio::fs::read(path).await {
                    Ok(c) => c,
                    Err(e) => return Err(io_err(e, path)),
                },
                ContentSource::Preloaded(c) => c.to_vec(),
            };
            let current_hash = tokio::task::spawn_blocking(move || VersionHash::compute(&content))
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!("spawn_blocking hash panicked: {e}");
                    VersionHash::compute(&[])
                });

            let mut lock = self.entries.lock();
            if let Some(entry) = lock.get_mut(path) {
                if entry.content_hash == current_hash && entry.lang == lang {
                    entry.mtime = current_mtime;
                    tracing::Span::current().record("cache_hit", true);
                    return Ok((entry.tree.clone(), entry.source.clone()));
                }
            }
        }

        get_or_parse_inner(&self.in_flight, path, || {
            let path_owned = path.to_path_buf();
            let source = match &content_source {
                ContentSource::Disk => None,
                ContentSource::Preloaded(c) => Some(Arc::clone(c)), // CLONE: cloning Arc to pass to the closure
            };
            async move {
                // --- Slow path: full read + hash + parse ---
                let content_arc = if let Some(c) = source {
                    c
                } else {
                    let content = tokio::fs::read(&path_owned)
                        .await
                        .map_err(|e| io_err(e, &path_owned))?;
                    Arc::from(content)
                };

                // CLONE: content must be moved into spawn_blocking.
                // The hash/parse CPU work goes to the blocking pool.
                let (tree, current_hash, content_arc) = tokio::task::spawn_blocking({
                    let path = path_owned.clone(); // CLONE: path clone for spawn_blocking
                    let content = Arc::clone(&content_arc); // CLONE: Arc clone for spawn_blocking
                    move || {
                        let current_hash = VersionHash::compute(&content);
                        // For Vue SFCs, preprocess extracts the <script> block before parsing.
                        // The original `content` is kept for version hashing and change detection —
                        // only the input to the AST parser uses the processed bytes.
                        let parse_input = lang.preprocess_source(&content);
                        let tree = AstParser::parse_source(&path, lang, &parse_input)?;
                        Ok::<_, SurgeonError>((tree, current_hash, content))
                    }
                })
                .await
                .map_err(|join_err| SurgeonError::ParseError {
                    path: path_owned.clone(), // CLONE: path clone for error details
                    reason: format!("spawn_blocking panicked: {join_err}"),
                })??;

                // Fast cache insertion back on async worker (nanoseconds)
                self.entries.lock().put(
                    path_owned.clone(), // CLONE: path clone for cache key
                    CacheEntry {
                        tree: tree.clone(),          // CLONE: tree clone for cache entry
                        source: content_arc.clone(), // CLONE: content clone for cache entry
                        content_hash: current_hash,
                        lang,
                        mtime: current_mtime,
                    },
                );

                Ok::<_, SurgeonError>((tree, content_arc))
            }
        })
        .await
    }

    async fn get_or_parse_vue_impl(
        &self,
        path: &Path,
        current_mtime: SystemTime,
        content_source: ContentSource,
    ) -> Result<(MultiZoneTree, VersionHash), SurgeonError> {
        let needs_hash_check = {
            let mut lock = self.vue_entries.lock();
            if let Some(entry) = lock.get(path) {
                if entry.mtime == current_mtime {
                    tracing::Span::current().record("cache_hit", true);
                    let multi = (*entry.multi).clone();
                    return Ok((multi, entry.content_hash.clone()));
                }
                true
            } else {
                false
            }
        };

        if needs_hash_check {
            let content = match &content_source {
                ContentSource::Disk => match tokio::fs::read(path).await {
                    Ok(c) => c,
                    Err(e) => return Err(io_err(e, path)),
                },
                ContentSource::Preloaded(c) => c.to_vec(),
            };
            let current_hash = tokio::task::spawn_blocking(move || VersionHash::compute(&content))
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!("spawn_blocking hash panicked: {e}");
                    VersionHash::compute(&[])
                });

            let mut lock = self.vue_entries.lock();
            if let Some(entry) = lock.get_mut(path) {
                if entry.content_hash == current_hash {
                    entry.mtime = current_mtime;
                    tracing::Span::current().record("cache_hit", true);
                    let multi = (*entry.multi).clone();
                    return Ok((multi, entry.content_hash.clone()));
                }
            }
        }

        get_or_parse_inner(&self.vue_in_flight, path, || {
            let path_owned = path.to_path_buf();
            let source = match &content_source {
                ContentSource::Disk => None,
                ContentSource::Preloaded(c) => Some(Arc::clone(c)), // CLONE: cloning Arc to pass to the closure
            };
            async move {
                // --- Slow path ---
                let content_arc = if let Some(c) = source {
                    c
                } else {
                    let content = tokio::fs::read(&path_owned)
                        .await
                        .map_err(|e| io_err(e, &path_owned))?;
                    Arc::from(content)
                };

                // CLONE: content must be moved into spawn_blocking.
                // The hash + parse_vue_multizone CPU work goes to blocking pool.
                let (multi, content_hash) = tokio::task::spawn_blocking({
                    let path = path_owned.clone(); // CLONE: path clone for spawn_blocking
                    let content = Arc::clone(&content_arc); // CLONE: Arc clone for spawn_blocking
                    move || {
                        let content_hash = VersionHash::compute(&content);
                        let multi = parse_vue_multizone(&content).map_err(|e| {
                            SurgeonError::ParseError {
                                path,
                                reason: format!("Vue multi-zone parse failed: {e}"),
                            }
                        })?;
                        Ok::<_, SurgeonError>((multi, content_hash))
                    }
                })
                .await
                .map_err(|join_err| SurgeonError::ParseError {
                    path: path_owned.clone(), // CLONE: path clone for error details
                    reason: format!("spawn_blocking panicked: {join_err}"),
                })??;

                let cached_multi = Arc::new(multi.clone()); // CLONE: multi clone for cached Arc

                // Fast cache insertion back on async worker (nanoseconds)
                self.vue_entries.lock().put(
                    path_owned.clone(), // CLONE: path clone for cache key
                    MultiZoneEntry {
                        multi: cached_multi,
                        content_hash: content_hash.clone(), // CLONE: content_hash clone for cache entry
                        mtime: current_mtime,
                    },
                );

                Ok::<_, SurgeonError>((multi, content_hash))
            }
        })
        .await
    }

    /// Remove a file from the cache, forcing a re-parse on next access.
    ///
    /// Flushes the file from *both* single-zone and Vue multi-zone caches so
    /// that all paths are invalidated simultaneously.
    pub fn invalidate(&self, path: &Path) {
        self.entries.lock().pop(path);
        self.vue_entries.lock().pop(path);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "cache_test.rs"]
mod tests;
