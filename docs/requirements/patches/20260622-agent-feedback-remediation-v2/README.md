# 20260622 Agent Feedback Remediation v2

Date: 2026-06-22
Source: Re-audit of v0.22.0 (includes all v1 remediation commits through 18cfd82)
Status: Spec — awaiting implementation

## Background

The v1 remediation cycle (`20260620-agent-feedback-remediation`, commits
`47743cd`..`3ab3992`) addressed 11 findings from 7 independent agent
assessment reports. A re-audit of v0.22.0 found that only 2 of 11 findings
were actually fixed at the root cause. The remainder were either:

- **Documentation-only bandaids** — prose added to SKILL.md explaining
  confusing behavior, but the protocol still emits the confusing data
- **Adjacent fixes** — a related but different problem was fixed, and the
  commit message implied the reported issue was resolved
- **Worsened** — one "fix" (PATCH-004) introduced a NEW contradiction that
  didn't exist before

Agents running v0.22.0 still struggle with the same issues reported in the
original assessments. This v2 cycle targets the root causes that v1 missed.

## What v1 Actually Fixed (2 of 11)

| Finding | v1 Patch | Verified Working? |
|---------|----------|-------------------|
| Batch operations for inspect/locate | PATCH-005 (`e35ac5f`) + 2 bugfixes (`f0ab157`, `6966f33`) | YES — 13 tests pass |
| `kind=type` umbrella filter | PATCH-002 (`eb5f7c6`) | YES |

## What v1 Did NOT Fix (9 of 11)

| Finding | v1 Claim | Actual State |
|---------|----------|--------------|
| `files_scanned: 0` confusion | "Documented as correct" | Protocol still emits `0` with no disambiguation field |
| `null` vs `[]` footgun | "Documented with warning" | No per-field uncertainty flag; `hint` suppressed in exact catastrophic case |
| `navigation_ready=true` + `status="degraded"` | "Fixed via PATCH-004" | WORSENED — PATCH-004 introduced the downgrade path; `navigation_ready` is write-once, never reconciled |
| Trait method semantic paths | "Fixed via SPIKE-A" | SPIKE-A fixed `super::` prefix stripping, NOT trait method resolution. Real root cause: `function_signature_item` not extracted |
| TypeScript call hierarchy | "Fixed via SPIKE-B" | Capability declaration correct, but zero e2e tests prove it works. Stale code still asserts TS LS doesn't support call hierarchy |
| Grep fallback noise | "Fixed via SPIKE-C Pri A" | Partial — word boundary done; scope filtering, confidence scoring deferred |
| explore max_tokens default | "Fixed via PATCH-003" | Partial — `suggested_max_tokens` added, but agent must retry to get it |
| search did-you-mean | "Documented fallback pattern" | `find_symbol.rs` still has no did-you-mean; only `definition.rs` has it |
| `comments_only` naming | "Fixed via non_code alias" | Partial — alias added, original misleading name still exists |

## Patch Index

Progressive deliverables ordered by impact and dependency:

| Patch | Title | Type | Effort | Priority |
|-------|-------|------|--------|----------|
| [PATCH-001](PATCH-001-health-status-semantic-reconciliation.md) | Health Status Semantic Reconciliation | Code | ~3h | P0 |
| [PATCH-002](PATCH-002-trait-method-resolution.md) | Trait Method Resolution | Code | ~3h | P0 |
| [PATCH-003](PATCH-003-degraded-hint-visibility.md) | Degraded Hint Visibility | Code | ~2h | P0 |
| [PATCH-004](PATCH-004-explore-structure-mode-disambiguation.md) | Explore Structure Mode Disambiguation | Code | ~1h | P1 |
| [PATCH-005](PATCH-005-stale-ts-call-hierarchy-assertions.md) | Stale TS Call Hierarchy Assertions | Code + Tests | ~2h | P1 |
| [PATCH-006](PATCH-006-find-symbol-did-you-mean.md) | find_symbol Did-You-Mean | Code | ~1.5h | P1 |

Total effort: ~12.5 hours

## Dependency Graph

```
PATCH-001 (health status)          PATCH-002 (trait resolution)
    |                                   |
    v                                   |
PATCH-005 (TS stale assertions)         v
    |                               PATCH-006 (did-you-mean)
    |                                   |
    v                                   |
PATCH-003 (degraded hint)               |
    |                                   |
    v                                   |
PATCH-004 (explore disambiguation)      |
```

- PATCH-001 and PATCH-002 are independent — start in parallel
- PATCH-005 depends on PATCH-001 (both touch health.rs status logic)
- PATCH-006 benefits from PATCH-002 (shared did_you_mean helper)
- PATCH-003 and PATCH-004 are independent of each other and everything else

## Suggested Implementation Order

**Session 1** (P0 correctness, ~5h):
- PATCH-001: Health status semantic reconciliation
- PATCH-003: Degraded hint visibility (independent, can parallelize)

**Session 2** (P0 trait resolution, ~3h):
- PATCH-002: Trait method resolution

**Session 3** (P1 ergonomic, ~4.5h):
- PATCH-005: Stale TS call hierarchy assertions (depends on PATCH-001)
- PATCH-004: Explore structure mode disambiguation
- PATCH-006: find_symbol did-you-mean (depends on PATCH-002)

## Deferred Items (documented, not spec'd)

These findings are acknowledged but deferred to a future cycle:

1. **Grep scope filtering** (SPIKE-C Priority B) — tree-sitter scope filtering
   for grep fallback results. Medium effort, HIGH impact. Research complete
   in v1 SPIKE-C findings.
2. **Confidence scoring** (SPIKE-C Priority C) — per-match confidence levels
   for heuristic results. Medium effort, MEDIUM impact.
3. **Import graph filtering** (SPIKE-C Priority D) — cross-file import graph
   for grep noise reduction. High effort, MEDIUM impact.
4. **explore `estimate_only` mode** — token estimation without full scan.
   Low marginal value after `suggested_max_tokens` field exists.
5. **Unify search response shape** — `matches[]` vs `symbols[]` consolidation.
   Breaking change, defer to major version bump.
6. **`is_stdlib` filter for inspect dependencies** — `inspect(include_dependencies=true)`
   returns stdlib paths mixed with project deps. Valid finding, low frequency.

## Verification Plan

After all patches are implemented:

```bash
cargo clippy -- -D warnings
cargo test
cargo test -- --ignored  # integration tests requiring real LSP binaries
```

Manual verification:
- `health()` on a Rust project with a briefly-slowed rust-analyzer — verify
  `status`, `navigation_ready`, `degraded_tools` are consistent
- `trace(semantic_path="...::TraitName.method")` on a trait with
  signature-only methods — verify it resolves and expands to impls
- `trace(scope="callers")` with LSP down — verify `hint` is populated
  and `incoming_verified: false`
- `explore(detail="structure")` — verify `dirs_scanned` and `mode` fields
- `health()` on a TypeScript project with TS 3.8.0+ — verify no stale
  "does not support call hierarchy" message
- `search(mode="symbol", query="NonExistent")` — verify `did_you_mean`
  field populated with suggestions
