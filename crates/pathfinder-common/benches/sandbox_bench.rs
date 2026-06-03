#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use pathfinder_common::config::SandboxConfig;
use pathfinder_common::sandbox::Sandbox;
use std::path::Path;

fn bench_sandbox_check(c: &mut Criterion) {
    let mut group = c.benchmark_group("sandbox_check");

    let workspace = tempfile::tempdir().unwrap();
    let sandbox = Sandbox::with_user_rules(workspace.path(), &SandboxConfig::default(), None);

    let allowed_cases = [
        ("normal_rs", Path::new("src/main.rs")),
        ("normal_ts", Path::new("src/auth.ts")),
        ("readme", Path::new("README.md")),
        ("nested", Path::new("crates/pathfinder-common/src/lib.rs")),
        ("gitignore", Path::new(".gitignore")),
        ("github_workflow", Path::new(".github/workflows/ci.yml")),
    ];

    for (name, path) in &allowed_cases {
        group.bench_with_input(BenchmarkId::new("allowed", *name), path, |b, path| {
            b.iter(|| sandbox.check(black_box(path)).is_ok());
        });
    }

    let denied_cases = [
        ("git_objects", Path::new(".git/objects/abc123")),
        ("pem_file", Path::new("certs/server.pem")),
        ("key_file", Path::new("keys/private.key")),
        ("env_file", Path::new(".env")),
        ("env_local", Path::new(".env.local")),
        ("node_modules", Path::new("node_modules/express/index.js")),
        ("vendor", Path::new("vendor/github.com/pkg")),
        ("traversal", Path::new("../../etc/passwd")),
    ];

    for (name, path) in &denied_cases {
        group.bench_with_input(BenchmarkId::new("denied", *name), path, |b, path| {
            b.iter(|| sandbox.check(black_box(path)).is_err());
        });
    }

    group.finish();
}

fn bench_sandbox_check_with_additional_deny(c: &mut Criterion) {
    let mut group = c.benchmark_group("sandbox_check_additional_deny");

    let workspace = tempfile::tempdir().unwrap();
    let config = SandboxConfig {
        additional_deny: vec![
            "*.generated.ts".to_owned(),
            "secrets/".to_owned(),
            "temp/".to_owned(),
        ],
        allow_override: vec![],
    };
    let sandbox = Sandbox::with_user_rules(workspace.path(), &config, None);

    group.bench_function("normal_file_with_extra_rules", |b| {
        b.iter(|| sandbox.check(black_box(Path::new("src/main.rs"))).is_ok());
    });

    group.bench_function("extension_deny_match", |b| {
        b.iter(|| {
            sandbox
                .check(black_box(Path::new("src/schema.generated.ts")))
                .is_err()
        });
    });

    group.bench_function("directory_deny_match", |b| {
        b.iter(|| {
            sandbox
                .check(black_box(Path::new("secrets/config.toml")))
                .is_err()
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_sandbox_check,
    bench_sandbox_check_with_additional_deny,
);
criterion_main!(benches);
