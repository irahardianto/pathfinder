use crate::error::SurgeonError;
use crate::language::SupportedLanguage;
use crate::parser::AstParser;
use crate::surgeon::ExtractedSymbol;
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

/// Maximum allowed file size for AST parsing (2 MB).
pub const MAX_FILE_SIZE_BYTES: u64 = 2 * 1024 * 1024;

/// Default byte budget for the AST cache (64 MB).
pub const DEFAULT_MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// Type alias for cached single-zone AST parse results.
pub type CachedParseResult = (Tree, Arc<[u8]>, Arc<Vec<ExtractedSymbol>>);

/// Type alias for in-flight Parse operations.
type InFlightParse = Arc<OnceCell<Result<CachedParseResult, SurgeonError>>>;

/// Type alias for in-flight Vue parse operations.
type InFlightVueParse =
    Arc<OnceCell<Result<(MultiZoneTree, Arc<Vec<ExtractedSymbol>>, VersionHash), SurgeonError>>>;

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
    /// Extracted symbols cached alongside the tree (TS-A13).
    pub symbols: Arc<Vec<ExtractedSymbol>>,
    /// The content hash when parsed.
    pub content_hash: VersionHash,
    /// The language used for parsing.
    pub lang: SupportedLanguage,
    /// Last modification time of the file at the time of parsing.
    pub mtime: SystemTime,
    /// File size in bytes when parsed (checked alongside mtime).
    pub file_size: u64,
}

/// Cached parse result for a Vue SFC across all three zones.
#[derive(Debug)]
pub struct MultiZoneEntry {
    /// The parsed Vue multi-zone tree structure.
    pub multi: Arc<MultiZoneTree>,
    /// Extracted symbols cached alongside the multi-zone tree (TS-A13).
    pub symbols: Arc<Vec<ExtractedSymbol>>,
    /// The SHA-256 fingerprint hash of the cached file content.
    pub content_hash: VersionHash,
    /// Last modified time of the file when cached.
    pub mtime: SystemTime,
    /// File size in bytes when parsed.
    pub file_size: u64,
}

/// Cached negative parse result (e.g. file size exceeded or unparseable file).
#[derive(Debug, Clone)]
pub struct NegativeCacheEntry {
    /// Last modification time when the failure was cached.
    pub mtime: SystemTime,
    /// File size in bytes when the failure was cached.
    pub file_size: u64,
    /// The cached error.
    pub error: SurgeonError,
}

/// Internal wrapper tracking LRU cache and its current memory usage in bytes.
#[derive(Debug)]
pub(crate) struct BoundedLruCache<V> {
    pub(crate) cache: LruCache<PathBuf, V>,
    pub(crate) current_bytes: usize,
    pub(crate) max_bytes: usize,
}

impl<V> BoundedLruCache<V> {
    fn new(cap: NonZeroUsize, max_bytes: usize) -> Self {
        Self {
            cache: LruCache::new(cap),
            current_bytes: 0,
            max_bytes,
        }
    }
}

impl<V> std::ops::Deref for BoundedLruCache<V> {
    type Target = LruCache<PathBuf, V>;
    fn deref(&self) -> &Self::Target {
        &self.cache
    }
}

impl<V> std::ops::DerefMut for BoundedLruCache<V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.cache
    }
}

/// A thread-safe, parse-on-demand cache for ASTs and extracted symbols (TS-A13).
#[derive(Debug)]
pub struct AstCache {
    pub(crate) entries: Mutex<BoundedLruCache<CacheEntry>>,
    /// Separate LRU cache for Vue SFC multi-zone parse results.
    pub(crate) vue_entries: Mutex<BoundedLruCache<MultiZoneEntry>>,
    /// Negative cache for failed parses and oversized files.
    pub(crate) negative_entries: Mutex<LruCache<PathBuf, NegativeCacheEntry>>,
    /// Singleflight locks for in-flight parses to prevent redundant work.
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

    result.as_ref().cloned().map_err(Clone::clone)
}

impl AstCache {
    /// Create a new AST cache with the given capacity.
    #[must_use]
    pub fn new(max_entries: usize) -> Self {
        let cap = NonZeroUsize::new(max_entries.max(1)).unwrap_or(NonZeroUsize::MIN);
        Self {
            entries: Mutex::new(BoundedLruCache::new(cap, DEFAULT_MAX_CACHE_BYTES)),
            vue_entries: Mutex::new(BoundedLruCache::new(cap, DEFAULT_MAX_CACHE_BYTES)),
            negative_entries: Mutex::new(LruCache::new(cap)),
            in_flight: Mutex::new(HashMap::default()),
            vue_in_flight: Mutex::new(HashMap::default()),
        }
    }

