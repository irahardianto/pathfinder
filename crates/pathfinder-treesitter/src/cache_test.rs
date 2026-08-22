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
    let (tree1, src1, sym1) = cache
        .get_or_parse(&path, SupportedLanguage::Go)
        .await
        .unwrap();
    assert_eq!(src1.len(), 25);
    assert_eq!(sym1.len(), 1);
    assert_eq!(sym1[0].name, "A");

    // 2. Second access — fast path via mtime: no disk read, returns cached symbols
    let (tree2, src2, sym2) = cache
        .get_or_parse(&path, SupportedLanguage::Go)
        .await
        .unwrap();
    assert_eq!(src2.len(), 25);
    assert_eq!(sym2.len(), 1);
    assert_eq!(sym2[0].name, "A");
    assert_eq!(
        tree1.root_node().child_count(),
        tree2.root_node().child_count()
    );
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
    std::thread::sleep(std::time::Duration::from_millis(10));
    writeln!(file, "func B() {{}}").unwrap();
    let (_tree3, src3, sym3) = cache
        .get_or_parse(&path, SupportedLanguage::Go)
        .await
        .unwrap();
    assert!(src3.len() > 25);
    assert_eq!(sym3.len(), 2);
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

    cache
        .get_or_parse(f1.path(), SupportedLanguage::Go)
        .await
        .unwrap();
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

    std::thread::sleep(std::time::Duration::from_millis(10));
    cache
        .get_or_parse(f1.path(), SupportedLanguage::Go)
        .await
        .unwrap();

    cache
        .get_or_parse(f3.path(), SupportedLanguage::Go)
        .await
        .unwrap();

    {
        let lock = cache.entries.lock();
        assert_eq!(lock.len(), 2);
        assert!(lock.contains(f1.path()));
        assert!(!lock.contains(f2.path()));
        assert!(lock.contains(f3.path()));
    }

    std::thread::sleep(std::time::Duration::from_millis(10));
    let mtime_f1 = std::fs::metadata(f1.path()).unwrap().modified().unwrap();
    let content_f1 = std::fs::read(f1.path()).unwrap();
    cache
        .get_or_parse_preloaded(
            f1.path(),
            SupportedLanguage::Go,
            Arc::from(content_f1),
            mtime_f1,
        )
        .await
        .unwrap();

    cache
        .get_or_parse(f2.path(), SupportedLanguage::Go)
        .await
        .unwrap();

    {
        let lock = cache.entries.lock();
        assert_eq!(lock.len(), 2);
        assert!(lock.contains(f1.path()));
        assert!(lock.contains(f2.path()));
        assert!(!lock.contains(f3.path()));
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
    let (multi1, sym1, hash1) = cache.get_or_parse_vue(file.path()).await.unwrap();
    assert!(multi1.script_tree.is_some());
    assert!(multi1.template_tree.is_some());
    assert!(!multi1.degraded);
    assert!(!sym1.is_empty());

    // Second call — cache hit (mtime unchanged)
    let (_multi2, sym2, hash2) = cache.get_or_parse_vue(file.path()).await.unwrap();
    assert_eq!(hash1, hash2, "hash must be stable across cache hits");
    assert_eq!(sym1.len(), sym2.len());

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

    cache.get_or_parse_vue(f1.path()).await.unwrap();
    std::thread::sleep(std::time::Duration::from_millis(10));
    cache.get_or_parse_vue(f2.path()).await.unwrap();

    {
        let lock = cache.vue_entries.lock();
        assert_eq!(lock.len(), 2);
        assert!(lock.contains(f1.path()));
        assert!(lock.contains(f2.path()));
    }

    std::thread::sleep(std::time::Duration::from_millis(10));
    cache.get_or_parse_vue(f1.path()).await.unwrap();

    cache.get_or_parse_vue(f3.path()).await.unwrap();

    {
        let lock = cache.vue_entries.lock();
        assert_eq!(lock.len(), 2);
        assert!(lock.contains(f1.path()));
        assert!(!lock.contains(f2.path()));
        assert!(lock.contains(f3.path()));
    }

    std::thread::sleep(std::time::Duration::from_millis(10));
    let mtime_f1 = std::fs::metadata(f1.path()).unwrap().modified().unwrap();
    cache
        .get_or_parse_vue_preloaded(f1.path(), sfc_a, mtime_f1)
        .await
        .unwrap();

    cache.get_or_parse_vue(f2.path()).await.unwrap();

    {
        let lock = cache.vue_entries.lock();
        assert_eq!(lock.len(), 2);
        assert!(lock.contains(f1.path()));
        assert!(lock.contains(f2.path()));
        assert!(!lock.contains(f3.path()));
    }
}

#[tokio::test]
async fn test_vue_cache_invalidation_clears_both_caches() {
    let cache = AstCache::new(2);

    let mut go_file = NamedTempFile::new().unwrap();
    writeln!(go_file, "func A() {{}}").unwrap();
    cache
        .get_or_parse(go_file.path(), SupportedLanguage::Go)
        .await
        .unwrap();

    let sfc = b"<template><div/></template>\n";
    let mut vue_file = NamedTempFile::new().unwrap();
    vue_file.write_all(sfc).unwrap();
    cache.get_or_parse_vue(vue_file.path()).await.unwrap();

    assert_eq!(cache.entries.lock().len(), 1);
    assert_eq!(cache.vue_entries.lock().len(), 1);

    cache.invalidate(vue_file.path());
    assert_eq!(cache.vue_entries.lock().len(), 0, "Vue entry cleared");
}

