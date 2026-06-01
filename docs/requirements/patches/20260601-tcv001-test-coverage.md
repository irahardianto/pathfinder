# TCV-001 Remediation Plan: Test Coverage Gap Closure

Date: 2026-06-01
DeepSource Issue: TCV-001 (Lines not covered in tests)
Severity: CRITICAL | Category: COVERAGE | Analyzer: test-coverage
Current LCV: 88.9% (Rust) | Target: 92%+

---

## Problem Statement

DeepSource reports 699 uncovered line occurrences across the pathfinder codebase. Line coverage has regressed from 90.5% peak to 88.9% over the last 10 commits. No coverage threshold is set, allowing silent regression.

The gap is concentrated in two monolithic files that are structurally difficult to test:
- `client/mod.rs` (3657 lines, ~80% of gaps)
- `navigation.rs` (7508 lines, ~13% of gaps)

---

## File-by-File Breakdown

### Tier 1: Critical (~93% of all gaps)

#### 1A. `crates/pathfinder-lsp/src/client/mod.rs` (~560 uncovered lines)

Lines: 108-2440 (spread across the entire file)
Status: All EXISTING (long-standing, never tested)
Test count: 61 tests exist (pure functions, state management, mock-based lawyer tests)

**Uncovered regions (by line range):**

| Lines | Function / Block | Why Uncovered |
|---|---|---|
| 108-154 | `LspClient::new()` full path | Requires real filesystem + config. Tests use `client_no_languages()` / `client_with_descriptors()` helpers that bypass `new()`. |
| 228 | `touch()` idle timer extension | Requires a `ManagedProcess` with an active timer -- integration-level. |
| 315-338 | `request()` JSON-RPC dispatch | Needs a live `ManagedProcess` connected to a real/stdin-stdout child. |
| 333-338 | `capabilities_for()` | Depends on `request()` which needs live process. |
| 398-431 | `notify()` JSON-RPC notification | Same as `request()`. |
| 615-625 | `ensure_process()` happy path (actual spawn) | Calls `start_process()` which spawns a real OS process. |
| 644-669 | `ensure_process()` unavailable cooldown branch | Requires time-based state. Tests cover edge cases but miss the cooldown-elapsed-then-spawn path. |
| 675-701 | `detect_concurrent_lsp()` | Reads `/proc` filesystem. Linux-specific, needs process fixtures. |
| 715-755 | `did_open()` / `did_close()` | Needs `ensure_process()` to succeed (live process). |
| 797-812 | `open_document()` happy path | Calls `did_open()` which needs live process. |
| 838-869 | `open_document()` error paths | Same dependency chain. |
| 922-1077 | `goto_definition()` / `call_hierarchy_*` | All need live LSP process via `request()`. |
| 1112-1219 | `call_hierarchy_incoming()` / `outgoing()` | Same. |
| 1234-1281 | `references()` / `goto_implementation()` | Same. |
| 1311-1367 | `force_respawn()` kill + restart cycle | Needs live process. |
| 1391-1540 | `start_process()` full lifecycle | Spawns real child process, does initialization handshake. |
| 1583-1953 | `reader_supervisor_task()` | Background task reading stdout of LSP process. |
| 1976-2044 | `progress_watcher_task()` | Background task for `$/progress` notifications. |
| 2065-2440 | `registration_watcher_task()` / `idle_timeout_task()` | Background tasks for capability registration and idle cleanup. |

**Key insight:** The gap is NOT in pure logic. All pure parse/validation functions ARE tested. The gap is in code paths requiring a live LSP child process: JSON-RPC I/O, process lifecycle, background task supervision.

**Strategy:**
1. Extract background tasks into testable pure functions where possible
2. Build a `FakeLspTransport` that simulates JSON-RPC without a real process
3. Use the existing `MockLawyer` pattern for integration-level tests

---

#### 1B. `crates/pathfinder/src/server/tools/navigation.rs` (~90 uncovered lines)

Lines: 2365-7418 (scattered across tool implementations)
Status: All NEW (recently added code)
Test count: 97 tests exist

**Uncovered regions:**

| Lines | Function / Block | Why Uncovered |
|---|---|---|
| 2365-2747 | `get_definition_impl` grep fallback expansion | New grep extraction logic added in recent patches. Tests cover mock-based paths but not the full grep pipeline. |
| 2893-3134 | `analyze_impact_impl` BFS expansion | New BFS call-graph traversal. Tests cover mock lawyer but not the grep-fallback BFS path. |
| 3596-3752 | `read_with_deep_context_impl` outgoing deps | New dependency extraction with grep fallback. |
| 4028-4547 | `find_callers_callees_impl` | New tool implementation. Tests exist but miss some branches. |
| 4917-5224 | `find_all_references_impl` | New tool. |
| 6747-7418 | `symbol_overview_impl` | New tool. Aggregates callers + callees + references. |

**Key insight:** These are all NEW tool implementations that grew rapidly. The existing mock-based tests cover the happy path and error cases, but miss the grep-fallback integration paths and some edge cases in the BFS traversal.

**Strategy:**
1. Extend existing mock-based tests to cover grep-fallback branches
2. Add tests for BFS edge cases (cycles, depth limits, partial graphs)
3. Test `symbol_overview_impl` aggregation logic

---

### Tier 2: Minor (~5% of gaps)

#### 2A. `crates/pathfinder/src/server/tools/read_files.rs` (~10 uncovered lines)

Lines: 156-241
Uncovered: Binary file detection path, duplicate path handling, version hash for non-source files, truncation edge cases.

