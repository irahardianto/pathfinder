use crate::error::SurgeonError;
use crate::language::SupportedLanguage;
use crate::parser::AstParser;
use crate::vue_zones::{parse_vue_multizone, MultiZoneTree};
use lru::LruCache;
use pathfinder_common::types::VersionHash;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
    /// Multi-zone trees (script + template + style).
    pub multi: MultiZoneTree,
    /// SHA-256 hash of the *original* SFC content (for OCC).
    pub content_hash: VersionHash,
    /// Mtime at parse time (fast-path guard).
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
    /// Separate LRU cache for Vue SFC multi-zone parse results.
    vue_entries: Mutex<LruCache<PathBuf, MultiZoneEntry>>,
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
        let content_arc: Arc<[u8]> = Arc::from(content);
        // For Vue SFCs, preprocess extracts the <script> block before parsing.
        // The original `content` is kept for version hashing and OCC checks —
        // only the input to the AST parser uses the processed bytes.
        let parse_input = lang.preprocess_source(&content_arc);
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
                source: content_arc.clone(),
                content_hash: current_hash,
                lang,
                mtime: current_mtime,
            },
        );

        Ok((tree, content_arc))
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
        // --- Fast-path guard ---
        let meta = tokio::fs::metadata(path).await?;
        let current_mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);

        {
            let mut lock = self
                .vue_entries
                .lock()
                .map_err(|_| SurgeonError::ParseError {
                    path: path.to_path_buf(),
                    reason: "Vue cache lock poisoned".into(),
                })?;

            if let Some(entry) = lock.get(path) {
                if entry.mtime == current_mtime {
                    tracing::Span::current().record("cache_hit", true);
                    let multi = MultiZoneTree {
                        script_tree: entry.multi.script_tree.clone(),
                        template_tree: entry.multi.template_tree.clone(),
                        style_tree: entry.multi.style_tree.clone(),
                        zones: entry.multi.zones.clone(),
                        source: entry.multi.source.clone(),
                        degraded: entry.multi.degraded,
                    };
                    return Ok((multi, entry.content_hash.clone()));
                }
            }
        } // lock released

        // --- Slow path ---
        let content = tokio::fs::read(path).await?;
        let content_hash = VersionHash::compute(&content);
        let multi = parse_vue_multizone(&content).map_err(|e| SurgeonError::ParseError {
            path: path.to_path_buf(),
            reason: format!("Vue multi-zone parse failed: {e}"),
        })?;

        let mut lock = self
            .vue_entries
            .lock()
            .map_err(|_| SurgeonError::ParseError {
                path: path.to_path_buf(),
                reason: "Vue cache lock poisoned".into(),
            })?;

        let cached_multi = MultiZoneTree {
            script_tree: multi.script_tree.clone(),
            template_tree: multi.template_tree.clone(),
            style_tree: multi.style_tree.clone(),
            zones: multi.zones.clone(),
            source: multi.source.clone(),
            degraded: multi.degraded,
        };

        lock.put(
            path.to_path_buf(),
            MultiZoneEntry {
                multi: cached_multi,
                content_hash: content_hash.clone(),
                mtime: current_mtime,
            },
        );

        Ok((multi, content_hash))
    }

    /// Remove a file from the cache, forcing a re-parse on next access.
    ///
    /// Flushes the file from *both* single-zone and Vue multi-zone caches so
    /// that all paths are invalidated simultaneously.
    pub fn invalidate(&self, path: &Path) {
        if let Ok(mut lock) = self.entries.lock() {
            lock.pop(path);
        }
        if let Ok(mut lock) = self.vue_entries.lock() {
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
            let lock = cache.vue_entries.lock().unwrap();
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

        assert_eq!(cache.entries.lock().unwrap().len(), 1);
        assert_eq!(cache.vue_entries.lock().unwrap().len(), 1);

        // Invalidate the Vue file — should clear from vue_entries
        // (and defensively from entries too, even though it's not there)
        cache.invalidate(vue_file.path());

        assert_eq!(
            cache.vue_entries.lock().unwrap().len(),
            0,
            "Vue entry cleared"
        );
        // Non-Vue entry must not be disturbed
        assert_eq!(cache.entries.lock().unwrap().len(), 1, "Go entry untouched");
    }
}
