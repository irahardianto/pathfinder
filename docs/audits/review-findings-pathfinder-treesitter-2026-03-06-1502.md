# Code Audit: Pathfinder Tree-sitter Integration
Date: 2026-03-06

## Summary
- **Files reviewed:** 6 (`lib.rs`, `surgeon.rs`, `treesitter_surgeon.rs`, `symbols.rs`, `cache.rs`, `parser.rs`, and parts of `server.rs`)
- **Issues found:** 4 (0 critical, 1 major, 3 minor)
- **Test coverage:** 100% of the 14 new tests in `pathfinder_treesitter` pass, along with the full workspace suite.

## Critical Issues
None found.

## Major Issues
Issues that should be fixed in the near term.
- [x] **[PERF/ARCH] Cache Inefficiency** — In `AstCache::get_or_parse`, `tokio::fs::read` and `VersionHash::compute` are called *unconditionally* before checking the cache. This means every cache hit still incurs a full disk read and hashing overhead, negating the primary performance benefit of the in-memory AST cache. Consider using file mtime (stat) for fast-path invalidation or only reading if missing/changed. — [crates/pathfinder-treesitter/src/cache.rs:58](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-treesitter/src/cache.rs#L58)
  - **Fix:** Introduced `mtime: SystemTime` field in `CacheEntry`. `get_or_parse` now calls `tokio::fs::metadata` first (single `stat(2)` syscall). On cache hit with matching `mtime` and `lang`, returns the clone directly — no disk read. The `Mutex` is dropped before the async `tokio::fs::read` call to avoid holding it across `await`.

## Minor Issues
Style, naming, or minor improvements.
- [x] **[PAT] Clippy failures in tests** — The workspace lint `unsafe_code = "deny"` (or similar strict lints) causes `unwrap_used` and `unwrap_err_used` to fail the build in `pathfinder-treesitter/src/treesitter_surgeon.rs` tests. Add `#![allow(clippy::unwrap_used)]` to the test module to fix `cargo clippy --workspace` errors. — [crates/pathfinder-treesitter/src/treesitter_surgeon.rs:167](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-treesitter/src/treesitter_surgeon.rs#L167)
  - **Fix:** Added `#[allow(clippy::unwrap_used)]` to the `mod tests` blocks in both `treesitter_surgeon.rs` and `cache.rs`.
- [x] **[ARCH] Cache Stampede Risk** — Concurrent requests for the same file under load will all concurrently read and parse because the lock is not held across the `await` boundaries. This is mostly acceptable for a local CLI/IDE server but technically a race condition resulting in redundant work. — [crates/pathfinder-treesitter/src/cache.rs:58](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-treesitter/src/cache.rs#L58)
  - **Fix (doc-only, YAGNI):** Added a `// NOTE:` comment in `AstCache`'s doc block documenting the known limitation and the `tokio::sync::OnceCell` singleflight path as a future remedy if contention becomes measurable.
- [x] **[PERF] String allocations in `did_you_mean`** — Flattens all `ExtractedSymbol` paths into new strings to map and compute Levenshtein distances for *every* symbol in the file. Might be slow on unusually large files with thousands of symbols, though perfectly fine for v1. — [crates/pathfinder-treesitter/src/symbols.rs:137](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-treesitter/src/symbols.rs#L137)
  - **Fix:** `collect_paths` now collects `&'a str` references (zero cloning). `levenshtein` receives `&str` directly. Only the final `max_suggestions` paths are converted to `String` at collection time.

## Verification Results
- Lint: PASS (`cargo clippy --workspace -- -D warnings` — 0 warnings, 0 errors)
- Tests: PASS (14 passed in `pathfinder_treesitter`, 83 total passed workspace-wide)
- Build: PASS
- Format: PASS (`cargo fmt --check` — clean)