#### 2B. `crates/pathfinder/src/server/tools/repo_map.rs` (~8 uncovered lines)

Lines: 190-497
Uncovered: `changed_since` git integration with real git, LSP pre-warm trigger paths, error fallback branches.

#### 2C. `crates/pathfinder/src/server/tools/search.rs` (2 uncovered lines)

Lines: 192, 217
Uncovered: Edge cases in `enrich_matches` -- the enrichment concurrency path.

#### 2D. `crates/pathfinder/src/server/types.rs` (1 uncovered line)

Lines: 1119-1120
Uncovered: `ReadWithDeepContextParams::default` or `AnalyzeImpactParams::default` -- trivially testable.

#### 2E. `crates/pathfinder-common/src/git.rs` (4 uncovered lines)

Lines: 79, 89, 109, 217
Uncovered: `SystemGit::diff_name_only` error paths (timeout, non-utf8 output). Integration test file exists but misses these branches.

#### 2F. `crates/pathfinder-treesitter/src/parser.rs` (2 uncovered lines)

Lines: 31-45
Uncovered: `AstParser::parse_source` error branch when tree has errors flag set but no root node.

---

## Remediation Phases

### Phase 0: Set Coverage Threshold (5 min)

Set DeepSource LCV threshold to 88% (current floor) to prevent further regression.

```
DeepSource: pathfinder repo settings -> metric threshold
LCV key=Rust -> 88%
```

Revisit and raise to 90% after Phase 2 completes.

---

### Phase 1: Quick Wins -- Tier 2 Files (1 session, ~30 new lines covered)

Order of work (by difficulty, easiest first):

