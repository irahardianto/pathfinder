# LSP-INIT-002: Cross-Language LSP Dispatch Isolation

**Date**: 2026-06-03
**Severity**: P0 (cross-language blast radius on crash) + P1 (incorrect state)
**Status**: Open
**Affects**: Pathfinder v0.4.0, all LSP languages when multiple are active concurrently
**Related**: LSP-HEALTH-001, DashMap init_locks refactor (completed)

---

## Executive Summary

The DashMap refactor for `init_locks` eliminated per-language lock contention during LSP initialization. However, the shared `RequestDispatcher` architecture introduces four categories of cross-language interference bugs that manifest when multiple LSP servers run concurrently:

1. A single language crash cancels ALL pending requests across ALL languages
2. Progress notifications from one language falsely mark other languages as indexing-complete
3. Dynamic capability registrations from one language pollute other languages' state
4. `force_respawn` bypasses init_locks, enabling orphaned process spawns

These bugs are invisible in single-language projects and only surface in polyglot workspaces (e.g., Rust backend + TypeScript frontend + Go tools).

---

## Finding Validation Summary

| Report Finding | Real? | Severity | Worth Fixing? | Root Cause Confirmed? |
|---|---|---|---|---|
| BUG-1: cancel_all cross-language blast | YES | P0 | YES | YES |
| BUG-2: Progress notification bleed | YES | P1 | YES | YES |
| BUG-3: Registration notification bleed | YES | P1 | YES | YES |
| BUG-4: force_respawn bypasses init_locks | YES | P2 | YES | YES |
| BUG-5: request() TOCTOU with idle timeout | YES | P3 | YES (defensive) | YES |
| OBS-1: init_locks never cleaned up | YES | P4 | NO (bounded) | N/A |
| OBS-2: detect_concurrent_lsp Linux-only | YES | P3 | YES (future) | N/A |
| OBS-3: std::sync::Mutex in async context | YES | P4 | NO (safe in practice) | N/A |

---

## Root Cause: Shared RequestDispatcher Architecture

### The Problem

```
                          ┌─────────────────────┐
                          │  RequestDispatcher   │
                          │                     │
  rust reader ──────────► │  pending: Mutex<HashMap<u64, oneshot::Sender>>  │
                          │  notification_tx: broadcast::Sender             │◄── rust progress_watcher
  go reader ─────────────►│  server_request_tx: broadcast::Sender          │◄── go progress_watcher
                          │                     │                           │◄── ts progress_watcher
  ts reader ─────────────►│  cancel_all() ──────┼── drains ALL pending     │◄── py progress_watcher
                          │                     │                           │◄── java progress_watcher
  py reader ─────────────►│  dispatch_response()┼── broadcasts to ALL      │
                          │                     │
  java reader ───────────►│                     │
                          └─────────────────────┘
```

Every language's reader task, progress watcher, and registration watcher shares one `RequestDispatcher`. There is no per-language partitioning.

### The Trigger Conditions

