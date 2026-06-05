#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use pathfinder_lsp::client::response_parsers::{
    parse_call_hierarchy_prepare_response, parse_definition_response, parse_references_response,
    parse_single_definition_location,
};
use serde_json::json;
use std::path::Path;
use tokio::runtime::Runtime;

fn bench_parse_definition_response(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_definition_response");
    let rt = Runtime::new().unwrap();

    let null_response = json!(null);
    group.bench_function("null_response", |b| {
        b.iter(|| {
            rt.block_on(parse_definition_response(
                black_box(null_response.clone()),
                Path::new("/"),
            ))
        });
    });

    let location_response = json!({
        "uri": "file:///workspace/src/auth.rs",
        "range": {
            "start": { "line": 41, "character": 4 },
            "end":   { "line": 41, "character": 9 }
        }
    });
    group.bench_function("location_response_no_file", |b| {
        b.iter(|| {
            rt.block_on(parse_definition_response(
                black_box(location_response.clone()),
                Path::new("/workspace"),
            ))
        });
    });

    let array_response = json!([{
        "uri": "file:///workspace/src/lib.rs",
        "range": {
            "start": { "line": 9, "character": 0 },
            "end":   { "line": 9, "character": 5 }
        }
    }]);
    group.bench_function("array_response_no_file", |b| {
        b.iter(|| {
            rt.block_on(parse_definition_response(
                black_box(array_response.clone()),
                Path::new("/workspace"),
            ))
        });
    });

    let location_link_response = json!({
        "targetUri": "file:///workspace/src/types.rs",
        "targetRange": {
            "start": { "line": 19, "character": 0 },
            "end":   { "line": 25, "character": 1 }
        },
        "targetSelectionRange": {
            "start": { "line": 19, "character": 4 },
            "end":   { "line": 19, "character": 9 }
        }
    });
    group.bench_function("location_link_no_file", |b| {
        b.iter(|| {
            rt.block_on(parse_definition_response(
                black_box(location_link_response.clone()),
                Path::new("/workspace"),
            ))
        });
    });

    let empty_array_response = json!([]);
    group.bench_function("empty_array", |b| {
        b.iter(|| {
            rt.block_on(parse_definition_response(
                black_box(empty_array_response.clone()),
                Path::new("/workspace"),
            ))
        });
    });

    group.finish();
}

fn bench_parse_definition_response_with_file(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_definition_response_with_file");
    group.sample_size(20);
    let rt = Runtime::new().unwrap();

    let temp = tempfile::tempdir().unwrap();
    let src_dir = temp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let file_path = src_dir.join("auth.rs");
    let content = (0..50)
        .map(|i| format!("fn function_{i}() {{ let x = {i}; }}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&file_path, &content).unwrap();

    let file_uri = url::Url::from_file_path(&file_path).unwrap().to_string();
    let response = json!({
        "uri": file_uri,
        "range": {
            "start": { "line": 41, "character": 4 },
            "end":   { "line": 41, "character": 9 }
        }
    });

    group.bench_function("with_real_file_read", |b| {
        b.iter(|| {
            rt.block_on(parse_definition_response(
                black_box(response.clone()),
                temp.path(),
            ))
        });
    });

    group.finish();
}

fn bench_parse_single_definition_location(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_single_definition_location");
    let rt = Runtime::new().unwrap();

    let null_location = json!(null);
    group.bench_function("null", |b| {
        b.iter(|| {
            rt.block_on(parse_single_definition_location(
                black_box(&null_location),
                Path::new("/workspace"),
            ))
        });
    });

    let location = json!({
        "uri": "file:///workspace/src/utils.rs",
        "range": {
            "start": { "line": 10, "character": 2 },
            "end":   { "line": 10, "character": 7 }
        }
    });
    group.bench_function("location_no_file", |b| {
        b.iter(|| {
            rt.block_on(parse_single_definition_location(
                black_box(&location),
                Path::new("/workspace"),
            ))
        });
    });

    group.finish();
}

fn bench_parse_references_response(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_references_response");
    let rt = Runtime::new().unwrap();

    let null_response = json!(null);
    group.bench_function("null", |b| {
        b.iter(|| {
            rt.block_on(parse_references_response(
                black_box(&null_response),
                Path::new("/workspace"),
            ))
        });
    });

    let single_ref = json!([{
        "uri": "file:///workspace/src/lib.rs",
        "range": {
            "start": { "line": 5, "character": 0 },
            "end":   { "line": 5, "character": 10 }
        }
    }]);
    group.bench_function("single_ref_no_file", |b| {
        b.iter(|| {
            rt.block_on(parse_references_response(
                black_box(&single_ref),
                Path::new("/workspace"),
            ))
        });
    });

    let multi_refs = json!([
        {"uri": "file:///workspace/src/a.rs", "range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 5}}},
        {"uri": "file:///workspace/src/b.rs", "range": {"start": {"line": 2, "character": 0}, "end": {"line": 2, "character": 5}}},
        {"uri": "file:///workspace/src/c.rs", "range": {"start": {"line": 3, "character": 0}, "end": {"line": 3, "character": 5}}},
        {"uri": "file:///workspace/src/d.rs", "range": {"start": {"line": 4, "character": 0}, "end": {"line": 4, "character": 5}}},
        {"uri": "file:///workspace/src/e.rs", "range": {"start": {"line": 5, "character": 0}, "end": {"line": 5, "character": 5}}},
    ]);
    group.bench_function("five_refs_no_file", |b| {
        b.iter(|| {
            rt.block_on(parse_references_response(
                black_box(&multi_refs),
                Path::new("/workspace"),
            ))
        });
    });

    group.finish();
}

fn bench_parse_references_response_with_files(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_references_response_with_files");
    group.sample_size(20);
    let rt = Runtime::new().unwrap();

    let temp = tempfile::tempdir().unwrap();
    let src_dir = temp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    let mut refs = Vec::new();
    for i in 0..5u8 {
        let file_path = src_dir.join(format!("mod_{i}.rs"));
        let content = (0..20)
            .map(|j| format!("fn func_{j}() -> i32 {{ {j} }}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&file_path, &content).unwrap();
        let file_uri = url::Url::from_file_path(&file_path).unwrap().to_string();
        refs.push(json!({
            "uri": file_uri,
            "range": {
                "start": { "line": i, "character": 0 },
                "end":   { "line": i, "character": 5 }
            }
        }));
    }
    let response = json!(refs);

    group.bench_function("five_refs_with_file_reads", |b| {
        b.iter(|| rt.block_on(parse_references_response(black_box(&response), temp.path())));
    });

    group.finish();
}

fn bench_parse_call_hierarchy_prepare(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_call_hierarchy_prepare");

    let null_response = json!(null);
    group.bench_function("null", |b| {
        b.iter(|| {
            parse_call_hierarchy_prepare_response(
                black_box(&null_response),
                Path::new("/workspace"),
            )
        });
    });

    let items = json!([{
        "name": "main",
        "kind": 12,
        "detail": "fn()",
        "uri": "file:///workspace/src/main.rs",
        "selectionRange": {
            "start": { "line": 0, "character": 2 },
            "end": { "line": 0, "character": 6 }
        }
    }]);
    group.bench_function("single_item", |b| {
        b.iter(|| {
            parse_call_hierarchy_prepare_response(black_box(&items), Path::new("/workspace"))
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_parse_definition_response,
    bench_parse_definition_response_with_file,
    bench_parse_single_definition_location,
    bench_parse_references_response,
    bench_parse_references_response_with_files,
    bench_parse_call_hierarchy_prepare,
);
criterion_main!(benches);
