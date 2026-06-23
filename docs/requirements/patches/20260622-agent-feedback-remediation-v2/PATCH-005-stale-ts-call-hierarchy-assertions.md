# PATCH-005: Stale TS Call Hierarchy Assertions

Date: 2026-06-22
Source: Fath reports (Go/JS/TS/Vue/Python stack) + SPIKE-B findings
Priority: P1 — stale assertions mislead agents on TypeScript projects
Status: Spec — awaiting implementation
Depends on: PATCH-001 (health status reconciliation)

## Problem Statement

v1 SPIKE-B (commit `3ab3992`) correctly added `callHierarchy` and
`references` capability declarations to the LSP initialize request. This
enables `typescript-language-server` to activate `callHierarchyProvider`
when TypeScript 3.8.0+ is installed.

However, the codebase still contains stale assertions that TypeScript/
JavaScript language servers do NOT support call hierarchy. These
assertions were written BEFORE SPIKE-B, when the capability was never
declared so TS LS never enabled it. After SPIKE-B, they are false — but
no test verifies the post-SPIKE-B behavior, so the contradiction survives
unchallenged.

### Stale Assertions

File: `crates/pathfinder/src/server/tools/navigation/health.rs`

**Line 378** — `lsp_health_impl` known_limitations:
```rust
if is_ts_js {
    known_limitations.push(format!(
        "{}: TypeScript/JavaScript language servers do not support call hierarchy. trace uses grep fallback (less accurate)",
        lang_health.language
    ));
}
```

**Line 831** — `compute_degraded_tools` trace description:
```rust
let trace_desc = if is_ts_js {
    "TypeScript/JavaScript language servers do not support call hierarchy. trace uses grep fallback (less accurate)."
        .to_owned()
} else { ... };
```

**Line 839** — `compute_degraded_tools` inspect description:
```rust
let inspect_desc = if is_ts_js {
    "TypeScript/JavaScript language servers do not support call hierarchy. inspect returns source only, no dependency signatures."
        .to_owned()
} else { ... };
```

**Line 821** — `is_ts_js` check:
```rust
let is_ts_js = status.server_name.as_ref().is_some_and(|n| {
    let lower = n.to_lowercase();
    lower.contains("typescript")
        || lower.contains("tsserver")
        || lower.contains("vtsls")
        || lower.contains("typescript-language-server")
});
```

This check fires whenever the server name matches TS patterns AND
`supports_call_hierarchy != Some(true)`. After SPIKE-B, with TS 3.8.0+,
`supports_call_hierarchy` should be `Some(true)`, so the check should
NOT fire. But if the TS version is < 3.8.0, or if capability negotiation
fails, it correctly fires.

The problem is the MESSAGE is wrong — it says "do not support call
hierarchy" (absolute, permanent) when the real condition is "call
hierarchy not available in this session" (conditional, possibly
fixable with a TS upgrade).

### Stale Test

File: `crates/pathfinder/src/server/tools/navigation/health_test.rs:2351`

```rust
async fn test_health_typescript_call_hierarchy_limitation() {
```

This test mocks `supports_call_hierarchy: Some(false)` for TS and asserts
the limitation message fires. The test itself is not wrong — it tests a
valid scenario (TS < 3.8.0). But the test name and assertions encode the
assumption that TS LS NEVER supports call hierarchy, which is now false.

### SPIKE-B Documentation Error

File: `docs/requirements/patches/20260620-agent-feedback-remediation/SPIKE-B-typescript-call-hierarchy.md:47-49`

```
`dynamicRegistration: true` allows LSP servers that support it to enable
these capabilities via `client/registerCapability` after initialization,
which is the pattern used by `typescript-language-server`.
```

This is WRONG. `typescript-language-server` does NOT use
`client/registerCapability` for call hierarchy. It checks
`textDocument?.callHierarchy` during `initialize` and STATICALLY sets
`callHierarchyProvider: true` in the initialize result (lsp-server.ts:339-340
in upstream TS LS). The `dynamicRegistration: true` sub-field is irrelevant
to TS LS's enablement decision — what matters is the presence of the
`callHierarchy` object. Any value (even `{}`) would satisfy the check.

### Agent Impact

Agents working on TypeScript projects see:
- `health()` known_limitations: "TypeScript/JavaScript language servers
  do not support call hierarchy"
- `degraded_tools`: trace and inspect listed as degraded with TS-specific
  messages

Even when TS LS DOES support call hierarchy (TS 3.8.0+ installed, SPIKE-B
capability declaration sent). Agents fall back to grep unnecessarily,
getting less accurate results.

---

## DELIVERABLE A: Replace Stale TS Limitation Messages

Priority: P1
Effort: Low (30 minutes)
Risk: Low (string changes only)

**Steps**:

1. In `crates/pathfinder/src/server/tools/navigation/health.rs:378`,
   replace the known_limitations message:

   Before:
   ```rust
   "{}: TypeScript/JavaScript language servers do not support call hierarchy. trace uses grep fallback (less accurate)"
   ```

   After:
   ```rust
   "{}: Call hierarchy not available for this TypeScript/JavaScript LSP session. \
    This may indicate TypeScript < 3.8.0 or capability negotiation failure. \
    trace uses grep fallback (less accurate)."
   ```

2. In `health.rs:831`, replace the trace_desc:

   Before:
   ```rust
   "TypeScript/JavaScript language servers do not support call hierarchy. trace uses grep fallback (less accurate)."
   ```

   After:
   ```rust
   "Call hierarchy not negotiated for this TS/JS LSP session (requires TypeScript 3.8.0+). trace uses grep fallback (less accurate)."
   ```

3. In `health.rs:839`, replace the inspect_desc:

   Before:
   ```rust
   "TypeScript/JavaScript language servers do not support call hierarchy. inspect returns source only, no dependency signatures."
   ```

   After:
   ```rust
   "Call hierarchy not negotiated for this TS/JS LSP session (requires TypeScript 3.8.0+). inspect returns source only, no dependency signatures."
   ```

4. Add a comment above the `is_ts_js` check explaining when it fires:

   ```rust
   // This check fires when supports_call_hierarchy is not Some(true),
   // meaning either: (a) TypeScript < 3.8.0, (b) capability negotiation
   // failed, or (c) LSP server doesn't implement callHierarchyProvider.
   // After SPIKE-B, typescript-language-server with TS 3.8.0+ SHOULD
   // set supports_call_hierarchy=Some(true), making this check NOT fire.
   let is_ts_js = ...;
   ```

**Files to modify**:
- `crates/pathfinder/src/server/tools/navigation/health.rs`

**Acceptance**:
- Messages say "not available for this session" not "do not support"
- Messages mention TypeScript 3.8.0+ requirement
- Comment explains when the check fires vs doesn't

---

## DELIVERABLE B: Update Stale Test

Priority: P1
Effort: Low (30 minutes)
Risk: Low

**Steps**:

1. In `crates/pathfinder/src/server/tools/navigation/health_test.rs:2351`,
   rename the test:

   Before:
   ```rust
   async fn test_health_typescript_call_hierarchy_limitation() {
   ```

   After:
   ```rust
   async fn test_health_typescript_call_hierarchy_unavailable_when_not_negotiated() {
   ```

2. Update the test to be explicit about WHAT scenario it tests:

   ```rust
   /// Verifies that when TS LS does NOT negotiate call hierarchy
   /// (e.g., TypeScript < 3.8.0 or capability negotiation failure),
   /// the health response correctly reports the limitation.
   ///
   /// After SPIKE-B, TS LS with TS 3.8.0+ SHOULD negotiate call hierarchy.
   /// This test covers the fallback case when it doesn't.
   #[tokio::test]
   async fn test_health_typescript_call_hierarchy_unavailable_when_not_negotiated() {
       // ... existing setup with supports_call_hierarchy: Some(false)
       // ... existing assertions for limitation message
       // ADD: assert message contains "not available" not "do not support"
       assert!(limitation_msg.contains("not available"));
       assert!(!limitation_msg.contains("do not support"));
   }
   ```

3. Add a NEW test for the happy path (TS LS with call hierarchy):

   ```rust
   /// Verifies that when TS LS DOES negotiate call hierarchy
   /// (TypeScript 3.8.0+ with SPIKE-B capability declaration),
   /// the health response does NOT report any TS limitation.
   #[tokio::test]
   async fn test_health_typescript_call_hierarchy_available_no_limitation() {
       let surgeon = Arc::new(MockSurgeon::default());
       let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
       let lawyer_clone = lawyer.clone();

       lawyer_clone.set_status(... // TS with supports_call_hierarchy: Some(true)
       );

       let result = server.lsp_health_impl(HealthParams::default()).await;

       let lang = find_language(&result, "typescript");
       assert_eq!(lang.supports_call_hierarchy, Some(true));
       // Assert NO TS-specific limitation message
       assert!(lang.known_limitations.iter().all(|l| !l.contains("TypeScript")));
       // Assert degraded_tools does NOT contain TS-specific call hierarchy message
       assert!(lang.degraded_tools.iter().all(|d| !d.description.contains("TypeScript")));
   }
   ```

**Files to modify**:
- `crates/pathfinder/src/server/tools/navigation/health_test.rs`

**Acceptance**:
- Renamed test still passes
- New happy-path test passes
- Test names and comments clearly distinguish "not negotiated" from
  "not supported"

---

## DELIVERABLE C: Add E2E Integration Test for TS Call Hierarchy

Priority: P2
Effort: Medium (1 hour)
Risk: Medium (requires real `typescript-language-server` binary)

