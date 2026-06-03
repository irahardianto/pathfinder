#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use pathfinder_common::types::{SemanticPath, VersionHash, WorkspaceRoot};
use std::path::Path;

fn bench_semantic_path_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("semantic_path_parse");

    let inputs = [
        ("bare_file", "src/utils.ts"),
        ("file_and_symbol", "src/auth.ts::AuthService.login"),
        ("overloaded", "src/auth.ts::AuthService.refreshToken#2"),
        ("deep_chain", "src/mod.rs::Outer::Inner::method"),
        (
            "long_path",
            "crates/pathfinder-common/src/types.rs::SymbolChain.parse",
        ),
    ];

    for (name, input) in &inputs {
        group.bench_with_input(BenchmarkId::new("parse", *name), input, |b, input| {
            b.iter(|| SemanticPath::parse(black_box(input)));
        });
    }
    group.finish();
}

fn bench_semantic_path_display(c: &mut Criterion) {
    let mut group = c.benchmark_group("semantic_path_display");

    let cases = [
        ("bare_file", SemanticPath::parse("src/utils.ts").unwrap()),
        (
            "file_and_symbol",
            SemanticPath::parse("src/auth.ts::AuthService.login").unwrap(),
        ),
        (
            "overloaded",
            SemanticPath::parse("src/auth.ts::AuthService.refreshToken#2").unwrap(),
        ),
        (
            "deep_chain",
            SemanticPath::parse("src/mod.rs::Outer::Inner::method").unwrap(),
        ),
    ];

    for (name, sp) in &cases {
        group.bench_with_input(BenchmarkId::new("display", *name), sp, |b, sp| {
            b.iter(|| black_box(sp.to_string()));
        });
    }
    group.finish();
}

fn bench_version_hash_compute(c: &mut Criterion) {
    let mut group = c.benchmark_group("version_hash_compute");

    let cases = [
        ("empty", b"" as &[u8]),
        ("small_100b", &[b'x'; 100] as &[u8]),
        ("medium_1kb", &[b'x'; 1_000] as &[u8]),
        ("large_10kb", &[b'x'; 10_000] as &[u8]),
        ("realistic_4kb", {
            static CONTENT: &[u8] = b"fn main() { println!(\"hello\"); }\n";
            CONTENT
        }),
    ];

    for (name, content) in &cases {
        group.throughput(Throughput::Bytes(content.len() as u64));
        group.bench_with_input(BenchmarkId::new("compute", *name), content, |b, content| {
            b.iter(|| VersionHash::compute(black_box(content)));
        });
    }
    group.finish();
}

fn bench_version_hash_matches(c: &mut Criterion) {
    let mut group = c.benchmark_group("version_hash_matches");
    let hash = VersionHash::compute(b"benchmark content for hash matching");

    let cases = [
        ("short_no_prefix", hash.short().to_string()),
        ("short_with_prefix", format!("sha256:{}", hash.short())),
        ("full_hash", hash.as_str().to_string()),
        ("wrong_hash", "0000000".to_string()),
        ("too_short", "abc".to_string()),
    ];

    for (name, input) in &cases {
        group.bench_with_input(BenchmarkId::new("matches", *name), input, |b, input| {
            b.iter(|| hash.matches(black_box(input)));
        });
    }
    group.finish();
}

fn bench_workspace_root_resolve(c: &mut Criterion) {
    let mut group = c.benchmark_group("workspace_root_resolve");
    let dir = tempfile::tempdir().unwrap();
    let root = WorkspaceRoot::new(dir.path()).unwrap();

    let cases = [
        ("simple", Path::new("src/main.rs")),
        ("nested", Path::new("crates/pathfinder-common/src/lib.rs")),
        ("traversal", Path::new("../../etc/passwd")),
        ("deep", Path::new("a/b/c/d/e/f/g/h/file.rs")),
    ];

    for (name, path) in &cases {
        group.bench_with_input(BenchmarkId::new("resolve", *name), path, |b, path| {
            b.iter(|| black_box(root.resolve(black_box(path))));
        });
    }
    group.finish();
}

fn bench_workspace_root_resolve_strict(c: &mut Criterion) {
    let mut group = c.benchmark_group("workspace_root_resolve_strict");
    let dir = tempfile::tempdir().unwrap();
    let root = WorkspaceRoot::new(dir.path()).unwrap();

    let valid = Path::new("src/main.rs");
    let traversal = Path::new("../../etc/passwd");
    let absolute = Path::new("/etc/passwd");

    group.bench_function("valid_relative", |b| {
        b.iter(|| black_box(root.resolve_strict(black_box(valid)).is_ok()));
    });
    group.bench_function("traversal_reject", |b| {
        b.iter(|| black_box(root.resolve_strict(black_box(traversal)).is_err()));
    });
    group.bench_function("absolute_reject", |b| {
        b.iter(|| black_box(root.resolve_strict(black_box(absolute)).is_err()));
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_semantic_path_parse,
    bench_semantic_path_display,
    bench_version_hash_compute,
    bench_version_hash_matches,
    bench_workspace_root_resolve,
    bench_workspace_root_resolve_strict,
);
criterion_main!(benches);
