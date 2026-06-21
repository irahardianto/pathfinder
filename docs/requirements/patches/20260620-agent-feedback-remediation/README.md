# 20260620 Agent Feedback Remediation

Date: 2026-06-20
Source: 7 independent agent assessment reports (4 tool reports, 3 skill reports)
Status: Implemented

## Background

Four agent simulation reports and three skill documentation assessments were
conducted against the latest Pathfinder version. Reports came from three
different stacks:

1. **Pathfinder** — Rust-only (testing against Pathfinder's own codebase)
2. **Bank of Anthos** — Java with Python
3. **Fath** (2 reports) — Go, JS/TS + Vue, Python

All reports converge on the same core findings. Previous remediation session
(conversation 1348cb13, 10 commits) fixed low-hanging code bugs but did NOT
address the ergonomic/documentation issues that agents actually struggle with.
One claimed fix (`suggested_max_tokens` structured field) was never applied.

## Patch Index

Progressive deliverables ordered by impact and dependency:

| Patch | Title | Type | Effort | Priority |
|-------|-------|------|--------|----------|
| [PATCH-001](PATCH-001-skill-documentation-overhaul.md) | Skill & Documentation Overhaul | Docs only | ~2h | P1 |
| [PATCH-002](PATCH-002-search-ergonomics.md) | Search Ergonomics (kind=type, non_code) | Code | ~1.5h | P1 |
| [PATCH-003](PATCH-003-explore-ergonomics.md) | Explore Ergonomics (suggested_max_tokens) | Code | ~35min | P2 |
| [PATCH-004](PATCH-004-health-readiness-consistency.md) | Health & Readiness Consistency | Code | ~3h | P2 |
| [PATCH-005](PATCH-005-batch-apis.md) | Batch APIs (inspect, locate) | Code | ~5h | P2 |
| [PATCH-006](PATCH-006-investigation-spikes.md) | Investigation Spikes (research) | Research | ~7h | P1-P2 |

Total effort: ~19 hours

## Dependency Graph

```
PATCH-001 (docs)          PATCH-006 (research spikes)
    |                         |
    v                         v (findings may spawn follow-up patches)
PATCH-002 (search code)   PATCH-003 (explore code)
    |                         |
    +---- PATCH-004 (health) -+
              |
              v
         PATCH-005 (batch APIs)
```

PATCH-001 and PATCH-006 have no code dependencies — start immediately.
PATCH-002 and PATCH-003 are independent of each other.
PATCH-004 is independent but lower priority.
PATCH-005 benefits from PATCH-002 being done first.

## Suggested Implementation Order

**Session 1** (quick wins, ~2.5h):
- PATCH-001: All doc deliverables
- PATCH-003: Explore ergonomics (small code change)

**Session 2** (search + spikes, ~3.5h):
- PATCH-002: kind=type alias + non_code alias
- PATCH-006 Spike A: Semantic path reproduction

**Session 3** (health + research, ~5h):
- PATCH-004: Health readiness improvements
- PATCH-006 Spike B: TS LSP investigation

**Session 4** (batch APIs + noise, ~8h):
- PATCH-005: Batch inspect/locate
- PATCH-006 Spike C: Grep noise assessment

## Findings Source

Full analysis: `findings_analysis.md` in conversation 0265ca76
