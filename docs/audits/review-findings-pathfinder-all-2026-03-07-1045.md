# Code Audit: Full Codebase (Post-LSP Integration)
Date: 2026-03-07
Reviewer: AI Agent (fresh context)

## Summary
- **Files reviewed:** ~35 source files across 5 crates (`pathfinder`, `pathfinder-common`, `pathfinder-search`, `pathfinder-treesitter`, `pathfinder-lsp`)
- **Issues found:** 7 (0 critical, 3 major, 2 minor, 2 nit)
- **Test coverage:** 198 tests, all passing
- **Focus:** First audit of the new `pathfinder-lsp` crate; delta review of other crates since last audit

## Critical Issues
None.

## Major Issues

- [x] **[ERR]** `path_to_file_uri()` manually constructs `file://` URIs instead of using the `url::Url` crate — The function at `process.rs:216-229` uses `format!("file://{path_str}")` to build URIs. This is incorrect for paths containing special characters (spaces, `%`, `#`, `?`, non-ASCII) which must be percent-encoded per RFC 8089. The `url::Url::from_file_path()` function (already imported via the `url` crate in `client/mod.rs:32`) handles this correctly. The same crate already uses `Url::from_file_path` in `goto_definition` (line 267), making this an inconsistency. — [process.rs:216-229](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src/client/process.rs#L216-L229)

- [x] **[RES]** `detect_languages()` uses blocking `std::fs` (`.exists()`, `.is_dir()`) in a synchronous function called during `LspClient::new()` → `PathfinderServer::new()` — This function calls `Path::exists()` up to 7 times (7 marker files), each a blocking `stat(2)` syscall. Since `LspClient::new()` is called from `PathfinderServer::new()` and the server starts on the tokio runtime, this blocks the async executor. While the impact is small (7 fast filesystem checks on the local disk), it violates the Rust Idioms rule "Never call blocking I/O inside async context" and creates a precedent. Should either be made `async` using `tokio::fs`, or wrapped in `spawn_blocking`.  — [detect.rs:34-79](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src/client/detect.rs#L34-L79)

- [x] **[SEC]** `lsp-types = "0.97"` declared as a dependency in `pathfinder-lsp/Cargo.toml` but never used in any source file — The crate is mentioned only in a comment in `capabilities.rs:9`. Unused dependencies increase attack surface and compile time. Per the Rust Idioms rule: "Minimize dependency count — each dependency is an attack surface." — [Cargo.toml:14](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/Cargo.toml#L14)

## Minor Issues

- [x] **[PAT]** `path_to_file_uri()` uses `path.is_dir()`, another blocking `std::fs` call inside what becomes async context — Same root cause as F2, but isolated to a single call in the shutdown/init path. When `path_to_file_uri` is replaced with `Url::from_file_path` (F1), this also gets resolved. — [process.rs:222](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src/client/process.rs#L222)

- [x] **[TEST]** `process.rs` has no unit tests — `spawn_and_initialize`, `send`, `start_reader_task`, and `shutdown` are untested. These are the most critical functions in the LSP lifecycle (crash recovery, process management). While they are hard to unit test without a real LSP binary, the `path_to_file_uri` helper function can and should be unit tested. The lifecycle functions should be covered by integration tests in a future `/e2e-test` cycle. — [process.rs](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src/client/process.rs)

## Nit

- [x] `cargo fmt` not run on `pathfinder-lsp` crate — Multiple formatting deviations detected by `cargo fmt --check`. All are trivial whitespace/line-break issues. Run `cargo fmt --all` to fix. — [pathfinder-lsp/src/](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src)

- [x] Extra trailing blank lines in `process.rs:230-232`, `protocol.rs:117-118`, `error.rs:46-47`, `lib.rs:28-29`, `capabilities.rs:131-133` — Minor style issue, will be auto-fixed by `cargo fmt`.

## Verification Results
- Lint: **PASS** (`cargo clippy --workspace --all-targets` — 0 warnings)
- Tests: **PASS** (198 passed, 0 failed across all crates)
- Build: **PASS**
- Fmt: **FAIL** (formatting issues in `pathfinder-lsp` crate — run `cargo fmt --all`)
- Coverage: N/A

## Previously Resolved Findings

All 7 findings from the [2026-03-07 audit](review-findings-pathfinder-all-2026-03-07.md) are verified resolved.
Both nits from the [2026-03-07 edit-tools audit](review-findings-edit-tools-2026-03-07-0621.md) are verified resolved (`flush_edit_with_toctou` helper extracted, path resolution unified).

## Recommended Fix Workflows

| Finding | Type           | Workflow                   |
| ------- | -------------- | -------------------------- |
| F1      | Isolated fix   | `/quick-fix`               |
| F2      | Isolated fix   | `/quick-fix`               |
| F3      | Dependency     | `/quick-fix`               |
| F4      | Resolved by F1 | (included in F1)           |
| F5      | Missing tests  | `/2-implement`             |
| Nits    | Formatting     | `cargo fmt --all` (direct) |

## Rules Applied
- Rust Idioms and Patterns (blocking I/O in async, dependency management, `cargo fmt`)
- Security Mandate (attack surface from unused dependencies)
- Rugged Software Constitution (defense-in-depth URI encoding)
- Testing Strategy (missing tests on critical paths)
- Concurrency and Threading Mandate (blocking I/O in async context)
- Code Completion Mandate (`cargo fmt` must pass)
- Core Design Principles (DRY, pattern consistency)
