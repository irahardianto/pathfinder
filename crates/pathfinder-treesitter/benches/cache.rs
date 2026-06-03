#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{criterion_group, criterion_main, Criterion};
use pathfinder_treesitter::cache::AstCache;
use pathfinder_treesitter::language::SupportedLanguage;
use std::io::Write;
use tempfile::NamedTempFile;

fn bench_cache(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache");

    let mut file = NamedTempFile::new().expect("tempfile");
    let content = "package main\n".repeat(500);
    file.write_all(content.as_bytes()).expect("write");
    let path = file.path().to_path_buf();

    let rt = tokio::runtime::Runtime::new().expect("runtime");

    group.bench_function("miss_parse_go_25kb", |b| {
        b.iter(|| {
            let cache = AstCache::new(100);
            rt.block_on(cache.get_or_parse(&path, SupportedLanguage::Go))
                .expect("parse")
        });
    });

    let cache = AstCache::new(100);
    rt.block_on(cache.get_or_parse(&path, SupportedLanguage::Go))
        .expect("initial parse");

    group.bench_function("hit_go_25kb_mtime", |b| {
        b.iter(|| {
            rt.block_on(cache.get_or_parse(&path, SupportedLanguage::Go))
                .expect("cache hit")
        });
    });

    let mut vue_file = NamedTempFile::with_suffix(".vue").expect("tempfile");
    let vue_sfc = br#"<template>
  <div class="app">
    <MyButton @click="doThing">Click me</MyButton>
    <router-view />
  </div>
</template>
<script setup lang="ts">
import { ref } from 'vue'
const count = ref(0)
function doThing() { count.value++ }
function another() { count.value-- }
function third() { count.value = 0 }
</script>
<style scoped>
.app { color: red; }
#main { font-size: 16px; }
@media (max-width: 768px) { .app { display: none; } }
</style>"#;
    vue_file.write_all(vue_sfc).expect("write vue");
    let vue_path = vue_file.path().to_path_buf();

    group.bench_function("miss_parse_vue_sfc", |b| {
        b.iter(|| {
            let cache = AstCache::new(100);
            rt.block_on(cache.get_or_parse_vue(&vue_path))
                .expect("vue parse")
        });
    });

    let vue_cache = AstCache::new(100);
    rt.block_on(vue_cache.get_or_parse_vue(&vue_path))
        .expect("initial vue parse");

    group.bench_function("hit_vue_sfc_mtime", |b| {
        b.iter(|| {
            rt.block_on(vue_cache.get_or_parse_vue(&vue_path))
                .expect("vue cache hit")
        });
    });

    group.bench_function("singleflight_5_concurrent", |b| {
        b.iter(|| {
            let cache = std::sync::Arc::new(AstCache::new(100));
            let path = std::sync::Arc::new(path.clone());
            let mut handles = Vec::new();
            for _ in 0..5 {
                let cache = std::sync::Arc::clone(&cache);
                let path = std::sync::Arc::clone(&path);
                handles.push(
                    rt.spawn(async move { cache.get_or_parse(&path, SupportedLanguage::Go).await }),
                );
            }
            for h in handles {
                rt.block_on(h).expect("join").expect("parse");
            }
        });
    });

    group.finish();
}

criterion_group!(benches, bench_cache);
criterion_main!(benches);
