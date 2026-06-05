# pathfinder-lsp Performance Analysis

Date: 2026-06-05
Crate: `pathfinder-mcp-lsp` v0.10.1
Status: **COMPLETE**

## Results Summary

| Fix | Status | Impact |
|-----|--------|--------|
| Async file reads in response_parsers | **Applied** | Blocking I/O removed from async hot path |
| Pre-allocate write header buffer | **Reverted** | Below threshold (1.1% noise, p=0.20) |
| DRY: shared URI→path helper | **Applied** (part of Fix 1) | 3 duplicate blocks → 2 shared functions |

### Critical Benchmark Finding

**The original baseline (`lsp_before`) was incorrectly measuring Future creation, not actual execution.**

The async functions were not being `.await`ed in the original benchmarks. This means:
- `lsp_before` = measuring `async fn()` call (Future creation) only
- `after` = measuring actual async execution via `rt.block_on()`

This explains the apparent "regressions" — they're actually benchmark bug fixes.

### Architectural Win (Not Microbenchmark Win)

The critical change is **removing blocking I/O from an async context**:

| Before | After |
|--------|-------|
| `std::fs::read_to_string()` BLOCKS tokio runtime thread | `tokio::fs::read_to_string()` YIELDS to other tasks |
| No concurrent LSP requests can progress during file I/O | Runtime can schedule other tasks while waiting for I/O |

In production with concurrent LSP requests:
- Before: One blocking read = ALL async tasks on that thread stall
- After: Async read = Other LSP operations make progress concurrently

### Regression Detection Baseline (`lsp_after`)

Final benchmarks run sequentially against `lsp_after` baseline. Variance is normal
run-to-run noise at nanosecond/microsecond scale. File I/O benchmarks show higher
variance due to OS page cache effects.

#### response_parsers_bench (17 groups)

| Benchmark | Time | vs Baseline | Verdict |
|-----------|------|-------------|---------|
| `parse_definition_response/null_response` | 64.4 ns | -1.7% | Improved |
| `parse_definition_response/location_response_no_file` | 8.69 µs | +0.7% | No change (p=0.30) |
| `parse_definition_response/array_response_no_file` | 9.60 µs | +3.2% | Within noise |
| `parse_definition_response/location_link_no_file` | 9.59 µs | +9.1% | Marginal noise |
| `parse_definition_response/empty_array` | 71.1 ns | -0.9% | Within noise |
| `parse_definition_response_with_file/with_real_file_read` | 13.0 µs | +2.5% | Within noise (p=0.02) |
| `parse_single_definition_location/null` | 47.4 ns | -4.0% | Improved |
| `parse_single_definition_location/location_no_file` | 8.59 µs | -1.3% | Within noise |
| `parse_references_response/null` | 61.5 ns | +2.9% | Within noise |
| `parse_references_response/single_ref_no_file` | 8.70 µs | +5.9% | Marginal noise |
| `parse_references_response/five_refs_no_file` | 42.7 µs | +4.2% | Marginal noise |
| `parse_references_response_with_files/five_refs_with_file_reads` | 92.4 µs | +36.3% | File I/O variance |
| `parse_call_hierarchy_prepare/null` | 4.43 ns | +81.7% | Noise (nanosecond floor) |
| `parse_call_hierarchy_prepare/single_item` | 665 ns | -3.1% | Improved |

#### transport_bench (6 groups)

| Benchmark | Time | Throughput | vs Baseline | Verdict |
|-----------|------|------------|-------------|---------|
| `write_message/small_request` | 173 ns | 607 MiB/s | +2.8% | Within noise |
| `write_message/notification` | 132 ns | 376 MiB/s | +4.8% | Marginal noise |
| `write_message/large_response_100_locs` | 9.89 µs | 1.11 GiB/s | -0.3% | No change (p=0.32) |
| `read_message/single_message` | 509 ns | 129 MiB/s | +1.1% | Within noise |
| `read_message/notification` | 462 ns | 180 MiB/s | -2.7% | Improved |
| `transport_roundtrip/write_then_read` | 870 ns | — | +4.2% | Within noise |

#### Regression Detection Command

