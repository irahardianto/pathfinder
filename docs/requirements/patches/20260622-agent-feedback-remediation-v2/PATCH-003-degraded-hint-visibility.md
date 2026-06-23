# PATCH-003: Degraded Hint Visibility

Date: 2026-06-22
Source: Pathfinder report (Rust stack) — "null vs [] is catastrophic footgun"
Priority: P0 — correctness bug; agents make wrong refactoring decisions
Status: Spec — awaiting implementation

## Problem Statement

When `trace(scope="callers")` returns `degraded: true` with
`incoming: null` (LSP unavailable, callers UNKNOWN), the `hint` field is
NOT populated. The exact scenario that needs a warning is silent.

### Root Cause

File: `crates/pathfinder/src/server/tools/navigation/impact.rs:943-949`

```rust
let hint = if !degraded && incoming.as_ref().is_some_and(Vec::is_empty) {
    Some(
        "LSP confirmed zero incoming callers. This symbol may be an entry point, \
         unused, or only called via dynamic dispatch/reflection."
            .to_owned(),
    )
} else {
    None
};
```

The `hint` is ONLY populated when `!degraded && incoming.is_empty()`.
When `degraded=true` AND `incoming=null` (the catastrophic footgun
scenario), `hint` is `None`.

This means the agent gets:
```json
{
  "degraded": true,
  "degraded_reason": "no_lsp",
  "incoming": null,
  "hint": null
}
```

No hint, no warning, no explicit "do not treat null as zero" message.
The agent must correctly interpret the combination of `degraded: true` +
`incoming: null` on its own. Documentation in SKILL.md warns about this,
but documentation cannot intercept programmatic `incoming ?? []` coercion
in agent code.

### The Report's Recommendation

The Pathfinder report recommended three options:
- **Option A**: Return `degraded_incoming_uncertain: true` flag explicitly
- **Option B**: Never return null array; return empty with `uncertain: true` flag
- **Option C**: Put the distinction into the error message when degraded

v1 implemented NONE of these. This patch implements Option A (per-field
uncertainty flag) + populates `hint` in the degraded case.

### Agent Impact

Agents that:
- Use null-coalescing accessors (`incoming ?? []`, `incoming or []`)
- Check `incoming.length == 0` after coercion
- Sum `incoming?.length ?? 0` across multiple trace calls

…all silently treat `null` as zero, leading to incorrect "no callers"
conclusions and wrong dead-code-removal refactoring decisions.

---

## DELIVERABLE A: Populate `hint` in Degraded+Null Scenario

