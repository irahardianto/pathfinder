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
        SurgeonError::Io(e)
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
    #[expect(
        clippy::too_many_lines,
        reason = "content-hash fallback adds medium-path between fast-path and singleflight"
    )]
    pub async fn get_or_parse(
        &self,
        path: &Path,
        lang: SupportedLanguage,
    ) -> Result<(Tree, Arc<[u8]>), SurgeonError> {
        // --- Fast-path guard: single stat syscall ---
        let meta = tokio::fs::metadata(path)
            .await
            .map_err(|e| io_err(e, path))?;
        let current_mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);

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
            let content = match tokio::fs::read(path).await {
                Ok(c) => c,
                Err(e) => return Err(io_err(e, path)),
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

        // --- Singleflight: check if another request is already parsing this file ---
        let cell = {
            let mut in_flight = self.in_flight.lock();
            in_flight
                .entry(path.to_path_buf())
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };

        // Use get_or_init to ensure only one parse happens per file
        let result = cell
            .get_or_init(|| {
                let path_owned = path.to_path_buf();
                async move {
                    // --- Slow path: full read + hash + parse ---
                    let content = tokio::fs::read(&path_owned)
                        .await
                        .map_err(|e| io_err(e, &path_owned))?;

                    // CLONE: content must be moved into spawn_blocking.
                    // The hash/parse CPU work goes to the blocking pool.
                    let (tree, current_hash, content_arc) = tokio::task::spawn_blocking({
                        let path = path_owned.clone();
                        move || {
                            let current_hash = VersionHash::compute(&content);
                            let content_arc: Arc<[u8]> = Arc::from(content);
                            // For Vue SFCs, preprocess extracts the <script> block before parsing.
                            // The original `content` is kept for version hashing and change detection —
                            // only the input to the AST parser uses the processed bytes.
                            let parse_input = lang.preprocess_source(&content_arc);
                            let tree = AstParser::parse_source(&path, lang, &parse_input)?;
                            Ok::<_, SurgeonError>((tree, current_hash, content_arc))
                        }
                    })
                    .await
                    .map_err(|join_err| SurgeonError::ParseError {
                        path: path_owned.clone(),
                        reason: format!("spawn_blocking panicked: {join_err}"),
                    })??;

                    // Fast cache insertion back on async worker (nanoseconds)
                    self.entries.lock().put(
                        path_owned.clone(),
                        CacheEntry {
                            tree: tree.clone(),
                            source: content_arc.clone(),
                            content_hash: current_hash,
                            lang,
                            mtime: current_mtime,
                        },
                    );

                    Ok::<_, SurgeonError>((tree, content_arc))
                }
            })
            .await;

        {
            let mut in_flight = self.in_flight.lock();
            in_flight.remove(path);
        }

        match result.as_ref() {
            Ok((tree, source)) => Ok((tree.clone(), source.clone())),
            Err(SurgeonError::FileNotFound(p)) => Err(SurgeonError::FileNotFound(p.clone())),
            Err(SurgeonError::UnsupportedLanguage(p)) => {
                Err(SurgeonError::UnsupportedLanguage(p.clone()))
            }
            Err(SurgeonError::ParseError { path: p, reason }) => Err(SurgeonError::ParseError {
                path: p.clone(),
                reason: reason.clone(),
            }),
            Err(SurgeonError::SymbolNotFound {
                path: p,
                did_you_mean: dym,
            }) => Err(SurgeonError::SymbolNotFound {
                path: p.clone(),
                did_you_mean: dym.clone(),
            }),
            Err(SurgeonError::Io(e)) => Err(SurgeonError::ParseError {
                path: path.to_path_buf(),
                reason: e.to_string(),
            }),
        }
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
        let needs_hash_check = {
            let lock = self.entries.lock();
            if let Some(entry) = lock.peek(path) {
                if entry.mtime == mtime && entry.lang == lang {
                    tracing::Span::current().record("cache_hit", true);
                    return Ok((entry.tree.clone(), entry.source.clone()));
                }
                entry.lang == lang
            } else {
                false
            }
        };

        if needs_hash_check {
            let content_clone = content.clone();
            let current_hash =
                tokio::task::spawn_blocking(move || VersionHash::compute(&content_clone))
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!("spawn_blocking hash panicked: {e}");
                        VersionHash::compute(&[])
                    });

            let mut lock = self.entries.lock();
            if let Some(entry) = lock.get_mut(path) {
                if entry.content_hash == current_hash && entry.lang == lang {
                    entry.mtime = mtime;
                    tracing::Span::current().record("cache_hit", true);
                    return Ok((entry.tree.clone(), entry.source.clone()));
                }
            }
        }

        let cell = {
            let mut in_flight = self.in_flight.lock();
            in_flight
                .entry(path.to_path_buf())
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };

        let result = cell
            .get_or_init(|| {
                let path_owned = path.to_path_buf();
                // CLONE: content_arc must be moved into spawn_blocking.
                // It's Arc, so clone is refcount increment only.
                let content_arc = content.clone();
                async move {
                    let (tree, current_hash, content_arc) = tokio::task::spawn_blocking({
                        let path = path_owned.clone();
                        let content = content_arc.clone();
                        move || {
                            let current_hash = VersionHash::compute(&content);
                            let parse_input = lang.preprocess_source(&content);
                            let tree = AstParser::parse_source(&path, lang, &parse_input)?;
                            Ok::<_, SurgeonError>((tree, current_hash, content))
                        }
                    })
                    .await
                    .map_err(|join_err| SurgeonError::ParseError {
                        path: path_owned.clone(),
                        reason: format!("spawn_blocking panicked: {join_err}"),
                    })??;

                    // Fast cache insertion back on async worker (nanoseconds)
                    self.entries.lock().put(
                        path_owned.clone(),
                        CacheEntry {
                            tree: tree.clone(),
                            source: content_arc.clone(),
                            content_hash: current_hash,
                            lang,
                            mtime,
                        },
                    );

                    Ok::<_, SurgeonError>((tree, content_arc))
                }
            })
            .await;

        {
            let mut in_flight = self.in_flight.lock();
            in_flight.remove(path);
        }

        match result.as_ref() {
            Ok((tree, source)) => Ok((tree.clone(), source.clone())),
            Err(SurgeonError::FileNotFound(p)) => Err(SurgeonError::FileNotFound(p.clone())),
            Err(SurgeonError::UnsupportedLanguage(p)) => {
                Err(SurgeonError::UnsupportedLanguage(p.clone()))
            }
            Err(SurgeonError::ParseError { path: p, reason }) => Err(SurgeonError::ParseError {
                path: p.clone(),
                reason: reason.clone(),
            }),
            Err(SurgeonError::SymbolNotFound {
                path: p,
                did_you_mean: dym,
            }) => Err(SurgeonError::SymbolNotFound {
                path: p.clone(),
                did_you_mean: dym.clone(),
            }),
            Err(SurgeonError::Io(e)) => Err(SurgeonError::ParseError {
                path: path.to_path_buf(),
                reason: e.to_string(),
            }),
        }
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
    #[expect(
        clippy::too_many_lines,
        reason = "content-hash fallback adds medium-path between fast-path and singleflight"
    )]
    pub async fn get_or_parse_vue(
        &self,
        path: &Path,
    ) -> Result<(MultiZoneTree, VersionHash), SurgeonError> {
        let meta = tokio::fs::metadata(path)
            .await
            .map_err(|e| io_err(e, path))?;
        let current_mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);

        let needs_hash_check = self
            .vue_entries
            .lock()
            .peek(path)
            .is_some_and(|entry| entry.mtime != current_mtime);

        if needs_hash_check {
            let content = tokio::fs::read(path).await.map_err(|e| io_err(e, path))?;

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
        } else {
            let lock = self.vue_entries.lock();
            if let Some(entry) = lock.peek(path) {
                if entry.mtime == current_mtime {
                    tracing::Span::current().record("cache_hit", true);
                    let multi = (*entry.multi).clone();
                    return Ok((multi, entry.content_hash.clone()));
                }
            }
        }

        // --- Singleflight: check if another request is already parsing this Vue file ---
        let cell = {
            let mut vue_in_flight = self.vue_in_flight.lock();
            vue_in_flight
                .entry(path.to_path_buf())
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };

        // Use get_or_init to ensure only one parse happens per file
        let result = cell
            .get_or_init(|| {
                let path_owned = path.to_path_buf();
                async move {
                    // --- Slow path ---
                    let content = tokio::fs::read(&path_owned)
                        .await
                        .map_err(|e| io_err(e, &path_owned))?;

                    // CLONE: content must be moved into spawn_blocking.
                    // The hash + parse_vue_multizone CPU work goes to blocking pool.
                    let (multi, content_hash) = tokio::task::spawn_blocking({
                        let path = path_owned.clone();
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
                        path: path_owned.clone(),
                        reason: format!("spawn_blocking panicked: {join_err}"),
                    })??;

                    let cached_multi = Arc::new(MultiZoneTree {
                        script_tree: multi.script_tree.clone(),
                        template_tree: multi.template_tree.clone(),
                        style_tree: multi.style_tree.clone(),
                        zones: multi.zones.clone(),
                        source: multi.source.clone(),
                        degraded: multi.degraded,
                    });

                    // Fast cache insertion back on async worker (nanoseconds)
                    self.vue_entries.lock().put(
                        path_owned.clone(),
                        MultiZoneEntry {
                            multi: cached_multi,
                            content_hash: content_hash.clone(),
                            mtime: current_mtime,
                        },
                    );

                    Ok::<_, SurgeonError>((multi, content_hash))
                }
            })
            .await;

        {
            let mut vue_in_flight = self.vue_in_flight.lock();
            vue_in_flight.remove(path);
        }

        match result.as_ref() {
            Ok((multi, hash)) => Ok((multi.clone(), hash.clone())),
            Err(SurgeonError::FileNotFound(p)) => Err(SurgeonError::FileNotFound(p.clone())),
            Err(SurgeonError::UnsupportedLanguage(p)) => {
                Err(SurgeonError::UnsupportedLanguage(p.clone()))
            }
            Err(SurgeonError::ParseError { path: p, reason }) => Err(SurgeonError::ParseError {
                path: p.clone(),
                reason: reason.clone(),
            }),
            Err(SurgeonError::SymbolNotFound {
                path: p,
                did_you_mean: dym,
            }) => Err(SurgeonError::SymbolNotFound {
                path: p.clone(),
                did_you_mean: dym.clone(),
            }),
            Err(SurgeonError::Io(e)) => Err(SurgeonError::ParseError {
                path: path.to_path_buf(),
                reason: e.to_string(),
            }),
        }
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
    #[expect(
        clippy::too_many_lines,
        reason = "content-hash fallback adds medium-path between fast-path and singleflight"
    )]
    pub async fn get_or_parse_vue_preloaded(
        &self,
        path: &Path,
        content: &[u8],
        mtime: SystemTime,
    ) -> Result<(MultiZoneTree, VersionHash), SurgeonError> {
        let needs_hash_check = self
            .vue_entries
            .lock()
            .peek(path)
            .is_some_and(|entry| entry.mtime != mtime);

        if needs_hash_check {
            let owned_content = content.to_vec();
            let current_hash =
                tokio::task::spawn_blocking(move || VersionHash::compute(&owned_content))
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!("spawn_blocking hash panicked: {e}");
                        VersionHash::compute(&[])
                    });

            let mut lock = self.vue_entries.lock();
            if let Some(entry) = lock.get_mut(path) {
                if entry.content_hash == current_hash {
                    entry.mtime = mtime;
                    tracing::Span::current().record("cache_hit", true);
                    let multi = (*entry.multi).clone();
                    return Ok((multi, entry.content_hash.clone()));
                }
            }
        } else {
            let lock = self.vue_entries.lock();
            if let Some(entry) = lock.peek(path) {
                if entry.mtime == mtime {
                    tracing::Span::current().record("cache_hit", true);
                    let multi = (*entry.multi).clone();
                    return Ok((multi, entry.content_hash.clone()));
                }
            }
        }

        let cell = {
            let mut vue_in_flight = self.vue_in_flight.lock();
            vue_in_flight
                .entry(path.to_path_buf())
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };

        let result = cell
            .get_or_init(|| {
                let path_owned = path.to_path_buf();
                // CLONE: content is &[u8], must own it for spawn_blocking.
                let owned_content = content.to_vec();
                async move {
                    // The hash + parse_vue_multizone CPU work goes to blocking pool.
                    let (multi, content_hash) = tokio::task::spawn_blocking({
                        let path = path_owned.clone();
                        move || {
                            let content_hash = VersionHash::compute(&owned_content);
                            let multi = parse_vue_multizone(&owned_content).map_err(|e| {
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
                        path: path_owned.clone(),
                        reason: format!("spawn_blocking panicked: {join_err}"),
                    })??;

                    let cached_multi = Arc::new(MultiZoneTree {
                        script_tree: multi.script_tree.clone(),
                        template_tree: multi.template_tree.clone(),
                        style_tree: multi.style_tree.clone(),
                        zones: multi.zones.clone(),
                        source: multi.source.clone(),
                        degraded: multi.degraded,
                    });

                    // Fast cache insertion back on async worker (nanoseconds)
                    self.vue_entries.lock().put(
                        path_owned.clone(),
                        MultiZoneEntry {
                            multi: cached_multi,
                            content_hash: content_hash.clone(),
                            mtime,
                        },
                    );

                    Ok::<_, SurgeonError>((multi, content_hash))
                }
            })
            .await;

        {
            let mut vue_in_flight = self.vue_in_flight.lock();
            vue_in_flight.remove(path);
        }

        match result.as_ref() {
            Ok((multi, hash)) => Ok((multi.clone(), hash.clone())),
            Err(SurgeonError::FileNotFound(p)) => Err(SurgeonError::FileNotFound(p.clone())),
            Err(SurgeonError::UnsupportedLanguage(p)) => {
                Err(SurgeonError::UnsupportedLanguage(p.clone()))
            }
            Err(SurgeonError::ParseError { path: p, reason }) => Err(SurgeonError::ParseError {
                path: p.clone(),
                reason: reason.clone(),
            }),
            Err(SurgeonError::SymbolNotFound {
                path: p,
                did_you_mean: dym,
            }) => Err(SurgeonError::SymbolNotFound {
                path: p.clone(),
                did_you_mean: dym.clone(),
            }),
            Err(SurgeonError::Io(e)) => Err(SurgeonError::ParseError {
                path: path.to_path_buf(),
                reason: e.to_string(),
            }),
        }
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
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::{tempdir, NamedTempFile};

    #[tokio::test]
    async fn test_cache_hits_and_misses() {
        let cache = AstCache::new(2);

        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "package main\nfunc A() {{}}").unwrap();
        let path = file.path().to_path_buf();

        // 1. Initial parse (miss)
        let (tree1, src1) = cache
            .get_or_parse(&path, SupportedLanguage::Go)
            .await
            .unwrap();
        assert_eq!(src1.len(), 25);

        // 2. Second access — fast path via mtime: no disk read
        let (tree2, src2) = cache
            .get_or_parse(&path, SupportedLanguage::Go)
            .await
            .unwrap();
        // Source length must be identical (same bytes returned from cache)
        assert_eq!(src2.len(), 25);
        // Tree structure must match
        assert_eq!(
            tree1.root_node().child_count(),
            tree2.root_node().child_count()
        );
        // Fast path: cached mtime must equal file mtime
        {
            let lock = cache.entries.lock();
            let entry = lock.peek(&path).unwrap();
            let meta = std::fs::metadata(&path).unwrap();
            assert_eq!(
                entry.mtime,
                meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)
            );
        }

        // 3. Modify file — mtime changes → slow path (re-parse)
        // Small sleep to guarantee mtime changes on filesystems with 1s granularity.
        std::thread::sleep(std::time::Duration::from_millis(10));
        writeln!(file, "func B() {{}}").unwrap();
        let (_tree3, src3) = cache
            .get_or_parse(&path, SupportedLanguage::Go)
            .await
            .unwrap();
        assert!(src3.len() > 25); // Re-parsed new content
    }

    #[tokio::test]
    async fn test_cache_eviction_lru() {
        let cache = AstCache::new(2);

        let mut f1 = NamedTempFile::new().unwrap();
        writeln!(f1, "func A() {{}}").unwrap();
        let mut f2 = NamedTempFile::new().unwrap();
        writeln!(f2, "func B() {{}}").unwrap();
        let mut f3 = NamedTempFile::new().unwrap();
        writeln!(f3, "func C() {{}}").unwrap();

        // Load F1 and F2
        cache
            .get_or_parse(f1.path(), SupportedLanguage::Go)
            .await
            .unwrap();
        // slight sleep to ensure timestamps are different
        std::thread::sleep(std::time::Duration::from_millis(10));
        cache
            .get_or_parse(f2.path(), SupportedLanguage::Go)
            .await
            .unwrap();

        {
            let lock = cache.entries.lock();
            assert_eq!(lock.len(), 2);
            assert!(lock.contains(f1.path()));
            assert!(lock.contains(f2.path()));
        }

        // Access F1 again, making F2 the LRU
        std::thread::sleep(std::time::Duration::from_millis(10));
        cache
            .get_or_parse(f1.path(), SupportedLanguage::Go)
            .await
            .unwrap();

        // Load F3. Should evict F2.
        cache
            .get_or_parse(f3.path(), SupportedLanguage::Go)
            .await
            .unwrap();

        {
            let lock = cache.entries.lock();
            assert_eq!(lock.len(), 2);
            assert!(lock.contains(f1.path()));
            assert!(!lock.contains(f2.path())); // F2 evicted
            assert!(lock.contains(f3.path()));
        }
    }

    #[tokio::test]
    async fn test_cache_invalidation() {
        let cache = AstCache::new(2);
        let mut f1 = NamedTempFile::new().unwrap();
        writeln!(f1, "func A() {{}}").unwrap();

        cache
            .get_or_parse(f1.path(), SupportedLanguage::Go)
            .await
            .unwrap();
        assert_eq!(cache.entries.lock().len(), 1);

        cache.invalidate(f1.path());
        assert_eq!(cache.entries.lock().len(), 0);
    }

    #[tokio::test]
    async fn test_vue_cache_hits_and_misses() {
        let cache = AstCache::new(2);

        let sfc = b"<template>\n<div>Hello</div>\n</template>\n<script setup lang=\"ts\">\nconst x = 1\n</script>\n";
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(sfc).unwrap();

        // First call — cache miss → full parse
        let (multi1, hash1) = cache.get_or_parse_vue(file.path()).await.unwrap();
        assert!(multi1.script_tree.is_some());
        assert!(multi1.template_tree.is_some());
        assert!(!multi1.degraded);

        // Second call — cache hit (mtime unchanged)
        let (_multi2, hash2) = cache.get_or_parse_vue(file.path()).await.unwrap();
        assert_eq!(hash1, hash2, "hash must be stable across cache hits");

        {
            let lock = cache.vue_entries.lock();
            assert_eq!(lock.len(), 1, "exactly one Vue entry cached");
        }
    }

    #[tokio::test]
    async fn test_vue_cache_invalidation_clears_both_caches() {
        let cache = AstCache::new(2);

        // Populate single-zone cache via Go parse
        let mut go_file = NamedTempFile::new().unwrap();
        writeln!(go_file, "func A() {{}}").unwrap();
        cache
            .get_or_parse(go_file.path(), SupportedLanguage::Go)
            .await
            .unwrap();

        // Populate Vue cache
        let sfc = b"<template><div/></template>\n";
        let mut vue_file = NamedTempFile::new().unwrap();
        vue_file.write_all(sfc).unwrap();
        cache.get_or_parse_vue(vue_file.path()).await.unwrap();

        assert_eq!(cache.entries.lock().len(), 1);
        assert_eq!(cache.vue_entries.lock().len(), 1);

        // Invalidate the Vue file — should clear from vue_entries
        // (and defensively from entries too, even though it's not there)
        cache.invalidate(vue_file.path());

        assert_eq!(cache.vue_entries.lock().len(), 0, "Vue entry cleared");
        // Non-Vue entry must not be disturbed
        assert_eq!(cache.entries.lock().len(), 1, "Go entry untouched");
    }

    /// Test that singleflight prevents redundant parsing when multiple
    /// concurrent requests target the same uncached file.
    #[tokio::test]
    async fn test_singleflight_prevents_redundant_parsing() {
        let cache = AstCache::new(10);

        // Create a file that takes some time to read/parse
        let mut file = NamedTempFile::new().unwrap();
        let content = "package main\n".repeat(1000); // Large enough to take some time
        file.write_all(content.as_bytes()).unwrap();
        let path = Arc::new(file.path().to_path_buf());

        // Launch multiple concurrent requests for the same uncached file
        let cache = Arc::new(cache);
        let handles = (0..5).map(|_| {
            let cache = Arc::clone(&cache);
            let path = Arc::clone(&path);
            tokio::spawn(async move { cache.get_or_parse(&path, SupportedLanguage::Go).await })
        });

        // All requests should complete successfully
        let results: Vec<_> = futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| r.unwrap().unwrap())
            .collect();

        // All results should be identical (same tree and source)
        for result in &results[1..] {
            assert_eq!(
                results[0].0.root_node().child_count(),
                result.0.root_node().child_count()
            );
            assert_eq!(results[0].1.len(), result.1.len());
        }

        // Only one entry should be in the cache (not 5)
        assert_eq!(cache.entries.lock().len(), 1);
    }

    /// Stress test: verify singleflight + `spawn_blocking` work correctly together
    /// under high concurrency (10+ concurrent tasks). THR-001-A regression guard.
    ///
    /// This tests the exact scenario that caused MCP timeouts: concurrent requests
    /// triggering tree-sitter parses that now run on the blocking pool via `spawn_blocking`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_concurrent_spawn_blocking_stress() {
        let cache = Arc::new(AstCache::new(100));

        // Create multiple test files - keep NamedTempFile in scope to prevent deletion
        let mut temp_files = Vec::with_capacity(5);
        let mut file_paths = Vec::with_capacity(5);
        for i in 0..5 {
            let mut file = NamedTempFile::new().unwrap();
            let content = format!("package main\n\nfunc Func{i}() int {{ return {i}; }}\n");
            file.write_all(content.as_bytes()).unwrap();
            file_paths.push(Arc::new(file.path().to_path_buf()));
            temp_files.push(file);
        }

        // Spawn 10 concurrent tasks: each task accesses all 5 files
        // This creates contention: same files requested simultaneously
        let mut handles = Vec::with_capacity(10);
        for task_id in 0..10 {
            let cache = Arc::clone(&cache);
            let files = file_paths.clone();
            handles.push(tokio::spawn(async move {
                // Round-robin through files to create overlapping access patterns
                for i in 0..5 {
                    let file_idx = (task_id + i) % 5;
                    let result = cache
                        .get_or_parse(&files[file_idx], SupportedLanguage::Go)
                        .await;
                    assert!(result.is_ok(), "task {task_id} file {file_idx} failed");
                    let (_tree, source) = result.unwrap();
                    assert!(!source.is_empty(), "task {task_id} got empty source");
                }
            }));
        }

        // All tasks must complete successfully
        let results = futures::future::join_all(handles).await;
        for result in results {
            assert!(result.is_ok(), "spawned task panicked");
        }

        // Only 5 entries should be in cache (one per unique file, not 50)
        assert_eq!(
            cache.entries.lock().len(),
            5,
            "singleflight should have prevented redundant parses"
        );

        // Keep temp_files alive until end of test
        drop(temp_files);
    }

    /// Vue stress test: verify singleflight + `spawn_blocking` work correctly
    /// for Vue SFC multi-zone parsing under high concurrency. THR-001-A regression guard.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_concurrent_spawn_blocking_vue_stress() {
        let cache = Arc::new(AstCache::new(100));

        // Create multiple Vue SFC files - keep them in scope to prevent deletion
        let mut temp_files = Vec::with_capacity(3);
        let mut file_paths = Vec::with_capacity(3);
        for i in 0..3 {
            let mut file = NamedTempFile::new().unwrap();
            let sfc = format!(
                "<template>\n  <div>Comp{i}</div>\n</template>\n<script setup lang=\"ts\">\nconst count{i} = ref(0);\n</script>\n<style scoped>\n.comp{i} {{ color: red; }}\n</style>\n"
            );
            file.write_all(sfc.as_bytes()).unwrap();
            file_paths.push(Arc::new(file.path().to_path_buf()));
            temp_files.push(file);
        }

        // Spawn 10 concurrent tasks: each task accesses all 3 Vue files
        let mut handles = Vec::with_capacity(10);
        for task_id in 0..10 {
            let cache = Arc::clone(&cache);
            let files = file_paths.clone();
            handles.push(tokio::spawn(async move {
                for i in 0..3 {
                    let file_idx = (task_id + i) % 3;
                    let result = cache.get_or_parse_vue(&files[file_idx]).await;
                    assert!(result.is_ok(), "task {task_id} vue file {file_idx} failed");
                    let (multi, _hash) = result.unwrap();
                    assert!(multi.script_tree.is_some(), "script tree missing");
                }
            }));
        }

        // All tasks must complete successfully
        let results = futures::future::join_all(handles).await;
        for result in results {
            assert!(result.is_ok(), "spawned task panicked");
        }

        // Only 3 entries should be in Vue cache (one per unique file, not 30)
        assert_eq!(
            cache.vue_entries.lock().len(),
            3,
            "singleflight should have prevented redundant Vue parses"
        );

        // Keep temp_files alive until end of test
        drop(temp_files);
    }

    /// Test that singleflight works correctly for Vue SFC parsing as well.
    #[tokio::test]
    async fn test_singleflight_vue() {
        let cache = AstCache::new(10);

        let sfc = b"<template><div/></template>\n<script>const x = 1;</script>\n";
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(sfc).unwrap();
        let path = Arc::new(file.path().to_path_buf());

        // Launch multiple concurrent requests for the same Vue file
        let cache = Arc::new(cache);
        let handles = (0..3).map(|_| {
            let cache = Arc::clone(&cache);
            let path = Arc::clone(&path);
            tokio::spawn(async move { cache.get_or_parse_vue(&path).await })
        });

        let results: Vec<_> = futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| r.unwrap().unwrap())
            .collect();

        // All results should have the same hash
        for result in &results[1..] {
            assert_eq!(results[0].1, result.1);
        }

        // Only one entry should be in the Vue cache
        assert_eq!(cache.vue_entries.lock().len(), 1);
    }

    // ── FileNotFound propagation tests ───────────────────────────────────────

    /// `get_or_parse` must return `SurgeonError::FileNotFound` when the path
    /// does not exist, not a generic `Io` or `ParseError`.
    ///
    /// This ensures the MCP layer surfaces `INVALID_PARAMS / FILE_NOT_FOUND`
    /// (-32602) instead of the misleading `INTERNAL_ERROR` (-32603).
    #[tokio::test]
    async fn test_get_or_parse_missing_file_returns_file_not_found() {
        let cache = AstCache::new(2);
        let temp_dir = tempdir().unwrap();
        let missing = temp_dir
            .path()
            .join("this_file_does_not_exist_pathfinder.go");

        let err = cache
            .get_or_parse(&missing, SupportedLanguage::Go)
            .await
            .unwrap_err();

        assert!(
            matches!(err, SurgeonError::FileNotFound(_)),
            "expected FileNotFound, got: {err:?}"
        );
    }

    /// `get_or_parse_vue` must return `SurgeonError::FileNotFound` when the
    /// Vue SFC path does not exist.
    #[tokio::test]
    async fn test_get_or_parse_vue_missing_file_returns_file_not_found() {
        let cache = AstCache::new(2);
        let dir = tempdir().unwrap();
        let missing = dir.path().join("this_file_does_not_exist_pathfinder.vue");

        let err = cache.get_or_parse_vue(&missing).await.unwrap_err();

        assert!(
            matches!(err, SurgeonError::FileNotFound(_)),
            "expected FileNotFound, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_vue_cache_hit_when_mtime_changes_but_content_unchanged() {
        let cache = AstCache::new(2);

        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "<template><div>Hello</div></template>").unwrap();
        writeln!(file, "<script>const a = 1;</script>").unwrap();
        let path = file.path().to_path_buf();

        let (multi1, hash1) = cache.get_or_parse_vue(&path).await.unwrap();
        let original_script_has_tree = multi1.script_tree.is_some();

        let original_content_hash = {
            let lock = cache.vue_entries.lock();
            let entry = lock.peek(&path).unwrap();
            entry.content_hash.clone()
        };

        std::thread::sleep(std::time::Duration::from_millis(10));
        file.as_file_mut()
            .set_modified(std::time::SystemTime::now())
            .unwrap();

        let (multi2, hash2) = cache.get_or_parse_vue(&path).await.unwrap();

        assert_eq!(
            hash1, hash2,
            "hash should match — cache hit via content hash"
        );
        assert_eq!(
            multi2.script_tree.is_some(),
            original_script_has_tree,
            "tree presence should match — no re-parse"
        );

        let updated_content_hash = {
            let lock = cache.vue_entries.lock();
            let entry = lock.peek(&path).unwrap();
            entry.content_hash.clone()
        };
        assert_eq!(
            original_content_hash, updated_content_hash,
            "content hash should be unchanged"
        );

        let meta = std::fs::metadata(&path).unwrap();
        {
            let lock = cache.vue_entries.lock();
            let entry = lock.peek(&path).unwrap();
            assert_eq!(
                entry.mtime,
                meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                "stored mtime should be updated to current file mtime"
            );
        }
    }

    #[tokio::test]
    async fn test_vue_cache_miss_when_content_changes() {
        let cache = AstCache::new(2);

        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "<template><div>Hello</div></template>").unwrap();
        writeln!(file, "<script>const a = 1;</script>").unwrap();
        let path = file.path().to_path_buf();

        let (_multi1, hash1) = cache.get_or_parse_vue(&path).await.unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        writeln!(file, "<script>const b = 2;</script>").unwrap();

        let (_multi2, hash2) = cache.get_or_parse_vue(&path).await.unwrap();

        assert_ne!(
            hash1, hash2,
            "hash should differ — cache miss with content change"
        );
    }

    #[tokio::test]
    async fn test_cache_hit_when_mtime_changes_but_content_unchanged() {
        let cache = AstCache::new(2);

        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "package main\nfunc A() {{}}").unwrap();
        let path = file.path().to_path_buf();

        let (tree1, src1) = cache
            .get_or_parse(&path, SupportedLanguage::Go)
            .await
            .unwrap();
        let original_node_count = tree1.root_node().child_count();

        let original_content_hash = {
            let lock = cache.entries.lock();
            let entry = lock.peek(&path).unwrap();
            entry.content_hash.clone()
        };

        std::thread::sleep(std::time::Duration::from_millis(10));
        file.as_file_mut()
            .set_modified(std::time::SystemTime::now())
            .unwrap();

        let (tree2, src2) = cache
            .get_or_parse(&path, SupportedLanguage::Go)
            .await
            .unwrap();

        assert_eq!(
            src1.len(),
            src2.len(),
            "source length should match — cache hit via content hash"
        );
        assert_eq!(
            tree2.root_node().child_count(),
            original_node_count,
            "tree structure should match — no re-parse"
        );

        let updated_content_hash = {
            let lock = cache.entries.lock();
            let entry = lock.peek(&path).unwrap();
            entry.content_hash.clone()
        };
        assert_eq!(
            original_content_hash, updated_content_hash,
            "content hash should be unchanged"
        );

        let meta = std::fs::metadata(&path).unwrap();
        {
            let lock = cache.entries.lock();
            let entry = lock.peek(&path).unwrap();
            assert_eq!(
                entry.mtime,
                meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                "stored mtime should be updated to current file mtime"
            );
        }
    }

    #[tokio::test]
    async fn test_cache_miss_when_content_changes() {
        let cache = AstCache::new(2);

        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "package main\nfunc A() {{}}").unwrap();
        let path = file.path().to_path_buf();

        let (_tree1, src1) = cache
            .get_or_parse(&path, SupportedLanguage::Go)
            .await
            .unwrap();
        let original_len = src1.len();

        std::thread::sleep(std::time::Duration::from_millis(10));
        writeln!(file, "func B() {{}}").unwrap();

        let (_tree2, src2) = cache
            .get_or_parse(&path, SupportedLanguage::Go)
            .await
            .unwrap();

        assert!(
            src2.len() > original_len,
            "source should be re-parsed with new content: got {} vs original {}",
            src2.len(),
            original_len
        );
    }
}
