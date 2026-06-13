#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::format_push_string,
    clippy::format_collect
)]

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use pathfinder_common::types::SymbolChain;
use pathfinder_treesitter::language::SupportedLanguage;
use pathfinder_treesitter::parser::AstParser;
use pathfinder_treesitter::symbols::{did_you_mean, extract_symbols_from_tree};
use std::hint::black_box;

fn bench_did_you_mean(c: &mut Criterion) {
    let mut group = c.benchmark_group("did_you_mean");

    let source = (0..200)
        .map(|i| format!("fn func_{i:04}() -> i32 {{ {i} }}\n"))
        .collect::<String>();
    let tree = AstParser::parse_source(
        std::path::Path::new("bench.rs"),
        SupportedLanguage::Rust,
        source.as_bytes(),
    )
    .expect("parse");
    let symbols = extract_symbols_from_tree(&tree, source.as_bytes(), SupportedLanguage::Rust);

    let near_miss = SymbolChain::parse("func_0099").expect("chain");
    group.bench_with_input(
        BenchmarkId::new("near_miss", 200),
        &(&symbols, &near_miss),
        |b, (syms, chain)| {
            b.iter(|| did_you_mean(black_box(syms), black_box(chain), black_box(3)));
        },
    );

    let far_miss = SymbolChain::parse("totally_wrong_name_xyz").expect("chain");
    group.bench_with_input(
        BenchmarkId::new("far_miss", 200),
        &(&symbols, &far_miss),
        |b, (syms, chain)| {
            b.iter(|| did_you_mean(black_box(syms), black_box(chain), black_box(3)));
        },
    );

    let exact = SymbolChain::parse("func_0050").expect("chain");
    group.bench_with_input(
        BenchmarkId::new("exact_match", 200),
        &(&symbols, &exact),
        |b, (syms, chain)| {
            b.iter(|| did_you_mean(black_box(syms), black_box(chain), black_box(3)));
        },
    );

    let small_source = (0..20)
        .map(|i| format!("fn func_{i}() -> i32 {{ {i} }}\n"))
        .collect::<String>();
    let small_tree = AstParser::parse_source(
        std::path::Path::new("small.rs"),
        SupportedLanguage::Rust,
        small_source.as_bytes(),
    )
    .expect("parse small");
    let small_symbols = extract_symbols_from_tree(
        &small_tree,
        small_source.as_bytes(),
        SupportedLanguage::Rust,
    );
    let small_miss = SymbolChain::parse("func_1").expect("chain");
    group.bench_with_input(
        BenchmarkId::new("near_miss", 20),
        &(&small_symbols, &small_miss),
        |b, (syms, chain)| {
            b.iter(|| did_you_mean(black_box(syms), black_box(chain), black_box(3)));
        },
    );

    group.finish();
}

criterion_group!(benches, bench_did_you_mean);
criterion_main!(benches);
