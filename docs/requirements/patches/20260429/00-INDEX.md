# Pathfinder Remediation Patches — 2026-04-29

## How to Use These Documents

Each PATCH-XXX file is a **self-contained remediation task** designed for an AI coding agent with a 128k context window. Each document contains:

1. **OBJECTIVE** — What to fix and why (1 paragraph)
2. **SCOPE** — Exact files and functions to modify (table)
3. **CURRENT CODE** — Verbatim snippets of what exists today
4. **TARGET CODE** — Exact replacement code
5. **EXCLUSIONS** — What NOT to touch (prevents false matches)
6. **VERIFICATION** — Exact commands to confirm success
7. **TESTS** — New tests to add

> **IMPORTANT**: Execute patches in order. Later patches assume earlier ones are complete.

---

## Patch Index (actionable)

| Patch | Title | Files Changed | Risk | Est. Effort |
|-------|-------|---------------|------|-------------|
| [PATCH-001](./PATCH-001-serialization-safety.md) | Serialization Safety | 8 files | Low | 15 min |
| [PATCH-002](./PATCH-002-diagnostic-dedup.md) | Diagnostic Diffing Deduplication | 1 file | Low | 10 min |
| [PATCH-003](./PATCH-003-input-validation.md) | Input Validation Guards | 4 files | Low | 15 min |
| [PATCH-004](./PATCH-004-dead-code-cleanup.md) | Dead Code Removal | 2 files | Low | 10 min |
| [PATCH-005](./PATCH-005-filesystem-edge-cases.md) | File System Edge Cases | 1 file | Low | 10 min |
| [PATCH-006](./PATCH-006-false-positive-docs.md) | False Positive Documentation | 4 files | None | 5 min |
| [PATCH-007](./PATCH-007-search-returned-count.md) | Search `returned_count` Field | 2 files | Low | 5 min |

**Total estimated effort: ~70 minutes. Zero breaking changes.**

---

## Reference Documents (no action needed)

| Document | Purpose |
|----------|---------|
| [DEFERRED-001](./DEFERRED-001-not-remediated.md) | Registry of 16 findings deliberately not fixed, with reasons and reconsider triggers |
| [FEATURE-001](../FEATURE-001-new-tools-epic.md) | Full spec for 5 new MCP tools (`rename_symbol`, `find_all_references`, `move_symbol`, `format_file`, `list_languages`) |

> **Do not implement findings listed in DEFERRED-001.** Each entry explains why the "fix" would either be incorrect, break correct behavior, or belong in a different layer.

---

## Coverage Summary

| Category | Count | Disposition |
|----------|-------|-------------|
| Already fixed before this session | 12 | No action |
| Remediated by PATCH-001 to PATCH-007 | 16 | Execute patches in order |
| False positives (comments only, PATCH-006) | 4 | Execute PATCH-006 |
| Deliberately deferred (DEFERRED-001, D-01 to D-16) | 16 | Do not implement |
| Feature requests (FEATURE-001) | 5 | Plan a dedicated sprint |

### Already Fixed (pre-session, no action required)

| Finding | Description |
|---------|-------------|
| F1.1c | Call hierarchy duplication — unified into `call_hierarchy_request()` |
| F1.1d | BFS loop duplication — refactored into `bfs_call_hierarchy()` with `CallDirection` enum |
| F1.2c | `RipgrepScout` clone overhead — now a unit struct with no state |
| F1.5a | `extract_symbols_recursive` complexity — refactored into `SymbolExtractionContext` |
| F1.5b | `generate_skeleton_text` 10 params — now takes `&SkeletonConfig` struct |
| F2.1a | LSP skip reasons not granular — `lsp_error_to_skip_reason()` maps 7 specific reasons |
| F2.1c | `InvalidSemanticPath` error code missing — fully implemented with structured error data |
| F2.1d | Batch edit_type error messages poor — `unsupported_edit_type_error()` lists valid types |
| F2.3a | `RequestDispatcher::remove` dead code — false positive, used in 5+ call sites |
| F2.3b | `any_degraded` never mutated — removed from `search.rs` |
| F5.5b | `VERSION_MISMATCH` lacks `lines_changed` — `compute_lines_changed()` implemented and tested |
| F5.5c | `UNSUPPORTED_LANGUAGE` no suggestion — `hint()` returns actionable guidance |

---

## Pre-Requisites

- Rust toolchain with `cargo fmt`, `cargo clippy`
- All patches target the `pathfinder` workspace at the repository root

## Global Verification (run after ALL patches)

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```
