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

### Phase 4: Verification + Residual Gap Closure (3-4 sessions)

Using Phase 3 infrastructure, close remaining coverage gaps in navigation tools and LSP client.

Phase 2 left `symbol_overview_impl` (0 tests) and `find_all_references_impl` (under-tested).
Phase 3 left background task loops and `spawn_indexing_timeout_fallback` without direct tests.

Split into 4 progressive subphases. Each independently committable and verifiable.

**Current state at Phase 4 start:**

| File | Existing Tests | Known Gaps |
|---|---|---|
| `navigation.rs` | 103 | `symbol_overview_impl` (0 tests), `find_all_references_impl` (2 tests, missing degraded/error/pagination) |
| `client/mod.rs` | 125 | `spawn_indexing_timeout_fallback` (0 tests), background task loops (helpers tested, loops untested), `parse_references_response` (no isolated tests) |

---

#### Subphase 4A: `symbol_overview_impl` Coverage (1 session, HIGHEST priority)

**Goal:** Cover `symbol_overview_impl` (lines 2893-3135, ~243 lines of orchestration logic).
Zero tests exist. This is the largest single uncovered function in navigation.rs.

**Why first:** `symbol_overview_impl` orchestrates `analyze_impact_impl` + `find_all_references_impl`
and contains deserialization fallback logic with `debug_assert` guards that are completely untested.

**Files to modify:**
- `crates/pathfinder/src/server/tools/navigation.rs` -- add tests to `mod tests`

**Test infrastructure:** Same pattern as existing navigation tests. Use `make_server_with_lawyer(surgeon, lawyer)` for LSP paths, or `PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, NoOpLawyer)` for degraded paths. Deserialize `SymbolOverviewResponse` from `structured_content`.

**4A.1 Happy path tests (3 tests)**

```
test_symbol_overview_aggregates_callers_callees_references
test_symbol_overview_no_impact_no_references_shows_unavailable
test_symbol_overview_with_implementations_and_references
```

Each test:
1. Create `MockSurgeon` + `MockLawyer` configured for `call_hierarchy_prepare` + `call_hierarchy_incoming`/`outgoing` + `references` + `goto_implementation`
2. Call `server.symbol_overview_impl(params).await`
3. Deserialize `SymbolOverviewResponse` from `structured_content`
4. Assert callers/callees/references counts, degraded=false, lsp_readiness="ready"

Coverage targets:
- Source extraction via `read_symbol_scope_enriched` (L2921-2930)
- Impact aggregation + `ImpactSummary` construction (L2976-3018)
- References aggregation (L3032-3062)
- Text formatting: source_block + impact_block + refs_block + degraded_block (L3095-3128)
- `SymbolOverviewResponse` construction (L3084-3093)

**4A.2 Degraded path tests (3 tests)**

```
test_symbol_overview_degraded_when_lsp_unavailable
test_symbol_overview_degraded_on_lsp_error
test_symbol_overview_partial_degradation_impact_fails_refs_ok
```

Pattern: Use `NoOpLawyer` for NoLspAvailable, `MockLawyer` with error for LSP error paths.
The partial degradation test: `analyze_impact_impl` returns degraded, `find_all_references_impl` returns ok.

Coverage targets:
- `Err(_)` branch for impact_result (L3021)
- `Err(_)` branch for refs_result (L3061)
- Degraded flag merging: `impact_degraded || refs_degraded` (L3066)
- Degraded reason priority: impact first, then refs (L3067-3073)
- `lsp_readiness` based on degraded_reason variant (L3075-3082)
- Degraded text formatting (L3117-3124)

**4A.3 Validation + edge case tests (3 tests)**

```
test_symbol_overview_rejects_empty_semantic_path
test_symbol_overview_file_not_found_returns_error
test_symbol_overview_respects_max_callers_callees_limit
```

Coverage targets:
- `parse_semantic_path` + `require_symbol_target` validation (L2905-2906)
- Sandbox check (L2908-2910)
- File existence check (L2912-2919)
- `max_callers_callees` parameter forwarding to `analyze_impact_impl` (L2969)
- `open_document` error path -> `None` doc_guard (L2955-2963)

**Completion criteria for 4A:**
- 9 new tests pass
- `symbol_overview_impl` has >90% line coverage
- All existing tests still pass
- `cargo clippy -- -D warnings` passes
- DeepSource LCV should reach ~90.5-91%

---

#### Subphase 4B: `find_all_references_impl` Full Coverage (1 session)

**Goal:** Cover remaining branches in `find_all_references_impl` (lines 2540-2886).
Currently only 2 tests (happy path + max_results). Missing: degraded paths, error paths, pagination, implementations.

**Files to modify:**
- `crates/pathfinder/src/server/tools/navigation.rs` -- extend existing test section

**4B.1 Degraded path tests (3 tests)**

```
test_find_all_references_degraded_when_no_lsp
test_find_all_references_lsp_error_degraded
test_find_all_references_connection_lost_degraded
```

Pattern:
- NoLsp: use `NoOpLawyer` -> expect `degraded=true`, `degraded_reason=Some(NoLsp)`
- LSP error: `MockLawyer` with `set_references_result(Err("protocol error"))` -> degraded
- Connection lost: `MockLawyer` with `set_references_result(Err("connection lost"))` -> degraded

Coverage targets:
- `Err(LspError::NoLspAvailable)` match arm (L2799-2833)
- `Err(e)` generic error match arm (L2834-2884)
- `DegradedReason` mapping for Protocol vs ConnectionLost vs Timeout (L2844-2849)
- `lsp_readiness` and `warm_start_in_progress` for timeout vs other errors (L2852-2858)

