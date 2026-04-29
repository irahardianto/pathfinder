# Task: Remediation Patches 2026-04-29

**Current Phase:** COMPLETED

**Source:** `docs/requirements/patches/20260429/00-INDEX.md`

---

## Task Status

| # | Patch | Status | Started | Completed |
|---|-------|--------|---------|-----------|
| 1 | [PATCH-001] Serialization Safety | [x] | 2026-04-29 | 2026-04-29 |
| 2 | [PATCH-002] Diagnostic Diffing Deduplication | [x] | 2026-04-29 | 2026-04-29 |
| 3 | [PATCH-003] Input Validation Guards | [x] | 2026-04-29 | 2026-04-29 |
| 4 | [PATCH-004] Dead Code Removal | [x] | 2026-04-29 | 2026-04-29 |
| 5 | [PATCH-005] File System Edge Cases | [x] | 2026-04-29 | 2026-04-29 |
| 6 | [PATCH-006] False Positive Documentation | [x] | 2026-04-29 | 2026-04-29 |
| 7 | [PATCH-007] Search returned_count Field | [x] | 2026-04-29 | 2026-04-29 |

---

## All Patches Completed ✅

### PATCH-001: Serialization Safety ✅
- Added `serialize_metadata<T: serde::Serialize>(metadata: &T) -> Option<serde_json::Value>` to `helpers.rs`
- Replaced 7 call sites of `serde_json::to_value(...).unwrap_or_default()`
- Added test `test_serialize_metadata_success`

### PATCH-002: Diagnostic Diffing Deduplication ✅
- `collect_introduced`: Added `post_counts` parameter, removed `post_counts_local` HashMap
- `collect_resolved`: Added `pre_counts` parameter, removed `pre_counts_local` HashMap
- Updated `diffing:collect_introduced` and `diffing:collect_resolved` doc comments to use backticks around `HashMap`

### PATCH-003: Input Validation Guards ✅
- `navigation.rs::analyze_impact_impl`: `max_depth` clamped to 1..=5 with `.clamp(1, 5)` (floor at 1 to guarantee at least one level of traversal)
- `repo_map.rs`: `max_tokens` clamped to 500..=100_000
- `search.rs::search_codebase_impl`: empty/whitespace query rejected early with `query must not be empty`
- `text_edit.rs::resolve_text_edit`: added warning log when `context_line > total_lines` (comparing `usize`)
- Updated 2 tests that used empty queries:
  - `test_search_codebase_filter_mode_all_returns_everything`: `query: String::default()` → `query: "test".to_owned()`
  - `test_search_codebase_handles_scout_error`: `Default::default()` → explicit `query: "test".to_owned()`

### PATCH-004: Dead Code Removal ✅
- `server.rs::PathfinderServer`:
  - Removed unused `config: Arc<PathfinderConfig>` field and its `#[allow(dead_code)]`
  - Removed erroneous `#[allow(dead_code)]` from `sandbox` field (it IS actively used)
- `pathfinder-lsp/src/client/process.rs::ManagedProcess`:
  - Removed unused `language_id: String` field and its `#[allow(dead_code)]`
  - Removed `language_id: language_id.to_owned()` from struct initializer
  - Note: `language_id` parameter still used in `spawn_and_initialize` for tracing and `spawn_lsp_child` - just not stored in the struct

### PATCH-005: File System Edge Cases ✅
- `file_ops.rs::read_file_impl`: Added `ErrorKind::InvalidData` match arm
- New error message: "file appears to be binary (not valid UTF-8). read_file only supports text files."
- Replaces generic "failed" to read file" error for non-UTF-8 files

### PATCH-006: False Positive Documentation ✅
- `normalize.rs::strip_outer_braces`: Added Safety comment above byte-indexed slice (ASCII delimiters)
- `vue_zones.rs::byte_to_point`: Added doc comment explaining column is byte-based for tree-sitter interop
- `types.rs::WorkspaceRoot::resolve`: Added "Path traversal protection" and "Symlink behavior" sections
- `sandbox.rs::Sandbox::new`: Added comment explaining `.exists()` is one-time startup blocking I/O

### PATCH-007: Search returned_count Field ✅
- Added `returned_count: usize` field to `SearchCodebaseResponse`
- Set `returned_count = flat_matches.len()` in `search_codebase_impl` handler
- Updated doc comment to use backticks around `filter_mode`

---

## Global Verification ✅

```bash
cargo fmt --check          # ✅ OK
cargo clippy --all-targets --all-features -- -D warnings  # ✅ OK
cargo test --all           # ✅ 614 tests passed
```

## Commit: 8872271

```
refactor(core): implement 2026-04-29 patch batch (7 patches)
```

All 7 remediation patches successfully implemented, verified, and committed.

## Gap Fixes Applied During Final Audit

1. **PATCH-004** - `config` field removal was incomplete in original implementation. Fixed by:
   - Removing `config: Arc<PathfinderConfig>` field from `PathfinderServer` struct
   - Removing `#[allow(dead_code)]` from both `config` and `sandbox` fields
   - Removing `config: Arc::new(config)` from struct initializer
   - Adding `#[allow(clippy::needless_pass_by_value)]` to `with_all_engines` for API compatibility

2. **PATCH-003** - `max_depth=0` edge case fixed:
   - Changed from `.min(5)` to `.clamp(1, 5)`
   - Guarantees at least one level of BFS traversal
   - Agents passing `max_depth=0` will no longer get misleading "zero references" results

3. **PATCH-006** - `spawn_blocking` migration path added to `Sandbox::new` comment:
   - Original comment didn't mention `spawn_blocking` for async-host migration
   - Now explicitly documents: "should move into `tokio::task::spawn_blocking`" for future multi-tenant async host embedding

4. **PATCH-007** - `total_matches` documentation corrected:
   - Was: "before filtering or truncation"
   - Now: "before `filter_mode` filtering, **after** ripgrep truncation"
   - Correctly reflects that when `truncated=true`, `total_matches == max_results`
