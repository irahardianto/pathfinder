use crate::error::SurgeonError;
use crate::language::SupportedLanguage;
use crate::parser::AstParser;
use lru::LruCache;
use pathfinder_common::types::VersionHash;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;
use tracing::instrument;
use tree_sitter::Tree;

/// Contains the cached parsing result for a file.
#[derive(Debug)]
pub struct CacheEntry {
    /// The parsed AST tree.
    pub tree: Tree,
    /// The raw source code bytes.
    pub source: Vec<u8>,
    /// The content hash when parsed.
    pub content_hash: VersionHash,
    /// The language used for parsing.
    pub lang: SupportedLanguage,
    /// Last modification time of the file at the time of parsing (fast-path invalidation).
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
/// ## Concurrency note
///
/// // NOTE: Concurrent requests for the same file may race through the slow path
/// // simultaneously, resulting in redundant parsing work. For v1 (local MCP
/// // server, low concurrency) this is acceptable. A singleflight /
/// // `tokio::sync::OnceCell` approach would eliminate it if contention becomes
/// // measurable.
#[derive(Debug)]
pub struct AstCache {
    entries: Mutex<LruCache<PathBuf, CacheEntry>>,
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
    ) -> Result<(Tree, Vec<u8>), SurgeonError> {
        // --- Fast-path guard: single stat syscall ---
        let meta = tokio::fs::metadata(path).await?;
        let current_mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);

        // Check cache while holding the lock; release before any async I/O.
        {
            let mut lock = self.entries.lock().map_err(|_| SurgeonError::ParseError {
                path: path.to_path_buf(),
                reason: "Lock poisoned".into(),
            })?;

            if let Some(entry) = lock.get(path) {
                if entry.mtime == current_mtime && entry.lang == lang {
                    tracing::Span::current().record("cache_hit", true);
                    return Ok((entry.tree.clone(), entry.source.clone()));
                }
            }
        } // lock released here — safe to await below

        // --- Slow path: full read + hash + parse ---
        let content = tokio::fs::read(path).await?;
        let current_hash = VersionHash::compute(&content);
        // For Vue SFCs, preprocess extracts the <script> block before parsing.
        // The original `content` is kept for version hashing and OCC checks —
        // only the input to the AST parser uses the processed bytes.
        let parse_input = lang.preprocess_source(&content);
        let tree = AstParser::parse_source(path, lang, &parse_input)?;

        // Re-acquire the lock to insert/update.
        let mut lock = self.entries.lock().map_err(|_| SurgeonError::ParseError {
            path: path.to_path_buf(),
            reason: "Lock poisoned".into(),
        })?;

        // LruCache automatically evicts the least recently used item if capacity is reached
        lock.put(
            path.to_path_buf(),
            CacheEntry {
                tree: tree.clone(),
                source: content.clone(),
                content_hash: current_hash,
                lang,
                mtime: current_mtime,
            },
        );

        Ok((tree, content))
    }

    /// Remove a file from the cache, forcing a re-parse on next access.
    pub fn invalidate(&self, path: &Path) {
        if let Ok(mut lock) = self.entries.lock() {
            lock.pop(path);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

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
            let lock = cache.entries.lock().unwrap();
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
            let lock = cache.entries.lock().unwrap();
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
            let lock = cache.entries.lock().unwrap();
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
        assert_eq!(cache.entries.lock().unwrap().len(), 1);

        cache.invalidate(f1.path());
        assert_eq!(cache.entries.lock().unwrap().len(), 0);
    }
}