**4B.2 Pagination + implementations tests (3 tests)**

```
test_find_all_references_with_implementations_and_references
test_find_all_references_offset_skips_implementations
test_find_all_references_offset_past_implementations_paginates_references
```

Pattern:
- Configure `MockLawyer` with both `set_references_result(Ok([...]))` and `set_goto_implementation_result(Ok([...]))`
- Test pagination logic: implementations first, then references (L2687-2708)
- Test offset past implementation count (L2688-2696)

Coverage targets:
- `implementations_result` branch: Ok vs Err (L2647-2658)
- `all_files` chain for `files_referenced` (L2660-2665)
- Pagination logic: offset >= impl_count vs offset < impl_count (L2687-2708)
- Text formatting: implementations header + references header (L2727-2750)
- Summary formatting: impl+ref, impl-only, ref-only, zero (L2763-2776)
- Pagination note when truncated (L2752-2761)

**4B.3 Edge case tests (2 tests)**

```
test_find_all_references_zero_references_zero_implementations
test_find_all_references_rejects_sandbox_denied_path
```

Coverage targets:
- Zero references path (L2774)
- Sandbox check failure (L2555-2564)
- File not found path (L2567-2578)

**Completion criteria for 4B:**
- 8 new tests pass
- `find_all_references_impl` has >90% line coverage
- All existing tests still pass
- `cargo clippy -- -D warnings` passes
- DeepSource LCV should reach ~91%

---

#### Subphase 4C: Navigation Residual Gaps (1 session)

**Goal:** Close remaining Phase 2 gaps in `analyze_impact_impl` and callers/callees paths.

**Files to modify:**
- `crates/pathfinder/src/server/tools/navigation.rs` -- add tests to `mod tests`

**4C.1 BFS cycle detection + deduplication (2 tests)**

```
test_analyze_impact_bfs_handles_cycle_in_call_graph
test_analyze_impact_bfs_deduplicates_cross_referenced_symbols
```

Pattern:
- Configure `MockLawyer` to return call hierarchy items that reference each other (A->B->A cycle)
- Verify BFS terminates without infinite loop and deduplicates entries

Coverage targets:
- `seen.insert(key)` deduplication check (bfs_call_hierarchy L1917)
- `queue.push_back` with incremented depth (L1918)
- `remaining_references` budget decrement (L1936)

**4C.2 Callers/callees grep fallback paths (2 tests)**

```
test_find_callers_callees_grep_fallback_incoming
test_find_callers_callees_grep_fallback_outgoing
```

Pattern:
- Use `NoOpLawyer` + `MockScout` with search results
- Test that grep fallback provides incoming/outgoing heuristic references when LSP unavailable

Coverage targets:
- Grep fallback path in `analyze_impact_impl` for both `CallDirection::Incoming` and `CallDirection::Outgoing`
- Direction-specific formatting (L1930-1933)

**Completion criteria for 4C:**
- 4 new tests pass
- All existing tests still pass
- `cargo clippy -- -D warnings` passes
- DeepSource LCV should reach ~91-91.5%

---

#### Subphase 4D: LSP Client Residual Gaps (1 session)

**Goal:** Cover `spawn_indexing_timeout_fallback`, `parse_references_response`, and background task
integration paths in `client/mod.rs`.

**Files to modify:**
- `crates/pathfinder-lsp/src/client/mod.rs` -- add tests to `mod tests`

**4D.1 `spawn_indexing_timeout_fallback` tests (2 tests)**

```
test_spawn_indexing_timeout_fallback_sets_complete_after_timeout
test_spawn_indexing_timeout_fallback_noop_if_already_complete
```

Pattern:
- Use `tokio::time::pause()` + `tokio::time::advance(timeout)` to simulate timeout
- Create the `indexing_complete`, `indexing_completion_source`, `indexing_duration_secs` flags
- Call the function, advance time past the timeout, assert `indexing_complete` is true
- Second test: set `indexing_complete = true` before advancing, verify no mutation

Coverage targets:
- Branch A: timeout fires before progress End -> sets complete + source + duration (L868-876)
- Branch B: flag already true (progress End arrived first) -> no-op (L878)

**4D.2 `parse_references_response` isolated tests (3 tests)**

```
test_parse_references_response_with_locations
test_parse_references_response_null_returns_empty
test_parse_references_response_invalid_uri_skipped
```

These are pure functions already tested indirectly via Lawyer integration tests.
Isolated unit tests ensure edge cases (null, invalid URI) are covered directly.

Coverage targets:
- Null response -> empty vec
- Location with invalid URI -> skipped
- Location with valid URI -> included

**4D.3 Background task integration tests (3 tests)**

```
test_progress_watcher_end_after_timeout_fallback_logs_warning
test_registration_watcher_send_failure_continues
test_reader_supervisor_crash_inserts_unavailable
```

Pattern:
- Progress watcher: create dispatcher, subscribe, inject `$/progress` End after setting `indexing_complete = true`
- Registration watcher: create FakeTransport that fails `send()`, inject registration request, verify no panic
- Reader supervisor: create `JoinHandle` that panics, spawn supervisor, verify `Unavailable` entry inserted

Coverage targets:
- Progress watcher Branch A1a: End arrives AFTER timeout fallback already fired (L2212-2215)
- Registration watcher Branch A3a: `transport.send` fails (L2298-2299)
- Reader supervisor Branch B+C3: reader panic + crash path inserting UnavailableState (L2119-2122, L2134-2136)