**1.1 `types.rs` line 1119-1120**
- File: `crates/pathfinder/src/server/types.rs`
- Test file: `crates/pathfinder/src/server/types.rs` (inline `mod tests` doesn't exist; add one)
- What: Test `ReadWithDeepContextParams::default()` and `AnalyzeImpactParams::default()` field values
- Pattern: Standard Rust unit test

**1.2 `parser.rs` lines 31-45**
- File: `crates/pathfinder-treesitter/src/parser.rs`
- Test file: inline `mod tests` already exists in same file
- What: Test `AstParser::parse_source` with malformed input that produces a tree with error flag but no root node
- Pattern: Follow existing `test_parse_invalid_source_returns_tree_with_errors` -- the uncovered path is a specific error variant

**1.3 `search.rs` lines 192, 217**
- File: `crates/pathfinder/src/server/tools/search.rs`
- Test file: inline `mod tests` already exists
- What: Test `enrich_matches` when the enrichment task fails for one match but succeeds for others (concurrent error path)
- Pattern: Follow existing `test_search_codebase_*` tests, use `MockScout`

**1.4 `read_files.rs` lines 156-241**
- File: `crates/pathfinder/src/server/tools/read_files.rs`
- Test file: inline `mod tests` already exists
- What:
  - Test `read_single_file` with a binary file (PNG header bytes)
  - Test `read_files_impl` with duplicate paths in the input array
  - Test truncation edge case where content is exactly at `max_lines_per_file` boundary
  - Test version hash computation for non-source files
- Pattern: Follow existing `test_read_files_binary_file`, `test_read_files_duplicate_paths`

**1.5 `git.rs` lines 79, 89, 109, 217**
- File: `crates/pathfinder-common/src/git.rs`
- Test file: inline `mod tests` + `tests/git_integration.rs`
- What:
  - Test `SystemGit::diff_name_only` when git binary times out (set GIT_TIMEOUT to 1ms)
  - Test non-UTF-8 output from git (mock with invalid bytes)
  - Test the error branch in `get_changed_files_since` when `diff_name_only` returns `Err`
- Pattern: Follow existing `FakeGitRunner` mock in inline tests

**1.6 `repo_map.rs` lines 190-497**
- File: `crates/pathfinder/src/server/tools/repo_map.rs`
- Test file: inline `mod tests` already exists
- What:
  - Test `get_repo_map_impl` with `changed_since` when git returns an error (falls back to full scan)
  - Test LSP pre-warm trigger when repo has fewer than 4 source files
  - Test `empty_changes_response` helper
- Pattern: Follow existing `test_get_repo_map_*` tests using `make_server()` helper

**Completion criteria for Phase 1:**
- All 6 files have new/extended tests covering the identified lines
- `cargo test` passes
- `cargo clippy -- -D warnings` passes
- DeepSource LCV should move from 88.9% to ~89.5%

---

### Phase 2: Navigation Tool Coverage (1-2 sessions, ~90 new lines covered)

Target: `crates/pathfinder/src/server/tools/navigation.rs`

**2.1 Understand the test infrastructure**

The file already has 97 tests using these mocks:
- `MockLawyer` from `crates/pathfinder-lsp/src/mock.rs` -- stubs LSP responses
- `MockScout` from `crates/pathfinder-search/src/mock.rs` -- stubs search results
- `TreeSitterSurgeon` with temp workspace -- real AST parsing
- Helper functions: `make_server()`, `make_search_match()`, `make_lawyer_server()`

Read these files before starting:
1. `crates/pathfinder-lsp/src/mock.rs` -- understand `MockLawyer` API
2. `crates/pathfinder-search/src/mock.rs` -- understand `MockScout` API
3. `crates/pathfinder/src/server/tools/navigation.rs` lines 7430-7508 -- test module setup helpers

**2.2 Uncovered function groups (work in this order)**

**2.2.1 `get_definition_impl` grep fallback paths (lines 2365-2747)**

Uncovered grep-fallback branches:
- Lines 2371-2394: `fallback_definition_grep` when scout returns matches -> symbol extraction
- Lines 2397-2405: Multiple match ranking / deduplication
- Lines 2402-2410: Symbol extraction from grep matches using tree-sitter
- Lines 2467-2543: `extract_call_candidates` integration in definition search

Tests to write:
```
test_get_definition_grep_fallback_extracts_symbol_from_match
test_get_definition_grep_fallback_ranks_by_line_proximity
test_get_definition_grep_fallback_handles_multiple_files
test_get_definition_grep_fallback_symbol_not_in_match
```

**2.2.2 `analyze_impact_impl` BFS paths (lines 2893-3134)**

Uncovered BFS branches:
- Lines 2893-2930: BFS traversal with mixed incoming/outgoing results
- Lines 2932-2972: Cycle detection in call graph
- Lines 2974-3016: Partial graph construction when some LSP calls fail
- Lines 3042-3134: Result deduplication and formatting

Tests to write:
```
test_analyze_impact_bfs_handles_cycle_in_call_graph
test_analyze_impact_bfs_partial_failure_continues_traversal
test_analyze_impact_bfs_deduplicates_cross_referenced_symbols
test_analyze_impact_bfs_formats_response_correctly
```

**2.2.3 `read_with_deep_context_impl` outgoing deps (lines 3596-3752)**

Uncovered paths:
- Lines 3596-3650: Outgoing dependency extraction with callee signatures
- Lines 3653-3691: Dependency deduplication
- Lines 3691-3752: Response formatting with dependencies

Tests to write:
```
test_read_with_deep_context_extracts_callee_signatures
test_read_with_deep_context_deduplicates_dependencies
test_read_with_deep_context_formats_dependency_response
```

**2.2.4 `find_callers_callees_impl` edge cases (lines 4028-4547)**

Tests to write:
```
test_find_callers_callees_handles_empty_incoming_and_outgoing
test_find_callers_callees_respects_max_depth
test_find_callers_callees_grep_fallback_incoming
test_find_callers_callees_grep_fallback_outgoing
```

**2.2.5 `find_all_references_impl` (lines 4917-5224)**

Tests to write:
```
test_find_all_references_lsp_returns_references
test_find_all_references_grep_fallback
test_find_all_references_deduplicates_across_lsp_and_grep
test_find_all_references_respects_max_references
```

**2.2.6 `symbol_overview_impl` (lines 6747-7418)**

Tests to write:
```
test_symbol_overview_aggregates_callers_callees_references
test_symbol_overview_degraded_when_lsp_unavailable
test_symbol_overview_partial_degradation
test_symbol_overview_respects_limits
```

**Completion criteria for Phase 2:**
- All 20+ new tests pass
- `cargo test` + `cargo clippy -- -D warnings` pass
- DeepSource LCV should reach ~90-91%
- Raise DeepSource threshold to 90%

---

### Phase 3: LSP Client Test Infrastructure (4-5 sessions)

This is the highest-impact phase (80% of all gaps). Split into 5 progressive subphases.
Each subphase is independently committable and verifiable.

**Architecture decision: Trait injection (approach 3.2)**

After analyzing `process.rs`, `transport.rs`, `protocol.rs`, `client/mod.rs`, and `mock.rs`:

- `ManagedProcess` holds concrete OS types (`Child`, `BufWriter<ChildStdin>`) that cannot be faked without a real process
- `LanguageState` tightly couples state management with process I/O via `ManagedProcess`
- A trait at the I/O boundary (same pattern as `Lawyer`/`MockLawyer`) enables unit testing without OS process spawning
- The trait surface area is small: `send()`, `is_alive()`, `last_used`, `in_flight`, `capabilities`
- The existing `client_with_descriptors()` test helper can only create `Unavailable` entries -- a trait allows creating `Running` entries with `FakeTransport`

**Key files reference (read before starting any subphase):**
1. `crates/pathfinder-lsp/src/client/process.rs` -- `ManagedProcess` struct + `spawn_and_initialize` + `send` + `shutdown`
2. `crates/pathfinder-lsp/src/client/transport.rs` -- `read_message`/`write_message` JSON-RPC framing
3. `crates/pathfinder-lsp/src/client/protocol.rs` -- `RequestDispatcher` request/response correlation
4. `crates/pathfinder-lsp/src/client/mod.rs` L350-590 -- `LspClient` struct + constructor + state management
5. `crates/pathfinder-lsp/src/client/mod.rs` L615-1046 -- `ensure_process` + `start_process` + document ops
6. `crates/pathfinder-lsp/src/client/mod.rs` L1053-1350 -- `detect_concurrent_lsp` + `request` + `notify` + `capabilities_for`
7. `crates/pathfinder-lsp/src/client/mod.rs` L2057-2593 -- background tasks + extracted pure functions

---

#### Subphase 3A: Extract `LspTransport` trait (1 session)

**Goal:** Define the trait, implement for `ManagedProcess`, update `LanguageState`. All existing tests must pass unchanged.

**Files to modify:**
- `crates/pathfinder-lsp/src/client/process.rs` -- add `impl LspTransport for ManagedProcess`
- `crates/pathfinder-lsp/src/client/mod.rs` -- update `LanguageState`, `ProcessEntry`, `LspClient` methods

**3A.1 Define `LspTransport` trait**

Location: `crates/pathfinder-lsp/src/client/process.rs` (alongside `ManagedProcess`)

```rust
/// I/O boundary between LspClient and the LSP child process.
///
/// Production: ManagedProcess (real OS child via tokio::process).
/// Tests: FakeTransport (in-memory channels, no OS process).
///
/// This trait captures ONLY the operations LspClient performs on a running
/// process. Process lifecycle (spawn, shutdown, reap) remains in
/// ManagedProcess and is not trait-virtualized -- those are tested via
/// integration tests with real child processes.
#[async_trait]
pub(super) trait LspTransport: Send + Sync {
    /// Write a JSON-RPC message to the process stdin.
    async fn send(&self, message: &Value) -> Result<(), LspError>;

    /// Check if the transport is alive (process still running).
    fn is_alive(&self) -> bool;

    /// Get the last-used timestamp.
    fn last_used(&self) -> Instant;

    /// Set the last-used timestamp.
    fn set_last_used(&self, when: Instant);

    /// Get the in-flight request counter.
    fn in_flight(&self) -> &AtomicU32;

    /// Get a snapshot of the detected capabilities.
    fn capabilities(&self) -> DetectedCapabilities;

    /// Get a cloned handle to the stdin writer for registration responses.
    /// Returns None for FakeTransport (registration watcher not needed in tests).
    fn stdin_writer(&self) -> Option<Arc<Mutex<tokio::io::BufWriter<tokio::process::ChildStdin>>>>;
}
```

**3A.2 Implement `LspTransport` for `ManagedProcess`**

Pure delegation to existing fields. No logic changes.

```rust
#[async_trait]
impl LspTransport for ManagedProcess {
    async fn send(&self, message: &Value) -> Result<(), LspError> {
        process::send(self, message).await
    }

    fn is_alive(&self) -> bool {
        // Note: requires &mut self in current impl.
        // Use try_wait via a wrapper or change is_alive to take &self.
        // See implementation notes below.
    }

    fn last_used(&self) -> Instant { self.last_used }
    fn set_last_used(&self, when: Instant) { self.last_used = when; }
    fn in_flight(&self) -> &AtomicU32 { &self.in_flight }
    fn capabilities(&self) -> DetectedCapabilities { self.capabilities.clone() }
    fn stdin_writer(&self) -> Option<...> { Some(Arc::clone(&self.stdin)) }
}
```

**Implementation note on `is_alive()`:** Current `ManagedProcess::is_alive()` takes `&mut self` because `Child::try_wait()` takes `&mut self`. Options:
1. Wrap `child` in `Mutex<Child>` -- allows `&self` but adds lock overhead
2. Use `unsafe` with `AssertUnwindSafe` -- risky
3. Change trait to `fn is_alive(&mut self)` -- but trait objects need `&self`
4. **Recommended:** Wrap `child` in `Mutex<Child>`. The lock is never contended (single reader task, single supervisor). This makes `is_alive` take `&self`.

**3A.3 Update `LanguageState`**

```rust
struct LanguageState {
    transport: Box<dyn LspTransport>,
    // Removed: process: ManagedProcess  (now via transport)
    // Removed: live_capabilities  (now via transport.capabilities())
    reader_handle: tokio::task::JoinHandle<()>,
    restart_count: u32,
    spawned_at: Instant,
    indexing_complete: Arc<AtomicBool>,
    indexing_completion_source: Arc<Mutex<Option<IndexingCompletionSource>>>,
    indexing_duration_secs: Arc<Mutex<Option<u64>>>,
    indexing_progress_percent: Arc<Mutex<Option<u8>>>,
    live_capabilities: Arc<RwLock<DetectedCapabilities>>,  // kept for dynamic reg
    in_coexistence_mode: bool,
}
```

**3A.4 Update `LspClient` methods**

Every place that accesses `state.process.*` changes to `state.transport.*`:

| Before | After |
|---|---|
| `send(&state.process, &message)` | `state.transport.send(&message).await` |
| `state.process.last_used` | `state.transport.last_used()` |
| `state.process.last_used = Instant::now()` | `state.transport.set_last_used(Instant::now())` |
| `state.process.in_flight` | `state.transport.in_flight()` |
| `state.process.capabilities` | `state.transport.capabilities()` |
| `state.process.stdin` (in registration watcher) | `state.transport.stdin_writer()` |
| `state.process.child` (in shutdown, supervisor) | See 3A.5 |

**3A.5 Handle process lifecycle (shutdown, supervisor, zombie reap)**

These operations need `&mut Child` and cannot be trait-virtualized cleanly:

- `shutdown()` in process.rs -- sends shutdown+exit requests, force-kills child
- `reader_supervisor_task` -- calls `child.wait()` to reap zombie
- `idle_timeout_task` -- calls `shutdown()` and `child.wait()`

**Solution:** Add a separate `ProcessLifecycle` struct that `LanguageState` holds alongside the trait:

```rust
/// OS process lifecycle handle. Only present for real child processes.
/// None for FakeTransport (no OS process to manage).
struct ProcessLifecycle {
    child: tokio::process::Child,
}

struct LanguageState {
    transport: Box<dyn LspTransport>,
    lifecycle: Option<ProcessLifecycle>,  // None in tests
    // ... rest unchanged
}
```

Then `shutdown()`, `reader_supervisor_task`, and `idle_timeout_task` check `lifecycle` and operate on `child` directly. In tests, `lifecycle` is `None` so these paths are skipped.

**3A.6 Update `start_process()`**

Change from creating `ManagedProcess` directly to:
1. Call `spawn_and_initialize()` which returns `(ManagedProcess, reader_handle)`
2. Create `Box<dyn LspTransport>` from the `ManagedProcess`
3. Extract `Child` into `ProcessLifecycle`
4. Build `LanguageState { transport, lifecycle: Some(...), ... }`

**3A.7 Update test helpers**

- `client_with_descriptors()` -- unchanged (no Running entries)
- `client_no_languages()` -- unchanged
- Existing tests -- all pass unchanged (they use Unavailable entries or no entries)

**Completion criteria for 3A:**
- `LspTransport` trait defined with `send`, `is_alive`, `last_used`/`set_last_used`, `in_flight`, `capabilities`, `stdin_writer`
- `ManagedProcess` implements `LspTransport` via delegation
- `LanguageState` holds `Box<dyn LspTransport>` + `Option<ProcessLifecycle>`
- All 61 existing tests in `client/mod.rs` pass
- All 20 tests in `process.rs` pass
- `cargo clippy -- -D warnings` passes
- DeepSource LCV unchanged (~90.5%)

---

#### Subphase 3B: Build `FakeTransport` (1 session)

**Goal:** Create the test double. Wire it into test helpers. Write first tests proving the fake works.

**Files to create/modify:**
- `crates/pathfinder-lsp/src/client/fake_transport.rs` -- new file (~200 lines)
- `crates/pathfinder-lsp/src/client/mod.rs` -- add `mod fake_transport` + test helpers

**3B.1 Create `FakeTransport` struct**

```rust
/// In-memory LSP transport for unit testing. No OS process spawned.
///
/// Reads are satisfied from a pre-configured response queue.
/// Writes are recorded for test assertions.
pub(crate) struct FakeTransport {
    /// Method -> queue of responses. Each request pops the next response.
    responses: Arc<Mutex<HashMap<String, VecDeque<Value>>>>,
    /// All notifications sent (method, params).
    notifications_sent: Arc<Mutex<Vec<(String, Value)>>>,
    /// Whether the transport reports as alive.
    alive: Arc<AtomicBool>,
    /// Last-used timestamp.
    last_used: Arc<Mutex<Instant>>,
    /// In-flight request counter.
    in_flight: Arc<AtomicU32>,
    /// Capabilities snapshot.
    capabilities: DetectedCapabilities,
}
```

**3B.2 Implement `LspTransport` for `FakeTransport`**

- `send()` -- if message has `id` (request), look up response by method, return it. If no `id` (notification), record it.
- `is_alive()` -- returns `alive.load()`
- `last_used()` / `set_last_used()` -- delegates to `Arc<Mutex<Instant>>`
- `in_flight()` -- returns `&AtomicU32`
- `capabilities()` -- returns clone
- `stdin_writer()` -- returns `None` (no registration watcher needed in tests)

**3B.3 Create `FakeTransport` builder**

```rust
impl FakeTransport {
    pub fn new() -> Self { /* defaults */ }

    /// Configure a response for a specific LSP method.
    /// Multiple calls queue multiple responses (FIFO).
    pub fn set_response(&self, method: &str, result: Value) -> &Self;

    /// Configure an error response for a specific LSP method.
    pub fn set_error(&self, method: &str, error_message: &str) -> &Self;

    /// Set the capabilities returned by this transport.
    pub fn with_capabilities(&self, caps: DetectedCapabilities) -> &Self;

    /// Take all recorded notifications, clearing the buffer.
    pub fn take_notifications(&self) -> Vec<(String, Value)>;

    /// Kill the transport (set alive to false).
    pub fn kill(&self);
}
```

**3B.4 Create `make_running_client()` test helper**

```rust
/// Create an LspClient with a Running entry using FakeTransport.
/// Returns (client, fake_transport) so tests can configure responses.
fn make_running_client(
    language_id: &str,
) -> (LspClient, Arc<FakeTransport>) {
    let fake = Arc::new(FakeTransport::new());
    let dispatcher = Arc::new(RequestDispatcher::new());
    let (shutdown_tx, _) = broadcast::channel(1);

    let reader_handle = tokio::spawn(async {}); // dummy

    let entry = ProcessEntry::Running(Box::new(LanguageState {
        transport: Box::new(Arc::clone(&fake)) as Box<dyn LspTransport>,
        lifecycle: None,
        reader_handle,
        restart_count: 0,
        spawned_at: Instant::now(),
        indexing_complete: Arc::new(AtomicBool::new(true)),
        indexing_completion_source: Arc::new(Mutex::new(Some(
            IndexingCompletionSource::Progress,
        ))),
        indexing_duration_secs: Arc::new(Mutex::new(Some(0))),
        indexing_progress_percent: Arc::new(Mutex::new(None)),
        live_capabilities: Arc::new(RwLock::new(DetectedCapabilities::default())),
        in_coexistence_mode: false,
    }));

    let descriptors = vec![LspDescriptor {
        language_id: language_id.to_owned(),
        command: "fake-lsp".to_owned(),
        args: vec![],
        root: std::env::temp_dir(),
        init_timeout_secs: None,
        auto_plugins: vec![],
        init_options: serde_json::Value::Null,
    }];

    let processes = DashMap::new();
    processes.insert(language_id.to_owned(), entry);

    let client = LspClient {
        descriptors: Arc::new(descriptors),
        missing_languages: Arc::new(Vec::new()),
        processes: Arc::new(processes),
        init_locks: Arc::new(DashMap::new()),
        dispatcher,
        shutdown_tx: Arc::new(shutdown_tx),
        doc_versions: Arc::new(DashMap::new()),
        warm_start_complete: Arc::new(AtomicBool::new(false)),
    };

    (client, fake)
}
```

**3B.5 Proving tests (5 tests)**

Verify the FakeTransport infrastructure works end-to-end:

```
test_fake_transport_request_returns_configured_response
test_fake_transport_notify_records_notification
test_fake_transport_kill_reports_not_alive
test_running_client_request_sends_via_transport
test_running_client_notify_records_notification
```

**Completion criteria for 3B:**
- `FakeTransport` implements `LspTransport`
- Builder API: `set_response`, `set_error`, `with_capabilities`, `take_notifications`, `kill`
- `make_running_client()` helper creates LspClient with Running FakeTransport entry
- 5 proving tests pass
- All existing tests still pass
- `cargo clippy -- -D warnings` passes
- DeepSource LCV unchanged (~90.5%)

---

#### Subphase 3C: Test request routing + document operations (1 session)

**Goal:** Cover `request()`, `notify()`, `did_open()`, `did_close()`, `open_document()`, `DocumentGuard`.

**Files to modify:**
- `crates/pathfinder-lsp/src/client/mod.rs` -- add tests to `mod tests`

**3C.1 Request routing tests (7 tests)**

```
test_request_with_running_process_returns_response
test_request_with_running_process_times_out
test_request_with_dead_reader_removes_entry
test_request_in_flight_guard_on_running_process
test_notify_with_running_process_records_notification
test_notify_updates_last_used_timestamp
test_capabilities_for_running_process_returns_caps
```

Coverage targets:
- `request()` lines 1186-1244 (send message, await response, in-flight guard)
- `notify()` lines 1250-1264 (send notification)
- `capabilities_for()` lines 1269-1284 (read caps from Running entry)
- `touch()` line 228 (update last_used)

**3C.2 Document operation tests (6 tests)**

```
test_did_open_sends_notification_and_tracks_version
test_did_close_sends_notification_and_removes_version
test_open_document_returns_document_guard
test_document_guard_drop_sends_did_close
test_did_open_unknown_extension_returns_no_lsp
test_did_close_removes_doc_version_even_if_notify_fails
```

Coverage targets:
- `did_open()` lines 633-670 (build params, notify, track version, touch)
- `did_close()` lines 675-702 (build params, notify, remove version)
- `open_document()` lines 615-625 (call did_open, return DocumentGuard)
- `DocumentGuard::drop()` lines 329-339 (spawn did_close task)

**3C.3 Version tracking tests (3 tests)**

```
test_doc_versions_inserted_on_did_open
test_doc_versions_removed_on_did_close
test_multiple_opens_track_latest_version
```

Coverage targets:
- `doc_versions` DashMap operations in `did_open` and `did_close`

**Completion criteria for 3C:**
- 16 new tests pass
- Covers `request()`, `notify()`, `did_open()`, `did_close()`, `open_document()`, `DocumentGuard`, `touch()`, `capabilities_for()`
- All existing tests still pass
- `cargo clippy -- -D warnings` passes
- DeepSource LCV should reach ~90.8%

---

#### Subphase 3D: Test Lawyer trait methods + background tasks (1 session)

**Goal:** Cover `goto_definition()`, `call_hierarchy_*`, `references()`, `goto_implementation()`, `force_respawn()`, and background task integration.

**Files to modify:**
- `crates/pathfinder-lsp/src/client/mod.rs` -- add tests to `mod tests`

**3D.1 Lawyer trait full-flow tests (8 tests)**

Configure FakeTransport with specific responses to exercise response parsing through the full pipeline:

```
test_lawyer_goto_definition_with_location_response
test_lawyer_goto_definition_with_null_response
test_lawyer_goto_definition_with_array_response
test_lawyer_call_hierarchy_prepare_with_items
test_lawyer_call_hierarchy_incoming_with_calls
test_lawyer_call_hierarchy_outgoing_with_calls
test_lawyer_references_with_locations
test_lawyer_goto_implementation_with_locations
```

Each test:
1. Creates `make_running_client("rust")`
2. Configures FakeTransport with a specific JSON-RPC response
3. Calls the Lawyer trait method
4. Asserts parsed result matches expected

Coverage targets:
- `goto_definition()` lines 1372-1428
- `call_hierarchy_prepare()` lines 1430-1487
- `call_hierarchy_incoming()` / `outgoing()` via `call_hierarchy_request()` lines 1293-1339
- `references()` lines 1521-1575
- `goto_implementation()` lines 1577-1630
- Response parsers called from the above methods

**3D.2 `force_respawn()` tests (3 tests)**

```
test_force_respawn_removes_running_entry_and_starts_new
test_force_respawn_no_descriptor_returns_no_lsp
test_force_respawn_unavailable_entry_removed_directly
```

Note: `force_respawn` with a Running entry needs to kill the old process (via `lifecycle`). With `FakeTransport`, `lifecycle` is `None`, so the shutdown path is skipped. The test verifies the entry removal and re-insertion logic. To fully test the kill path, we need a real process (integration test).

**3D.3 Background task integration tests (6 tests)**

Inject messages into the dispatcher to simulate background task scenarios:

```
test_progress_watcher_receives_end_notification
test_progress_watcher_receives_report_notification
test_progress_watcher_exits_on_channel_close
test_registration_watcher_handles_register
test_registration_watcher_handles_unregister
test_reader_supervisor_on_crash_inserts_unavailable
```

These test the full async task loop, not just the pure functions. They create a `RequestDispatcher`, subscribe to its channels, inject messages, and verify state changes.

**3D.4 Refactor background tasks to use pure functions**

The pure functions `extract_progress_action`, `apply_progress_action`, `extract_registration_action`, `build_registration_response` are extracted but the actual tasks still have inline duplicate logic. Refactor:

- `progress_watcher_task` (lines ~2140-2234): Replace inline kind-checking with `extract_progress_action` + `apply_progress_action`
- `registration_watcher_task` (lines ~2244-2351): Replace inline registration handling with `extract_registration_action` + `build_registration_response`

This ensures the tasks exercise the tested pure functions and eliminates the coverage gap where the pure functions are tested but the task loops are not.

**Completion criteria for 3D:**
- 17 new tests pass
- Background tasks refactored to use extracted pure functions
- Covers all Lawyer trait methods + `force_respawn` + background task loops
- All existing tests still pass
- `cargo clippy -- -D warnings` passes
- DeepSource LCV should reach ~91.0%

---

#### Subphase 3E: Test process lifecycle + idle timeout + edge cases (1 session)

**Goal:** Cover `start_process()`, `ensure_process()` full lifecycle, `idle_timeout_task`, `detect_concurrent_lsp`, and remaining edge cases.

**Files to modify:**
- `crates/pathfinder-lsp/src/client/mod.rs` -- add tests to `mod tests`

**3E.1 `start_process()` integration tests (4 tests)**

These tests spawn a real minimal process (not FakeTransport) to test the full spawn + initialize handshake:

```
test_start_process_spawns_and_initializes_successfully
test_start_process_inserts_unavailable_on_spawn_failure
test_start_process_records_running_entry_with_capabilities
test_ensure_process_full_lifecycle_unavailable_to_running
```

Use `cat` or `sleep` as the child process. The echoed initialize request satisfies the handshake (returns `result: null`, capabilities default to all-disabled).

Note: `start_process()` is `async fn` on `LspClient`, not public. Test via `ensure_process()` which calls it internally. Create a descriptor with command="/usr/bin/cat" or similar.

**3E.2 `idle_timeout_task` tests (3 tests)**

```
test_idle_timeout_removes_process_after_timeout
test_idle_timeout_does_not_remove_process_with_in_flight
test_idle_timeout_shutdown_terminates_all_processes
```

Approach: Create a `DashMap` with a Running entry whose `last_used` is set to > 15 minutes ago. Spawn `idle_timeout_task`. Verify the entry is removed. For in-flight test, set `in_flight` > 0.

Note: These tests need `ProcessLifecycle` with a real child (for the `shutdown` call). Use `sleep 999` as the child.

**3E.3 `detect_concurrent_lsp` test (1 test)**

```
test_detect_concurrent_lsp_returns_false_for_build_artifact
```

Only test the build-artifact guard (path contains "target" or ".cargo"). Skip `/proc` scanning tests -- they're Linux-specific and require specific process fixtures.

**3E.4 Edge case tests (5 tests)**

```
test_capability_status_with_running_entry_shows_connected
test_ensure_process_concurrent_init_prevents_double_spawn
test_reader_supervisor_on_clean_eof_no_unavailable_entry
test_idle_timeout_reaps_dead_zombie_processes
test_request_with_killed_transport_returns_connection_lost
```

**Completion criteria for 3E:**
- 13 new tests pass
- Covers `start_process()`, `ensure_process()` lifecycle, `idle_timeout_task`, `detect_concurrent_lsp`
- All existing tests still pass
- `cargo clippy -- -D warnings` passes
- DeepSource LCV should reach ~91-92%

---

**Overall Phase 3 completion criteria:**
- `LspTransport` trait + `FakeTransport` test double merged
- `LanguageState` uses `Box<dyn LspTransport>` + `Option<ProcessLifecycle>`
- Background tasks refactored to use extracted pure functions
- ~51 new tests across 5 subphases
- All existing tests pass
- DeepSource LCV should reach ~91-92%

---

### Phase 4: LSP Client Method Coverage (2-3 sessions)

Using the infrastructure from Phase 3, systematically cover `client/mod.rs` methods.

**Work order (by dependency chain, bottom-up):**

**4.1 Pure extraction targets (no transport needed)**

These can be done immediately without Phase 3 infrastructure:

- `indexing_timeout_for_language()` -- already tested, verify coverage
- `validation_status_from_parts()` -- already tested, verify coverage
- `parse_definition_response()` -- already tested, verify coverage
- `parse_call_hierarchy_*` -- already tested, verify coverage

If these still show as uncovered, the issue is that they're called inline (not via the public test-exposed versions). Check if the uncovered lines are in private helper variants.

**4.2 Process management (needs Phase 3 infrastructure)**

Now simplified by Phase 3's FakeTransport:
- `ensure_process()` -- test spawn, unavailable cooldown, concurrent detection (covered in 3E)
- `start_process()` -- test initialization handshake, error handling (covered in 3E)
- `force_respawn()` -- test kill + restart cycle (covered in 3D)
- `detect_concurrent_lsp()` -- test `/proc` scanning (Linux-only, covered in 3E)

**4.3 Document operations (covered in Phase 3C)**

- `did_open()` / `did_close()` -- covered
- `open_document()` -- `DocumentGuard` RAII lifecycle -- covered
- `DocumentGuard` drop sends `did_close` -- covered

**4.4 LSP request methods (covered in Phase 3D)**

- `goto_definition()` -- covered
- `call_hierarchy_prepare()` / `incoming()` / `outgoing()` -- covered
- `references()` / `goto_implementation()` -- covered
- `capability_status()` -- covered
- `missing_languages()` -- trivially tested, verify coverage

**4.5 Lawyer trait methods (covered in Phase 3D)**

The `impl Lawyer for LspClient` block delegates to the internal methods above. After covering those in Phase 3, the lawyer methods are covered automatically.

Phase 4 is now primarily a **verification phase** to confirm coverage and fill any remaining gaps.

**Completion criteria for Phase 4:**
- All methods in `client/mod.rs` have >80% line coverage
- DeepSource LCV should reach ~93-95%
- Raise DeepSource threshold to 92%

---

### Phase 5: Structural Improvement (Optional, Future)

These are not required for coverage but would prevent future regression:

**5.1 Split `client/mod.rs` into modules**

Current: 3657 lines, 80% of coverage gap.
Target structure:

```
crates/pathfinder-lsp/src/client/
  mod.rs              -- LspClient struct + public API (~500 lines)
  lifecycle.rs        -- new(), shutdown(), warm_start() (~300 lines)
  process.rs          -- already exists (961 lines)
  document.rs         -- did_open/did_close/open_document/DocumentGuard (~400 lines)
  requests.rs         -- goto_definition, call_hierarchy_*, references (~500 lines)
  capabilities.rs     -- already exists (606 lines)
  background.rs       -- reader_supervisor, progress_watcher, etc. (~600 lines)
  transport.rs        -- already exists (402 lines)
  protocol.rs         -- already exists (434 lines)
  detect.rs           -- already exists (1937 lines)
```

This makes it feasible to achieve high coverage on each module independently.

**5.2 Split `navigation.rs` into modules**

Current: 7508 lines.
Target structure:

```
crates/pathfinder/src/server/tools/
  navigation/
    mod.rs              -- shared types, helpers (~500 lines)
    definition.rs       -- get_definition_impl (~800 lines)
    impact.rs           -- analyze_impact_impl, BFS (~1200 lines)
    deep_context.rs     -- read_with_deep_context_impl (~800 lines)
    references.rs       -- find_all_references_impl (~800 lines)
    symbol_overview.rs  -- symbol_overview_impl (~800 lines)
    grep_fallback.rs    -- shared grep fallback logic (~500 lines)
```

---

## Session Execution Guide

### How to use this document across sessions

Each phase is designed to be independently executable. Follow this protocol:

**At session start:**
1. Read the phase you're working on
2. Read the "Uncovered regions" table to understand what lines need coverage
3. Read the existing test file to understand current patterns
4. Check `cargo test` baseline

**During the session:**
1. Write tests following existing patterns in the target file
2. Run `cargo test` after every 2-3 new tests
3. Run `cargo clippy -- -D warnings` before committing
4. If a test requires infrastructure changes (Phase 3+), build the infrastructure first

**At session end:**
1. Run full `cargo test` + `cargo clippy`
2. Note remaining gaps in this document (update line numbers if they shifted)
3. Commit with message: `test(TCV-001): cover <file> <region> (<N> lines)`

### Dependency graph

```
Phase 0 (threshold) ─── independent, do first
Phase 1 (Tier 2)   ─── independent of everything
Phase 2 (navigation)── independent of Phase 1
Phase 3 (infra)     ─── independent, but do before Phase 4
Phase 4 (client)    ─── depends on Phase 3
Phase 5 (split)     ─── depends on Phase 4
```

Phases 1 and 2 can be done in parallel or in any order.
Phase 3 is the gate for Phase 4.
Phase 5 is optional cleanup.

### Expected coverage progression

| After Phase | LCV Estimate | Threshold |
|---|---|---|
| Current | 88.9% | None |
| Phase 0 | 88.9% | 88% |
| Phase 1 | ~89.5% | 88% |
| Phase 2 | ~90.5% | 90% |
| Phase 3 | ~91.0% | 90% |
| Phase 4 | ~93-95% | 92% |

---

## DeepSource Configuration

After Phase 0, set this threshold:

```
Repository: irahardianto/pathfinder
Metric: LCV (Line Coverage)
Key: Rust
Threshold: 88%
```

Raise to 90% after Phase 2.
Raise to 92% after Phase 4.

---

## Key Files Reference

### Files to read before starting ANY phase

1. `crates/pathfinder-lsp/src/mock.rs` -- existing mock patterns (MockLawyer, MockDocumentLease)
2. `crates/pathfinder-search/src/mock.rs` -- existing MockScout pattern
3. `crates/pathfinder-lsp/src/client/process.rs` -- ManagedProcess internals
4. `crates/pathfinder-lsp/src/client/transport.rs` -- transport layer
5. `crates/pathfinder-lsp/src/lawyer.rs` -- Lawyer trait definition

### Test file locations

| Source File | Test Location | Test Count |
|---|---|---|
| `client/mod.rs` | Inline `mod tests` | 61 |
| `client/detect.rs` | Inline `mod tests` | 58 |
| `client/process.rs` | Inline `mod tests` | 20 |
| `client/transport.rs` | Inline `mod tests` | 17 |
| `navigation.rs` | Inline `mod tests` | 97 |
| `read_files.rs` | Inline `mod tests` | 14 |
| `repo_map.rs` | Inline `mod tests` | 9 |
| `search.rs` | Inline `mod tests` | 10 |
| `git.rs` | Inline `mod tests` + `tests/git_integration.rs` | 6+5 |
| `parser.rs` | Inline `mod tests` | 11 |
