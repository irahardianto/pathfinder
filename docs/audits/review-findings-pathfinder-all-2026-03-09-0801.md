# Code Audit: Full Codebase
Date: 2026-03-09
Reviewer: AI Agent (fresh context)

## Summary
- **Files reviewed:** 48 source files across 5 crates (`pathfinder`, `pathfinder-common`, `pathfinder-lsp`, `pathfinder-search`, `pathfinder-treesitter`)
- **Issues found:** 3 (0 critical, 1 major, 2 minor)
- **Test coverage:** 264 tests, all passing

## Critical Issues
None.

## Major Issues
- [x] **[RES]** `run_lsp_validation` never sends `textDocument/didClose` — After a successful `did_open` call, the function has **five** early-return paths (pre-edit `pull_diagnostics` error, `pull_diagnostics_unsupported`, `did_change` error, post-edit `pull_diagnostics` error) and one happy path that all exit **without** calling `did_close`. The revert `did_change` (line 782–785) restores the original content but does not close the document. Each edit tool invocation leaks one document in the LSP's tracked-document set, consuming memory until the LSP process is idle-terminated. The previous audit (0613) incorrectly marked this as resolved. **Fixed:** added `did_close` fire-and-forget on all 5 code paths (4 early-return exit points + end of happy path). Added `#[expect(clippy::too_many_lines)]` with justification since the pipeline grew 2 lines past the limit. — [edit.rs:682–800](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder/src/server/tools/edit.rs#L682-L800)

## Minor Issues
- [x] **[OBS]** `insert_before_impl` and `insert_after_impl` are missing the `tracing::info!(... "start")` log line — All other edit tools (`replace_body`, `replace_full`, `delete_symbol`, `validate_only`) emit a structured `"<tool_name>: start"` log at entry. `insert_before_impl` (line 238) and `insert_after_impl` (line 330) skip this log while still having `#[instrument]` on the function. The start log provides valuable latency attribution and is part of the 3-point operation logging contract (start/complete/fail). **Fixed:** added `tracing::info!` start logs to both functions. — [edit.rs:238](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder/src/server/tools/edit.rs#L238), [edit.rs:330](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder/src/server/tools/edit.rs#L330)

- [x] **[OBS]** `replace_full_impl` silently swallows sandbox denial without logging — When the sandbox check fails at line 186, `replace_full_impl` returns the error but does **not** emit a `tracing::warn!` log like the other four edit tools do. `replace_body` (line 94–100) and `delete_symbol` (line 438–440) both log the sandbox denial before returning. This makes it harder to diagnose access-denied errors in production when only `replace_full` is used. **Fixed:** added `tracing::warn!` with `tool`, `semantic_path`, and `error` fields before returning `Err`. — [edit.rs:185–187](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder/src/server/tools/edit.rs#L185-L187)

## Nit
None.

## Verification Results
- Lint: **PASS** (`cargo clippy --workspace --all-targets` — 0 warnings)
- Tests: **PASS** (264 passed, 0 failed)
- Build: **PASS**
- Fmt: **PASS** (`cargo fmt --all --check` — clean)
- Post-fix re-verification: **PASS** (all checks green after fixes applied)

## Recommended Fix Workflows

| Finding | Type              | Workflow         |
| ------- | ----------------- | ---------------- |
| Major   | Resource leak     | `/quick-fix`     |
| Minor 1 | Missing logs      | `/quick-fix`    |
| Minor 2 | Missing log       | `/quick-fix`    |

## Rules Applied
- Resources and Memory Management Principles (LSP document lifecycle)
- Logging and Observability Mandate (3-point operation logging)
- Error Handling Principles (graceful degradation)
- Architectural Patterns (Lawyer trait — I/O behind interface, verified)
- Security Mandate (sandbox checks — all tools verified)
- Testing Strategy (264 tests, full coverage on critical paths)
- Rust Idioms and Patterns (clippy, fmt — clean)
