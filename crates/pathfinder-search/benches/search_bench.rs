#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::semicolon_if_nothing_returned,
    clippy::assigning_clones
)]

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use pathfinder_search::{RipgrepScout, Scout, SearchParams};
use std::hint::black_box;
use std::io::Write;
use tempfile::TempDir;

const SOURCE_LINES: &[&str] = &[
    "use std::collections::HashMap;",
    "use std::sync::Arc;",
    "",
    "/// A sample struct for benchmarking.",
    "pub struct UserService {",
    "    db: Arc<dyn Database>,",
    "    cache: HashMap<String, User>,",
    "}",
    "",
    "impl UserService {",
    "    pub fn new(db: Arc<dyn Database>) -> Self {",
    "        Self {",
    "            db,",
    "            cache: HashMap::new(),",
    "        }",
    "    }",
    "",
    "    pub async fn find_user(&self, id: &str) -> Option<User> {",
    "        if let Some(cached) = self.cache.get(id) {",
    "            return Some(cached.clone());",
    "        }",
    "        let user = self.db.query(id).await?;",
    "        self.cache.insert(id.to_owned(), user.clone());",
    "        Some(user)",
    "    }",
    "",
    "    pub async fn create_user(&self, user: User) -> Result<(), Error> {",
    "        self.db.insert(&user.id, &user).await",
    "    }",
    "",
    "    pub fn list_users(&self) -> Vec<&User> {",
    "        self.cache.values().collect()",
    "    }",
    "}",
    "",
    "#[cfg(test)]",
    "mod tests {",
    "    use super::*;",
    "",
    "    #[test]",
    "    fn test_find_user_cached() {",
    "        let service = UserService::new(Arc::new(MockDb));",
    "        let result = service.find_user(\"abc\");",
    "        assert!(result.is_some());",
    "    }",
    "}",
];

struct WorkspaceFixture {
    _temp: TempDir,
    params: SearchParams,
}

fn create_workspace(file_count: usize, files_with_needle: usize) -> WorkspaceFixture {
    let dir = tempfile::tempdir().expect("create tempdir");

    let needle_line = "    let token = auth::generate_token(user_id);\n";

    for i in 0..file_count {
        let has_needle = i < files_with_needle;
        let ext = if i % 3 == 0 {
            "rs"
        } else if i % 3 == 1 {
            "ts"
        } else {
            "go"
        };
        let dir_name = if i % 5 == 0 {
            "src"
        } else if i % 5 == 1 {
            "lib"
        } else {
            "pkg"
        };

        let path = dir
            .path()
            .join(dir_name)
            .join(format!("module_{i:03}.{ext}"));
        std::fs::create_dir_all(path.parent().unwrap()).expect("create dirs");

        let mut f = std::fs::File::create(&path).expect("create file");
        for line in SOURCE_LINES {
            writeln!(f, "{line}").expect("write line");
        }
        if has_needle {
            writeln!(f, "{needle_line}").expect("write needle");
        }
        for line in &SOURCE_LINES[..10] {
            writeln!(f, "{line}").expect("write line");
        }
    }

    let params = SearchParams {
        workspace_root: dir.path().to_path_buf(),
        query: "generate_token".to_owned(),
        is_regex: false,
        path_glob: "**/*".to_owned(),
        exclude_glob: String::default(),
        max_results: 50,
        offset: 0,
        context_lines: 2,
    };

    WorkspaceFixture { _temp: dir, params }
}

fn run_search(scout: &RipgrepScout, params: &SearchParams) -> pathfinder_search::SearchResult {
    tokio::runtime::Runtime::new()
        .expect("create runtime")
        .block_on(scout.search(params))
        .expect("search should succeed")
}

fn bench_literal_small(c: &mut Criterion) {
    let fixture = create_workspace(10, 5);
    let scout = RipgrepScout;

    c.bench_function("literal_small_10files", |b| {
        b.iter(|| black_box(run_search(&scout, &fixture.params)))
    });
}

fn bench_literal_large(c: &mut Criterion) {
    let fixture = create_workspace(200, 40);
    let scout = RipgrepScout;

    c.bench_function("literal_large_200files", |b| {
        b.iter(|| black_box(run_search(&scout, &fixture.params)))
    });
}

fn bench_regex_pattern(c: &mut Criterion) {
    let mut fixture = create_workspace(200, 40);
    fixture.params.query = r"generate_\w+".to_owned();
    fixture.params.is_regex = true;
    let scout = RipgrepScout;

    c.bench_function("regex_200files", |b| {
        b.iter(|| black_box(run_search(&scout, &fixture.params)))
    });
}

fn bench_no_context(c: &mut Criterion) {
    let mut fixture = create_workspace(200, 40);
    fixture.params.context_lines = 0;
    let scout = RipgrepScout;

    c.bench_function("no_context_200files", |b| {
        b.iter(|| black_box(run_search(&scout, &fixture.params)))
    });
}

fn bench_truncation(c: &mut Criterion) {
    let mut fixture = create_workspace(200, 40);
    fixture.params.max_results = 10;
    let scout = RipgrepScout;

    c.bench_function("truncation_max10_200files", |b| {
        b.iter(|| black_box(run_search(&scout, &fixture.params)))
    });
}

fn bench_path_glob(c: &mut Criterion) {
    let mut fixture = create_workspace(200, 40);
    fixture.params.path_glob = "src/**/*.rs".to_owned();
    let scout = RipgrepScout;

    c.bench_function("glob_filtered_200files", |b| {
        b.iter(|| black_box(run_search(&scout, &fixture.params)))
    });
}

fn bench_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("scaling");
    for file_count in [10, 50, 100, 200, 500] {
        let needle_count = file_count / 5;
        let fixture = create_workspace(file_count, needle_count);
        let scout = RipgrepScout;

        group.bench_with_input(
            BenchmarkId::new("files", file_count),
            &file_count,
            |b, _| {
                b.iter(|| black_box(run_search(&scout, &fixture.params)));
            },
        );
    }
    group.finish();
}

fn bench_repeated_pattern(c: &mut Criterion) {
    let fixture = create_workspace(200, 40);
    let scout = RipgrepScout;
    let mut group = c.benchmark_group("repeated_pattern");

    group.bench_function("cold_compile_200files", |b| {
        b.iter(|| black_box(run_search(&scout, &fixture.params)));
    });

    let warmup = run_search(&scout, &fixture.params);
    let _ = black_box(&warmup);

    group.bench_function("warm_cache_5x_same_pattern", |b| {
        b.iter(|| {
            for _ in 0..5 {
                black_box(run_search(&scout, &fixture.params));
            }
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_literal_small,
    bench_literal_large,
    bench_regex_pattern,
    bench_no_context,
    bench_truncation,
    bench_path_glob,
    bench_scaling,
    bench_repeated_pattern,
);
criterion_main!(benches);
