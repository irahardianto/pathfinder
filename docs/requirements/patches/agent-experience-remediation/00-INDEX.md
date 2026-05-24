# Pathfinder Agent Experience Remediation Plan

Date: 2026-05-23
Source: 6+ independent agent reports from real coding sessions using Pathfinder MCP
Status: Planning
Affects: Pathfinder MCP v0.9.x

---

## Motivation

Multiple AI agents used Pathfinder during real refactoring, audit, and code review sessions.
They consistently report the same friction points. The findings cluster into 4 themes:

1. Agents cannot make decisions when degraded mode is ambiguous
2. Tool selection causes ceremony overhead and decision fatigue
3. Error recovery is manual, not automated
4. Workflows require multi-step tool chains with no composition

The codebase already has many of the primitives (DegradedReason enum, did_you_mean, grep
fallback chains, lsp_readiness). The gaps are in the agent experience layer: surfacing
actionable guidance, reducing ceremony, and making Pathfinder opinionated about what agents
should do next.

---

## Relationship to Existing Plans

This plan supersedes and consolidates:

- `20260508_REMEDIATION_PLAN.md` (ergonomics F1-F8)
- `20260508_remediation-plan-pathfinder-ergonomics.md` (Findings 1-8)
- `20260509_AGENT_FEEDBACK_REMEDIATION_PLAN.md` (R1-R11)
- `20260512-lsp-hardening/README.md` (Epic 1-4, specs 001-010)
- `PATHFINDER_ERGONOMICS_REPORT.md` (Session 1 scorecard)
- `PATHFINDER_ERGONOMICS_ADDENDUM_2026-05-04.md` (Session 2 scorecard)

Items already implemented are marked [DONE] and not repeated here.

---

## Delivery Structure

The plan is organized into 5 epics delivered incrementally. Each epic has its own spec
document with bite-sized deliverables.

| Doc | Epic | Theme | Specs | Status |
|-----|------|-------|-------|--------|
| `01-degraded-mode-actionability.md` | Epic 1 | Make degraded mode unambiguous and actionable | 8 specs | Pending |
| `02-error-recovery-automation.md` | Epic 2 | Automate error recovery for common failure modes | 6 specs | Pending |
| `03-tool-selection-clarity.md` | Epic 3 | Reduce tool selection ceremony and decision fatigue | 5 specs | Pending |
| `04-workflow-composition.md` | Epic 4 | Enable composed multi-tool workflows | 4 specs | Pending |
| `05-performance-metrics.md` | Epic 5 | Add observability for agent timeout/budget decisions | 3 specs | Pending |

Total: 26 specs across 5 epics.

---

## Already Implemented (Do Not Re-implement)

These items from prior plans are confirmed DONE in the current codebase:

| Prior ID | Description | Evidence |
|----------|-------------|----------|
| F3 (20260508) | did_you_mean wired into get_definition | `compute_did_you_mean()` at navigation.rs:1118 |
| F4 (20260508) / TASK-4 | DegradedReason enum standardized | `types.rs:404-437` with 12 variants + Display impl |
| F5 (20260508) / TASK-5 | Auto-scale max_tokens for large repos | `repo_map.rs` auto-scaling logic |
| F6/F7 (20260508) / TASK-6/7 | max_references + max_dependencies budget controls | `AnalyzeImpactParams.max_references`, `ReadWithDeepContextParams.max_dependencies` |
| R2 (20260509) | Default max_depth changed to 3 | `default_max_depth() -> u32 { 3 }` |
| R6 (20260509) | source_only detail level added | `source_file.rs` handles `"source_only"` mode |
| R11 (20260509) | analyze_impact renamed to find_callers_callees | Tool registered as both names |
| LSP-HEALTH-001 | All 7 probe/lifecycle tasks | Probe cache, per-language timeouts, two-phase readiness |
| HARDENING Epic 1 | Specs 001-003 (grep fallback extraction, search hint, repo map LSP status) | README.md shows COMPLETE |
| HARDENING Spec 004 | Warm start tracking | `warm_start_complete` in LspHealthResponse |
| HARDENING Spec 006 | Per-language indexing timeouts | `spawn_indexing_timeout_fallback` enhanced |
| R5 (20260509) | find_all_references tool added | Tool registered in server.rs |
| DegradedToolInfo | Structured degraded tool info in lsp_health | `types.rs:779-797` |
| lsp_readiness | Added to GetDefinitionResponse, ReadWithDeepContextMetadata, AnalyzeImpactMetadata, FindAllReferencesMetadata | Confirmed in types.rs |
| R8 (20260509) | Search coverage metadata | `files_searched`, `files_in_scope`, `coverage_percent` in SearchCodebaseResponse |
| R9 (20260509) | Consistent degraded text prefixes | All LSP-dependent tools prepend DEGRADED notices |
| find_symbol tool | Bare name -> semantic path discovery | `find_symbol.rs` + `FindSymbolResponse` |
| read_files tool | Batch multi-file read | `read_files.rs` + `ReadFilesResponse` |

---

## Finding Validation Matrix

Each finding from the agent reports was validated against source code:

| ID | Finding | Source Report | Verdict | Why |
|----|---------|---------------|---------|-----|
| AE-1 | Degraded mode lacks actionable next steps | All 6 reports | CONFIRMED | `degraded: true` + `degraded_reason` exist but no `actionable_next_step`, `retry_after`, or `fallback_tool` field |
| AE-2 | Tool selection causes decision fatigue | 4 reports | CONFIRMED | 15+ tools, 3 read variants, no decision tree in tool descriptions |
| AE-3 | SYMBOL_NOT_FOUND error recovery is manual | 5 reports | CONFIRMED | `did_you_mean` exists but only populated in 2 of 5 semantic-path tools. No auto-retry. No cross-file search. |
| AE-4 | Bare file path returns SYMBOL_NOT_FOUND | 2 reports | CONFIRMED | `require_symbol_target()` at helpers.rs:118 returns SymbolNotFound with empty did_you_mean |
| AE-5 | No file existence check before symbol lookup | 3 reports | CONFIRMED | `treesitter_surgeon.rs` parses AST first, no early FileNotFound |
| AE-6 | No fuzzy matching at lookup time | 3 reports | CONFIRMED | `levenshtein` only used for suggestions, not for resolution fallback |
| AE-7 | No cross-file symbol search when wrong file | 4 reports | CONFIRMED | `resolve_symbol_chain` only searches within specified file |
| AE-8 | read_with_deep_context / analyze_impact don't call compute_did_you_mean | 2 reports | CONFIRMED | Only `get_definition_impl` calls `compute_did_you_mean`. Other tools return raw tree-sitter error. |
| AE-9 | grep fallback patterns miss definition styles | 2 reports | CONFIRMED | `definition_patterns()` at navigation.rs:115 covers basics but misses complex patterns (macro-generated, impl with lifetimes) |
| AE-10 | No duration_ms in responses | 3 reports | CONFIRMED | No performance metrics in any tool response |
| AE-11 | LSP warmup silent killer | 4 reports | PARTIALLY FIXED | `warm_start_complete` added but agents must check lsp_health proactively. No auto-signal on tool failure. |
| AE-12 | lsp_health reports ready but tools still degrade | 2 reports | CONFIRMED | Capability advertisement (supports_call_hierarchy=true) doesn't guarantee runtime success for interfaces, Spring proxies, macro-generated code |
| AE-13 | find_all_references misses class hierarchy | 1 report | CONFIRMED | Uses textDocument/references (usages) not textDocument/implementation (subclasses). Both are called but implementations may not return subtypes. |
| AE-14 | search_codebase enclosing_semantic_path wrong for some matches | 1 report | CONFIRMED | Tree-sitter's innermost enclosing scope picks wrong ancestor for some patterns |
| AE-15 | 15+ tool parameters cause tuning fatigue | 3 reports | CONFIRMED | max_depth, max_references, max_dependencies, max_tokens, max_tokens_per_file, max_results, filter_mode, context_lines, detail_level, visibility, include_imports, project_only, offset |
| AE-16 | No "give me everything about X" single call | 4 reports | CONFIRMED | Requires read_symbol_scope + analyze_impact + search_codebase separately |

---

## Priority Order

```
Epic 1 (P0): Degraded mode actionability
  -> Agents cannot make good decisions without knowing what to do.
  -> Unblocks all other epics by giving agents reliable fallback signals.

Epic 2 (P1): Error recovery automation
  -> Most common agent failure is SYMBOL_NOT_FOUND.
  -> Fixing auto-recovery eliminates 40%+ of agent friction.

Epic 3 (P1): Tool selection clarity
  -> Reduces ceremony from 5+ calls to 1-2 calls for common workflows.
  -> Documentation-heavy, low code risk.

Epic 4 (P2): Workflow composition
  -> New composite tools require design and testing.
  -> Lower priority because find_symbol + read_files already reduce ceremony.

Epic 5 (P3): Performance metrics
  -> Nice-to-have for timeout-aware agents.
  -> Can be deferred without blocking agent workflows.
```

---

## Implementation Rules

1. Each spec is an atomic commit: `fix(pathfinder): <spec title>` or `feat(pathfinder): <spec title>`
2. TDD: RED (failing test) -> GREEN (minimal impl) -> REFACTOR (clean up)
3. Every spec must pass `cargo test --workspace` + `cargo clippy --workspace -- -D warnings`
4. Epics execute in order (1 -> 5); specs within an epic can be parallelized
5. Each spec document includes: Problem, Root Cause, Files, Changes, Test Plan, Acceptance Criteria
6. Specs are sized for 1-2 hours of focused work each

---

## Cross-References

- [LSP Architecture](../LSP-ARCHITECTURE.md)
- [PRD v4.6](../pathfinder-prd-v4.6.md)
- [Prior Ergonomics Report](../patches/PATHFINDER_ERGONOMICS_REPORT.md)
- [Prior Ergonomics Addendum](../patches/PATHFINDER_ERGONOMICS_ADDENDUM_2026-05-04.md)
- [Prior Remediation Plan (May 8)](../patches/20260508_REMEDIATION_PLAN.md)
- [Prior Agent Feedback Plan (May 9)](../patches/20260509_AGENT_FEEDBACK_REMEDIATION_PLAN.md)
- [LSP Hardening Specs](../patches/20260512-lsp-hardening/README.md)
