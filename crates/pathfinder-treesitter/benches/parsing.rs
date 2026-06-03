#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use pathfinder_treesitter::language::SupportedLanguage;
use pathfinder_treesitter::parser::AstParser;

fn generate_large_source(n_functions: usize) -> Vec<u8> {
    let mut src = Vec::with_capacity(n_functions * 60);
    for i in 0..n_functions {
        src.extend_from_slice(format!("fn func_{i}() -> i32 {{ {i} }}\n").as_bytes());
    }
    src
}

fn bench_parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_source");
    group.sample_size(20);

    let languages: Vec<(&str, SupportedLanguage)> = vec![
        ("go", SupportedLanguage::Go),
        ("typescript", SupportedLanguage::TypeScript),
        ("python", SupportedLanguage::Python),
        ("rust", SupportedLanguage::Rust),
        ("java", SupportedLanguage::Java),
        ("javascript", SupportedLanguage::JavaScript),
    ];

    for &(name, lang) in &languages {
        let source = generate_large_source(500);
        let ext = format!("bench.{name}");
        let path = std::path::Path::new(&ext);
        group.throughput(Throughput::Bytes(source.len() as u64));
        group.bench_with_input(
            BenchmarkId::new(name, 500),
            &(path, lang, &source),
            |b, (path, lang, src)| {
                b.iter(|| {
                    AstParser::parse_source(black_box(path), black_box(*lang), black_box(src))
                        .expect("parse")
                });
            },
        );
    }

    for &size in &[100, 500, 1000] {
        let source = generate_large_source(size);
        let path = std::path::Path::new("bench.rs");
        group.throughput(Throughput::Bytes(source.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("rust_scaling", size),
            &(path, SupportedLanguage::Rust, &source),
            |b, (path, lang, src)| {
                b.iter(|| {
                    AstParser::parse_source(black_box(path), black_box(*lang), black_box(src))
                        .expect("parse")
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_parsing);
criterion_main!(benches);
