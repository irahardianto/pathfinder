# Pathfinder MCP Ergonomics Remediation — 2026-05-02

Derived from: Two independent AI agent evaluation reports (Rust-focused + Fullstack).
Cross-referenced against: 20260429 patches (completed), 20260430 patches (completed),
20260501 patches (in progress), DEFERRED-001 (no overlap).

## Triage Summary

The two reports produced 28 distinct findings. After code-level verification:

- 8 findings are CONFIRMED REAL and NOT yet addressed by any existing patch
- 6 findings are CONFIRMED REAL but ALREADY ADDRESSED by 20260430 or 20260501 patches
- 7 findings are BY DESIGN or DEFERRED (already tracked in DEFERRED-001)
- 7 findings are NOT REPRODUCIBLE or PARTIALLY INACCURATE (explained below)

## What's Already Fixed (No Action Needed)

| Finding | Source | Fixed By |
|---------|--------|----------|
| validate_only dishonest about validation skip | Both reports | PATCH-008 (20260430) |
| analyze_impact no grep fallback | Rust report | PATCH-009 (20260430) — but only for NoLspAvailable, NOT for Timeout (see GAP-001) |
| insert_after spacing for doc comments | Rust report | PATCH-010 (20260430) |
| did_you_mean not surfacing in MCP errors | Both reports | PATCH-014 (20260430) — surfaces in MCP error responses |
| lsp_health tool missing | Both reports | PATCH-012 (20260430) |
| get_definition column-1 root cause | Rust report | PATCH-001 + PATCH-002 (20260430) |
| Diagnostics protocol mismatch for Go/TS | Fullstack report | PATCH-001 + PATCH-002 (20260501) |
| Python LSP not installed | Fullstack report | PATCH-007 + PATCH-008 (20260501) |

## What's Deferred (Already Tracked, No New Action)

| Finding | Source | Tracked As |
|---------|--------|------------|
| search_codebase pagination | Both reports | Not deferred — NEW finding (see GAP-004) |
| Tool overlap (read_symbol_scope vs deep context) | Both reports | D-13 (by design) |
| SYMBOL_NOT_FOUND full symbol list | Both reports | D-12 (payload size risk) |
| OCC version hash tracking burden | Both reports | D-11 (by design) |
| Rate limiting | Fullstack report | D-15 (deployment layer) |
| Very large file handling | Rust report | D-08 (timeout mitigates) |
| LSP crash recovery | Rust report | D-09 (acceptable risk) |

## Not Reproducible / Partially Inaccurate

| Finding | Source | Explanation |
|---------|--------|-------------|
| read_with_deep_context 100% timeout | Rust report | Cannot reproduce with current code — the tool has degraded mode for NoLspAvailable. The actual issue is Timeout not triggering fallback (see GAP-001), not the tool being entirely broken |
| Go LSP 100% failure on all navigation | Fullstack report | Root cause is the Timeout-not-triggering-fallback issue (GAP-001), not a Go-specific bug. Go LSP works when responsive |
| delete_symbol broken for TS | Fullstack report | Requires root cause analysis — may be related to TS class method body detection or cross-file reference check false positives. See GAP-005 |
| lsp_health 100% false positive | Both reports | Exaggerated — the probe DOES exist (probe_language_readiness) but only runs for "warming_up" languages. The real issue is no re-probe for "ready" languages (see GAP-002) |
| search_codebase group_by_file redundant output | Both reports | Minor token cost issue, not a correctness bug |
| read_file version_hash appended to content | Both reports | By design for OCC chain usability. Not a bug |
| insert_before extra blank line | Rust report | Cosmetic, normalize_blank_lines handles it |

---

## Delivery Groups

| Group | Patches | Theme | Risk | Est. Effort | Impact |
|-------|---------|-------|------|-------------|--------|
| A — Critical | GAP-001, GAP-002 | LSP timeout fallback + health re-probe | Medium | 3h | Fixes 100% of LSP-dependent tool failures |
| B — High | GAP-003, GAP-004 | Indentation fix + version_hash in text output | Low | 2h | Eliminates silent correctness bugs |
| C — Medium | GAP-005, GAP-006 | delete_symbol TS + insert_into Rust warning | Low | 2h | Language-specific correctness |
| D — Low | GAP-007, GAP-008 | search pagination + error response quality | Low | 1.5h | Agent quality of life |

**Total estimated effort: ~8.5 hours across 4 delivery groups.**

## Dependency Graph

```
A-001 ──> A-002
                (independent)
B-003 ──> B-004
                (independent)
C-005      C-006 (independent of each other)
                (independent)
D-007      D-008 (independent of each other)
```

All groups are independent.

## Patch Index

| Patch | Title | Group | Files Changed | Risk | Effort |
|-------|-------|-------|---------------|------|--------|
| [GAP-001](./GAP-001-lsp-timeout-fallback.md) | Handle LspError::Timeout in navigation tools with grep fallback | A | 1 file | Medium | 1.5h |
| [GAP-002](./GAP-002-health-reprobe-ready.md) | Re-probe "ready" languages on lsp_health calls | A | 1 file | Low | 1.5h |
| [GAP-003](./GAP-003-indentation-nested.md) | Fix dedent_then_reindent for nested blocks | B | 2 files | Low | 1h |
| [GAP-004](./GAP-004-version-hash-text.md) | Append version_hash to text output of read tools | B | 2 files | Low | 1h |
| [GAP-005](./GAP-005-delete-symbol-ts.md) | Fix delete_symbol for TypeScript class methods | C | 1-2 files | Low | 1.5h |
| [GAP-006](./GAP-006-insert-into-rust-warning.md) | Warn when insert_into targets a Rust struct | C | 1 file | Low | 30 min |
| [GAP-007](./GAP-007-search-pagination.md) | Add offset pagination to search_codebase | D | 2 files | Low | 1h |
| [GAP-008](./GAP-008-error-responses.md) | Improve error responses with remediation hints | D | 2 files | Low | 30 min |

## Priority Justification

**GAP-001 is the single highest-impact fix.** Both reports independently identified this:
> "LSP-dependent tools suffered a 100% timeout rate... the grep fallback did NOT activate"

Root cause confirmed in code:
- `get_definition_impl`: `Err(LspError::Timeout)` → generic error, no fallback
- `resolve_lsp_dependencies`: `Err(LspError::Timeout)` → logged and ignored, no fallback
- `analyze_impact_impl`: `Err(LspError::Timeout)` → generic error, no fallback

The grep fallback code EXISTS but only matches `NoLspAvailable`, not `Timeout`.
Fixing this single match arm unblocks all three navigation tools.

## Relationship to Prior Work

| Prior Doc | Relationship |
|-----------|-------------|
| PATCH-009 (20260430) | Added grep fallback for `NoLspAvailable` in analyze_impact. GAP-001 EXTENDS this to also cover `Timeout` across ALL navigation tools |
| PATCH-008 (20260430) | Made validate_only honest. GAP-001 is the companion fix for navigation tools |
| PATCH-010 (20260430) | Fixed insert_after doc comment spacing. GAP-003 addresses a different indentation issue (replace_body nested blocks) |
| PATCH-014 (20260430) | Surfaces did_you_mean in error responses. GAP-008 extends error quality for LSP timeout errors |
| PATCH-006 (20260501) | Added probe-based health for warming_up languages. GAP-002 extends probing to "ready" languages |
| DEFERRED-001 (D-11) | Content duplication in structured_content. GAP-004 is additive — appends hash to text, doesn't change structured_content |

## Global Verification (run after ALL patches in a group)

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```