**Completion criteria for 4D:**
- 8 new tests pass
- Covers `spawn_indexing_timeout_fallback`, `parse_references_response`, background task edge cases
- All existing tests still pass
- `cargo clippy -- -D warnings` passes
- DeepSource LCV should reach ~91.5-92%

---

**Overall Phase 4 completion criteria:**
- ~29 new tests across 4 subphases
- `symbol_overview_impl` >90% coverage
- `find_all_references_impl` >90% coverage
- All navigation tool BFS/grep fallback paths covered
- LSP client background task edge cases covered
- DeepSource LCV should reach ~91.5-92%
- Raise DeepSource threshold to 92%

---

### Phase 5: Structural Improvement (5-7 sessions)

Split the two monolithic files that caused 93% of coverage gaps into focused
sub-modules. No new tests -- purely structural refactoring that prevents future
regression by making per-module coverage tracking feasible.

**Post-Phase 4 file sizes:**

| File | Current Lines | Problem |
|---|---|---|
| `navigation.rs` | 8975 | Monolithic, hard to navigate, coverage gaps clustered |
| `client/mod.rs` | 5522 | 80% of historical coverage gaps, largest file in crate |

**Dependency analysis (from codebase exploration):**

navigation.rs -- only 1 truly shared helper: `read_symbol_scope_enriched`
(called by 4 tool methods). All other helpers are private to their tool's call
tree. Clean split boundaries exist.

client/mod.rs -- response parsers and background tasks are free functions (easy
to move). `impl LspClient` methods access private fields, requiring `pub(crate)`
field visibility before extraction.

**Guiding principles for all sub-phases:**

1. Each sub-phase compiles and passes all tests independently
2. Each sub-phase = one commit
3. No public API changes -- all `pub(crate)` or internal
4. Move tests alongside their implementations (Rust convention)
5. Extract in dependency order: zero-dep pieces first

---

#### Subphase 5A: Split `navigation.rs` into directory module (8 sub-phases)

Target: `crates/pathfinder/src/server/tools/navigation.rs` (8975 lines)

Current structure:

```
navigation.rs
  L1-179     Constants, shared types (CallDirection, LspResolution)
  L184-421   Shared free fns (extract_call_candidates, keywords_for_language,
             language_to_file_glob, definition_patterns)
  L423-3732  impl PathfinderServer -- single large block containing:
    L434-501    grep_reference_fallback         (PRIVATE to analyze_impact)
    L516-670    resolve_lsp_dependencies         (PRIVATE to read_with_deep_context)
    L673-718    resolve_candidate_via_grep       (PRIVATE to read_with_deep_context)
    L721-800    attempt_grep_fallback            (PRIVATE to read_with_deep_context)
    L808-862    append_outgoing_deps             (PRIVATE to read_with_deep_context)
    L881-1224   get_definition_impl              (TOOL METHOD)
    L1230-1264  get_def_to_call_result           (PRIVATE to get_definition)
    L1266-1281  compute_did_you_mean             (PRIVATE to get_definition)
    L1286-1341  enrich_did_you_mean              (PRIVATE to read_symbol_scope_enriched)
    L1347-1374  read_symbol_scope_enriched       (SHARED: 4 tools)
    L1382-1415  fallback_definition_grep         (PRIVATE to get_definition)
    L1420-1473  grep_definition_in_file          (PRIVATE to get_definition)
    L1476-1542  grep_impl_method                 (PRIVATE to get_definition)
    L1546-1590  grep_definition_global           (PRIVATE to get_definition)
    L1596-1633  grep_symbol_broad                (PRIVATE to get_definition)
    L1644-1834  read_with_deep_context_impl      (TOOL METHOD)
    L1841-1958  bfs_call_hierarchy               (PRIVATE to analyze_impact)
    L1968-2531  analyze_impact_impl              (TOOL METHOD)
    L2540-2886  find_all_references_impl          (TOOL METHOD)
    L2893-3135  symbol_overview_impl              (TOOL METHOD)
    L3144-3589  lsp_health_impl                   (TOOL METHOD)
    L3592-3617  probe_language_readiness          (PRIVATE to lsp_health)
    L3620-3669  find_probe_file                   (PRIVATE to lsp_health)
    L3673-3731  find_file_by_extension_recursive  (PRIVATE to lsp_health)
  L3735-3850  Free helper fns (ProbeAction, format_uptime,
              compute_degraded_tools, parse_uptime_to_seconds)
  L3852-8975  Tests module (~5120 lines, ~90+ tests)
```

Target structure:

```
crates/pathfinder/src/server/tools/navigation/
  mod.rs            -- shared types, constants, free fns, read_symbol_scope_enriched (~500 lines)
  health.rs         -- lsp_health_impl + probe/uptime helpers + ~25 tests (~1200 lines)
  definition.rs     -- get_definition_impl + grep fallback chain + ~15 tests (~1100 lines)
  deep_context.rs   -- read_with_deep_context_impl + dep resolution + ~6 tests (~650 lines)
  impact.rs         -- analyze_impact_impl + bfs_call_hierarchy + grep_reference_fallback
                       + ~18 tests (~1000 lines)
  references.rs     -- find_all_references_impl + ~10 tests (~550 lines)
  overview.rs       -- symbol_overview_impl + ~9 tests (~450 lines)
```

---

**5A.1 -- Prep: Create directory module** (~15 min)

Rename `navigation.rs` to `navigation/mod.rs`. Zero logic changes. Rust
resolves `navigation` the same way whether it's a file or directory.

```
git mv navigation.rs navigation/mod.rs
cargo test
```

