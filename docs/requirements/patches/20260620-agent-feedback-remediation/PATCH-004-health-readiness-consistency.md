# PATCH-004: Health & Readiness Consistency

Date: 2026-06-20
Source: 2 Fath reports (Go/JS/TS/Vue/Python stack)
Status: Implemented

## Problem Statement

Both Fath reports independently found that `health()` can report
`status="ready"` with `navigation_ready=true` while `trace()` returns
`lsp_readiness="warming_up"` in the same session.

Root cause: `health()` checks LSP capabilities and cached liveness probes
(120s re-probe interval), while `trace()` checks actual BFS call success
at runtime. The 120s probe interval is too coarse to catch transient
states, especially during Go LSP (gopls) warm-up which takes 10-30s.

**Relevant code**:
- `health.rs:72-89` — two-phase readiness model
  (`navigation_ready` + `indexing_complete`)
- `health.rs:237-310` — liveness probing with 120s interval, positive
  results cached indefinitely
- `impact.rs` — BFS with `BFS_TIMEOUT_SECS=30`,
  `BFS_CONSECUTIVE_FAILURE_LIMIT=2`

The trace tool has safeguards (2 consecutive failures → abort BFS, 30s
wall-clock timeout) but by then the agent has already wasted a round-trip
based on `health()` saying "ready".

---

## DELIVERABLE A: Add Freshness Signal to Health Response

Priority: P1
Effort: Low (30 minutes)
Risk: Low (additive field)

**Problem**: Agents call `health()` to decide if `trace()` will work.
But `health()` returns cached probe results that could be up to 120
seconds stale. Agent has no way to know freshness.

**Steps**:

1. In types.rs, add to the per-language health status struct:
   ```rust
   /// Seconds since last liveness probe for this language.
   #[serde(skip_serializing_if = "Option::is_none")]
   pub last_probe_age_secs: Option<u32>,

   /// Whether readiness was confirmed by an actual LSP navigation
   /// operation (not just capability advertisement).
   pub probe_verified: bool,
   ```

2. In `health.rs`, where the health response is constructed:
   - Calculate `Instant::now() - last_probe_time` for each language
   - Set `last_probe_age_secs = Some(elapsed.as_secs() as u32)`
   - Set `probe_verified` based on whether last probe actually tested
     navigation (definition lookup) vs just connection/capabilities

3. Add tests:
   - `test_health_shows_probe_age`
   - `test_health_probe_verified_true_after_successful_navigation`
   - `test_health_probe_verified_false_when_only_capability_checked`

**Files to modify**:
- Types file (health status struct)
- `health.rs` (response construction)

**Acceptance**:
- `health()` includes `last_probe_age_secs` per language
- Agent can distinguish "readiness verified 2s ago" vs "95s ago"
- `probe_verified` distinguishes capability-based vs navigation-tested

---

## DELIVERABLE B: Shorten Probe Interval for Recently-Started LSPs

Priority: P2
Effort: Medium (1 hour)
Risk: Low (only affects probe timing)

**Problem**: 120s re-probe interval is appropriate for stable LSPs but
too coarse for recently-started ones. gopls takes 10-30s to fully index.
During this window, `health()` reports stale "ready" while gopls is
actually warming up.

**Steps**:

1. In `health.rs`, modify the liveness probe scheduler:
   - Track LSP start time per language
   - Ramp-up probing schedule:

   | Time since LSP start | Probe interval |
   |---------------------|----------------|
   | 0-60 seconds | Every 10 seconds |
   | 60-300 seconds | Every 30 seconds |
   | 300+ seconds | Every 120 seconds (current) |

2. Store LSP start time in health tracking state:
   ```rust
   lsp_started_at: Option<Instant>,  // per language
   ```
   Set when LSP is first detected as connected.

3. Add tests:
   - `test_probe_interval_short_after_lsp_start`
   - `test_probe_interval_medium_after_60s`
   - `test_probe_interval_normal_after_300s`

**Files to modify**:
- `health.rs` (probe scheduler logic)

**Acceptance**:
- Recently-started LSPs probed more frequently
- `health()` reflects actual readiness within ~10s of LSP becoming ready
- No change to probe behavior for stable, long-running LSPs

---

## DELIVERABLE C: Live Probe on Explicit health() Call

Priority: P2
Effort: Medium (1-2 hours)
Risk: Medium (adds latency to health() call — 100-500ms when stale)

**Problem**: Even with shorter intervals, the probe is async/background.
When an agent explicitly calls `health()` to make a decision, it should
get the freshest possible data.

**Steps**:

1. In `health.rs`, when `health()` is called explicitly (not as
   background probe):
   - If `last_probe_age > 30s` for any language: trigger synchronous
     probe before returning
   - If `last_probe_age <= 30s`: use cached result (fast path)

2. The synchronous probe should:
   - Send a `textDocument/definition` request to a known file/position
   - If success: update probe cache, set `probe_verified=true`
   - If fail/timeout (2s timeout): mark degraded, update probe cache

3. Add `health()` parameter:
   ```
   force_probe: bool  (default false)
   ```
   - `true`: always synchronous probe regardless of cache age
   - `false`: use 30-second threshold logic

4. Add tests:
   - `test_health_force_probe_triggers_live_check`
   - `test_health_uses_cache_when_fresh`
   - `test_health_live_probe_timeout_marks_degraded`

**Files to modify**:
- `health.rs` (explicit call detection, synchronous probe)
- Types file (add `force_probe` parameter)
- Tool schema (expose `force_probe` parameter)

**Acceptance**:
- `health(force_probe=true)` returns fresh data
- `health()` (default) returns fresh data when cache > 30s stale
- `health()` is fast when cache < 30s old
- Agents calling `health()` before `trace()` get reliable info

---

## Dependency Order

```
A (freshness signal) — standalone
|
B (shorter probe interval) — standalone, benefits from A's fields
|
C (live probe) — benefits from A (probe_verified) and B (scheduling)
```

## Suggested Implementation Order

Batch 1 (30 min): A — freshness signal
Batch 2 (1 hour): B — ramp-up probing
Batch 3 (1-2 hours): C — live probe on explicit call

Total effort: ~2.5-3.5 hours

## Verification Plan

```bash
cargo test -p pathfinder-core  # or relevant crate for health
cargo clippy -- -D warnings
```

Manual verification:
- Start Pathfinder on a Go project
- Call `health()` immediately → should show appropriate warm-up state
- Call `health()` after gopls finishes indexing → should show ready
- Compare `last_probe_age_secs` before and after probe cycle
- Verify `health(force_probe=true)` returns current state
