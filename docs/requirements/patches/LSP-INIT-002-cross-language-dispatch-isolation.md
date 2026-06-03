# LSP-INIT-002: Cross-Language LSP Dispatch Isolation

**Date**: 2026-06-03
**Severity**: P0 (cross-language blast radius on crash) + P1 (incorrect state)
**Status**: Closed
**Affects**: Pathfinder v0.4.0, all LSP languages when multiple are active concurrently
**Related**: LSP-HEALTH-001, DashMap init_locks refactor (completed)

---

## Executive Summary

The DashMap refactor for `init_locks` eliminated per-language lock contention during LSP initialization. However, the shared `RequestDispatcher` architecture introduced four categories of cross-language interference bugs that manifest when multiple LSP servers run concurrently:

1. A single language crash cancels ALL pending requests across ALL languages
2. Progress notifications from one language falsely mark other languages as indexing-complete
3. Dynamic capability registrations from one language pollute other languages' state
4. `force_respawn` bypasses init_locks, enabling orphaned process spawns

These bugs are invisible in single-language projects and only surface in polyglot workspaces (e.g., Rust backend + TypeScript frontend + Go tools).

All deliverables have been implemented and verified. Additional bugs discovered during implementation have been fixed and documented below.

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

## Root Cause: Shared RequestDispatcher Architecture (RESOLVED)

### The Problem (Before Fix)

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

### The Solution (After Fix)

```
  rust reader ──────► dispatch_response_for_language("rust", msg)
                           │
                           ├── notification_channels["rust"] ──► rust progress_watcher
                           ├── server_request_channels["rust"] ──► rust registration_watcher
                           └── pending[id=("rust", sender)] ──► resolve oneshot

  go reader ────────► dispatch_response_for_language("go", msg)
                           │
                           ├── notification_channels["go"] ──► go progress_watcher
                           ├── server_request_channels["go"] ──► go registration_watcher
                           └── pending[id=("go", sender)] ──► resolve oneshot

  (each language fully isolated)
```

### The Trigger Conditions

**BUG-1 (cancel_all blast)** — FIXED:
1. Language A's LSP process dies (crash, OOM, manual kill)
2. Language A's reader task reads EOF from stdout
3. Reader task calls `dispatcher.cancel_for_language(language_id)` (scoped to language A only)
4. Only Language A's pending oneshot channels receive `Err(ConnectionLost)`
5. Language B's `goto_definition` or `analyze_impact` calls remain unaffected

**BUG-2 (progress bleed)** — FIXED:
1. Rust's rust-analyzer sends `$/progress` with `kind: "end"` after indexing
2. `dispatch_response_for_language("rust", msg)` sees no `id` field → sends to `notification_channels["rust"]`
3. Only Rust's `progress_watcher_task` receives the notification
4. TypeScript's `indexing_complete` flag is unaffected

**BUG-3 (registration bleed)** — FIXED:
1. Rust's rust-analyzer sends `client/registerCapability` (e.g., for pull diagnostics)
2. `dispatch_response_for_language("rust", msg)` sees `id` + `method` → sends to `server_request_channels["rust"]`
3. Only Rust's `registration_watcher_task` receives the request
4. TypeScript's `live_capabilities` remains unaffected

**BUG-4 (force_respawn race)** — FIXED:
1. Thread A: `ensure_process("rust")` acquires init_lock, calls `start_process`
2. Thread B: `force_respawn("rust")` also acquires init_lock (waits for Thread A)
3. Only one rust-analyzer process is spawned at a time

---

## Source Code References (Current)