Completion:
- `cargo test` passes
- `cargo clippy -- -D warnings` passes
- Commit: `refactor(navigation): convert to directory module`

**5A.2 -- Extract `health.rs`** (~45 min)

Most independent module. Zero cross-deps on other tool methods.

Items to move:

| Item | Current Lines | Visibility |
|---|---|---|
| `lsp_health_impl` | L3144-3589 (~445 lines) | `pub(crate)` |
| `probe_language_readiness` | L3592-3617 | `pub(super)` |
| `find_probe_file` | L3620-3669 | `pub(super)` |
| `find_file_by_extension_recursive` | L3673-3731 | `pub(super)` |
| `format_uptime` | L3735-3755 | `pub(super)` free fn |
| `ProbeAction` enum | L3761-3768 | `pub(super)` |
| `compute_degraded_tools` | L3773-3806 | `pub(super)` free fn |
| `parse_uptime_to_seconds` | L3808-3850 | `pub(super)` free fn |
| ~25 health-related tests | L5239-6550 | move to `mod tests` in health.rs |

Total: ~1200 lines moved.

New file: `navigation/health.rs` with `impl PathfinderServer { pub(crate) async fn lsp_health_impl(...) }`
`mod.rs`: add `mod health;` + remove moved items.

Completion:
- All tests pass, clippy clean
- Commit: `refactor(navigation): extract lsp_health into health.rs`

**5A.3 -- Extract `definition.rs`** (~45 min)

`get_definition_impl` + its entire grep fallback chain. All helpers are private
to this tool's call tree.

Items to move:

| Item | Current Lines | Visibility |
|---|---|---|
| `get_definition_impl` | L881-1224 (~343 lines) | `pub(crate)` |
| `get_def_to_call_result` | L1230-1264 | private |
| `compute_did_you_mean` | L1266-1281 | private |
| `fallback_definition_grep` | L1382-1415 | private |
| `grep_definition_in_file` | L1420-1473 | private |
| `grep_impl_method` | L1476-1542 | private |
| `grep_definition_global` | L1546-1590 | private |
| `grep_symbol_broad` | L1596-1633 | private |
| ~15 definition tests | L3931-4588 | move to definition.rs |

Total: ~1100 lines moved.

Uses shared free fns from mod.rs: `extract_call_candidates`,
`keywords_for_language`, `language_to_file_glob`, `definition_patterns`.
Calls `read_symbol_scope_enriched` (shared, stays in mod.rs).

Completion:
- All tests pass, clippy clean
- Commit: `refactor(navigation): extract get_definition into definition.rs`

**5A.4 -- Extract `deep_context.rs`** (~30 min)

`read_with_deep_context_impl` + dependency resolution helpers.

Items to move:

| Item | Current Lines | Visibility |
|---|---|---|
| `read_with_deep_context_impl` | L1644-1834 (~190 lines) | `pub(crate)` |
| `resolve_lsp_dependencies` | L516-670 | private |
| `resolve_candidate_via_grep` | L673-718 | private |
| `attempt_grep_fallback` | L721-800 | private |
| `append_outgoing_deps` | L808-862 | private |
| ~6 deep_context tests | L4042-4160, L4701-4830 | move to deep_context.rs |

Total: ~650 lines moved.

Calls `read_symbol_scope_enriched` (shared, in mod.rs).

Completion:
- All tests pass, clippy clean
- Commit: `refactor(navigation): extract read_with_deep_context into deep_context.rs`

**5A.5 -- Extract `impact.rs`** (~45 min)

`analyze_impact_impl` + BFS call hierarchy traversal + grep reference fallback.

Items to move:

| Item | Current Lines | Visibility |
|---|---|---|
| `analyze_impact_impl` | L1968-2531 (~563 lines) | `pub(crate)` |
| `bfs_call_hierarchy` | L1841-1958 | private |
| `grep_reference_fallback` | L434-501 | private |
| ~18 impact tests | L4158-4563, L4830-5238, L7665-7805, L8668-8892 | move to impact.rs |

Total: ~1000 lines moved.

Calls `read_symbol_scope_enriched` (shared, in mod.rs).
Uses free fns: `is_source_file`, `is_workspace_file`, `is_test_file`.

Completion:
- All tests pass, clippy clean
- Commit: `refactor(navigation): extract analyze_impact into impact.rs`

**5A.6 -- Extract `references.rs`** (~30 min)

`find_all_references_impl` + its tests. No private helpers -- all logic inline.

Items to move:

| Item | Current Lines | Visibility |
|---|---|---|
| `find_all_references_impl` | L2540-2886 (~346 lines) | `pub(crate)` |
| ~10 reference tests | L7721-8159 | move to references.rs |

Total: ~550 lines moved.

Calls `read_symbol_scope_enriched` (shared, in mod.rs).

Completion:
- All tests pass, clippy clean
- Commit: `refactor(navigation): extract find_all_references into references.rs`

**5A.7 -- Extract `overview.rs`** (~30 min)

`symbol_overview_impl` + its tests. Cross-module calls to impact + references.

Items to move:

| Item | Current Lines | Visibility |
|---|---|---|
| `symbol_overview_impl` | L2893-3135 (~242 lines) | `pub(crate)` |
| ~9 overview tests | L8183-8668 | move to overview.rs |

Total: ~450 lines moved.

Calls `analyze_impact_impl` (impact.rs) + `find_all_references_impl` (references.rs)
+ `read_symbol_scope_enriched` (mod.rs). All `pub(crate)` -- accessible across
sub-modules within the same crate.

