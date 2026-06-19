#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::semicolon_if_nothing_returned
)]

use criterion::{criterion_group, criterion_main, Criterion};
use pathfinder_search::{RipgrepScout, Scout, SearchParams};
use std::hint::black_box;
use std::path::PathBuf;

fn run_search(scout: &RipgrepScout, params: &SearchParams) -> pathfinder_search::SearchResult {
    tokio::runtime::Runtime::new()
        .expect("create runtime")
        .block_on(scout.search(params))
        .expect("search should succeed")
}

fn bench_real_workspace_grep_fallback(c: &mut Criterion) {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();

    // Query a nonexistent pattern globally to measure worst-case traversal time.
    let params = SearchParams {
        workspace_root,
        query: "NONEXISTENT_SYMBOL_XYZ_12345".to_owned(),
        is_regex: false,
        path_glob: "**/*".to_owned(),
        exclude_glob: Vec::new(),
        max_results: 1,
        offset: 0,
        context_lines: 0,
    };

    let scout = RipgrepScout;

    let mut group = c.benchmark_group("grep_fallback");
    // Reduce sample count for slow benchmark so it finishes in a reasonable time.
    group.sample_size(10);

    group.bench_function("global_search_worst_case", |b| {
        b.iter(|| black_box(run_search(&scout, &params)))
    });

    group.finish();
}

criterion_group!(benches, bench_real_workspace_grep_fallback);
criterion_main!(benches);