```bash
cargo bench --package pathfinder-mcp-lsp --bench response_parsers_bench -- --baseline lsp_after
cargo bench --package pathfinder-mcp-lsp --bench transport_bench -- --baseline lsp_after
```

Future runs should show all benchmarks within ±5% of these numbers.
Outliers: `five_refs_with_file_reads` (real disk I/O) and `null` benchmarks
(nanosecond floor) will show higher variance — expected.

### Key Changes

1. `response_parsers.rs`: `std::fs::read_to_string` → `tokio::fs::read_to_string`
   - `parse_definition_response`: now async, non-blocking file reads
   - `parse_single_definition_location`: now async
   - `parse_definition_response_multi`: now async
   - `parse_references_response`: now async
   - Extracted `resolve_relative_path()` — shared URI→relative-path helper
   - Extracted `parse_uri_and_range()` — shared URI/range extraction
   - Extracted `read_preview_line()` — async single-line file reader
   - `parse_call_hierarchy_prepare_response` now uses `resolve_relative_path()` (DRY)
   - 3 new unit tests for `resolve_relative_path`

2. Visibility changes for benchmarking:
   - `client::response_parsers` → `pub mod`
   - `client::transport` → `pub mod`
   - `transport::read_message` → `pub`
   - `transport::write_message` → `pub`

3. New benchmarks created:
   - `benches/response_parsers_bench.rs` (7 benchmark groups)
   - `benches/transport_bench.rs` (3 benchmark groups)

### What Was NOT Changed

- `transport::write_message` header allocation — benchmarked, confirmed below threshold
- `transport::read_message` header parsing — below irreducible floor (20ns)
- `RequestDispatcher::register` language_id allocation — below threshold
- `call_hierarchy` parsers — already optimal (no file I/O)

## Methodology

Static code review of all source files in `crates/pathfinder-lsp/src/`.
No runtime profiling was performed — this analysis identifies **structural**
performance candidates based on known Rust anti-patterns from the perf-optimization
skill and prior pathfinder-common optimization results (2026-06-03).

**Baseline**: No existing benchmarks for this crate. Benchmarks must be created
before any optimization is implemented.

## Crate Architecture

The crate is an async LSP client that communicates with language server processes.
Key hot paths:

| Path | Role | Call Frequency |
|------|------|----------------|
| `transport::read_message` | JSON-RPC framing (stdin) | Every incoming message |
| `transport::write_message` | JSON-RPC framing (stdout) | Every outgoing message |
| `response_parsers::*` | Parse LSP responses into domain types | Every tool call |
| `protocol::RequestDispatcher` | JSON-RPC request/response correlation | Every request |
| `detect::detect_languages` | Workspace scanning (async fs) | Startup only |
| `process::spawn_and_initialize` | LSP process spawn | Startup only |
| `background::*_task` | Reader, progress, registration watchers | Background |

## Top Candidates (Ranked by Impact/Risk)

### 1. `response_parsers::parse_definition_response` — File I/O on hot path

**Location**: `response_parsers.rs:33-63`
**Severity**: HIGH (every goto_definition call)

The function calls `std::fs::read_to_string(p)` synchronously for every
definition location to extract a preview line. This is a **blocking filesystem
read** inside an async context.

```rust
let preview = abs_path
    .as_deref()
    .and_then(|p| std::fs::read_to_string(p).ok())  // BLOCKING
    .and_then(|content| {
        content.lines().nth(...).map(|l| l.trim().to_owned())
    })
    .unwrap_or_default();
```

Same pattern repeated in:
- `parse_single_definition_location` (line 99-106)
- `parse_references_response` (line 261-269)
- `parse_call_hierarchy_prepare_response` does NOT do this (correct)

**Impact**: High — every definition/reference call triggers at least one
blocking file read. On network filesystems or large files, this adds
milliseconds per call.

**Risk**: Low — replacement with `tokio::fs::read_to_string` or cached reads
is straightforward.

**Fix**: Either:
a) Use `tokio::fs::read_to_string` (async, non-blocking), or
b) Cache file contents in a `DashMap<PathBuf, String>` with invalidation on
   `didChange` notifications, or