Priority: P0
Effort: Low (20 minutes)
Risk: Low (additive — hint was None, now it's Some)

**Steps**:

1. In `crates/pathfinder/src/server/tools/navigation/impact.rs:943-949`,
   expand the hint logic to cover the degraded+null case:

   ```rust
   let hint = if degraded && incoming.is_none() {
       // CRITICAL: callers are UNKNOWN, not zero.
       // This is the exact scenario where agents misinterpret null as empty.
       Some(
           "Callers UNKNOWN — LSP unavailable or warming up. \
            Do NOT treat null as zero callers. \
            Use search(mode='text', query='symbol_name') to verify manually."
               .to_owned(),
       )
   } else if degraded && outgoing.is_none() {
       Some(
           "Callees UNKNOWN — LSP unavailable or warming up. \
            Do NOT treat null as zero callees. \
            Use search(mode='text', query='symbol_name') to verify manually."
               .to_owned(),
       )
   } else if !degraded && incoming.as_ref().is_some_and(Vec::is_empty) {
       Some(
           "LSP confirmed zero incoming callers. This symbol may be an entry point, \
            unused, or only called via dynamic dispatch/reflection."
               .to_owned(),
       )
   } else {
       None
   };
   ```

2. Include the actual symbol name in the hint where possible. The
   `symbol_name` is available in scope at this point in the function.

**Files to modify**:
- `crates/pathfinder/src/server/tools/navigation/impact.rs` — hint logic

**Acceptance**:
- When `degraded=true` + `incoming=null`: `hint` is `Some(...)` with
  explicit "UNKNOWN, not zero" warning
- When `degraded=true` + `outgoing=null`: `hint` covers callees
- When `degraded=false` + `incoming=[]`: existing hint unchanged
- When `degraded=false` + `incoming=Some([...])`: `hint` is `None`

---

## DELIVERABLE B: Add `incoming_verified` and `outgoing_verified` Flags

Priority: P0
Effort: Medium (1 hour)
Risk: Low (additive fields, no breaking changes)

**Problem**: Even with the hint, agents that only parse structured fields
(not hint text) need a machine-readable flag. The `degraded` boolean is
global to the response — it doesn't tell the agent WHICH field is
uncertain. An agent needs per-field uncertainty.

**Steps**:

1. In `crates/pathfinder/src/server/types.rs`, add to
   `FindCallersCalleesMetadata` (after the `incoming` and `outgoing`
   fields):

   ```rust
   /// Whether `incoming` was verified by LSP (vs unknown due to degradation).
   ///
   /// - `Some(true)`: LSP confirmed the caller list. `incoming` is `Some(vec)`
   ///   containing verified callers (possibly empty = confirmed zero).
   /// - `Some(false)`: LSP was unavailable or failed. `incoming` is `null`
   ///   (UNKNOWN — do NOT treat as zero). Use `search` to verify manually.
   /// - `None`: Field not applicable (e.g., scope did not request callers).
   #[serde(skip_serializing_if = "Option::is_none")]
   pub incoming_verified: Option<bool>,

   /// Whether `outgoing` was verified by LSP (vs unknown due to degradation).
   ///
   /// Same semantics as `incoming_verified` but for the callee list.
   #[serde(skip_serializing_if = "Option::is_none")]
   pub outgoing_verified: Option<bool>,
   ```

2. In `crates/pathfinder/src/server/tools/navigation/impact.rs`,
   `find_callers_callees_impl`, set these fields based on the actual
   resolution path:

   ```rust
   // After all resolution logic, before constructing metadata:
   let incoming_verified = if degraded {
       Some(false)  // LSP unavailable — incoming is unknown
   } else {
       Some(true)   // LSP confirmed — incoming is verified (even if empty)
   };
   let outgoing_verified = if degraded {
       Some(false)
   } else {
       Some(true)
   };
   ```

   Edge case: when grep fallback produces heuristic results
   (`degraded=true` but `incoming=Some(vec)` with heuristic refs), the
   results exist but are NOT LSP-verified. Set `incoming_verified = Some(false)`
   with a hint that results are heuristic.

   Updated logic:
   ```rust
   let incoming_verified = match (&incoming, degraded) {
       (Some(_), false) => Some(true),   // LSP confirmed
       (Some(_), true) => Some(false),   // Heuristic grep results
       (None, true) => Some(false),      // Unknown (LSP unavailable)
       (None, false) => None,            // Not requested / not applicable
   };
   ```

3. Add to the `FindCallersCalleesMetadata` struct construction:

   ```rust
   let metadata = crate::server::types::FindCallersCalleesMetadata {
       incoming,
       outgoing,
       incoming_verified,
       outgoing_verified,
       // ... rest unchanged
   };
   ```

4. Update the `incoming` and `outgoing` field docs to reference
   `incoming_verified` / `outgoing_verified`:

   ```rust
   /// `null` when `incoming_verified` is `Some(false)` (LSP unavailable,
   /// callers UNKNOWN). Check `incoming_verified` before interpreting.
   ```

**Files to modify**:
- `crates/pathfinder/src/server/types.rs` — add fields + update docs
- `crates/pathfinder/src/server/tools/navigation/impact.rs` — set fields

**Acceptance**:
- `incoming_verified: Some(true)` when `degraded=false` (LSP confirmed)
- `incoming_verified: Some(false)` when `degraded=true` (LSP unavailable)
- `incoming_verified: Some(false)` when grep fallback produced heuristic
  results (degraded=true + incoming=Some(vec))
- Agent can check `incoming_verified == Some(false)` to know "do not trust
  incoming, verify manually"

---

## DELIVERABLE C: Update SKILL.md and AGENTS.md

Priority: P1
Effort: Low (20 minutes)
Risk: None

**Problem**: SKILL.md and AGENTS.md document the null vs [] distinction
in prose. Now that machine-readable fields exist, update docs to
reference them.

**Steps**:

1. In `docs/agent_directives/skills/pathfinder/SKILL.md`, update the
   "Null vs [] Distinction" section:

   ```markdown
   ### Null vs [] Distinction

   `trace()` results use `incoming: null` vs `incoming: []` to distinguish
   unknown from confirmed-zero. Check `incoming_verified` for a
   machine-readable flag:

   - `incoming_verified: true` + `incoming: []` → LSP confirmed zero callers
   - `incoming_verified: false` + `incoming: null` → callers UNKNOWN (LSP down)
   - `incoming_verified: false` + `incoming: [vec]` → heuristic grep results
     (may include false positives)

   When `incoming_verified: false`, do NOT treat results as complete.
   Use `search(mode='text')` to verify manually.
   ```

2. In the project root `AGENTS.md`, update the "Degraded Mode" section
   under Pathfinder Tool Routing:

   ```markdown
   **Critical — check `incoming_verified`/`outgoing_verified` in `trace` results:**
   - `verified: true` + `[]` = confirmed zero (safe for refactoring)
   - `verified: false` + `null` = UNKNOWN (degraded — do NOT treat as zero)
   - `verified: false` + `[vec]` = heuristic (may have false positives)
   ```

3. Remove or condense the old prose-only warning since the
   machine-readable flag supersedes it.

**Files to modify**:
- `docs/agent_directives/skills/pathfinder/SKILL.md`
- `AGENTS.md` (project root)

**Acceptance**:
- SKILL.md references `incoming_verified` / `outgoing_verified` fields
- AGENTS.md references the new flags
- Old prose-only warning updated to point to the machine-readable fields

---

## DELIVERABLE D: Tests

Priority: P0
Effort: Low (30 minutes)
Risk: None

**Steps**:

Add tests to `crates/pathfinder/src/server/tools/navigation/impact_test.rs`:

1. `test_trace_degraded_null_incoming_has_hint_and_unverified`
   - Setup: mock LSP unavailable
   - Input: `trace(scope="callers")`
   - Assert: `degraded == true`, `incoming == None`
   - Assert: `hint` is `Some(...)` containing "UNKNOWN"
   - Assert: `incoming_verified == Some(false)`

2. `test_trace_non_degraded_empty_incoming_has_hint_and_verified`
   - Setup: mock LSP ready, BFS returns empty
   - Input: `trace(scope="callers")`
   - Assert: `degraded == false`, `incoming == Some(vec![])`
   - Assert: `hint` is `Some(...)` containing "confirmed zero"
   - Assert: `incoming_verified == Some(true)`

3. `test_trace_degraded_heuristic_incoming_is_unverified`
   - Setup: mock LSP unavailable, grep fallback finds results
   - Input: `trace(scope="callers")`
   - Assert: `degraded == true`, `incoming == Some(vec![...])`
   - Assert: `incoming_verified == Some(false)` (heuristic, not LSP-verified)
   - Assert: `hint` is populated (degraded+heuristic case)

4. `test_trace_non_degraded_with_results_is_verified`
   - Setup: mock LSP ready, BFS returns callers
   - Input: `trace(scope="callers")`
   - Assert: `degraded == false`, `incoming == Some(vec![...])`
   - Assert: `incoming_verified == Some(true)`

**Files to modify**:
- `crates/pathfinder/src/server/tools/navigation/impact_test.rs`

**Acceptance**:
- All 4 tests pass
- Tests cover the 4 key scenarios: degraded+null, non-degraded+empty,
  degraded+heuristic, non-degraded+results

---

## Dependency Order

```
A (hint in degraded+null) → D (tests for hint)
B (verified flags)        → D (tests for flags)
C (docs update) — depends on A + B
```

A and B are independent — can be done in parallel.
C depends on both A and B.
D depends on A and B.

## Verification Plan

```bash
cargo test -p pathfinder navigation
cargo clippy -- -D warnings
```

Manual verification:
- Kill LSP process
- Call `trace(scope="callers", semantic_path="...")` on any symbol
- Verify response has `hint` populated with "UNKNOWN" warning
- Verify `incoming_verified: false`
- Start LSP, wait for ready
- Call `trace(scope="callers")` on a symbol with no callers
- Verify `hint` has "confirmed zero" message
- Verify `incoming_verified: true`

Total effort: ~2 hours