Completion:
- All tests pass, clippy clean
- Commit: `refactor(navigation): extract symbol_overview into overview.rs`

**5A.8 -- Clean up `mod.rs`** (~30 min)

What remains in `navigation/mod.rs` after all extractions:

| Item | Lines |
|---|---|
| Module doc comment + imports | ~30 |
| Constants (LIVENESS_PROBE_INTERVAL_SECS, BFS_TIMEOUT_SECS, etc.) | ~25 |
| `SOURCE_FILE_EXTENSIONS`, `is_source_file`, `is_test_file`, `is_workspace_file` | ~100 |
| `CallDirection` enum, `LspResolution` struct | ~15 |
| Shared free fns: `extract_call_candidates`, `keywords_for_language`, `language_to_file_glob`, `definition_patterns` | ~240 |
| `read_symbol_scope_enriched` + `enrich_did_you_mean` (shared helpers) | ~55 |
| `validation_status_from_parts` (if exists here) | ~85 |
| `mod health; mod definition; mod deep_context; mod impact; mod references; mod overview;` | ~10 |
| ~20 shared/pattern tests (grep patterns, keywords, file globs) | ~200 |

Total in mod.rs: ~500 lines (was 8975).

Commit: `refactor(navigation): clean up mod.rs with shared helpers and re-exports`

**Phase 5A completion criteria:**

- `navigation/mod.rs` ~500 lines
- 6 focused sub-modules, each 450-1200 lines
- Tests co-located with their implementations
- All tests pass, `cargo clippy -- -D warnings` passes
- No public API changes

---

#### Subphase 5B: Split `client/mod.rs` into sub-modules (7 sub-phases)

Target: `crates/pathfinder-lsp/src/client/mod.rs` (5522 lines)

Current structure:

```
client/mod.rs
  L13-50      Module declarations + pub use re-exports
  L47-51      Constants (DEFAULT_IDLE_TIMEOUT, MAX_BACKOFF_SECS, IDLE_CHECK_INTERVAL)
  L56-58      ProcessLifecycle struct
  L60-100     LanguageState struct
  L106-112    UnavailableState struct
  L114-118    ProcessEntry enum + to_validation_status
  L200-284    validation_status_from_parts (free fn)
  L287-302    InFlightGuard struct + impl
  L323-355    DocumentGuard struct + impl
  L365-390    LspClient struct (private fields)
  L393-401    indexing_timeout_for_language (free fn)
  L403-1392   impl LspClient -- private methods:
    L413-447    new (constructor)
    L452-459    warm_start
    L468-530    warm_start_for_languages_and_track
    L541-587    warm_start_for_languages
    L595-605    touch_language
    L616-619    shutdown
    L630-642    open_document
    L648-685    did_open (private)
    L690-717    did_close (private)
    L730-762    force_respawn
    L769-848    ensure_process (private)
    L856-888    spawn_indexing_timeout_fallback (private)
    L892-1074   start_process (private)
    L1081-1199  detect_concurrent_lsp (private)
    L1202-1208  touch (private)
    L1214-1280  request (private)
    L1286-1316  notify (private)
    L1321-1336  capabilities_for (private)
    L1345-1391  call_hierarchy_request (private)
  L1395-1729  impl Lawyer for LspClient -- trait methods:
    L1397-1402  warm_start_for_languages (dispatch to self)
    L1405-1407  touch_language (dispatch to self)
    L1414-1422  open_document (dispatch to self)
    L1424-1480  goto_definition
    L1482-1539  call_hierarchy_prepare
    L1541-1555  call_hierarchy_incoming
    L1557-1571  call_hierarchy_outgoing
    L1573-1627  references
    L1629-1682  goto_implementation
    L1684-1711  capability_status
    L1713-1715  missing_languages
    L1717-1719  force_respawn (dispatch to self)
    L1721-1724  is_warm_start_complete
    L1726-1728  warm_start_for_languages_and_track (dispatch to self)
  L1730-2097  Response parser free fns:
    L1730-1821  parse_definition_response
    L1823-1887  parse_single_definition_location
    L1889-1910  parse_definition_response_multi
    L1913-1978  parse_call_hierarchy_prepare_response
    L1981-2026  parse_call_hierarchy_calls_response
    L2028-2097  parse_references_response
  L2109-2585  Background tasks + pure functions:
    L2109-2178  reader_supervisor_task
    L2194-2246  progress_watcher_task
    L2256-2335  registration_watcher_task
    L2338-2438  idle_timeout_task
    L2442-2453  ProgressAction enum
    L2457-2480  extract_progress_action
    L2484-2518  apply_progress_action
    L2522-2529  RegistrationAction struct
    L2533-2575  extract_registration_action
    L2579-2585  build_registration_response
  L2589-5522  Tests module (~2933 lines, ~125 tests)
```

Target structure:

```
crates/pathfinder-lsp/src/client/
  mod.rs              -- struct defs, constants, validation_status_from_parts,
                         re-exports (~700 lines)
  response_parsers.rs -- parse_* free fns + ~20 parser tests (~550 lines)
  background.rs       -- background tasks + pure helpers + ~20 tests (~800 lines)
  document.rs         -- DocumentGuard + did_open/did_close/open_document + ~12 tests (~350 lines)
  lawyer_impl.rs      -- impl Lawyer for LspClient + ~8 tests (~500 lines)
  lifecycle.rs        -- new, warm_start*, ensure_process, start_process,
                         detect_concurrent_lsp, touch, request, notify,
                         capabilities_for, spawn_indexing_timeout_fallback,
                         indexing_timeout_for_language + ~30 tests (~600 lines)
  process.rs          -- already exists (1009 lines)
  capabilities.rs     -- already exists (606 lines)
  fake_transport.rs   -- already exists (164 lines)
  transport.rs        -- already exists (402 lines)
  protocol.rs         -- already exists (434 lines)
  detect.rs           -- already exists (1937 lines)
```