| Component | File | Lines | Role |
|---|---|---|---|
| `LspClient` struct | `client/mod.rs` | 277-295 | Shared state (DashMap + Arc) |
| `LanguageState` struct | `client/mod.rs` | 47-69 | Per-language state + watcher handles |
| `RequestDispatcher` | `client/protocol.rs` | 31-56 | Per-language dispatch (DashMap channels) |
| `PendingRequest` type | `client/protocol.rs` | 29 | `(String, oneshot::Sender)` with language tag |
| `register` | `client/protocol.rs` | 67-81 | Stores language_id with pending request |
| `subscribe_notifications_for_language` | `client/protocol.rs` | 105-118 | Per-language notification channel |
| `subscribe_server_requests_for_language` | `client/protocol.rs` | 123-136 | Per-language server request channel |
| `dispatch_response_for_language` | `client/protocol.rs` | 149-211 | Method-first dispatch (collision-safe) |
| `cancel_all` | `client/protocol.rs` | 257-268 | Drains ALL pending (shutdown only) |
| `cancel_for_language` | `client/protocol.rs` | 275-300 | Scoped cancel for single language |
| `notification_channels` | `client/protocol.rs` | 37-40 | Per-language DashMap broadcast |
| `server_request_channels` | `client/protocol.rs` | 43-49 | Per-language DashMap broadcast |
| `start_reader_task` | `client/process.rs` | 535-562 | Passes language_id to dispatch |
| `spawn_and_initialize` | `client/process.rs` | 232-330 | Full spawn + handshake |
| `progress_watcher_task` | `client/background.rs` | 92-144 | Subscribes to per-language channel |
| `extract_progress_action` | `client/background.rs` | 356-391 | Parse progress from notification |
| `registration_watcher_task` | `client/background.rs` | 147-222 | Subscribes to per-language channel |
| `extract_registration_action` | `client/background.rs` | 424-476 | Parse registration from request |
| `ensure_process` | `client/lifecycle.rs` | 275-350 | Falls through to start_process on backoff elapsed |
| `force_respawn` | `client/lifecycle.rs` | 229-272 | Acquires init_lock, aborts watchers |
| `request` | `client/lifecycle.rs` | 655-716 | Single-access with InFlightGuard |
| `idle_timeout_task` | `client/background.rs` | 227-353 | remove_if double-check + watcher abort |
| `warm_start_for_languages_and_track` | `client/lifecycle.rs` | 90-140 | Concurrent spawn via init_locks |

---

## Deliverables (All Implemented)

### Phase 1: Per-Language Dispatcher Tags (BUG-1, BUG-2, BUG-3)

#### DEL-1.1: Add language_id to pending request tracking — DONE

**File**: `client/protocol.rs:29,67-81`

`PendingRequest = (String, oneshot::Sender)`. `register(language_id)` stores language tag alongside sender. All callers updated.

#### DEL-1.2: Scope cancel to a single language — DONE

**File**: `client/protocol.rs:275-300`

`cancel_for_language(language_id)` drains only matching entries. Reader task calls this instead of `cancel_all`. `cancel_all` retained for shutdown path only.

#### DEL-1.3: Per-language notification channels — DONE

**File**: `client/protocol.rs:37-40,105-118,149-168`

`notification_channels: DashMap<String, broadcast::Sender<Value>>`. Reader dispatches to per-language channel via `dispatch_response_for_language(source_language_id, message)`.

#### DEL-1.4: Per-language server request channels — DONE

**File**: `client/protocol.rs:43-49,123-136,171-190`

`server_request_channels: DashMap<String, broadcast::Sender<Value>>`. Same pattern as notifications. Server requests routed by source language.

#### DEL-1.5: Update spawn_and_initialize to use tagged dispatch — DONE

**File**: `client/process.rs:535-562`

`start_reader_task` accepts `language_id: String`, passes to `dispatch_response_for_language`.

#### DEL-1.6: Update all callers of register() — DONE

**File**: `client/lifecycle.rs:655-716`, `client/process.rs:268-271`, test fixtures

All `register()` calls pass `language_id`.

#### DEL-1.7: Background task leak on respawn — DONE (Post-implementation fix)

**File**: `client/mod.rs:47-69`, `client/lifecycle.rs:474-518`, `client/background.rs`

**Problem**: When a language's LSP crashes and `start_process` respawns it, NEW `progress_watcher_task` and `registration_watcher_task` are spawned, but the OLD ones are never cancelled. They hold `broadcast::Receiver` handles from per-language DashMap channels. Since `broadcast::Sender` entries are never removed from the DashMap, old receivers block forever in `recv().await`.

Each respawn leaked 2 tokio tasks + their Arc clones. Under pathological crash loops, this accumulates.

**Fix**: `LanguageState` stores `watcher_handles: Vec<JoinHandle<()>>`. `abort_watchers()` method aborts all handles. Called at every process removal path:
- `reader_supervisor_task` — on crash/EOF cleanup
- `idle_timeout_task` — shutdown, zombie reap, idle timeout
- `force_respawn` — before starting new process
- `request()` / `notify()` — stale reader detection

---

### Phase 2: force_respawn Init Lock (BUG-4)

#### DEL-2.1: Acquire init_locks in force_respawn — DONE

**File**: `client/lifecycle.rs:229-272`

`force_respawn` acquires `init_locks` before killing existing process. Same pattern as `ensure_process`. Kill + `abort_watchers()` + `cancel_for_language()` happen under lock protection.

---

### Phase 3: request() TOCTOU Hardening (BUG-5)

#### DEL-3.1: Eliminate double-get pattern in request() — DONE

**File**: `client/lifecycle.rs:655-716`

Single `processes.get()` access. `InFlightGuard` cloned from `state.transport.in_flight()`, `Arc<transport>` extracted. Entry dropped before `transport.send()`. Guard keeps counter > 0, preventing `idle_timeout_task` removal.

#### DEL-3.2: Double-check in_flight at removal time in idle_timeout_task — DONE

