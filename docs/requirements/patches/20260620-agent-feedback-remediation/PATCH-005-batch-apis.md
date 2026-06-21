# PATCH-005: Batch APIs for inspect & locate

Date: 2026-06-20
Source: Pathfinder report (Rust stack), Bank-of-Anthos report (Java/Python)
Status: Implemented

## Problem Statement

Currently only `read()` supports batch operations via the `paths` parameter
(max 10 files). `inspect`, `locate`, and `trace` are all single-operation.
Agents frequently need to resolve multiple symbols, requiring N separate
MCP round-trips.

Example: inspecting 5 symbols from a `trace()` result requires 5 separate
`inspect()` calls. Each MCP call has overhead (JSON serialization, transport,
response parsing). Batch support reduces this to 1 call.

**Existing pattern** — `read()` batch:
- `ReadParams` accepts `filepath` (single) OR `paths: Vec<String>` (max 10)
- Returns array of results with per-file status
- Single-file mode unchanged (backward compatible)

---

## DELIVERABLE A: Batch inspect

Priority: P2
Effort: Medium (2-3 hours)
Risk: Low (new parameter, backward compatible)

**Design**:
- Add `semantic_paths: Option<Vec<String>>` parameter (max 10)
- When `semantic_paths` is provided, `semantic_path` is ignored
- Return array of results with per-symbol succeeded/failed status
- `include_dependencies` applies to all inspected symbols

**Steps**:

1. In types.rs, update `InspectParams`:
   ```rust
   #[serde(skip_serializing_if = "Option::is_none")]
   pub semantic_paths: Option<Vec<String>>,
   ```
   - Validation: if `semantic_paths` is Some, len must be 1-10
   - If both `semantic_path` and `semantic_paths` provided, prefer
     `semantic_paths`

2. In inspect tool implementation:
   - If `semantic_paths` is Some: iterate and call existing single-inspect
     logic for each
   - Use bounded concurrency: `buffer_unordered(4)` for parallel LSP
     requests
   - Collect results into `BatchInspectResult`

3. Define batch response type:
   ```rust
   pub struct BatchInspectResult {
       pub results: Vec<InspectResultEntry>,
       pub succeeded: usize,
       pub failed: usize,
       pub total_duration_ms: u64,
   }

   pub struct InspectResultEntry {
       pub semantic_path: String,
       pub status: String,  // "ok" or "error"
       pub source: Option<String>,
       pub start_line: Option<u32>,
       pub end_line: Option<u32>,
       pub language: Option<String>,
       pub dependencies: Option<Vec<Dependency>>,
       pub error: Option<String>,
   }
   ```

4. When single `semantic_path` is used: return existing single response
   format unchanged (backward compat).

5. Add tests:
   - `test_inspect_batch_multiple_symbols`
   - `test_inspect_batch_partial_failure`
   - `test_inspect_batch_max_10_limit`
   - `test_inspect_batch_empty_returns_error`
   - `test_inspect_single_unchanged` (backward compat)
   - `test_inspect_batch_with_dependencies`

**Files to modify**:
- Types file (`InspectParams`, new response types)
- Inspect tool implementation
- Tool schema (expose `semantic_paths` parameter)

**Acceptance**:
- `inspect(semantic_paths=["a.rs::foo", "b.rs::bar"])` returns both in
  single round-trip
- Partial failures don't block other results
- Max 10 paths enforced
- Single `semantic_path` still works unchanged

---

## DELIVERABLE B: Batch locate

Priority: P2
Effort: Medium (2-3 hours)
Risk: Low (new parameter, backward compatible)

**Design**:
- Add `locations: Option<Vec<LocateEntry>>` parameter (max 10)
- Each `LocateEntry` is either `{ semantic_path }` or `{ file, line }`
- Return array of results with per-entry status

**Steps**:

1. In types.rs, add:
   ```rust
   #[derive(Deserialize, Serialize)]
   pub struct LocateEntry {
       #[serde(skip_serializing_if = "Option::is_none")]
       pub semantic_path: Option<String>,
       #[serde(skip_serializing_if = "Option::is_none")]
       pub file: Option<String>,
       #[serde(skip_serializing_if = "Option::is_none")]
       pub line: Option<u32>,
   }
   ```

2. Update `LocateParams`:
   ```rust
   #[serde(skip_serializing_if = "Option::is_none")]
   pub locations: Option<Vec<LocateEntry>>,
   ```
   - Max 10 validation
   - If `locations` is Some, ignore single `semantic_path`/`file`/`line`

3. In locate tool implementation:
   - If `locations` is Some: iterate, call existing single-locate logic
   - Use bounded concurrency: `buffer_unordered(4)`
   - Collect into `BatchLocateResult`

4. Define batch response type:
   ```rust
   pub struct BatchLocateResult {
       pub results: Vec<LocateResultEntry>,
       pub succeeded: usize,
       pub failed: usize,
   }

   pub struct LocateResultEntry {
       pub input: LocateEntry,  // echo back for correlation
       pub status: String,
       pub file: Option<String>,
       pub line: Option<u32>,
       pub column: Option<u32>,
       pub semantic_path: Option<String>,
       pub preview: Option<String>,
       pub resolution_strategy: Option<String>,
       pub error: Option<String>,
   }
   ```

5. Add tests:
   - `test_locate_batch_semantic_paths`
   - `test_locate_batch_file_line_pairs`
   - `test_locate_batch_mixed_modes`
   - `test_locate_batch_partial_failure`
   - `test_locate_batch_max_10_limit`
   - `test_locate_single_unchanged` (backward compat)

**Files to modify**:
- Types file (`LocateEntry`, `LocateParams`, new response types)
- Locate tool implementation
- Tool schema (expose `locations` parameter)

**Acceptance**:
- `locate(locations=[{semantic_path: "a.rs::foo"}, {file: "b.rs", line: 42}])`
  returns all results in single round-trip
- Mixed mode (some semantic_path, some file+line) works
- Max 10 entries enforced
- Single-mode locate still works unchanged

---

## Dependency Order

A and B are independent — can be done in parallel.

Both benefit from PATCH-002 being done first (consistent `kind=type`
handling if inspect results include type information).

## Verification Plan

```bash
cargo test  # all relevant crates
cargo clippy -- -D warnings
```

Manual verification:
- Call inspect with `semantic_paths` array, verify batch response format
- Call locate with `locations` array, verify batch response format
- Verify single-operation mode unchanged (backward compat)
- Verify partial failures (one bad path, others good) work correctly

Total effort: ~4-6 hours