---

**5B.1 -- Prep: Make LspClient fields `pub(crate)`** (~15 min)

Change all private fields of `LspClient` (L365-390) to `pub(crate)`. This enables
`impl LspClient` blocks in sub-module files to access fields.

Also widen visibility on structs that sub-modules will construct:
- `LanguageState` fields -> `pub(crate)`
- `ProcessEntry` fields -> `pub(crate)` (already pub via enum variants)
- `InFlightGuard::new` -> `pub(crate)`
- `DocumentGuard::new` -> `pub(crate)`

No behavior changes. All tests pass.

Completion:
- All tests pass, clippy clean
- Commit: `refactor(client): widen LspClient field visibility to pub(crate)`

**5B.2 -- Extract `response_parsers.rs`** (~30 min)

All `parse_*` free functions. Zero field access on `self`, zero deps on LspClient.

Items to move:

| Item | Current Lines |
|---|---|
| `parse_definition_response` | L1730-1821 (~91 lines) |
| `parse_single_definition_location` | L1823-1887 (~64 lines) |
| `parse_definition_response_multi` | L1889-1910 (~21 lines) |
| `parse_call_hierarchy_prepare_response` | L1913-1978 (~65 lines) |
| `parse_call_hierarchy_calls_response` | L1981-2026 (~45 lines) |
| `parse_references_response` | L2028-2097 (~69 lines) |
| ~20 parser tests | L2820-3093 |

Total: ~550 lines moved. Pure functions, no `self` access.

`mod.rs`: add `mod response_parsers;` + `use response_parsers::*;` (or explicit imports).

Completion:
- All tests pass, clippy clean
- Commit: `refactor(client): extract response parsers into response_parsers.rs`

**5B.3 -- Extract `background.rs`** (~45 min)

Background task functions + extracted pure helpers. Accesses `self.processes`,
`self.shutdown_tx` via `pub(crate)` fields (from 5B.1).

Items to move:

| Item | Current Lines |
|---|---|
| `reader_supervisor_task` | L2109-2178 (~69 lines) |
| `progress_watcher_task` | L2194-2246 (~52 lines) |
| `registration_watcher_task` | L2256-2335 (~79 lines) |
| `idle_timeout_task` | L2338-2438 (~100 lines) |
| `ProgressAction` enum | L2442-2453 (~11 lines) |
| `extract_progress_action` | L2457-2480 (~23 lines) |
| `apply_progress_action` | L2484-2518 (~34 lines) |
| `RegistrationAction` struct | L2522-2529 (~7 lines) |
| `extract_registration_action` | L2533-2575 (~42 lines) |
| `build_registration_response` | L2579-2585 (~6 lines) |
| ~20 background task tests | L2676-2817, L4943-5522 |

Total: ~800 lines moved.

These functions take `Arc<DashMap<...>>` (processes), `Arc<broadcast::Sender>`
(shutdown_tx), etc. as explicit parameters -- not `&self`. Make them `pub(super)`
free functions in background.rs. If any take `&LspClient`, they access fields
through `pub(crate)` visibility.

Completion:
- All tests pass, clippy clean
- Commit: `refactor(client): extract background tasks into background.rs`

**5B.4 -- Extract `document.rs`** (~30 min)

Document operations + `DocumentGuard`. Accesses `self.processes`,
`self.doc_versions` via `pub(crate)` fields.

Items to move:

| Item | Current Lines |
|---|---|
| `DocumentGuard` struct + both impl blocks | L323-355 (~33 lines) |
| `did_open` (private method) | L648-685 (~37 lines) |
| `did_close` (private method) | L690-717 (~27 lines) |
| `open_document` from Lawyer impl | L1414-1422 (~8 lines) |
| ~12 document tests | L4379-4559 |

Total: ~350 lines moved.

New file: `navigation/document.rs` with `impl LspClient { async fn did_open(...), async fn did_close(...), pub async fn open_document(...) }`.

Completion:
- All tests pass, clippy clean
- Commit: `refactor(client): extract document operations into document.rs`

**5B.5 -- Extract `lawyer_impl.rs`** (~30 min)

`impl Lawyer for LspClient` trait methods. Uses response parsers (from 5B.2),
document ops (from 5B.4).

Items to move:

| Item | Current Lines |
|---|---|
| `call_hierarchy_request` (private helper) | L1345-1391 (~46 lines) |
| `warm_start_for_languages` (Lawyer dispatch) | L1397-1402 (~5 lines) |
| `touch_language` (Lawyer dispatch) | L1405-1407 (~2 lines) |
| `open_document` (Lawyer dispatch) | L1414-1422 (~8 lines) |
| `goto_definition` | L1424-1480 (~56 lines) |
| `call_hierarchy_prepare` | L1482-1539 (~57 lines) |
| `call_hierarchy_incoming` | L1541-1555 (~14 lines) |
| `call_hierarchy_outgoing` | L1557-1571 (~14 lines) |
| `references` | L1573-1627 (~54 lines) |
| `goto_implementation` | L1629-1682 (~53 lines) |
| `capability_status` | L1684-1711 (~27 lines) |
| `missing_languages` | L1713-1715 (~2 lines) |
| `force_respawn` (Lawyer dispatch) | L1717-1719 (~2 lines) |
| `is_warm_start_complete` | L1721-1724 (~3 lines) |
| `warm_start_for_languages_and_track` (Lawyer dispatch) | L1726-1728 (~2 lines) |
| ~8 lawyer trait tests | L4580-4877 |

