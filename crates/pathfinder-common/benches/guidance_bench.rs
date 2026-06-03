use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use pathfinder_common::types::DegradedReason;

fn bench_degraded_reason_guidance(c: &mut Criterion) {
    let mut group = c.benchmark_group("degraded_reason_guidance");

    let reasons = [
        ("no_lsp", DegradedReason::NoLsp),
        ("lsp_warmup_empty", DegradedReason::LspWarmupEmptyUnverified),
        ("lsp_warmup_grep", DegradedReason::LspWarmupGrepFallback),
        ("lsp_timeout_grep", DegradedReason::LspTimeoutGrepFallback),
        ("lsp_error_grep", DegradedReason::LspErrorGrepFallback),
        ("no_lsp_grep", DegradedReason::NoLspGrepFallback),
        ("grep_fallback_file", DegradedReason::GrepFallbackFileScoped),
        ("grep_fallback_impl", DegradedReason::GrepFallbackImplScoped),
        ("grep_fallback_global", DegradedReason::GrepFallbackGlobal),
        (
            "grep_fallback_deps",
            DegradedReason::GrepFallbackDependencies,
        ),
        (
            "unsupported_lang_bypassed",
            DegradedReason::UnsupportedLanguageFilterBypassed,
        ),
        ("unsupported_lang", DegradedReason::UnsupportedLanguage),
        ("git_error", DegradedReason::GitError),
    ];

    for (name, reason) in &reasons {
        group.bench_with_input(BenchmarkId::new("guidance", *name), reason, |b, reason| {
            b.iter(|| black_box(reason.guidance()));
        });
    }

    group.finish();
}

fn bench_degraded_reason_display(c: &mut Criterion) {
    let mut group = c.benchmark_group("degraded_reason_display");

    let reasons = [
        ("no_lsp", DegradedReason::NoLsp),
        ("lsp_warmup_grep", DegradedReason::LspWarmupGrepFallback),
        ("git_error", DegradedReason::GitError),
    ];

    for (name, reason) in &reasons {
        group.bench_with_input(BenchmarkId::new("display", *name), reason, |b, reason| {
            b.iter(|| black_box(reason.to_string()));
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_degraded_reason_guidance,
    bench_degraded_reason_display,
);
criterion_main!(benches);
