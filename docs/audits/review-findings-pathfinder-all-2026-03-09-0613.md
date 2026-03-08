# Code Audit: Full Codebase (Post-LSP Validation Pipeline)
Date: 2026-03-09
Reviewer: AI Agent (fresh context)

## Summary
- **Files reviewed:** 12 files changed since last audit (commit `5e83505`), across 2 crates (`pathfinder`, `pathfinder-lsp`)
- **Issues found:** 5 (0 critical, 1 major, 2 minor, 2 nit)
- **Test coverage:** 280 tests, all passing

## Critical Issues
None.

## Major Issues

- [x] ~~**[RES]** LSP validation pipeline never sends `textDocument/didClose`~~ — **RESOLVED**: `did_close` method added to `Lawyer` trait (lawyer.rs:77–85) with implementations in `LspClient`, `MockLawyer`, and `NoOpLawyer`. Call added to `run_lsp_validation` (edit.rs:1041) after the revert step, fired as fire-and-forget to prevent LSP memory leaks.

## Minor Issues

- [x] ~~**[TEST]** No unit tests for `run_lsp_validation`~~ — **RESOLVED**: 8 unit tests added to `edit.rs` covering all 5 LSP validation failure modes: `no_lsp` (did_open returns `NoLspAvailable`), `unsupported` (did_open returns `UnsupportedCapability`), `pre_diag_timeout` (first `pull_diagnostics` errors), `pull_diagnostics_unsupported` (first `pull_diagnostics` returns `UnsupportedCapability`), `post_diag_timeout` (second `pull_diagnostics` errors); plus `blocking` (`should_block=true` when new errors introduced), `blocking_ignored` (ignored when `ignore_validation_failures=true`), and `happy_path` (no new errors → `status="passed"`). `MockLawyer` extended with `did_open_error` fixture. `UnsupportedDiagLawyer` test double added for injecting `UnsupportedCapability` from `pull_diagnostics`. Total tests: 288 (up from 280). — [edit.rs](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder/src/server/tools/edit.rs)

## Nit

- [x] ~~**[PAT]** `cargo fmt` reveals unformatted code~~ — **RESOLVED**: `cargo fmt --all` applied, formatting check passes cleanly.

- [x] ~~**[PAT]** Heavy structural duplication across 5 edit tools~~ — **RESOLVED**: `finalize_edit` helper (edit.rs:1136–1198) encapsulates the `run_lsp_validation` → blocking check → `flush_edit_with_toctou` → logging → `EditResponse` tail. All 5 edit tools (`replace_body`, `replace_full`, `insert_before`, `insert_after`, `delete_symbol`) now delegate to it, removing ~150 lines of duplicated code.

## Verification Results
- Lint: **PASS** (`cargo clippy --workspace --all-targets` — 0 warnings)
- Tests: **PASS** (288 passed, 0 failed)
- Build: **PASS**
- Fmt: **PASS** (`cargo fmt --all --check` — clean)

## Previously Unresolved Findings (From 2026-03-09 0504 Audit)
- [x] ~~**[PAT]** Byte-based indentation logic in `pathfinder-common/src/indent.rs`~~ — **ALREADY RESOLVED**: `expand_tabs()` was already implemented in the current codebase, expanding tabs to 4-column boundaries before `min_indent` and `dedent` measure character counts. `test_dedent_tab_indented_normalises_to_spaces` validates the fix. No action needed.

## Open Findings Summary
All findings across all audits are resolved. No open items.

## Rules Applied
- Resources and Memory Management Principles (LSP document leak — resolved)
- Testing Strategy (missing unit tests on critical validation — partially resolved)
- Rust Idioms and Patterns (formatting, clippy compliance — resolved)
- Architectural Patterns (Lawyer trait — I/O behind interface, verified)
- Code Organization Principles (DRY — finalize_edit helper — resolved)
- Security Mandate (sandbox, OCC, TOCTOU — all verified)
- Error Handling Principles (graceful degradation on all LSP errors — verified)
- Logging and Observability Mandate (start/complete logging on all operations — verified)
