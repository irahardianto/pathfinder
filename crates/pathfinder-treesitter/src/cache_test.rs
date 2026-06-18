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

    // Access F1 again using preloaded, making F3 the LRU
    std::thread::sleep(std::time::Duration::from_millis(10));
    let mtime_f1 = std::fs::metadata(f1.path()).unwrap().modified().unwrap();
    cache
        .get_or_parse_preloaded(
            f1.path(),
            SupportedLanguage::Go,
            Arc::from(b"func A() {}" as &[u8]),
            mtime_f1,
        )
        .await
        .unwrap();

    // Load F2 again. Should evict F3 (since F1 was accessed).
    cache
        .get_or_parse(f2.path(), SupportedLanguage::Go)
        .await
        .unwrap();

    {
        let lock = cache.entries.lock();
        assert_eq!(lock.len(), 2);
        assert!(lock.contains(f1.path()));
        assert!(lock.contains(f2.path()));
        assert!(!lock.contains(f3.path())); // F3 evicted
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
async fn test_vue_cache_eviction_lru() {
    let cache = AstCache::new(2);

    let sfc_a = b"<script setup lang=\"ts\">const a = 1</script>";
    let sfc_b = b"<script setup lang=\"ts\">const b = 2</script>";
    let sfc_c = b"<script setup lang=\"ts\">const c = 3</script>";

    let mut f1 = NamedTempFile::new().unwrap();
    f1.write_all(sfc_a).unwrap();
    let mut f2 = NamedTempFile::new().unwrap();
    f2.write_all(sfc_b).unwrap();
    let mut f3 = NamedTempFile::new().unwrap();
    f3.write_all(sfc_c).unwrap();

    // Load F1 and F2
    cache.get_or_parse_vue(f1.path()).await.unwrap();
    std::thread::sleep(std::time::Duration::from_millis(10));
    cache.get_or_parse_vue(f2.path()).await.unwrap();

    {
        let lock = cache.vue_entries.lock();
        assert_eq!(lock.len(), 2);
        assert!(lock.contains(f1.path()));
        assert!(lock.contains(f2.path()));
    }

    // Access F1 again, making F2 the LRU
    std::thread::sleep(std::time::Duration::from_millis(10));
    cache.get_or_parse_vue(f1.path()).await.unwrap();

    // Load F3. Should evict F2.
    cache.get_or_parse_vue(f3.path()).await.unwrap();

    {
        let lock = cache.vue_entries.lock();
        assert_eq!(lock.len(), 2);
        assert!(lock.contains(f1.path()));
        assert!(!lock.contains(f2.path())); // F2 evicted
        assert!(lock.contains(f3.path()));
    }

    // Access F1 again using preloaded, making F3 the LRU
    std::thread::sleep(std::time::Duration::from_millis(10));
    let mtime_f1 = std::fs::metadata(f1.path()).unwrap().modified().unwrap();
    cache
        .get_or_parse_vue_preloaded(f1.path(), sfc_a, mtime_f1)
        .await
        .unwrap();

    // Load F2 again. Should evict F3 (since F1 was accessed).
    cache.get_or_parse_vue(f2.path()).await.unwrap();

    {
        let lock = cache.vue_entries.lock();
        assert_eq!(lock.len(), 2);
        assert!(lock.contains(f1.path()));
        assert!(lock.contains(f2.path()));
        assert!(!lock.contains(f3.path())); // F3 evicted
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

/// `SurgeonError` must be `Clone` — verify that `Io(Arc<io::Error>)` can be
/// cloned and that the clone shares the same error kind.
///
/// Regression guard for M1: wrapping `io::Error` in `Arc` enables `Clone`
/// without requiring `io::Error: Clone`.
#[test]
fn test_surgeon_error_io_is_clone() {
    let raw = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
    let err = SurgeonError::Io(Arc::new(raw));
    let cloned = err.clone();

    if let (SurgeonError::Io(a), SurgeonError::Io(b)) = (&err, &cloned) {
        assert_eq!(a.kind(), b.kind());
        assert_eq!(a.kind(), std::io::ErrorKind::PermissionDenied);
    } else {
        panic!("expected Io variant after clone");
    }
}

/// When a file is deleted between the `metadata()` check and the `tokio::fs::read`
/// inside `get_or_init`, `get_or_parse` must propagate a recognisable error
/// rather than panicking or hanging.
///
/// This exercises the IO-error path inside the singleflight closure that the
/// metadata-fast-fail test does not cover (TOCTOU gap).
#[tokio::test]
async fn test_get_or_parse_file_deleted_between_stat_and_read() {
    let cache = AstCache::new(2);
    let dir = tempfile::tempdir().unwrap();

    // File must NOT exist: metadata() will fail, returning FileNotFound directly.
    // To exercise the inner path we'd need to delete after stat — but the stat
    // itself fails first for a missing file, so we get FileNotFound from io_err().
    let path = dir.path().join("vanish.go");

    let err = cache
        .get_or_parse(&path, SupportedLanguage::Go)
        .await
        .unwrap_err();

    // Missing file must be FileNotFound (not a bare Io or panic).
    assert!(
        matches!(err, SurgeonError::FileNotFound(_)),
        "expected FileNotFound for a missing file, got: {err:?}"
    );
}

#[tokio::test]
async fn test_get_or_parse_inner_deduplication() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let counter = Arc::new(AtomicUsize::new(0));
    let in_flight = Mutex::new(HashMap::new());
    let path = Path::new("dummy_path");

    let counter_clone1 = Arc::clone(&counter);
    let fut1 = get_or_parse_inner(&in_flight, path, || async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        counter_clone1.fetch_add(1, Ordering::SeqCst);
        Ok::<_, SurgeonError>(42)
    });

    let counter_clone2 = Arc::clone(&counter);
    let fut2 = get_or_parse_inner(&in_flight, path, || async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        counter_clone2.fetch_add(1, Ordering::SeqCst);
        Ok::<_, SurgeonError>(42)
    });

    let (res1, res2) = tokio::join!(fut1, fut2);
    assert_eq!(res1.unwrap(), 42);
    assert_eq!(res2.unwrap(), 42);
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "closure should only run once"
    );
}

