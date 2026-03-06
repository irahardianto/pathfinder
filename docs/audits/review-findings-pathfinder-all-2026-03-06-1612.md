# Code Audit: Pathfinder — Full Codebase
Date: 2026-03-06
Reviewer: AI Agent (fresh context)

## Summary
- **Files reviewed:** 26 (`pathfinder/`: 5, `pathfinder-common/`: 6, `pathfinder-search/`: 5, `pathfinder-treesitter/`: 10)
- **Issues found:** 7 (1 critical, 3 major, 2 minor, 1 nit) — **all resolved in commit `89280f5`**
- **Test count at audit:** 80 passed, 0 failed, 1 ignored (doc-test)
- **Test count post-fix:** 83 passed, 0 failed (3 new tests added)
- **Crates audited:** `pathfinder`, `pathfinder-common`, `pathfinder-search`, `pathfinder-treesitter`

## Critical Issues

- [x] **[SEC]** `repo_map.rs::generate_skeleton_text` calls `std::fs::read(path).unwrap_or_default()` (line 155) inside an `async fn`. This is **blocking I/O on the async runtime** — it blocks the Tokio worker thread during the read. Per `rust-idioms-and-patterns.md` §3: "Never call blocking I/O inside async context. Use `tokio::fs` instead of `std::fs` inside async functions." Furthermore, `unwrap_or_default()` silently treats read errors (permission denied, file locked) as an empty file, which corrupts the version hash — agents will receive a `sha256` hash of empty bytes, causing immediate `VERSION_MISMATCH` on any subsequent OCC-gated edit. — [repo_map.rs:155](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-treesitter/src/repo_map.rs#L155)
  > **Resolved:** Replaced with `tokio::fs::read(path).await`; unreadable files are now skipped with `tracing::warn!` instead of silently corrupting the hash.

## Major Issues

- [x] **[ARCH]** Rust `impl` blocks are not extracted by the symbol extraction engine. `language.rs` defines `method_kinds` for Rust as an empty slice (line 78) with the comment "Inside `impl_item`" but never actually handles `impl_item`. This means Rust methods inside `impl` blocks (the standard pattern in Rust) are invisible to `get_repo_map`, `read_symbol_scope`, and `enclosing_symbol`. For a tool that prominently supports Rust, this is a significant gap — the repo map for any Rust project will only show free functions, structs, enums, and traits, but no methods. — [language.rs:75-80](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-treesitter/src/language.rs#L75-L80)
  > **Resolved:** Added `impl_kinds: &["impl_item"]` to `LanguageNodeTypes`; new `extract_impl_block()` function extracts associated functions as `SymbolKind::Method` children under the implementing type (e.g. `MyStruct.foo`). Two new unit tests added: `test_extract_rust_impl_methods`, `test_extract_rust_free_functions_unchanged`.

- [x] **[ERR]** `delete_file` handler has a TOCTOU race between the `absolute_path.exists()` check (line 657) and the subsequent `tfs::read()` (line 666). A concurrent deletion between these two operations causes a confusing `failed to read file` I/O error instead of the expected `FILE_NOT_FOUND` error. The fix is to remove the `exists()` pre-check and handle `NotFound` from the `tfs::read()` call directly, matching the pattern already used in `read_file` and `write_file`. — [server.rs:657-672](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder/src/server.rs#L657-L672)
  > **Resolved:** Removed `.exists()` pre-check; `tfs::read()` now handles `ErrorKind::NotFound` directly with a structured `FILE_NOT_FOUND` error response.

- [x] **[PAT]** `get_repo_map` handler accepts a `visibility` parameter (typed as raw `String` with default `"public"`) but the implementation silently ignores it — `generate_skeleton_text` receives `_visibility: &str` with a comment "Currently ignored, treats everything as public" (line 107). Agents requesting `visibility: "public"` (the default) will receive all symbols including private ones, which is misleading. At minimum, the tool description or response should indicate the parameter is not yet implemented, similar to how `filter_mode` sets `degraded: true` in `search_codebase`. — [repo_map.rs:107](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-treesitter/src/repo_map.rs#L107), [types.rs:49-51](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder/src/server/types.rs#L49-L51)
  > **Resolved:** `GetRepoMapResponse` now includes `visibility_degraded: Some(true)` so agents know the parameter has no effect. `visibility` and `include_imports` replaced with typed enums (`Visibility`, `IncludeImports`) matching the `FilterMode` pattern.

## Minor Issues

- [x] **[PAT]** `generate_skeleton_text` contains stream-of-consciousness comments that read like working notes rather than documentation (lines 149-154: "Wait, extract_symbols doesn't return version_hash.", "We probably need to read the file manually..."). These should be cleaned up to concise documentation comments explaining the design decision. — [repo_map.rs:149-154](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-treesitter/src/repo_map.rs#L149-L154)
  > **Resolved:** Replaced with a concise design-decision comment.

- [x] **[PAT]** `GetRepoMapParams::visibility` is a raw `String` (default `"public"`), but validation is not implemented. Any arbitrary string (e.g., `"foobar"`) is silently accepted. This should either be a typed enum (like `FilterMode`) or validated with an early-return error. The same applies to `include_imports` (default `"third_party"`). — [types.rs:49-55](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder/src/server/types.rs#L49-L55)
  > **Resolved:** `Visibility { Public, All }` and `IncludeImports { None, ThirdParty, All }` typed enums added to `types.rs`. Invalid values now fail at deserialization.

## Nit

- [x] `cargo fmt --check` reports formatting violations in `repo_map.rs` (trailing whitespace, single-line `let-else`, inline conditionals), `mock.rs` (long `assert!` not wrapped), and `server.rs` (trailing whitespace in test). Run `cargo fmt` to fix. — multiple files
  > **Resolved:** `cargo fmt --all` applied.

## Verification Results (at time of audit)
- **Fmt:** FAIL (formatting issues in `repo_map.rs`, `mock.rs`, `server.rs`)
- **Clippy:** PASS (0 warnings, 0 errors)
- **Tests:** PASS (80 passed, 0 failed, 1 ignored)
- **Build:** PASS (implied by tests)
- **Coverage:** Not measured (no coverage tooling configured)

## Resolution (2026-03-06) — commit `89280f5`
- **Fmt:** PASS
- **Clippy:** PASS (0 warnings)
- **Tests:** PASS (83 passed, 0 failed — 3 new tests added)

## Comparison with Previous Audit (2026-03-06-0940)
All 8 issues from the previous audit have been resolved:
- ✅ `search_codebase` logging moved to start of handler
- ✅ Stub handlers now log start/complete via `stub_response()` helper
- ✅ Server-level unit tests added (11 tests in `pathfinder` crate)
- ✅ `RipgrepScout::search_path` errors now propagated, not silently discarded
- ✅ `FilterMode` typed enum used instead of raw `String`
- ✅ `sandbox.rs::is_user_denied` no longer performs live I/O stat
- ✅ `VecDeque` used for context_before_buf (was `Vec::remove(0)`)
- ✅ `#[allow(clippy::unused_self)]` scoped to individual stub methods

## Rules Applied
- `rust-idioms-and-patterns.md` — blocking I/O in async, typed enums, `cargo fmt`
- `logging-and-observability-mandate.md` — silent failures, error logging
- `error-handling-principles.md` — TOCTOU races, `unwrap_or_default` on I/O
- `architectural-pattern.md` — symbol extraction completeness
- `code-organization-principles.md` — working-note comments, pattern consistency
- `documentation-principles.md` — self-documenting code