**File**: `client/background.rs:310-340`

`remove_if` with atomic re-check of `in_flight` and `last_used` before removal. Eliminates TOCTOU window between snapshot and actual remove.

---

### Phase 4: Cleanup, Observability, and Post-Implementation Fixes

#### DEL-4.1: Clean up init_locks on process removal — DONE (FUTURE comment)

**File**: `client/lifecycle.rs:268-271`

Comment-only. Bounded to 5 languages (~500 bytes total). Marked `// FUTURE: cleanup when dynamic language support is added`.

#### DEL-4.2: Add cross-language dispatch metrics — DONE

**File**: `client/protocol.rs:149-211`

`tracing::debug` in `dispatch_response_for_language` logs source language, message type, id. `cancel_for_language` logs count of cancelled requests per language.

#### DEL-4.3: ensure_process falls through to start_process on backoff elapsed — DONE (Pre-existing fix)

**File**: `client/lifecycle.rs:275-350`

**Problem (pre-existing)**: When an `Unavailable` entry had elapsed backoff, `ensure_process` removed the entry and returned `Ok(())` without starting a process. This defeated `warm_start_for_languages_and_track` for any language that previously crashed. The language had no entry at all until the next user request triggered `ensure_process` again.

**Fix**: Changed `return match { Ok(()) }` to `match { ... }` (no return) so the code falls through to `start_process(descriptor, 0)` when backoff has elapsed.

#### DEL-4.4: Server request ID collision prevention — DONE

**File**: `client/protocol.rs:149-211`

**Problem**: In `dispatch_response_for_language`, if a server sent `client/registerCapability` with an id matching a pending request id, it was treated as a response to our request (checked pending first). JSON-RPC 2.0 doesn't mandate separate ID namespaces.

**Fix**: Check `message.get("method").is_some()` BEFORE checking pending. A message with both `id` and `method` is always a server request, never a response.

#### DEL-4.5: FUTURE cleanup comments for DashMap channels — DONE

**File**: `client/protocol.rs:37-49`

`notification_channels` and `server_request_channels` DashMaps have FUTURE cleanup comments matching the `init_locks` pattern. Bounded to 5 languages, negligible memory.

---

## Known Constraints (Not Bugs)

### Notification subscription ordering

In `spawn_and_initialize` (`process.rs:262`), the reader task starts BEFORE `progress_watcher_task` is spawned (`lifecycle.rs:480`). There's a theoretical window where the reader dispatches notifications to the per-language channel, but no subscriber exists yet. `dispatch_response_for_language` silently drops these (no channel = no send).

**Why this is safe**: LSP servers don't send progress notifications during the `initialize` handshake. The first progress notification arrives after `initialized` is sent, by which point `start_process` has already spawned both watchers. This is documented as an ordering constraint, not a bug.

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

DEL-1.7 (post-implementation: watcher task leak fix)
DEL-4.3 (pre-existing: ensure_process backoff fix)
DEL-4.4 (post-implementation: ID collision fix)
DEL-4.5 (cleanup comments)
```

---

## Testing Strategy

### Per-Deliverable Tests

All unit tests are in `client/protocol.rs` (lines 302-940):

- `test_cancel_for_language_only_cancels_matching_language` — DEL-1.2
- `test_cancel_for_language_no_matching_entries_is_noop` — DEL-1.2
- `test_cancel_for_language_multiple_languages_isolated` — DEL-1.2
- `test_notification_routing_per_language` — DEL-1.3
- `test_server_request_routing_per_language` — DEL-1.4
- `test_registration_watcher_response_sent_to_correct_transport_scenario` — DEL-1.4
- `test_server_request_with_colliding_id_not_treated_as_response` — DEL-4.4

### Integration Test: Polyglot Workspace

**Status**: Tracked separately. The protocol-layer unit tests validate dispatch isolation. A full integration test with multiple fake LSPs exercising the complete `LspClient` lifecycle (spawn, crash, respawn, cross-language request) should be added when the fake transport infrastructure supports multi-language scenarios.

Spec:
1. Create a workspace with Rust + Go + TypeScript markers
2. `LspClient::new()` detects all three
3. `warm_start()` fires concurrently for all three
4. Kill the Rust process mid-indexing
5. Assert: Go and TypeScript requests still succeed (not cancelled)
6. Assert: Go and TypeScript `indexing_complete` not affected by Rust's progress

### Regression Test: Single Language

After each phase, verify that single-language workspaces (Rust-only, Go-only, etc.) continue to work identically. The per-language channel model is transparent when only one language is active. Verified: all 340 existing tests pass.

---

## Migration Notes

### Backward Compatibility

- `RequestDispatcher` API changes are `pub(crate)` — no public API breakage
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