Total: ~500 lines moved.

Completion:
- All tests pass, clippy clean
- Commit: `refactor(client): extract Lawyer trait impl into lawyer_impl.rs`

**5B.6 -- Extract `lifecycle.rs`** (~30 min)

Process lifecycle methods: construction, spawning, request routing, warm start.

Items to move:

| Item | Current Lines |
|---|---|
| `indexing_timeout_for_language` | L393-401 (~8 lines) |
| `new` (constructor) | L413-447 (~34 lines) |
| `warm_start` | L452-459 (~7 lines) |
| `warm_start_for_languages_and_track` | L468-530 (~62 lines) |
| `warm_start_for_languages` | L541-587 (~46 lines) |
| `touch_language` | L595-605 (~10 lines) |
| `shutdown` | L616-619 (~3 lines) |
| `force_respawn` (public method) | L730-762 (~32 lines) |
| `ensure_process` | L769-848 (~79 lines) |
| `spawn_indexing_timeout_fallback` | L856-888 (~32 lines) |
| `start_process` | L892-1074 (~182 lines) |
| `detect_concurrent_lsp` | L1081-1199 (~118 lines) |
| `touch` | L1202-1208 (~6 lines) |
| `request` | L1214-1280 (~66 lines) |
| `notify` | L1286-1316 (~30 lines) |
| `capabilities_for` | L1321-1336 (~15 lines) |
| ~30 lifecycle/state tests | L3098-3346, L3351-3651, L3940-4028 |

Total: ~600 lines moved.

Completion:
- All tests pass, clippy clean
- Commit: `refactor(client): extract lifecycle methods into lifecycle.rs`

**5B.7 -- Clean up `mod.rs`** (~30 min)

What remains in `client/mod.rs` after all extractions:

| Item | Lines |
|---|---|
| Module declarations + pub use re-exports | ~50 |
| Constants | ~5 |
| `ProcessLifecycle` struct | ~3 |
| `LanguageState` struct | ~40 |
| `UnavailableState` struct | ~7 |
| `ProcessEntry` enum + impl | ~90 |
| `validation_status_from_parts` | ~85 |
| `InFlightGuard` struct + impl | ~16 |
| `LspClient` struct definition (fields only) | ~26 |
| `mod response_parsers; mod background; mod document; mod lawyer_impl; mod lifecycle;` | ~5 |
| ~30 remaining tests (struct validation, warm_start flags, etc.) | ~400 |

Total in mod.rs: ~700 lines (was 5522).

Commit: `refactor(client): clean up mod.rs with struct defs and re-exports`

**Phase 5B completion criteria:**

- `client/mod.rs` ~700 lines (was 5522)
- 5 focused sub-modules: response_parsers, background, document, lawyer_impl, lifecycle
- All 125+ tests pass, clippy clean
- No public API changes

---

#### Phase 5 sub-phase summary

| # | Sub-phase | Target | Lines Moved | Est. Time |
|---|---|---|---|---|
| 5A.1 | navigation prep | `navigation/mod.rs` rename | 0 | 15 min |
| 5A.2 | health.rs | `lsp_health_impl` + helpers + tests | ~1200 | 45 min |
| 5A.3 | definition.rs | `get_definition_impl` + grep chain + tests | ~1100 | 45 min |
| 5A.4 | deep_context.rs | `read_with_deep_context_impl` + dep res + tests | ~650 | 30 min |
| 5A.5 | impact.rs | `analyze_impact_impl` + BFS + grep fallback + tests | ~1000 | 45 min |
| 5A.6 | references.rs | `find_all_references_impl` + tests | ~550 | 30 min |
| 5A.7 | overview.rs | `symbol_overview_impl` + tests | ~450 | 30 min |
| 5A.8 | nav mod.rs cleanup | shared helpers, re-exports, remaining tests | ~0 | 30 min |
| 5B.1 | client prep | `pub(crate)` field visibility | ~0 | 15 min |
| 5B.2 | response_parsers.rs | `parse_*` free fns + parser tests | ~550 | 30 min |
| 5B.3 | background.rs | background tasks + pure helpers + tests | ~800 | 45 min |
| 5B.4 | document.rs | `DocumentGuard` + did_open/close + tests | ~350 | 30 min |
| 5B.5 | lawyer_impl.rs | `impl Lawyer for LspClient` + tests | ~500 | 30 min |
| 5B.6 | lifecycle.rs | constructor, spawn, request routing + tests | ~600 | 30 min |
| 5B.7 | client mod.rs cleanup | struct defs, re-exports, remaining tests | ~0 | 30 min |

**Execution order and dependencies:**

```
5A.1 (prep) ─── 5A.2 (health) ─── 5A.3 (definition) ─── 5A.4 (deep_context)
                                                                    │
              5A.5 (impact) ─── 5A.6 (references) ─── 5A.7 (overview) ─── 5A.8 (cleanup)

5B.1 (prep) ─── 5B.2 (parsers) ─── 5B.3 (background) ─── 5B.4 (document)
                                                                    │
              5B.5 (lawyer_impl) ─── 5B.6 (lifecycle) ─── 5B.7 (cleanup)
```

5A and 5B are fully independent. Can be interleaved or done in parallel.

