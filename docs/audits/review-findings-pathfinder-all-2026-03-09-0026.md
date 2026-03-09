# Code Audit — Pathfinder (All Crates)

**Date:** 2026-03-09  
**Scope:** Full workspace — `pathfinder`, `pathfinder-common`, `pathfinder-lsp`, `pathfinder-search`, `pathfinder-treesitter`  
**Files reviewed:** 46 source files  
**Test count:** 94 `#[test]` functions, 264 test runs (all passing)

---

## Automated Verification

| Check | Result |
|---|---|
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ Clean |
| `cargo fmt --all -- --check` | ✅ Clean |
| `cargo test --workspace` | ✅ 264 passed, 0 failed |

---

## Architecture Summary

The codebase follows a clean hexagonal architecture:

- **I/O abstraction:** All external I/O is behind traits (`Surgeon`, `Lawyer`, `Scout`) with production and mock implementations
- **Sandbox enforcement:** Every file-touching operation goes through `Sandbox::check()` before any read/write
- **OCC + TOCTOU:** Edit operations use a two-phase hash check (early OCC, late TOCTOU) to prevent data races
- **LSP validation:** The "Shadow Editor" pipeline (`didOpen → pull_diagnostics → didChange → pull_diagnostics → diff`) validates edits before writing, with proper `didClose` cleanup
- **Error taxonomy:** Comprehensive `PathfinderError` enum with specific error codes and structured `ErrorData` for MCP responses
- **Graceful degradation:** LSP unavailability is handled at every callsite — tools return `degraded: true` rather than failing
- **Logging:** All operations have start/complete/failed tracing with `duration_ms`, `tool`, and `semantic_path` fields

---

## Findings

### F1 — `range_formatting` returns hardcoded `None` (Low / Observability)

**File:** [mod.rs](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src/client/mod.rs#L487-L538)  
**Severity:** Low  
**Category:** Observability / Completeness

The `range_formatting` method in `LspClient` sends the LSP request, receives the response (containing `TextEdit` objects), but always returns `Ok(None)`. The comment says "we just signal availability" and "return None to indicate no formatted text substitution". While this is intentional (the Tree-sitter indentation pre-pass is sufficient), the response is discarded silently.

**Recommendation:** Log the number of text edits received from the LSP at debug level so that future maintainers can see whether LSPs are returning formatting suggestions that are being ignored. No code change required now — this is a future enhancement note.

**Status:** ✅ RESOLVED — Added `tracing::debug!` with `edit_count` at line 530-536.

---

### F2 — `_restart_count` field in `LanguageState` is never read (Low / Dead Code)

**File:** [mod.rs](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src/client/mod.rs#L48)  
**Severity:** Low  
**Category:** Code Quality

The `_restart_count` field in `LanguageState` is written to but never read outside of construction. The leading underscore acknowledges this, but the field represents restart tracking that could be useful for observability (e.g., exposing restart count in health checks or structured logs).

**Recommendation:** Either wire the restart count into the idle-timeout logging or into a future health-check endpoint, or remove it entirely to avoid dead state. No urgency.

**Status:** ✅ RESOLVED — Renamed to `restart_count` (line 48) and logged in idle timeout task (line 767).

---

### F3 — `touch` takes write lock for a single timestamp update (Low / Performance)

**File:** [mod.rs](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src/client/mod.rs#L198-L205)  
**Severity:** Low  
**Category:** Performance

The `touch` method acquires a `write` lock on `self.processes` just to update `last_used` timestamps. With only a handful of language entries (typically 1-3), this is a non-issue in practice. However, under heavy concurrent request load, the write lock could briefly contend with `ensure_process`.

**Recommendation:** No action needed at current scale. If the process map ever grows significantly, consider using an `AtomicInstant` or a per-entry `Mutex` instead of the global `RwLock` write path.

**Status:** ✅ RESOLVED — No action needed (accepted as-is).

---

### F4 — `ManagedProcess.last_used` duplicated in `LanguageState.last_used` (Low / Redundancy)

**File:** [mod.rs](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src/client/mod.rs#L199-L204)  
**Severity:** Low  
**Category:** Code Quality

The `touch` method updates both `state.last_used` and `state.process.last_used`. This implies `last_used` exists in both `LanguageState` and `ManagedProcess`. The idle timeout task (line 748) checks `state.last_used`, so the `ManagedProcess.last_used` copy may be redundant — or vice versa.

**Recommendation:** Consolidate to a single `last_used` source of truth. If `ManagedProcess` needs it for shutdown logic, remove it from `LanguageState` and read from `process.last_used` in the idle check.

**Status:** ✅ RESOLVED — Removed `last_used` from `LanguageState`. Both `touch` (line 199) and `idle_timeout_task` (line 752) now use `process.last_used` exclusively.

---

### Overall Assessment

> [!TIP]
> **Verdict: PASS** — The codebase is in excellent health. All findings are Low severity with no security, reliability, or testability concerns. The architecture is well-designed for testability, the error taxonomy is comprehensive, and test coverage is solid.

No blocking issues found. All four findings were minor code quality / observability improvements. **All resolved as of 2026-03-09.**