#[tokio::test]
async fn test_vue_cache_hit_when_mtime_changes_but_content_unchanged() {
    let cache = AstCache::new(2);

    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "<template><div>Hello</div></template>").unwrap();
    writeln!(file, "<script>const a = 1;</script>").unwrap();
    let path = file.path().to_path_buf();

    let (multi1, _sym1, hash1) = cache.get_or_parse_vue(&path).await.unwrap();
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

    let (multi2, _sym2, hash2) = cache.get_or_parse_vue(&path).await.unwrap();

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

    let (_multi1, _sym1, hash1) = cache.get_or_parse_vue(&path).await.unwrap();

    std::thread::sleep(std::time::Duration::from_millis(10));
    writeln!(file, "<script>const b = 2;</script>").unwrap();

    let (_multi2, _sym2, hash2) = cache.get_or_parse_vue(&path).await.unwrap();

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

    let (tree1, src1, _sym1) = cache
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

    let (tree2, src2, _sym2) = cache
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

    let (_tree1, src1, _sym1) = cache
        .get_or_parse(&path, SupportedLanguage::Go)
        .await
        .unwrap();
    let original_len = src1.len();

    std::thread::sleep(std::time::Duration::from_millis(10));
    writeln!(file, "func B() {{}}").unwrap();

    let (_tree2, src2, _sym2) = cache
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

#[tokio::test]
async fn test_get_or_parse_file_deleted_between_stat_and_read() {
    let cache = AstCache::new(2);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vanish.go");

    let err = cache
        .get_or_parse(&path, SupportedLanguage::Go)
        .await
        .unwrap_err();

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

#[test]
fn test_io_err_permission_denied_returns_io_error() {
    let err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
    let result = io_err(err, Path::new("/tmp/test"));
    assert!(
        matches!(result, SurgeonError::Io(_)),
        "PermissionDenied should map to Io variant, got: {result:?}"
    );
}

#[test]
fn test_io_err_not_found_returns_file_not_found() {
    let err = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
    let result = io_err(err, Path::new("/tmp/missing"));
    assert!(
        matches!(result, SurgeonError::FileNotFound(_)),
        "NotFound should map to FileNotFound variant, got: {result:?}"
    );
}

#[tokio::test]
async fn test_get_or_parse_preloaded_cache_hit() {
    let cache = AstCache::new(2);

    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test.rs");
    let content = b"fn main() {}";
    std::fs::write(&file_path, content).unwrap();

    let mtime = std::fs::metadata(&file_path).unwrap().modified().unwrap();
    let content_arc = Arc::from(content.as_slice());

    let (tree1, src1, sym1) = cache
        .get_or_parse_preloaded(
            &file_path,
            SupportedLanguage::Rust,
            Arc::clone(&content_arc),
            mtime,
        )
        .await
        .unwrap();
    assert_eq!(src1.len(), content.len());
    assert_eq!(sym1.len(), 1);
    assert_eq!(sym1[0].name, "main");

    let (tree2, src2, sym2) = cache
        .get_or_parse_preloaded(&file_path, SupportedLanguage::Rust, content_arc, mtime)
        .await
        .unwrap();
    assert_eq!(src2.len(), content.len());
    assert_eq!(sym2.len(), 1);
    assert_eq!(
        tree1.root_node().child_count(),
        tree2.root_node().child_count(),
        "cache hit must return identical tree structure"
    );
}

#[tokio::test]
async fn test_get_or_parse_vue_preloaded_cache_hit() {
    let cache = AstCache::new(2);

    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test.vue");
    let sfc = b"<script setup lang=\"ts\">\nconst x = 1\n</script>\n";
    std::fs::write(&file_path, sfc).unwrap();

    let mtime = std::fs::metadata(&file_path).unwrap().modified().unwrap();

    let (multi1, sym1, hash1) = cache
        .get_or_parse_vue_preloaded(&file_path, sfc, mtime)
        .await
        .unwrap();
    assert!(multi1.script_tree.is_some());
    assert!(!sym1.is_empty());

    let (multi2, sym2, hash2) = cache
        .get_or_parse_vue_preloaded(&file_path, sfc, mtime)
        .await
        .unwrap();
    assert_eq!(hash1, hash2, "cache hit must return identical hash");
    assert_eq!(sym1.len(), sym2.len());
    assert_eq!(
        multi2.script_tree.is_some(),
        multi1.script_tree.is_some(),
        "cache hit must return identical tree presence"
    );
}

#[tokio::test]
async fn test_ast_cache_complex_rust_features() {
    let cache = AstCache::new(2);

    let dir = tempdir().unwrap();
    let file_path = dir.path().join("complex.rs");
    let content = r"
struct Ref<'a> {
    data: &'a str,
}
trait Mapper<T> {
    fn map(&self) -> T;
}
macro_rules! my_macro {
    () => {};
}
fn multi_line(
    a: i32,
) -> Result<(), ()> {
    Ok(())
}
";
    let bytes = content.as_bytes();
    std::fs::write(&file_path, bytes).unwrap();

    let mtime = std::fs::metadata(&file_path).unwrap().modified().unwrap();
    let content_arc = Arc::from(bytes);

    let (tree1, src1, sym1) = cache
        .get_or_parse_preloaded(
            &file_path,
            SupportedLanguage::Rust,
            Arc::clone(&content_arc),
            mtime,
        )
        .await
        .unwrap();
    assert_eq!(src1.len(), bytes.len());
    assert!(!sym1.is_empty());

    let (tree2, src2, sym2) = cache
        .get_or_parse_preloaded(&file_path, SupportedLanguage::Rust, content_arc, mtime)
        .await
        .unwrap();
    assert_eq!(src2.len(), bytes.len());
    assert_eq!(sym1.len(), sym2.len());
    assert_eq!(
        tree1.root_node().child_count(),
        tree2.root_node().child_count(),
        "complex AST features cache hit must return identical structure"
    );
}