**Coverage impact:** Zero new test lines. Coverage percentage unchanged. The
split makes it feasible to:
1. Track coverage per-module (DeepSource reports per-file)
2. Prevent gap accumulation (smaller files, focused reviews)
3. Write targeted tests easily (each file has clear responsibility)

**Rust visibility rules for the split:**

- Methods in sub-module `impl PathfinderServer` / `impl LspClient` blocks can
  access `pub(crate)` methods on `self` from any file in the same crate.
- Private methods (`fn method(&self)`) are only visible within their defining
  module. Use `pub(super)` for helpers shared across sub-modules within
  `navigation/` or `client/`.
- Free functions moved to sub-modules need `pub(super)` if called from sibling
  modules or `pub(crate)` if called from outside the parent module.

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

### Completion status

| Phase | Status | Commit | Notes |
|---|---|---|---|
| Phase 0 (threshold) | DONE | web UI | 88% floor set |
| Phase 1 (Tier 2) | DONE | `9bcabd8` | All 6 files covered |
| Phase 2 (navigation) | ~60% DONE | `e69da6f` | Gaps: symbol_overview (0%), find_all_references degraded/pagination, BFS cycles |
| Phase 3 (LSP infra) | DONE | `53151cd` + `b7c81c6` + `74049dc` | Trait + FakeTransport + 125 tests |
| Phase 4 (residual) | IN PROGRESS | -- | 4 subphases: 4A (symbol_overview), 4B (references), 4C (BFS/grep), 4D (LSP client) |
| Phase 5 (split) | PLANNED | -- | 15 sub-phases: 5A.1-5A.8 (navigation), 5B.1-5B.7 (client) |

### Dependency graph

```
Phase 0 (threshold) ─── DONE
Phase 1 (Tier 2)   ─── DONE
Phase 2 (navigation)── ~60% DONE (residual picked up in Phase 4A-4C)
Phase 3 (infra)     ─── DONE
Phase 4 (residual)  ─── IN PROGRESS (depends on Phase 3 for 4D only)
Phase 5 (split)     ─── PLANNED (depends on Phase 4)
  5A.1-5A.8: navigation.rs split (independent of 5B)
  5B.1-5B.7: client/mod.rs split (independent of 5A)
```

Subphases 4A, 4B, 4C (navigation) are independent of Phase 3 infrastructure.
Subphase 4D (LSP client) uses Phase 3 FakeTransport infrastructure.

### Expected coverage progression

| After Phase | LCV Estimate | Threshold | Actual |
|---|---|---|---|
| Baseline | 88.9% | None | 88.9% |
| Phase 0 | 88.9% | 88% | -- |
| Phase 1 | ~89.5% | 88% | -- |
| Phase 2 | ~90.5% | 90% | -- |
| Phase 3 | ~91.0% | 90% | -- |
| Phase 4A | ~91.0% | 90% | -- |
| Phase 4B | ~91.5% | 90% | -- |
| Phase 4C | ~91.5% | 90% | -- |
| Phase 4D | ~92% | 92% | -- |
| Phase 5 | ~92% | 92% | -- (structural only, no coverage change) |

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

**Pre-Phase 5 (current):**

| Source File | Test Location | Test Count (baseline) | Test Count (current) |
|---|---|---|---|
| `client/mod.rs` | Inline `mod tests` | 61 | 125 |
| `client/detect.rs` | Inline `mod tests` | 58 | 58 |
| `client/process.rs` | Inline `mod tests` | 20 | 20 |
| `client/transport.rs` | Inline `mod tests` | 17 | 17 |
| `client/fake_transport.rs` | Inline (test-only module) | 0 | -- (used by mod.rs tests) |
| `navigation.rs` | Inline `mod tests` | 97 | 103 |
| `read_files.rs` | Inline `mod tests` | 14 | 17 |
| `repo_map.rs` | Inline `mod tests` | 9 | 10 |
| `search.rs` | Inline `mod tests` | 10 | 9+ |
| `types.rs` | Inline `mod tests` | 0 | 2 |
| `git.rs` | Inline `mod tests` + `tests/git_integration.rs` | 6+5 | 8+5 |
| `parser.rs` | Inline `mod tests` | 11 | 11 |

**Post-Phase 5 (target):**

| Source File | Test Location | Est. Lines | Notes |
|---|---|---|---|
| `client/mod.rs` | Inline `mod tests` | ~700 | Struct defs, re-exports, ~30 remaining tests |
| `client/response_parsers.rs` | Inline `mod tests` | ~550 | ~20 parser tests |
| `client/background.rs` | Inline `mod tests` | ~800 | ~20 background task tests |
| `client/document.rs` | Inline `mod tests` | ~350 | ~12 document tests |
| `client/lawyer_impl.rs` | Inline `mod tests` | ~500 | ~8 lawyer trait tests |
| `client/lifecycle.rs` | Inline `mod tests` | ~600 | ~30 lifecycle/state tests |
| `navigation/mod.rs` | Inline `mod tests` | ~500 | Shared helpers, ~20 pattern tests |
| `navigation/health.rs` | Inline `mod tests` | ~1200 | ~25 health tests |
| `navigation/definition.rs` | Inline `mod tests` | ~1100 | ~15 definition tests |
| `navigation/deep_context.rs` | Inline `mod tests` | ~650 | ~6 deep context tests |
| `navigation/impact.rs` | Inline `mod tests` | ~1000 | ~18 impact tests |
| `navigation/references.rs` | Inline `mod tests` | ~550 | ~10 reference tests |
| `navigation/overview.rs` | Inline `mod tests` | ~450 | ~9 overview tests |