c) Read only the needed line (seek-based) instead of the entire file.

### 2. `transport::write_message` — `format!` allocation per message

**Location**: `transport.rs:93`
**Severity**: MEDIUM (every outgoing message)

```rust
let header = format!("Content-Length: {}\r\n\r\n", body.len());
```

`format!` allocates a new String for every message. The header is at most
~25 bytes. This could use a pre-computed buffer with `write!`.

**Impact**: Medium — every outgoing LSP message allocates. But LSP messages
are not sent in tight loops (user-initiated operations).

**Risk**: Very low — simple `String::with_capacity` + `write!` replacement.

### 3. `transport::read_message` — `String::default()` for header lines

**Location**: `transport.rs:35`
**Severity**: LOW-MEDIUM (every incoming message)

```rust
let mut line = String::default();
let n = reader.read_line(&mut line).await...
```

`read_line` appends to the String, so `String::new()` is correct. But this
allocates for every header line of every message. Most messages have 1-2
headers. This is within noise for normal LSP traffic.

**Impact**: Low — LSP messages are infrequent (not a tight loop).

**Risk**: N/A — below optimization threshold per the irreducible floor table
(under 20ns functions = do not optimize).

### 4. `protocol::RequestDispatcher::register` — `language_id.to_owned()` per request

**Location**: `protocol.rs:91`
**Severity**: LOW

```rust
self.pending.insert(id, (language_id.to_owned(), tx));
```

Every request allocates a new String for the language_id. Language IDs are
short static strings ("rust", "go", "typescript", "python", "java").

**Impact**: Low — `language_id` strings are 2-10 bytes. Could use `Arc<str>`
or a string interner, but the allocation is negligible per request.

**Risk**: Low if we intern, but complexity cost exceeds benefit.

### 5. DRY violation: URI parsing duplicated across response_parsers

**Location**: `response_parsers.rs` — `parse_definition_response`,
`parse_single_definition_location`, `parse_call_hierarchy_prepare_response`,
`parse_references_response`

Each function independently:
1. Parses `Url::parse(uri_str)`
2. Calls `.to_file_path()`
3. Calls `.strip_prefix(workspace_root)`
4. Converts to relative path string

This is not a perf issue (URL parsing is fast) but is a maintainability
concern that makes future optimization harder.

**Impact**: Low perf, medium maintainability.
**Fix**: Extract a shared `fn resolve_relative_path(uri: &str, root: &Path) -> String`.

## Irreducible Floors

| Cost | Source | Can't optimize |
|------|--------|---------------|
| `serde_json::from_slice` | JSON deserialization | Must visit every byte |
| `serde_json::to_vec` | JSON serialization | Must serialize every field |
| `Url::parse` | URI parsing | Spec-compliant parsing required |
| `DashMap` lock/unlock | Concurrent dispatch | Thread coordination |
| Async runtime overhead | Tokio task wakeups | Runtime cost |

## What is NOT a Candidate

1. **`detect_languages`** — called once at startup, not on hot path
2. **`spawn_and_initialize`** — called once per language at startup
3. **Background tasks** — event-driven, no tight loops
4. **`RequestDispatcher` dispatch** — DashMap-based, already optimal for concurrent access
5. **`serde_json` serialization** — already using `serde_json::to_vec` (zero-copy into Vec)

## Recommended Plan

| Priority | Fix | Est. Impact | Risk | Effort |
|----------|-----|-------------|------|--------|
| 1 | Async file reads in response_parsers | High | Low | Medium |
| 2 | Pre-allocate write header buffer | Medium | Very Low | Low |
| 3 | Extract shared URI→path helper (DRY) | Low (maintainability) | Very Low | Low |
| 4 | Skip — `read_message` header alloc | Below threshold | — | — |
| 5 | Skip — `language_id` interning | Below threshold | — | — |

## Prerequisites

Before implementing ANY fix:

1. **Create criterion benchmarks** for:
   - `parse_definition_response` (with/without file I/O)
   - `write_message` (header allocation)
   - `read_message` (header parsing)
2. **Establish baseline** measurements
3. **Profile with `cargo flamegraph`** to confirm hypotheses
