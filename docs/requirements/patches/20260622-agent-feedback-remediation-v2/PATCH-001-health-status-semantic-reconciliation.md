# PATCH-001: Health Status Semantic Reconciliation

Date: 2026-06-22
Source: Fath reports (Go/JS/TS/Vue/Python stack), Pathfinder report (Rust stack)
Priority: P0 — correctness bug that produces wrong agent decisions
Status: Spec — awaiting implementation

## Problem Statement

`health()` can return `status: "degraded"` with `navigation_ready: true`
simultaneously. This contradiction was INTRODUCED by v1 PATCH-004
(commit `b80df50`), which added liveness probing that downgrades `status`
from "ready" to "degraded" when a probe fails — but never reconciles
`navigation_ready`, which is write-once at response construction time.

### The Contradiction

**Step 1** — Initial status assignment (`health.rs:102-119`):

```rust
let (status_str, uptime) = if status.navigation_ready == Some(true) {
    ("ready", status.uptime_seconds.map(format_uptime))
} else if status.navigation_ready == Some(false)
    || status.indexing_complete == Some(false)
{
    ("warming_up", status.uptime_seconds.map(format_uptime))
} else if status.uptime_seconds.is_some() {
    ("starting", status.uptime_seconds.map(format_uptime))
} else {
    ("unavailable", None)
};
```

When `navigation_ready == Some(true)`, `status_str` is set to `"ready"`.

**Step 2** — `navigation_ready` written to response, never touched again
(`health.rs:149`):

```rust
languages.push(crate::server::types::LspLanguageHealth {
    ...
    navigation_ready: status.navigation_ready,  // write-once
    probe_verified: false,
    ...
    degraded_tools: compute_degraded_tools(status),  // computed from pre-probe state
    ...
});
```

**Step 3** — Probe loop overwrites `status` to "degraded" but NEVER touches
`navigation_ready` (`health.rs:204`, `252`, `284`):

```rust
// line 204 (cached negative entry, fresh):
} else {
    if lang_health.status == "ready" {
        lang_health.status = "degraded".to_string();  // status downgraded
    }
    lang_health.probe_verified = false;
    lang_health.navigation_tested = Some(false);
    lang_health.call_hierarchy_verified = false;
}
```

Same pattern at lines 252 and 284. `navigation_ready` is never reassigned
— confirmed: zero matches for `lang_health.navigation_ready =` in the codebase.

**Result**: `status: "degraded"` + `navigation_ready: Some(true)` +
`probe_verified: false` + `degraded_tools: []`.

### The Second Contradiction — `degraded_tools`

`compute_degraded_tools` (`health.rs:769-858`) keys off `navigation_ready`,
NOT off `status`:

```rust
pub(super) fn compute_degraded_tools(
    status: &pathfinder_lsp::types::LspLanguageStatus,
) -> Vec<crate::server::types::DegradedToolInfo> {
    let mut degraded = Vec::new();
    let warming_up = status.navigation_ready != Some(true);  // keys off navigation_ready
    if warming_up {
        // ... marks all tools degraded, returns early
    }
    // LSP is ready — only flag tools where specific capabilities are absent
    if status.supports_definition != Some(true) { ... }
    if status.supports_call_hierarchy != Some(true) { ... }
    degraded
}
```

When `navigation_ready=Some(true)` + `supports_definition=Some(true)` +
`supports_call_hierarchy=Some(true)`: `degraded_tools` is empty.

So the response has `status: "degraded"` + `degraded_tools: []`. The status
says "degraded" but the tool list says "nothing is degraded." An agent
checking `degraded_tools` to decide whether to trust `trace`/`locate` sees
an empty list and proceeds — directly into the broken navigation the probe
detected.

**Compounding bug**: `compute_degraded_tools` is called ONCE at line 157,
BEFORE the probe loop (lines 165-291). The probe loop mutates
`lang_health.status` to "degraded" but never recomputes
`lang_health.degraded_tools`. So `degraded_tools` is frozen at the pre-probe
value even after the probe invalidates the status it was derived from.