    /// Retrieve the tree, source code, and symbols for a file, parsing from disk on a cache miss.
    ///
    /// # Errors
    /// Returns `SurgeonError` if I/O fails, file exceeds size limit, or parsing fails.
    #[instrument(skip(self), fields(cache_hit = false))]
    pub async fn get_or_parse(
        &self,
        path: &Path,
        lang: SupportedLanguage,
    ) -> Result<CachedParseResult, SurgeonError> {
        let meta = tokio::fs::metadata(path)
            .await
            .map_err(|e| io_err(e, path))?;
        let current_mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let current_size = meta.len();

        if current_size > MAX_FILE_SIZE_BYTES {
            let err = SurgeonError::ParseError {
                path: path.to_path_buf(),
                reason: format!(
                    "File size ({current_size} bytes) exceeds maximum supported limit ({MAX_FILE_SIZE_BYTES} bytes)"
                ),
            };
            self.negative_entries.lock().put(
                path.to_path_buf(),
                NegativeCacheEntry {
                    mtime: current_mtime,
                    file_size: current_size,
                    error: err.clone(),
                },
            );
            return Err(err);
        }

        self.get_or_parse_single_zone_impl(
            path,
            lang,
            current_mtime,
            current_size,
            ContentSource::Disk,
        )
        .await
    }

    /// Retrieve the tree, source code, and symbols using pre-loaded content and mtime.
    ///
    /// # Errors
    /// Returns [`SurgeonError::ParseError`] if the file size exceeds the limit or parsing fails.
    #[instrument(skip(self, content), fields(cache_hit = false))]
    pub async fn get_or_parse_preloaded(
        &self,
        path: &Path,
        lang: SupportedLanguage,
        content: Arc<[u8]>,
        mtime: SystemTime,
    ) -> Result<CachedParseResult, SurgeonError> {
        let size = content.len() as u64;
        if size > MAX_FILE_SIZE_BYTES {
            return Err(SurgeonError::ParseError {
                path: path.to_path_buf(),
                reason: format!(
                    "File size ({size} bytes) exceeds maximum supported limit ({MAX_FILE_SIZE_BYTES} bytes)"
                ),
            });
        }
        self.get_or_parse_single_zone_impl(
            path,
            lang,
            mtime,
            size,
            ContentSource::Preloaded(content),
        )
        .await
    }

    /// Retrieve the multi-zone parse result and symbols for a Vue SFC.
    ///
    /// # Errors
    /// Returns [`SurgeonError`] if metadata lookup, file I/O, or parsing fails.
    #[instrument(skip(self), fields(cache_hit = false))]
    pub async fn get_or_parse_vue(
        &self,
        path: &Path,
    ) -> Result<(MultiZoneTree, Arc<Vec<ExtractedSymbol>>, VersionHash), SurgeonError> {
        let meta = tokio::fs::metadata(path)
            .await
            .map_err(|e| io_err(e, path))?;
        let current_mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let current_size = meta.len();

        if current_size > MAX_FILE_SIZE_BYTES {
            let err = SurgeonError::ParseError {
                path: path.to_path_buf(),
                reason: format!(
                    "File size ({current_size} bytes) exceeds maximum supported limit ({MAX_FILE_SIZE_BYTES} bytes)"
                ),
            };
            self.record_negative_cache(path, current_mtime, current_size, err.clone());
            return Err(err);
        }

        self.get_or_parse_vue_impl(path, current_mtime, current_size, ContentSource::Disk)
            .await
    }

    /// Retrieve the multi-zone parse result and symbols for a Vue SFC using pre-loaded content.
    ///
    /// # Errors
    /// Returns [`SurgeonError::ParseError`] if the file size exceeds the limit or parsing fails.
    #[instrument(skip(self, content), fields(cache_hit = false))]
    pub async fn get_or_parse_vue_preloaded(
        &self,
        path: &Path,
        content: &[u8],
        mtime: SystemTime,
    ) -> Result<(MultiZoneTree, Arc<Vec<ExtractedSymbol>>, VersionHash), SurgeonError> {
        let size = content.len() as u64;
        if size > MAX_FILE_SIZE_BYTES {
            return Err(SurgeonError::ParseError {
                path: path.to_path_buf(),
                reason: format!(
                    "File size ({size} bytes) exceeds maximum supported limit ({MAX_FILE_SIZE_BYTES} bytes)"
                ),
            });
        }
        let content_arc = Arc::from(content);
        self.get_or_parse_vue_impl(path, mtime, size, ContentSource::Preloaded(content_arc))
            .await
    }

