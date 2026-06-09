# BATCH-01 Design Gap Remediation for LspClient

> This document tracks design gaps found during BATCH-01 review rounds that require
> non-trivial architectural changes. These are not bugs -- the system works correctly --
> but the current design prevents certain classes of integration tests and limits
> testability of critical production paths.
>
> **Prerequisite:** BATCH-01-DEFERRED-ITEMS.md should be read first for original context.
> Items D-1 through D-5 in that document have been partially or fully implemented.
> This document tracks what remains.

---

## Review History

| Round | Date | Focus | Result |
|-------|------|-------|--------|
| 1 | 2026-06-09 | Verify D-1..D-5 implementations exist | All items implemented, tests pass |
| 2 | 2026-06-09 | Edge cases, assertion quality, test isolation | Found 7 issues (3 bugs, 2 coverage gaps, 2 nits). All fixed. |
| 3 | 2026-06-09 | Race conditions, resource leaks, cross-module edge cases | Found 10 production issues + 5 test infrastructure gaps. Tracked separately. |

### Round 2 Fixes Applied

| ID | File | Line | Fix |
|----|------|------|-----|
| R2-1 | `process.rs` | ~760 | Removed duplicate `consecutive_io_errors = 0` reset in `start_reader_task` -- `handle_reader_result` already owns all counter state |
| R2-2 | `transport.rs` | new | Added `test_content_length_zero_returns_protocol_error` -- Content-Length: 0 produces invalid JSON, returns `LspError::Protocol` |
| R2-3 | `transport.rs` | ~528,548,565 | Strengthened 3 `MockStdout` tests from bare `assert!(result.is_err())` to specific error variant matching |
| R2-4 | `lifecycle.rs` | ~2725 | Made `test_new_nonexistent_workspace` deterministic -- `detect_languages` always returns Ok with empty descriptors for nonexistent paths |
| R2-5 | `lifecycle.rs` | ~2728 | Renamed `test_new_starts_idle_timeout_task` to `test_new_shutdown_flag_toggles_correctly` -- test verifies shutdown flag, not idle timeout task |
| R2-6 | `transport.rs` | ~329 | Removed misleading comment in `test_empty_body` that said "Content-Length: 0 is valid" but test used Content-Length: 2 |
| R2-7 | `lifecycle.rs` | ~2559-2807 | Fixed D-2 tests PATH isolation -- replaced `which::which()` conditional assertions with config command overrides via `lsp_config_with_command()` helper. Eliminates flaky failures when `detect::tests::test_with_fake_python_binaries` replaces PATH concurrently |

---

## G-1: ProcessSpawner Not Injected into LspClient

### Status

Partially implemented. The `ProcessSpawner` trait exists and `spawn_and_initialize()` accepts
it as a parameter, but `LspClient::start_process()` hardcodes `RealProcessSpawner` instead
of using dependency injection through the `LspClient` struct.

### Problem

`LspClient` has no `spawner` field. `start_process()` creates a local `RealProcessSpawner`
on every call:

```
// lifecycle.rs:473
let spawner = crate::client::process::RealProcessSpawner;
let spawn_result = spawn_and_initialize(&spawner, ...);
```

This means:

- `MockProcessSpawner` (process.rs:1665) can only be used in direct trait tests
- No integration-level test of `start_process()` or `ensure_process()` can inject a mock
- The entire spawn + initialize + reader-start + supervisor lifecycle is untestable
  without spawning real OS processes

### What EXISTS Today

| Component | Location | Status |
|-----------|----------|--------|
| `ProcessSpawner` trait | `process.rs:98` | Defined with `fn spawn()` returning `(Child, ChildStdin, ChildStdout)` |
| `RealProcessSpawner` | `process.rs:109-122` | Delegates to `spawn_lsp_child()`. Production path works correctly. |
| `MockProcessSpawner` | `process.rs:1665-1747` | Records all 5 spawn arguments. Configurable fail mode. |
| `spawn_and_initialize()` | `process.rs:276` | Accepts `&dyn ProcessSpawner` parameter. Correctly uses trait. |
| Trait unit tests | `process.rs:1750-1821` | 4 tests: real-spawner-not-found, mock-records-calls, mock-failing, mock-multiple-calls |
| **Injection point** | `lifecycle.rs:473` | **HARDCODED** -- creates `RealProcessSpawner` locally |

### What's Missing

1. `LspClient` struct (mod.rs:287-299) needs a new field:

```rust
pub(crate) spawner: Arc<dyn ProcessSpawner>,
```

2. `LspClient::new()` (lifecycle.rs:46-77) constructs with `RealProcessSpawner`:

```rust
spawner: Arc::new(RealProcessSpawner),
```

3. `start_process()` (lifecycle.rs:473) uses `self.spawner` instead of local:

```rust
let spawn_result = spawn_and_initialize(&*self.spawner, ...);
```

4. Test helpers (mod.rs tests module) construct with `MockProcessSpawner`:

```rust
// client_no_languages(), client_with_descriptors(), make_running_client()
spawner: Arc::new(MockProcessSpawner::new()),
```

