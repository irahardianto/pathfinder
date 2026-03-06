# Code Audit: Pathfinder — Full Codebase
Date: 2026-03-06
Reviewer: AI Agent (fresh context)

## Summary
- **Files reviewed:** 14 (`pathfinder/`: 3, `pathfinder-common/`: 6, `pathfinder-search/`: 5)
- **Issues found:** 8 (0 critical, 4 major, 3 minor, 1 nit)
- **Test count:** 50 passed, 0 failed
- **Crates audited:** `pathfinder`, `pathfinder-common`, `pathfinder-search`

## Critical Issues
_None._

## Major Issues

- [x] **[OBS]** `search_codebase` logs start AFTER parameter building — if `SearchParams` construction panics (e.g., u32 cast overflow for huge values), there is no log at all. Log should be the **first** action inside the handler. — [server.rs:361–383](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder/src/server.rs#L361-L383)

- [x] **[OBS]** All 15 stub tool handlers (`get_repo_map`, `read_symbol_scope`, etc.) have **zero observability** — no start/success/failure logging. Per the Logging and Observability Mandate, every operation entry point must log start, success, and failure. Even a stub must log that it was called and returned `NOT_IMPLEMENTED`. — [server.rs:431–590](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder/src/server.rs#L431-L590)

- [x] **[TEST]** `ripgrep.rs` integration tests create real files via `tempfile::TempDir` — this is correct for an integration/adapter test, but there are **no `pathfinder` server-level unit tests** at all (`pathfinder_lib` reports 0 tests). The `PathfinderServer::with_scout` constructor exists and accepts `MockScout`, but is never exercised. At minimum, a test asserting that `search_codebase` routes correctly to the scout and handles error responses is required. — [pathfinder/src/lib.rs](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder/src/lib.rs)

- [x] **[ERR]** In `RipgrepScout::search`, `searcher.search_path()` errors are silently discarded via `let _ = searcher.search_path(...)`. A per-file I/O error means a file is silently skipped with no signal to the caller, which can lead an agent to believe a file has no matches when in reality it failed to read. — [ripgrep.rs:343](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-search/src/ripgrep.rs#L343)

## Minor Issues

- [x] **[PAT]** `filter_mode` in `SearchCodebaseParams` is a raw `String` (`"all"`, `"code_only"`, `"comments_only"`), but `pathfinder-common/src/types.rs` already defines a typed `FilterMode` enum with the same variants. The server deserializes `filter_mode` as `String` and then compares with `params.filter_mode != "all"`. This creates a footgun (any misspelled string will silently accept without being caught). The `FilterMode` struct from `types.rs` should be used directly. — [server.rs:88–89](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder/src/server.rs#L88-L89)

- [x] **[PAT]** `sandbox.rs::is_user_denied` calls `full_path.is_dir()` which performs a live filesystem stat inside the `check()` method. This breaks the pure-function contract for tier-3 enforcement: a pure logic check now does I/O. In most test paths this is harmless (temp dirs exist), but it makes the method non-deterministic if the workspace root disappears mid-run. — [sandbox.rs:231–232](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-common/src/sandbox.rs#L231-L232)

- [x] **[PAT]** `MatchCollector` in `ripgrep.rs` uses `Vec::remove(0)` on `context_before_buf` to maintain the sliding window (line 170). `Vec::remove(0)` is O(n) (must shift all elements left). For large `context_lines` values this degrades linearly. A `VecDeque` would give O(1) pop-front. — [ripgrep.rs:170](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-search/src/ripgrep.rs#L170)

## Nit

- [x] `server.rs`: The 15 stub handlers all use `#[allow(clippy::unused_self)]` at the `impl` block level. The comment above explains why, which is good, but the lint suppression scope is wider than necessary — it suppresses for all methods in that block including the implemented `search_codebase`. Consider moving `#[allow(clippy::unused_self)]` to only the stub handler methods. — [server.rs:350](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder/src/server.rs#L350)

## Verification Results
- **Fmt:** PASS (`cargo fmt --check` — zero violations)
- **Clippy:** PASS (0 warnings, 0 errors)
- **Tests:** PASS (50 passed, 0 failed, 1 ignored)
- **Build:** PASS (implied by tests)
- **Coverage:** Not measured (no coverage tooling configured)

## Rules Applied
- `logging-and-observability-mandate.md` — every operation entry point must log start/success/failure
- `architectural-pattern.md` — I/O behind interfaces; business logic pure
- `error-handling-principles.md` — no silently discarded errors
- `rust-idioms-and-patterns.md` — typed enums over stringly-typed values; algorithmic complexity
- `code-organization-principles.md` — pattern consistency across feature modules
- `testing-strategy.md` — consumer of an interface must have unit tests exercising it