### Documentation Gap

The `status` field doc (`types.rs:753-757`) does not list "degraded" as a
possible value:

```rust
/// Status of the language server process: `"ready"`, `"warming_up"`, `"starting"`, or `"unavailable"`.
```

"degraded" is absent. An agent reading the schema sees only 4 possible values.

The `navigation_ready` doc (`types.rs:784-794`) gives decision guidance that
ignores the degraded case:

```rust
/// Agents should use this signal to decide:
/// - `navigation_ready = true` + `indexing_status = "complete"` → full confidence
/// - `navigation_ready = true` + `indexing_status = "in_progress"` → results may be partial
/// - `navigation_ready = false` or `None` → fall back to Tree-sitter
```

An agent following this with `navigation_ready=true` concludes "full
confidence" — the opposite of what `status="degraded"` says.

### Agent Impact

Agents face decision paralysis with three contradictory signals:
1. `status: "degraded"` → "something is wrong, be cautious"
2. `navigation_ready: true` → "navigation is functional, proceed"
3. `degraded_tools: []` → "no tools are degraded, all good"

Different agents make different decisions depending on which field they
check first. This is the exact "Conflicting Status Semantics" issue the
Fath report flagged as P0.

---

## DELIVERABLE A: Introduce `navigation_verified` Distinct from `navigation_ready`

Priority: P0
Effort: Medium (1.5 hours)
Risk: Low (additive field, no breaking changes)

**Problem**: `navigation_ready` conflates two distinct concepts:
1. **Capability readiness** — LSP completed initialize handshake and advertises `definitionProvider`. This is static, set once at handshake.
2. **Operational verification** — a live probe actually tested navigation and it worked. This is dynamic, updated by liveness probes.

The probe loop needs to update the second concept without corrupting the
first. Adding a separate field is cleaner than mutating `navigation_ready`
(which would lose the capability information).

**Steps**:

1. In `crates/pathfinder/src/server/types.rs`, add to `LspLanguageHealth`:
   ```rust
   /// Whether navigation was verified by a live probe (not just capability
   /// advertisement). Distinct from `navigation_ready` which reflects
   /// capability negotiation only.
   ///
   /// - `Some(true)`: live probe succeeded — navigation is operational
   /// - `Some(false)`: live probe failed — navigation may be broken despite
   ///   `navigation_ready: true`
   /// - `None`: probe not yet run (freshness unknown)
   #[serde(skip_serializing_if = "Option::is_none")]
   pub navigation_verified: Option<bool>,
   ```

2. In `health.rs`, initialize `navigation_verified: None` at response
   construction (line ~149).

3. In the probe loop, set `navigation_verified` alongside
   `probe_verified` and `navigation_tested`:
   - On probe success: `navigation_verified = Some(true)`
   - On probe failure: `navigation_verified = Some(false)`
   - On cached positive entry: `navigation_verified = Some(true)`
   - On cached negative entry: `navigation_verified = Some(false)`

4. Update `navigation_ready` doc to explicitly distinguish it from
   `navigation_verified`:
   ```rust
   /// Whether the LSP advertised navigation capabilities during initialize.
   /// This reflects capability negotiation only — it does NOT guarantee
   /// navigation actually works. For live verification, check
   /// `navigation_verified`.
   ```

**Files to modify**:
- `crates/pathfinder/src/server/types.rs` — add field + update docs
- `crates/pathfinder/src/server/tools/navigation/health.rs` — set field in probe loop

**Acceptance**:
- `navigation_verified` populated after probe runs
- `navigation_ready` doc explicitly says "capability only, not operational"
- `navigation_verified` doc explicitly says "live probe result"
- When `status="degraded"`: `navigation_verified = Some(false)`
- When `status="ready"` + probe succeeded: `navigation_verified = Some(true)`

---

## DELIVERABLE B: Reconcile `status` with `navigation_verified`

Priority: P0
Effort: Medium (1 hour)
Risk: Low (status string is freeform, no enum constraint)