    fn check_negative_cache(
        &self,
        path: &Path,
        current_mtime: SystemTime,
        current_size: u64,
    ) -> Result<(), SurgeonError> {
        let mut neg = self.negative_entries.lock();
        if let Some(entry) = neg.get(path) {
            if entry.mtime == current_mtime && entry.file_size == current_size {
                return Err(entry.error.clone());
            }
        }
        Ok(())
    }

    fn record_negative_cache(
        &self,
        path: &Path,
        current_mtime: SystemTime,
        current_size: u64,
        error: SurgeonError,
    ) {
        self.negative_entries.lock().put(
            path.to_path_buf(),
            NegativeCacheEntry {
                mtime: current_mtime,
                file_size: current_size,
                error,
            },
        );
    }

    fn insert_single_zone_entry(&self, path: PathBuf, entry: CacheEntry) {
        let entry_bytes = entry.source.len() + 1024;
        let mut lock = self.entries.lock();
        while lock.current_bytes + entry_bytes > lock.max_bytes && !lock.cache.is_empty() {
            if let Some((_, evicted)) = lock.cache.pop_lru() {
                lock.current_bytes = lock
                    .current_bytes
                    .saturating_sub(evicted.source.len() + 1024);
            }
        }
        if let Some(old) = lock.cache.put(path, entry) {
            lock.current_bytes = lock.current_bytes.saturating_sub(old.source.len() + 1024);
        }
        lock.current_bytes += entry_bytes;
    }

    fn insert_vue_entry(&self, path: PathBuf, entry: MultiZoneEntry) {
        let entry_bytes = entry.multi.source.len() + 2048;
        let mut lock = self.vue_entries.lock();
        while lock.current_bytes + entry_bytes > lock.max_bytes && !lock.cache.is_empty() {
            if let Some((_, evicted)) = lock.cache.pop_lru() {
                lock.current_bytes = lock
                    .current_bytes
                    .saturating_sub(evicted.multi.source.len() + 2048);
            }
        }
        if let Some(old) = lock.cache.put(path, entry) {
            lock.current_bytes = lock
                .current_bytes
                .saturating_sub(old.multi.source.len() + 2048);
        }
        lock.current_bytes += entry_bytes;
    }

    async fn get_or_parse_single_zone_impl(
        &self,
        path: &Path,
        lang: SupportedLanguage,
        current_mtime: SystemTime,
        current_size: u64,
        content_source: ContentSource,
    ) -> Result<CachedParseResult, SurgeonError> {
        self.check_negative_cache(path, current_mtime, current_size)?;

        let needs_hash_check = {
            let mut lock = self.entries.lock();
            if let Some(entry) = lock.cache.get(path) {
                if entry.mtime == current_mtime
                    && entry.file_size == current_size
                    && entry.lang == lang
                {
                    tracing::Span::current().record("cache_hit", true);
                    return Ok((
                        entry.tree.clone(),
                        entry.source.clone(),
                        entry.symbols.clone(),
                    ));
                }
                entry.lang == lang
            } else {
                false
            }
        };

        if needs_hash_check {
            if let Some(res) = self
                .check_content_hash_match(path, lang, current_mtime, current_size, &content_source)
                .await?
            {
                return Ok(res);
            }
        }

        let res = get_or_parse_inner(&self.in_flight, path, || {
            let path_owned = path.to_path_buf();
            let source = match &content_source {
                ContentSource::Disk => None,
                ContentSource::Preloaded(c) => Some(Arc::clone(c)),
            };
            async move {
                let content_arc = if let Some(c) = source {
                    c
                } else {
                    let content = tokio::fs::read(&path_owned)
                        .await
                        .map_err(|e| io_err(e, &path_owned))?;
                    Arc::from(content)
                };

                let (tree, current_hash, content_arc, symbols_arc) = tokio::task::spawn_blocking({
                    let path = path_owned.clone();
                    let content = Arc::clone(&content_arc);
                    move || {
                        let current_hash = VersionHash::compute(&content);
                        let parse_input = lang.preprocess_source(&content);
                        let tree = AstParser::parse_source(&path, lang, &parse_input)?;
                        let symbols =
                            crate::symbols::extract_symbols_from_tree(&tree, &content, lang);
                        Ok::<_, SurgeonError>((tree, current_hash, content, Arc::new(symbols)))
                    }
                })
                .await
                .map_err(|join_err| SurgeonError::ParseError {
                    path: path_owned.clone(),
                    reason: format!("spawn_blocking panicked: {join_err}"),
                })??;

                self.insert_single_zone_entry(
                    path_owned,
                    CacheEntry {
                        tree: tree.clone(),
                        source: content_arc.clone(),
                        symbols: symbols_arc.clone(),
                        content_hash: current_hash,
                        lang,
                        mtime: current_mtime,
                        file_size: current_size,
                    },
                );

                Ok::<_, SurgeonError>((tree, content_arc, symbols_arc))
            }
        })
        .await;

        if let Err(ref err) = res {
            self.record_negative_cache(path, current_mtime, current_size, err.clone());
        }

        res
    }