**Problem**: No end-to-end test proves TS call hierarchy works after
SPIKE-B. The only real-TS-LS test (`test_new_with_tsconfig_detects_typescript`
in `lifecycle_test.rs:1985`) checks language detection only, never calls
`call_hierarchy_prepare`.

**Steps**:

1. In `crates/pathfinder-lsp/tests/lsp_client_integration.rs`, add a new
   integration test (gated on binary availability, like the Python test):

   ```rust
   /// E2E test: spawn real typescript-language-server, verify call
   /// hierarchy works after SPIKE-B capability declaration.
   ///
   /// Gated on `typescript-language-server` being installed and
   /// TypeScript 3.8.0+ being available.
   #[tokio::test]
   #[ignore = "requires typescript-language-server and TypeScript 3.8.0+"]
   async fn test_typescript_call_hierarchy_e2e() {
       // 1. Create a temp dir with a simple TS file:
       //    function foo() { bar(); }
       //    function bar() { }
       //    export { foo, bar };

       // 2. Spawn typescript-language-server via LspClient

       // 3. Wait for initialization (navigation_ready == Some(true))

       // 4. Call call_hierarchy_prepare on bar() definition

       // 5. Assert: returns non-empty items (foo should be a caller)

       // 6. Call call_hierarchy_incoming on the bar() item

       // 7. Assert: incoming calls include foo()
   }
   ```

2. Add a helper to check if `typescript-language-server` is available
   (similar to pyright check in the Python test):

   ```rust
   fn typescript_ls_available() -> bool {
       std::process::Command::new("typescript-language-server")
           .arg("--version")
           .output()
           .is_ok()
   }
   ```

3. Run the test with `cargo test -- --ignored test_typescript_call_hierarchy_e2e`
   to verify it passes when the binary is available.

**Files to modify**:
- `crates/pathfinder-lsp/tests/lsp_client_integration.rs`

**Acceptance**:
- E2E test exists and is gated on binary availability
- Test spawns real TS LS, initializes, and calls call_hierarchy methods
- Test passes when TS LS + TS 3.8.0+ are installed
- Test is skipped (not failed) when binary is missing

---

## DELIVERABLE D: Correct SPIKE-B Documentation

Priority: P2
Effort: Low (10 minutes)
Risk: None

**Steps**:

1. In `docs/requirements/patches/20260620-agent-feedback-remediation/SPIKE-B-typescript-call-hierarchy.md`,
   update the "Fix" section (around line 47-49):

   Before:
   ```
   `dynamicRegistration: true` allows LSP servers that support it to enable
   these capabilities via `client/registerCapability` after initialization,
   which is the pattern used by `typescript-language-server`.
   ```

   After:
   ```
   The `callHierarchy` and `references` objects in the client capabilities
   signal to `typescript-language-server` that the client supports these
   features. TS LS checks for `textDocument?.callHierarchy` during the
   `initialize` handshake and statically sets `callHierarchyProvider: true`
   in the initialize result (requires TypeScript 3.8.0+). This is a static
   capability check, NOT dynamic registration — the `dynamicRegistration:
   true` sub-field is harmless but not what triggers enablement. Any value
   (even `{}`) for the `callHierarchy` object would satisfy TS LS's check.
   ```

2. Add a "Verification Gap" note:
   ```
   > NOTE: As of v0.22.0, no end-to-end test proves TS call hierarchy
   > works with a real `typescript-language-server` binary. PATCH-005
   > (v2 remediation) adds this test.
   ```

**Files to modify**:
- `docs/requirements/patches/20260620-agent-feedback-remediation/SPIKE-B-typescript-call-hierarchy.md`

**Acceptance**:
- SPIKE-B doc correctly describes static capability check, not dynamic
  registration
- Verification gap noted

---

## Dependency Order

```
PATCH-001 (health reconciliation) → A (replace stale messages)
A → B (update test)
A → C (e2e test)
D (doc fix) — standalone
```

PATCH-001 must be done first because it changes the health response
structure (adds `navigation_verified`). The stale messages in Deliverable
A should be updated after the status reconciliation is in place, to
ensure consistency.

D is standalone — can be done any time.

## Verification Plan

```bash
cargo test -p pathfinder health
cargo clippy -- -D warnings
cargo test -- --ignored test_typescript_call_hierarchy_e2e  # if TS LS available
```

Manual verification:
- Start Pathfinder on a TypeScript project with TS 3.8.0+
- Call `health()`
- Verify: NO "do not support call hierarchy" message in known_limitations
- Verify: `supports_call_hierarchy: true` for TypeScript
- Verify: `degraded_tools` does NOT list trace/inspect as degraded for TS
- Call `trace(scope="callers")` on a TS function
- Verify: uses LSP call hierarchy (not grep fallback)

Total effort: ~2 hours