5. `force_respawn()` calls `start_process()` which would now go through the injected spawner.

### Limitations of Current MockProcessSpawner

`MockProcessSpawner` (process.rs:1715-1747) always returns `Err(LspError::Io(...))` even in
"success" mode (`should_fail=false`). This is because it cannot produce a real
`(Child, ChildStdin, ChildStdout)` tuple without spawning an actual OS process.

For full integration testing through `spawn_and_initialize`, the mock would need one of:

- **Option A:** Spawn a trivial real process (e.g. `sleep`) that provides real stdio pipes.
  The `ManagedProcess::shutdown()` test (process.rs:1454) already uses this pattern with `sleep`.
- **Option B:** Create a higher-level mock that wraps `spawn_and_initialize` entirely,
  returning a pre-built `ManagedProcess` with `FakeTransport`.

Option A is simpler and validates argument construction through the actual spawn path.
Option B is more isolated but doesn't test the real spawn pipeline.

### Scope

- Files affected: `mod.rs` (LspClient struct + test helpers), `lifecycle.rs` (new + start_process)
- Risk: Medium -- changes LspClient construction, all test helpers must be updated
- Estimated effort: 2-3 hours

### Acceptance Criteria

- [ ] `LspClient` struct has `spawner: Arc<dyn ProcessSpawner>` field
- [ ] `LspClient::new()` injects `RealProcessSpawner`
- [ ] `start_process()` uses `self.spawner` (no local `RealProcessSpawner`)
- [ ] All existing test helpers construct with appropriate spawner
- [ ] New test: `MockProcessSpawner` injected into `LspClient`, `ensure_process` records spawn call
- [ ] `cargo test -p pathfinder-mcp-lsp` passes
- [ ] `cargo clippy -p pathfinder-mcp-lsp -- -D warnings` passes

---

## G-2: BATCH-01 D-5 Documentation Factually Wrong

### Status

Documentation bug only. No code change needed.

### Problem

`BATCH-01-DEFERRED-ITEMS.md` lines 312-319 state:

> "delayed error responses are NOT supported -- when `response_delay` is active and
> the response has an error, the error is dispatched through the oneshot (via the
> delayed spawn task) rather than returned directly from `send()`."

This is factually incorrect. The actual behavior in `fake_transport.rs:192-200`:

```rust
if is_error {
    // Error: dispatch immediately via dispatcher (no delay), return Err from send()
    if let Some(ref dispatcher) = *self.dispatcher.lock() {
        dispatcher.dispatch_response_for_language(&self.language_id, &response);
    }
    return Err(LspError::Protocol(msg.to_owned()));
}
// Only non-error responses reach the delay logic below
if let Some(delay) = delay { ... }
```

Errors:
1. Are dispatched via the dispatcher immediately (no delay)
2. Return `Err(LspError::Protocol(...))` from `send()` immediately (no delay)
3. The `response_delay` is completely ignored for error responses
4. The delayed spawn task is never used for errors

The doc claims errors go "through the oneshot (via the delayed spawn task)" -- they do not.
Errors go through BOTH the dispatcher (oneshot) AND the `send()` return -- immediately,
with zero delay.

### What to Fix

Update `BATCH-01-DEFERRED-ITEMS.md` D-5 section (lines 310-334) to accurately describe
the actual implemented behavior:

1. Error responses bypass `response_delay` entirely
2. Errors dispatch immediately through the dispatcher AND return `Err` from `send()`
3. This is intentional -- it creates an asymmetry:
   - Success + delay -> `send()` returns `Ok(())`, response arrives via oneshot after delay
   - Error + delay -> `send()` returns `Err(Protocol)` immediately, delay ignored
4. This asymmetry is acceptable for current timeout tests
5. If asynchronous error arrival testing is needed in the future, add a
   `set_delayed_error()` method that returns `Ok(())` from `send()` and dispatches
   the error via the delayed task

### Scope

- Files affected: `BATCH-01-DEFERRED-ITEMS.md` only
- Risk: None -- documentation only
- Estimated effort: 15 minutes

### Acceptance Criteria

- [ ] D-5 status note accurately describes actual error/delay behavior
- [ ] Code comment in `fake_transport.rs:124` documents the delay/error asymmetry
- [ ] No code changes to `fake_transport.rs`

---

## Implementation Priority

| ID | Item | Risk | Effort | Value |
|----|------|------|--------|-------|
| G-1 | ProcessSpawner DI into LspClient | Medium | 2-3h | Enables integration testing of spawn lifecycle |
| G-2 | D-5 doc correction | None | 15m | Prevents future confusion |

### Recommended Order

1. G-2 first (quick win, no risk)
2. G-1 second (architectural change, needs careful test updates)

### Dependencies

- G-1 and G-2 are independent
- G-1 touches the same files as the D-2 test helpers in mod.rs

---

## Related Documents

- `BATCH-01-DEFERRED-ITEMS.md` -- original deferred items with proposed solutions
- `BATCH-01-lsp-client-coverage.md` -- original BATCH-01 coverage plan
- `00-MASTER-INDEX.md` -- overall patch tracking index
