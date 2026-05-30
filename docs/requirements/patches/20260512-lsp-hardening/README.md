# PATCH-LSP-HARDENING: Pathfinder LSP Reliability & Agent Ergonomics

**Date**: 2026-05-12
**Status**: ✅ COMPLETE (All 4 epics implemented)
**Affects**: Pathfinder MCP v0.9.x
**Priority**: P0–P2 across 4 epics

---

## Motivation

Agent-reported LSP instability, silent grep fallback failures, and tool ceremony overhead reduce Pathfinder's reliability as a code intelligence backend. This patch series addresses 8 identified gaps through 4 epics of increasing risk/effort.

### Key Findings

| ID | Issue | Impact | Resolution |
|----|-------|--------|------------|
| P1 | Grep fallback code duplication in `analyze_impact` | 3 identical 35-line blocks diverge silently | Epic 1.1 — extracted helper |
| P2 | `warm_start` is fire-and-forget | 5–30s latency on first call | Epic 2.1 — tracked completion |
| P3 | No `find_symbol` tool | 2–3 call ceremony for path discovery | Epic 4.1 — new tool |
| P4 | No batch `read_files` tool | Multi-file audits take 5–10 calls | Epic 4.2 — new tool |
| P5 | Grep fallback for `analyze_impact` is naive | Over-counts comments/strings | Epic 3 — language-aware patterns + tree-sitter enrichment |
| P6 | No LSP status in `get_repo_map` | Agents make extra `lsp_health` calls | Epic 1.3 — added `lsp_status` flat map |
| P7 | `search_codebase` silently drops filter-hidden results | Agents conclude symbols don't exist | Epic 1.2 — added `hint` field |
| P8 | `read_with_deep_context` has no grep fallback | Returns zero deps when LSP unavailable | Epic 2.2 — grep dep discovery |

---

## Relationship to LSP-HEALTH-001

[LSP-HEALTH-001](../LSP-HEALTH-001-lsp-health-status-discrepancy.md) documented 7 remediation tasks for LSP lifecycle bugs (navigation_ready decoupling, pyright detection, probe reliability, gopls isolation, warning messages, progress timeout, pytest cleanup). **All 7 tasks were implemented in a prior session** and are present in the codebase:

| LSP-HEALTH-001 Task | Status | Evidence |
|---|---|---|
| Task 1: Decouple navigation readiness from indexing | ✅ Done | `navigation_ready` field in `LspLanguageStatus`; two-phase readiness in `lsp_health_impl` |
| Task 2: Fix pyright diagnostics detection | ✅ Done | Resolved by Task 1 — pyright with `definition_provider: true` reports `"ready"` |
| Task 3: Improve probe for monorepo layouts | ✅ Done | `find_probe_file` with recursive depth-limited scan + well-known path fast path |
| Task 3.2: Cache probe results | ✅ Done | `probe_cache: Arc<Mutex<HashMap<String, ProbeCacheEntry>>>` on `PathfinderServer` |
| Task 4: gopls cache isolation | ✅ Done | `GOCACHE` + `GOMODCACHE` env vars set in `spawn_lsp_child` |
| Task 5: Fix concurrent LSP warning message | ✅ Done | Accurate per-language isolation description in `detect_concurrent_lsp` |
| Task 6: Progress watcher timeout (flat 30s) | ✅ Done | `spawn_indexing_timeout_fallback` with `INDEXING_FALLBACK_TIMEOUT_SECS = 30` |

**This patch series (LSP-HARDENING) builds on that foundation** with additional improvements:
- Spec 006 enhances Task 6 by making the flat 30s timeout per-language (Java needs 120s)
- Specs 001–003 address agent-side ergonomics gaps not covered by LSP-HEALTH-001
- Specs 004–005 address warm_start lifecycle and grep fallback gaps
- Specs 007–010 address grep enrichment and new tool development

---

## Spec Documents

Each spec is a self-contained, bite-sized deliverable with acceptance criteria and test plan.

### Epic 1: Quick Wins (Low Risk, High Impact) ✅ COMPLETE

| Spec | Title | Status |
|------|-------|--------|
| [001-grep-fallback-extraction.md](./001-grep-fallback-extraction.md) | Extract `grep_reference_fallback` helper | ✅ Done |
| [002-search-hint-field.md](./002-search-hint-field.md) | Add zero-result hint to `search_codebase` | ✅ Done |
| [003-repo-map-lsp-status.md](./003-repo-map-lsp-status.md) | Surface LSP status in `get_repo_map` | ✅ Done |

### Epic 2: LSP Hardening (Medium Risk, High Impact) ✅ COMPLETE

| Spec | Title | Status |
|------|-------|--------|
| [004-warm-start-tracking.md](./004-warm-start-tracking.md) | Track `warm_start` completion + expose in `lsp_health` | ✅ Done |
| [005-deep-context-grep-fallback.md](./005-deep-context-grep-fallback.md) | Grep fallback for `read_with_deep_context` | ✅ Done |
| [006-per-language-indexing-timeouts.md](./006-per-language-indexing-timeouts.md) | Per-language indexing timeouts (enhances LSP-HEALTH-001 Task 6) | ✅ Done |

### Epic 3: Richer Grep Fallbacks (Medium Risk, Medium Impact) ✅ COMPLETE

| Spec | Title | Status |
|------|-------|--------|
| [007-language-aware-definition-patterns.md](./007-language-aware-definition-patterns.md) | Language-aware regex patterns for grep fallbacks | ✅ Done |
| [008-callsite-aware-grep.md](./008-callsite-aware-grep.md) | Tree-sitter enriched grep for `analyze_impact` | ✅ Done |

### Epic 4: New Tools (Higher Risk, High Impact) ✅ COMPLETE

| Spec | Title | Status |
|------|-------|--------|
| [009-find-symbol-tool.md](./009-find-symbol-tool.md) | `find_symbol` — bare name → semantic path discovery | ✅ Done |
| [010-read-files-batch-tool.md](./010-read-files-batch-tool.md) | `read_files` — batch multi-file read in one call | ✅ Done |

---

## Execution Rules

1. Each spec follows TDD: RED (failing test) → GREEN (minimal impl) → REFACTOR (clean up)
2. Every spec must pass `cargo test --workspace` + `cargo clippy --workspace -- -D warnings` before marking complete
3. Epics execute in order (1 → 2 → 3 → 4); specs within an epic can be parallelized
4. Each spec is an atomic commit with conventional commit format: `fix(pathfinder): <spec title>`

## Cross-References

- [LSP-HEALTH-001](../LSP-HEALTH-001-lsp-health-status-discrepancy.md) — P0 lsp_health bugs (all 7 tasks implemented)
- [Implementation Plan (session artifact)](file:///home/irahardianto/.gemini/antigravity/brain/7139509f-875c-4368-8a0f-40e3a0800222/implementation_plan.md) — original remediation analysis
- [PRD v4.6](../pathfinder-prd-v4.6.md) — full product requirements