**Problem**: After Deliverable A, the relationship between `status` and
`navigation_verified` must be consistent. The current code allows
`status="degraded"` + `navigation_ready=true` — after Deliverable A this
becomes `status="degraded"` + `navigation_ready=true` +
`navigation_verified=Some(false)`, which is consistent IF the agent knows
to check `navigation_verified`. But `status` should also be unambiguous.

**Steps**:

1. Define the status lifecycle explicitly in the `status` field doc:
   ```rust
   /// Status of the language server process.
   ///
   /// Lifecycle: `unavailable` → `starting` → `warming_up` → `ready`
   /// A `ready` LSP may be downgraded to `degraded` if a live probe fails.
   ///
   /// - `"unavailable"`: No LSP process running or detected
   /// - `"starting"`: Process exists, no capability info yet (lazy start)
   /// - `"warming_up"`: Process running, navigation_ready not yet confirmed
   /// - `"ready"`: Initialize handshake complete, navigation_ready=true,
   ///   and live probe succeeded (navigation_verified=Some(true))
   /// - `"degraded"`: Initialize handshake completed (navigation_ready=true)
   ///   but live probe failed (navigation_verified=Some(false)).
   ///   Navigation MAY still work — retry or use with caution.
   ```

2. Ensure the code never produces invalid combinations:
   - `status="ready"` ⇒ `navigation_verified = Some(true)`
   - `status="degraded"` ⇒ `navigation_verified = Some(false)` AND
     `navigation_ready = Some(true)`
   - `status="warming_up"` ⇒ `navigation_verified = None` or
     `Some(false)`
   - `status="unavailable"` ⇒ `navigation_verified = None`

3. Add a final consistency pass after the probe loop (after line ~295):
   ```rust
   // Reconcile status with navigation_verified
   for lang_health in &mut languages {
       match (&lang_health.navigation_verified, &lang_health.status.as_str()) {
           (Some(true), "degraded") => {
               // Probe actually succeeded but status was downgraded —
               // shouldn't happen, but fix it
               lang_health.status = "ready".to_string();
           }
           (Some(false), "ready") => {
               // Probe failed but status still says ready — downgrade
               lang_health.status = "degraded".to_string();
           }
           _ => {}
       }
   }
   ```

**Files to modify**:
- `crates/pathfinder/src/server/types.rs` — update `status` field doc
- `crates/pathfinder/src/server/tools/navigation/health.rs` — add consistency pass

**Acceptance**:
- `status` field doc lists all 5 possible values including "degraded"
- No code path produces `status="ready"` + `navigation_verified=Some(false)`
- No code path produces `status="degraded"` + `navigation_verified=Some(true)`

---

## DELIVERABLE C: Recompute `degraded_tools` After Probe Loop

Priority: P0
Effort: Low (30 minutes)
Risk: Low

**Problem**: `compute_degraded_tools` is called at line 157, BEFORE the
probe loop. The probe loop changes `status` but never recomputes
`degraded_tools`. Result: `degraded_tools` reflects pre-probe state.

**Steps**:

1. Refactor `compute_degraded_tools` to accept the post-probe state.
   Currently it takes `&LspLanguageStatus` (the raw LSP client status).
   Change it to accept the reconciled health fields:

   ```rust
   pub(super) fn compute_degraded_tools_from_health(
       navigation_verified: Option<bool>,
       supports_definition: Option<bool>,
       supports_call_hierarchy: Option<bool>,
       server_name: &Option<String>,
   ) -> Vec<crate::server::types::DegradedToolInfo> {
       let navigation_broken = navigation_verified == Some(false);

       if navigation_broken || navigation_verified.is_none() {
           // Navigation not verified — flag all tools as potentially degraded
           // ... (same as current warming_up block but with "degraded" severity)
       }
       // ... rest same as current capability-based checks
   }
   ```

