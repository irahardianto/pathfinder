#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use pathfinder_lsp::client::transport::{read_message, write_message};
use serde_json::json;
use std::hint::black_box;
use tokio::io::BufReader;

fn bench_write_message(c: &mut Criterion) {
    let mut group = c.benchmark_group("write_message");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();

    let small_msg = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "textDocument/definition",
        "params": { "textDocument": { "uri": "file:///foo.rs" } }
    });
    group.throughput(Throughput::Bytes(
        serde_json::to_vec(&small_msg).unwrap().len() as u64,
    ));
    group.bench_function("small_request", |b| {
        b.iter(|| {
            let mut buf: Vec<u8> = Vec::new();
            rt.block_on(write_message(&mut buf, black_box(&small_msg)))
        });
    });

    let notification = json!({
        "jsonrpc": "2.0",
        "method": "initialized",
        "params": {}
    });
    group.throughput(Throughput::Bytes(
        serde_json::to_vec(&notification).unwrap().len() as u64,
    ));
    group.bench_function("notification", |b| {
        b.iter(|| {
            let mut buf: Vec<u8> = Vec::new();
            rt.block_on(write_message(&mut buf, black_box(&notification)))
        });
    });

    let large_response = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": (0..100).map(|i| json!({
            "uri": format!("file:///workspace/src/mod_{i}.rs"),
            "range": {
                "start": { "line": i, "character": 0 },
                "end": { "line": i, "character": 5 }
            }
        })).collect::<Vec<_>>()
    });
    group.throughput(Throughput::Bytes(
        serde_json::to_vec(&large_response).unwrap().len() as u64,
    ));
    group.bench_function("large_response_100_locs", |b| {
        b.iter(|| {
            let mut buf: Vec<u8> = Vec::new();
            rt.block_on(write_message(&mut buf, black_box(&large_response)))
        });
    });

    group.finish();
}

fn bench_read_message(c: &mut Criterion) {
    let mut group = c.benchmark_group("read_message");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();

    let msg = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": { "uri": "file:///foo.rs", "range": {} }
    });
    let body = serde_json::to_vec(&msg).unwrap();
    let framed = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut full: Vec<u8> = framed.as_bytes().to_vec();
    full.extend_from_slice(&body);

    group.throughput(Throughput::Bytes(body.len() as u64));
    group.bench_function("single_message", |b| {
        b.iter(|| {
            let mut reader = BufReader::new(full.as_slice());
            rt.block_on(read_message(&mut reader))
        });
    });

    let notification = json!({
        "jsonrpc": "2.0",
        "method": "window/logMessage",
        "params": { "message": "Indexing complete" }
    });
    let notif_body = serde_json::to_vec(&notification).unwrap();
    let notif_framed = format!("Content-Length: {}\r\n\r\n", notif_body.len());
    let mut notif_full: Vec<u8> = notif_framed.as_bytes().to_vec();
    notif_full.extend_from_slice(&notif_body);

    group.throughput(Throughput::Bytes(notif_body.len() as u64));
    group.bench_function("notification", |b| {
        b.iter(|| {
            let mut reader = BufReader::new(notif_full.as_slice());
            rt.block_on(read_message(&mut reader))
        });
    });

    group.finish();
}

fn bench_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("transport_roundtrip");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();

    let msg = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "textDocument/definition",
        "params": { "textDocument": { "uri": "file:///foo.rs" } }
    });

    group.bench_function("write_then_read", |b| {
        b.iter(|| {
            let mut buf: Vec<u8> = Vec::new();
            rt.block_on(write_message(&mut buf, black_box(&msg)))?;
            let mut reader = BufReader::new(buf.as_slice());
            rt.block_on(read_message(&mut reader))
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_write_message,
    bench_read_message,
    bench_roundtrip,
);
criterion_main!(benches);