    async fn check_content_hash_match(
        &self,
        path: &Path,
        lang: SupportedLanguage,
        current_mtime: SystemTime,
        current_size: u64,
        content_source: &ContentSource,
    ) -> Result<Option<CachedParseResult>, SurgeonError> {
        let content = match content_source {
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
        if let Some(entry) = lock.cache.get_mut(path) {
            if entry.content_hash == current_hash && entry.lang == lang {
                entry.mtime = current_mtime;
                entry.file_size = current_size;
                tracing::Span::current().record("cache_hit", true);
                return Ok(Some((
                    entry.tree.clone(),
                    entry.source.clone(),
                    entry.symbols.clone(),
                )));
            }
        }
        Ok(None)
    }

    async fn get_or_parse_vue_impl(
        &self,
        path: &Path,
        current_mtime: SystemTime,
        current_size: u64,
        content_source: ContentSource,
    ) -> Result<(MultiZoneTree, Arc<Vec<ExtractedSymbol>>, VersionHash), SurgeonError> {
        self.check_negative_cache(path, current_mtime, current_size)?;

        let needs_hash_check = {
            let mut lock = self.vue_entries.lock();
            if let Some(entry) = lock.cache.get(path) {
                if entry.mtime == current_mtime && entry.file_size == current_size {
                    tracing::Span::current().record("cache_hit", true);
                    let multi = (*entry.multi).clone();
                    return Ok((multi, entry.symbols.clone(), entry.content_hash.clone()));
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
            if let Some(entry) = lock.cache.get_mut(path) {
                if entry.content_hash == current_hash {
                    entry.mtime = current_mtime;
                    entry.file_size = current_size;
                    tracing::Span::current().record("cache_hit", true);
                    let multi = (*entry.multi).clone();
                    return Ok((multi, entry.symbols.clone(), entry.content_hash.clone()));
                }
            }
        }

        let res = get_or_parse_inner(&self.vue_in_flight, path, || {
            let path_owned = path.to_path_buf();
            let source = match &content_source {
                ContentSource::Disk => None,
                ContentSource::Preloaded(c) => Some(Arc::clone(c)),
            };
            async move {
                let content_arc = if let Some(c) = source {
                    c
                } else {
                    let content = tokio::fs::read(&path_owned)
                        .await
                        .map_err(|e| io_err(e, &path_owned))?;
                    Arc::from(content)
                };

                let (multi, content_hash, symbols_arc) = tokio::task::spawn_blocking({
                    let path = path_owned.clone();
                    let content = Arc::clone(&content_arc);
                    move || {
                        let content_hash = VersionHash::compute(&content);
                        let multi = parse_vue_multizone(&content).map_err(|e| {
                            SurgeonError::ParseError {
                                path,
                                reason: format!("Vue multi-zone parse failed: {e}"),
                            }
                        })?;
                        let symbols = crate::symbols::extract_symbols_from_multizone(&multi);
                        Ok::<_, SurgeonError>((multi, content_hash, Arc::new(symbols)))
                    }
                })
                .await
                .map_err(|join_err| SurgeonError::ParseError {
                    path: path_owned.clone(),
                    reason: format!("spawn_blocking panicked: {join_err}"),
                })??;

                let cached_multi = Arc::new(multi.clone());
                self.insert_vue_entry(
                    path_owned,
                    MultiZoneEntry {
                        multi: cached_multi,
                        symbols: symbols_arc.clone(),
                        content_hash: content_hash.clone(),
                        mtime: current_mtime,
                        file_size: current_size,
                    },
                );

                Ok::<_, SurgeonError>((multi, symbols_arc, content_hash))
            }
        })
        .await;

        if let Err(ref err) = res {
            self.record_negative_cache(path, current_mtime, current_size, err.clone());
        }

        res
    }

    /// Remove a file from the cache, forcing a re-parse on next access.
    pub fn invalidate(&self, path: &Path) {
        {
            let mut lock = self.entries.lock();
            if let Some(entry) = lock.cache.pop(path) {
                lock.current_bytes = lock.current_bytes.saturating_sub(entry.source.len() + 1024);
            }
        }
        {
            let mut lock = self.vue_entries.lock();
            if let Some(entry) = lock.cache.pop(path) {
                lock.current_bytes = lock
                    .current_bytes
                    .saturating_sub(entry.multi.source.len() + 2048);
            }
        }
        self.negative_entries.lock().pop(path);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "cache_test.rs"]
mod tests;