2. Call it AFTER the probe loop and consistency pass:

   ```rust
   // After probe loop + consistency pass:
   for lang_health in &mut languages {
       lang_health.degraded_tools = compute_degraded_tools_from_health(
           lang_health.navigation_verified,
           lang_health.supports_definition,
           lang_health.supports_call_hierarchy,
           &lang_health.server_name,
       );
   }
   ```

3. Remove the pre-probe `compute_degraded_tools(status)` call at line 157.
   Initialize `degraded_tools` to `vec![]` and let the post-probe
   recomputation fill it.

**Files to modify**:
- `crates/pathfinder/src/server/tools/navigation/health.rs` — refactor + move call

**Acceptance**:
- When `status="degraded"` + `navigation_verified=Some(false)`:
  `degraded_tools` is NON-empty (lists trace, locate, inspect as degraded)
- When `status="ready"` + `navigation_verified=Some(true)`:
  `degraded_tools` is empty (or only contains capability-specific entries)
- `degraded_tools` is consistent with `status` — no contradiction

---

## DELIVERABLE D: Tests for Status/Verified Consistency

Priority: P0
Effort: Medium (1 hour)
Risk: None

**Steps**:

Add tests to `crates/pathfinder/src/server/tools/navigation/health_test.rs`:

1. `test_health_degraded_status_has_navigation_verified_false`
   - Setup: mock LSP with `navigation_ready: Some(true)`, probe fails
   - Assert: `status == "degraded"`, `navigation_verified == Some(false)`
   - Assert: `navigation_ready == Some(true)` (capability unchanged)
   - Assert: `degraded_tools` is non-empty

2. `test_health_ready_status_has_navigation_verified_true`
   - Setup: mock LSP with `navigation_ready: Some(true)`, probe succeeds
   - Assert: `status == "ready"`, `navigation_verified == Some(true)`
   - Assert: `degraded_tools` is empty (assuming all capabilities present)

3. `test_health_degraded_tools_recomputed_after_probe`
   - Setup: mock LSP ready, probe fails
   - Assert: `degraded_tools` contains trace, locate, inspect entries
   - Assert: `degraded_tools` NOT empty (regression test for the
     frozen-pre-probe-state bug)

4. `test_health_status_never_ready_with_navigation_verified_false`
   - Setup: various probe scenarios
   - Assert: invariant `status == "ready"` ⟹ `navigation_verified == Some(true)`

5. `test_health_status_never_degraded_with_navigation_verified_true`
   - Setup: various probe scenarios
   - Assert: invariant `status == "degraded"` ⟹ `navigation_verified == Some(false)`

6. Update `test_lsp_health_probe_keeps_warming_up_when_probe_fails`
   (line 250): add assertion on `navigation_verified` value, not just
   `status` and `probe_verified`.

7. Update `test_health_typescript_call_hierarchy_limitation` (line 2351):
   this test currently asserts the TS limitation message fires. After
   PATCH-005, the test should assert it only fires when
   `supports_call_hierarchy == Some(false)` (i.e., TS < 3.8.0 or
   capability negotiation failure), not blanket for all TS. See
   PATCH-005 for details.

**Files to modify**:
- `crates/pathfinder/src/server/tools/navigation/health_test.rs`

**Acceptance**:
- All 6 new/updated tests pass
- Tests cover the 4 status values × 3 navigation_verified states matrix
- Regression test for frozen `degraded_tools` passes

---

## Dependency Order

```
A (navigation_verified field) → B (status reconciliation) → C (degraded_tools recompute) → D (tests)
```

A must be done first (adds the field). B uses the field. C uses the field.
D tests all three.

## Verification Plan

```bash
cargo test -p pathfinder health
cargo clippy -- -D warnings
```

Manual verification:
- Start Pathfinder on a Rust project
- Let rust-analyzer initialize fully (`status: "ready"`)
- Kill rust-analyzer process
- Call `health(force_probe=true)`
- Verify: `status: "degraded"`, `navigation_verified: Some(false)`,
  `navigation_ready: Some(true)`, `degraded_tools` non-empty
- All three signals agree: "navigation is broken"

Total effort: ~3 hours