**BUG-1 (cancel_all blast)**:
1. Language A's LSP process dies (crash, OOM, manual kill)
2. Language A's reader task reads EOF from stdout
3. Reader task calls `dispatcher.cancel_all()`
4. ALL pending oneshot channels (including Language B's active request) receive `Err(ConnectionLost)`
5. Language B's `goto_definition` or `analyze_impact` call fails with `ConnectionLost` even though Language B's LSP is healthy

**BUG-2 (progress bleed)**:
1. Rust's rust-analyzer sends `$/progress` with `kind: "end"` after indexing
2. `dispatch_response` sees no `id` field → broadcasts to `notification_tx`
3. ALL `progress_watcher_task`s (one per language) receive the notification
4. `extract_progress_action` does not filter by progress token or language
5. TypeScript's `indexing_complete` flag set to `true` even if tsserver is still indexing
6. `lsp_health` reports TypeScript as "ready" prematurely

**BUG-3 (registration bleed)**:
1. Rust's rust-analyzer sends `client/registerCapability` (e.g., for pull diagnostics)
2. `dispatch_response` sees `id` + `method`, not in pending → broadcasts to `server_request_tx`
3. ALL `registration_watcher_task`s receive the request
4. TypeScript's watcher applies rust-analyzer's registration to TypeScript's `live_capabilities`
5. TypeScript's watcher sends a response to TypeScript's transport (wrong LSP)
6. TypeScript's `capabilities_for()` reports capabilities it doesn't actually support

**BUG-4 (force_respawn race)**:
1. Thread A: `ensure_process("rust")` acquires init_lock, calls `start_process`
2. Thread B: `force_respawn("rust")` removes process, calls `start_process` (no init_lock)
3. Two rust-analyzer processes spawned, first becomes orphaned

---

## Source Code References

| Component | File | Lines | Role |
|---|---|---|---|
| `LspClient` struct | `client/mod.rs` | 267-277 | Shared state (DashMap + Arc) |
| `RequestDispatcher` | `client/protocol.rs` | 14-22 | Shared dispatch (root cause) |
| `dispatch_response` | `client/protocol.rs` | 101-132 | Routes to broadcast channels |
| `cancel_all` | `client/protocol.rs` | 134-143 | Drains ALL pending (BUG-1) |
| `start_reader_task` | `client/process.rs` | 165-182 | Calls cancel_all on EOF |
| `progress_watcher_task` | `client/background.rs` | 67-120 | Receives ALL notifications (BUG-2) |
| `extract_progress_action` | `client/background.rs` | 287-314 | No language/token filter |
| `registration_watcher_task` | `client/background.rs` | 122-193 | Receives ALL requests (BUG-3) |
| `extract_registration_action` | `client/background.rs` | 195-261 | No language filter |
| `ensure_process` | `client/lifecycle.rs` | 268-332 | Uses init_locks correctly |
| `force_respawn` | `client/lifecycle.rs` | 349-370 | Skips init_locks (BUG-4) |
| `request` | `client/lifecycle.rs` | 506-567 | TOCTOU window (BUG-5) |
| `idle_timeout_task` | `client/background.rs` | 204-280 | Collect-then-remove pattern |
| `warm_start_for_languages_and_track` | `client/lifecycle.rs` | 143-196 | Concurrent spawn via init_locks |

---

## Deliverables (Progressive, Bite-Sized)

### Phase 1: Per-Language Dispatcher Tags (BUG-1, BUG-2, BUG-3)

The core fix: add `language_id` awareness to the dispatcher so that cancel, progress, and registration events are scoped to their source language.

#### DEL-1.1: Add language_id to pending request tracking

**File**: `client/protocol.rs`

**Changes**:
- Change `pending` from `Mutex<HashMap<u64, oneshot::Sender<...>>>` to `Mutex<HashMap<u64, (String, oneshot::Sender<...>)>>`
- Update `register()` to accept `language_id: &str` and store it alongside the sender
- Update `register()` signature: `fn register(&self, language_id: &str) -> (u64, oneshot::Receiver<...>)`
- Update all callers: `request()`, `spawn_and_initialize()`, test fixtures

**Test**:
- Unit test: `register` stores language_id correctly
- Unit test: multiple languages' registrations coexist

**Risk**: Low. Internal API change, no behavior change yet.

---

#### DEL-1.2: Scope cancel_all to a single language

**File**: `client/protocol.rs`

**Changes**:
- Add `cancel_for_language(&self, language_id: &str)` that drains only entries matching the given language_id
- Change reader task to call `cancel_for_language(language_id)` instead of `cancel_all()`
- Keep `cancel_all()` for shutdown path only

**Test**:
- Unit test: `cancel_for_language("rust")` only cancels rust requests, leaves go requests intact
- Unit test: `cancel_for_language` with no matching entries is a no-op

**Risk**: Low. New method, existing `cancel_all` preserved for shutdown.

---

#### DEL-1.3: Per-language notification channels

**File**: `client/protocol.rs`, `client/background.rs`, `client/lifecycle.rs`

**Changes**:
- Change `notification_tx` from single broadcast to `DashMap<String, broadcast::Sender<Value>>`
- Add `get_or_create_notification_channel(&self, language_id: &str) -> broadcast::Receiver<Value>`
- Change `dispatch_response`: for notifications, extract language context or broadcast to all (with downstream filtering)
- Since LSP notifications don't carry a `language_id` field, the routing must be done by the reader task: tag each message with the source language before dispatch

**Approach**: Change `dispatch_response` signature to accept an optional `source_language_id: Option<&str>`. Reader tasks pass their language_id. When a notification arrives:
- If `source_language_id` is `Some(lang)`: send only to that language's notification channel
- If `None` (backward compat): broadcast to all

**Reader task change** (`start_reader_task`): accept `language_id: String` parameter, pass it to a new `dispatch_response_for_language(&self, language_id: &str, msg: &Value)` method.

**Test**:
- Unit test: notification from "rust" only reaches "rust" subscriber
- Unit test: notification from "go" does NOT reach "rust" subscriber

**Risk**: Medium. Core dispatch path changes. All reader task callers must pass language_id.

---

#### DEL-1.4: Per-language server request channels

**File**: `client/protocol.rs`, `client/background.rs`

**Changes**:
- Same pattern as DEL-1.3 but for `server_request_tx`
- Change to `DashMap<String, broadcast::Sender<Value>>`
- Add `get_or_create_server_request_channel(&self, language_id: &str) -> broadcast::Receiver<Value>`
- Server requests (has `id` + `method`, not in pending) are routed to the source language's channel

**Test**:
- Unit test: `client/registerCapability` from "rust" only reaches "rust" registration_watcher
- Unit test: response sent back through correct transport

**Risk**: Medium. Same scope as DEL-1.3.

---

#### DEL-1.5: Update spawn_and_initialize to use tagged dispatch

**File**: `client/process.rs`

**Changes**:
- `spawn_and_initialize` already receives `language_id: &str`
- Pass `language_id` to `start_reader_task`
- `start_reader_task` passes `language_id` to `dispatch_response_for_language`

**Test**:
- Integration test: spawn two fake LSPs (rust, go), send notification from rust, verify only rust's progress watcher receives it

**Risk**: Low. Wiring change only.

---

#### DEL-1.6: Update all callers of register()

**File**: `client/lifecycle.rs`, `client/process.rs`, test fixtures

**Changes**:
- `spawn_and_initialize`: `dispatcher.register(language_id)` instead of `dispatcher.register()`
- `request()`: `self.dispatcher.register(language_id)` instead of `self.dispatcher.register()`
- All test fixtures that call `register()`

**Test**:
- Existing tests pass with new signature

**Risk**: Low. Mechanical change.

---

### Phase 2: force_respawn Init Lock (BUG-4)

#### DEL-2.1: Acquire init_locks in force_respawn

**File**: `client/lifecycle.rs`

**Changes**:
- In `force_respawn`, acquire `init_locks` for the language before calling `start_process`
- Pattern: same as `ensure_process` -- `entry().or_insert_with().clone()` then `lock().await`
- Kill existing process AFTER acquiring the lock (not before) to prevent the race

**Before**:
```rust
pub async fn force_respawn(&self, language_id: &str) -> Result<(), LspError> {
    let descriptor = ...;
    if let Some((_, ProcessEntry::Running(state))) = self.processes.remove(language_id) {
        // kill
    }
    self.start_process(descriptor, 0).await
}
```

**After**:
```rust
pub async fn force_respawn(&self, language_id: &str) -> Result<(), LspError> {
    let descriptor = ...;

    let init_lock = self
        .init_locks
        .entry(language_id.to_owned())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone();
    let _guard = init_lock.lock().await;

    // Kill existing process under lock protection
    if let Some((_, ProcessEntry::Running(state))) = self.processes.remove(language_id) {
        // kill
    }

    self.start_process(descriptor, 0).await
}
```

**Test**:
- Test: concurrent `force_respawn("rust")` + `ensure_process("rust")` produces exactly 1 process entry
- Test: `force_respawn` while `ensure_process` holds init_lock waits and then sees the process already running

**Risk**: Low. Follows existing pattern from `ensure_process`.

---

### Phase 3: request() TOCTOU Hardening (BUG-5)

#### DEL-3.1: Eliminate double-get pattern in request()

**File**: `client/lifecycle.rs`

**Changes**:
- Refactor `request()` to use a single DashMap access for both InFlightGuard creation and send
- Option A: Hold the `RefMulti` guard across the send (guard is `Send` safe)
- Option B: Use `processes.entry()` API for atomic get-or-insert semantics
- Preferred: Option A -- simpler, hold the guard

**Before** (simplified):
```rust
let _in_flight_guard = {
    let entry = self.processes.get(language_id)?;
    InFlightGuard::new(Arc::clone(state.transport.in_flight()))
    // entry dropped here
};
{
    let entry = self.processes.get(language_id)?;  // TOCTOU: could be removed
    state.transport.send(&message).await?;
}
```

**After** (simplified):
```rust
let (in_flight_guard, transport) = {
    let entry = self.processes.get(language_id)?;
    let guard = InFlightGuard::new(Arc::clone(state.transport.in_flight()));
    let transport = Arc::clone(&state.transport);
    (guard, transport)
    // entry dropped, but in_flight_guard keeps counter > 0
};
transport.send(&message).await?;
```

This eliminates the second `processes.get()`. The `InFlightGuard` (counter > 0) prevents `idle_timeout_task` from removing the process during the send.

Note: `idle_timeout_task` must also be updated to re-check `in_flight` at removal time (not just at snapshot time) to close the remaining window.

**Test**:
- Test: send request while `idle_timeout_task` is collecting removal candidates
- Test: `idle_timeout_task` skips language with in_flight > 0

**Risk**: Low. Simplifies the code path.

---

#### DEL-3.2: Double-check in_flight at removal time in idle_timeout_task

**File**: `client/background.rs`

**Changes**:
- In the removal loop, re-check `in_flight` before actually removing:
```rust
for lang in languages_to_remove {
    if let Some(mut entry) = processes.get_mut(&lang) {
        if let ProcessEntry::Running(state) = entry.value_mut() {
            if state.transport.in_flight().load(Ordering::Acquire) > 0 {
                continue;  // race: request arrived after snapshot
            }
        }
    }
    drop(entry);
    if let Some((_, ProcessEntry::Running(state))) = processes.remove(&lang) {
        // shutdown
    }
}
```

**Test**:
- Test: `idle_timeout_task` skips process that acquired in_flight between snapshot and removal

**Risk**: Low. Defensive check.

---

### Phase 4: Cleanup and Observability (Low Priority)

#### DEL-4.1: Clean up init_locks on process removal

**File**: `client/lifecycle.rs`, `client/background.rs`

**Changes**:
- In `reader_supervisor_task`, after removing the process entry, also remove the init_lock:
```rust
processes.remove(&language_id);
// Optional: self.init_locks.remove(&language_id);
```

Note: This is only worthwhile if Pathfinder ever supports dynamic language addition. For the current 5-language limit, the memory cost is negligible (5 entries of ~100 bytes each). Mark as `// FUTURE: cleanup when dynamic language support is added`.

**Risk**: None. Comment-only change.

---

#### DEL-4.2: Add cross-language dispatch metrics

**File**: `client/protocol.rs`

**Changes**:
- Add `tracing::debug` to `dispatch_response_for_language` logging source language and message type
- Add `tracing::debug` to `cancel_for_language` logging count of cancelled requests per language

**Test**:
- Manual: verify log output during multi-language warm start

**Risk**: None. Observability only.

---

## Implementation Order and Dependencies

```
DEL-1.1 ───► DEL-1.2 ───► DEL-1.3 ───► DEL-1.4 ───► DEL-1.5 ───► DEL-1.6
                                                     │
                                                     ▼
                                                  (Phase 1 complete:
                                                   BUG-1, BUG-2, BUG-3 fixed)

DEL-2.1 (independent, can parallel with Phase 1)
                                                     │
                                                     ▼
                                                  (Phase 2 complete:
                                                   BUG-4 fixed)

DEL-3.1 ───► DEL-3.2
            │
            ▼
         (Phase 3 complete:
          BUG-5 fixed)

DEL-4.1 + DEL-4.2 (independent, any time)
```

Phase 1 is the critical path. DEL-1.1 and DEL-1.2 can ship together as a minimal fix for BUG-1 (the most severe). DEL-1.3 and DEL-1.4 address BUG-2 and BUG-3 and are more involved.

---

## Testing Strategy

### Per-Deliverable Tests

Each DEL includes specific unit tests in the description above.

### Integration Test: Polyglot Workspace

After Phase 1 is complete, add an integration test:

1. Create a workspace with Rust + Go + TypeScript markers
2. `LspClient::new()` detects all three
3. `warm_start()` fires concurrently for all three
4. Kill the Rust process mid-indexing
5. Assert: Go and TypeScript requests still succeed (not cancelled)
6. Assert: Go and TypeScript `indexing_complete` not affected by Rust's progress

### Regression Test: Single Language

After each phase, verify that single-language workspaces (Rust-only, Go-only, etc.) continue to work identically. The per-language channel model should be transparent when only one language is active.

---

## Migration Notes

### Backward Compatibility

- `RequestDispatcher` API changes are `pub(crate)` -- no public API breakage
- `cancel_all()` is preserved for shutdown path
- Existing single-language users see no behavior change

### Configuration Changes

None. All changes are internal to `pathfinder-lsp`.

---

## Appendix A: Language-Specific Initialization Summary

Confirmed correct for all languages. No changes needed.

| Language | Binary | Init Options | Init Timeout | Cache Isolation | Indexing Timeout Fallback |
|---|---|---|---|---|---|
| Rust | rust-analyzer | None | 10s (default) | `CARGO_TARGET_DIR=target/pathfinder-lsp` | 60s |
| Go | gopls | None | 10s (default) | `GOCACHE`/`GOMODCACHE` under `.pathfinder/gopls-cache` | 30s |
| TypeScript | typescript-language-server | Vue plugins, extraFileExtensions | 10s (default) | `TMPDIR=.pathfinder/tsserver-tmp` | 45s |
| Python | pyright/pylsp/ruff-lsp/jedi | venv `pythonPath` via `detect_python_init_options` | 10s (default) | `PYTHONPYCACHEPREFIX=.pathfinder/python-cache/pyc` | 30s |
| Java | jdtls | JAVA_HOME, Gradle/Maven enabled via `detect_java_init_options` | 180s (explicit) | jdtls `-data .pathfinder/jdtls-data` (always, not gated on coexistence) | 120s |

### Java Special Handling

- `-data` directory is ALWAYS set (functional requirement, not isolation)
- `ensure_pathfinder_in_gitignore` called for Java (jdtls creates files in `.pathfinder/`)
- Highest init timeout (180s) and indexing timeout (120s) to accommodate large Maven/Gradle projects

### Python Multi-Binary Fallback

Detection tries binaries in order: pyright-langserver, pylsp, ruff-lsp, jedi-language-server. First resolved binary wins. Args vary per binary.

---

## Appendix B: Shared std::sync::Mutex Usage Audit

All `std::sync::Mutex` instances in `pathfinder-lsp` are safe for async context:

| Location | Lock Duration | Across Await? | Verdict |
|---|---|---|---|
| `RequestDispatcher::pending` | HashMap insert/remove | No | Safe |
| `LanguageState::indexing_completion_source` | Write an Option | No | Safe |
| `LanguageState::indexing_duration_secs` | Write a u64 | No | Safe |
| `LanguageState::indexing_progress_percent` | Write a u8 | No | Safe |
| `ManagedProcess::last_used` | Write an Instant | No | Safe |

None of these hold the lock across an `.await` point. The lock durations are trivial (single assignment or HashMap operation).