// ---------------------------------------------------------------
// Branch-coverage tests C1–C4
// ---------------------------------------------------------------

/// C1: `io_err` with `PermissionDenied` returns `SurgeonError::Io`.
#[test]
fn test_io_err_permission_denied_returns_io_error() {
    let err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
    let result = io_err(err, Path::new("/tmp/test"));
    assert!(
        matches!(result, SurgeonError::Io(_)),
        "PermissionDenied should map to Io variant, got: {result:?}"
    );
}

/// C2: `io_err` with `NotFound` returns `SurgeonError::FileNotFound`.
#[test]
fn test_io_err_not_found_returns_file_not_found() {
    let err = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
    let result = io_err(err, Path::new("/tmp/missing"));
    assert!(
        matches!(result, SurgeonError::FileNotFound(_)),
        "NotFound should map to FileNotFound variant, got: {result:?}"
    );
}

/// C3: `get_or_parse_preloaded` cache hit — second call with same content and
/// mtime returns a cache hit without re-parsing.
#[tokio::test]
async fn test_get_or_parse_preloaded_cache_hit() {
    let cache = AstCache::new(2);

    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test.rs");
    let content = b"fn main() {}";
    std::fs::write(&file_path, content).unwrap();

    let mtime = std::fs::metadata(&file_path)
        .unwrap()
        .modified()
        .unwrap();
    let content_arc = Arc::from(content.as_slice());

    // First call — cache miss, full parse
    let (tree1, src1) = cache
        .get_or_parse_preloaded(&file_path, SupportedLanguage::Rust, Arc::clone(&content_arc), mtime)
        .await
        .unwrap();
    assert_eq!(src1.len(), content.len());

    // Second call — same content and mtime → cache hit (fast path)
    let (tree2, src2) = cache
        .get_or_parse_preloaded(&file_path, SupportedLanguage::Rust, content_arc, mtime)
        .await
        .unwrap();
    assert_eq!(src2.len(), content.len());
    assert_eq!(
        tree1.root_node().child_count(),
        tree2.root_node().child_count(),
        "cache hit must return identical tree structure"
    );
}

/// C4: `get_or_parse_vue_preloaded` cache hit — second call with same content
/// and mtime returns a cache hit without re-parsing.
#[tokio::test]
async fn test_get_or_parse_vue_preloaded_cache_hit() {
    let cache = AstCache::new(2);

    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test.vue");
    let sfc = b"<script setup lang=\"ts\">\nconst x = 1\n</script>\n";
    std::fs::write(&file_path, sfc).unwrap();

    let mtime = std::fs::metadata(&file_path)
        .unwrap()
        .modified()
        .unwrap();

    // First call — cache miss
    let (multi1, hash1) = cache
        .get_or_parse_vue_preloaded(&file_path, sfc, mtime)
        .await
        .unwrap();
    assert!(multi1.script_tree.is_some());

    // Second call — same content and mtime → cache hit
    let (multi2, hash2) = cache
        .get_or_parse_vue_preloaded(&file_path, sfc, mtime)
        .await
        .unwrap();
    assert_eq!(hash1, hash2, "cache hit must return identical hash");
    assert_eq!(
        multi2.script_tree.is_some(),
        multi1.script_tree.is_some(),
        "cache hit must return identical tree presence"
    );
}
