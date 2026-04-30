# Pathfinder Ergonomics Remediation — 2026-04-30

Derived from three independent agent evaluation sessions. All findings cross-referenced
against existing patches (20260429/) and deferred items (DEFERRED-001) to avoid
duplication and conflict.

## How to Use These Documents

Each PATCH is a self-contained, testable delivery. Execute in dependency order within
each delivery group. Groups are independent of each other.

1. OBJECTIVE — what and why (1 paragraph)
2. SCOPE — exact files and functions (table)
3. CURRENT CODE — verbatim snippets
4. TARGET CODE — exact replacement
5. EXCLUSIONS — what NOT to touch
6. VERIFICATION — commands to confirm success
7. TESTS — new tests to add

## Delivery Groups

| Group | Patches | Theme | Risk | Est. Effort | Impact |
|-------|---------|-------|------|-------------|--------|
| A — Critical | PATCH-001 through PATCH-003 | Column-1 root cause fix (name_column) | Medium | 3h | Fixes 3 broken tools, lifts navigation from 2/10 to 8/10 |
| B — High | PATCH-004 through PATCH-007 | search_codebase bugs + response envelope consistency | Low | 2h | Fixes data-loss bug, standardizes all tool responses |
| C — Medium | PATCH-008 through PATCH-011 | Validation honesty + grep fallback + insert_after spacing | Low | 2h | Eliminates false positives, improves degraded-mode UX |
| D — Low | PATCH-012 through PATCH-015 | Health check, empty result handling, doc improvements | Low | 2h | Improves agent self-adaptation, eliminates silent failures |

**Total estimated effort: ~8.5 hours across 4 delivery groups.**

## Dependency Graph

```
A-001 ──> A-002 ──> A-003
                                (independent)
B-004 ──> B-005 ──> B-006 ──> B-007
                                (independent)
C-008 ──> C-009 ──> C-010 ──> C-011
                                (independent)
D-012 ──> D-013 ──> D-014 ──> D-015
```

Groups A-D are independent. Within each group, patches must be applied in order.

## Patch Index

| Patch | Title | Group | Files Changed | Risk | Effort |
|-------|-------|-------|---------------|------|--------|
| [PATCH-001](./PATCH-001-symbol-name-column.md) | Add name_column to ExtractedSymbol and SymbolScope | A | 4 files | Medium | 45 min |
| [PATCH-002](./PATCH-002-navigation-column-fix.md) | Use name_column in all navigation LSP calls | A | 1 file | Medium | 45 min |
| [PATCH-003](./PATCH-003-deep-context-verification.md) | Add verification probe to resolve_lsp_dependencies | A | 1 file | Low | 30 min |
| [PATCH-004](./PATCH-004-search-group-by-file.md) | Fix search_codebase group_by_file serialization | B | 2 files | Low | 30 min |
| [PATCH-005](./PATCH-005-search-known-files-schema.md) | Fix known_files + group_by_file schema validation | B | 1 file | Low | 20 min |
| [PATCH-006](./PATCH-006-response-envelope-standardize.md) | Standardize all tool response envelopes | B | 3 files | Low | 40 min |
| [PATCH-007](./PATCH-007-delete-file-version-hash.md) | Add version_hash to delete_file response | B | 1 file | Low | 10 min |
| [PATCH-008](./PATCH-008-validation-honesty.md) | Make validate_only honest about LSP absence | C | 2 files | Low | 30 min |
| [PATCH-009](./PATCH-009-analyze-impact-grep-fallback.md) | Add grep fallback to analyze_impact when degraded | C | 1 file | Low | 30 min |
| [PATCH-010](./PATCH-010-insert-after-spacing.md) | Fix insert_after missing blank line before doc comments | C | 1 file | Low | 20 min |
| [PATCH-011](./PATCH-011-get-definition-self-reference.md) | Fix get_definition grep fallback for same-file symbols | C | 1 file | Low | 30 min |
| [PATCH-012](./PATCH-012-health-check-tool.md) | Add lsp_health tool for upfront LSP status | D | 3 files | Low | 30 min |
| [PATCH-013](./PATCH-013-empty-changed-since.md) | Structured empty result for changed_since | D | 2 files | Low | 20 min |
| [PATCH-014](./PATCH-014-error-data-surface.md) | Surface did_you_mean and current_version in MCP errors | D | 2 files | Low | 30 min |
| [PATCH-015](./PATCH-015-documentation-accuracy.md) | Fix agent-facing docs for inaccurate guidance | D | 2 files | Low | 30 min |

## Scores After Remediation (Projected)

| Dimension | Before | After | Delta |
|-----------|--------|-------|-------|
| Reliability | 6/10 | 9/10 | +3 |
| Ergonomics | 7/10 | 9/10 | +2 |
| Speed | 5/10 | 7/10 | +2 |
| Completeness | 9/10 | 9/10 | 0 |
| Navigation reliability | 2/10 | 9/10 | +7 |
| Validation reliability | 1/10 | 9/10 | +8 |
| Response format consistency | 6/10 | 10/10 | +4 |
| Error message quality | 7/10 | 9/10 | +2 |

## Relationship to Prior Work

| Prior Doc | Relationship |
|-----------|-------------|
| PATCH-001..007 (20260429) | All COMPLETED. No conflicts. This batch targets different bugs. |
| DEFERRED-001 (D-01..D-16) | No overlap. All items in this batch are NEW findings not covered by prior patches or deferrals. D-12 (SYMBOL_NOT_FOUND symbol list) is partially addressed by PATCH-014 (surface did_you_mean through MCP errors) but does not add full symbol list — that remains deferred per D-12 rationale. |
| FEATURE-001 (new tools epic) | Independent. PATCH-012 (lsp_health) is a lightweight status tool, not the list_languages tool from FEATURE-001. |

## Exclusions

These items from the agent reports are NOT addressed here and remain in their prior disposition:

1. **get_repo_map image responses** — The codebase uses `Content::text()` exclusively. The image rendering is NOT in Pathfinder, NOT in pi-mcp-adapter, and NOT in pi's TUI. Detailed investigation saved in [ISSUE-image-rendering.md](./ISSUE-image-rendering.md). Most likely an LLM provider behavior. No server-side fix; workarounds documented in the issue. See also [FEATURE-002](../../FEATURE-002-multi-file-batch-edit.md) for the multi-file batch edit tracking.
2. **Multi-file batch edit** — Architectural change requiring design review. Track separately.
3. **OCC initial read overhead** — By design. The read-before-edit contract is fundamental to OCC.
4. **LSP warmup time** — Infrastructure concern, not a code bug. PATCH-012 (health check) gives agents the signal to adapt.
5. **search_codebase comments_only with Rust /// doc comments** — Unable to reproduce. The `is_comment_node` function matches `line_comment` which is the actual tree-sitter-rust kind for `///` comments. Requires further investigation with a concrete reproduction case. Track as a research item.

## Global Verification (run after ALL patches in a group)

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```
