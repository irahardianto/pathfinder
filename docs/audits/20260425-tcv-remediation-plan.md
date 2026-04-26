# TCV-001 Test Coverage Remediation Plan

**Date:** 2026-04-25
**Severity:** Critical
**Current Coverage:** 83.78% (line), 82.49% (function), 82.92% (region)
**Target Coverage:** 90%+ line, 95%+ function
**Total Uncovered Lines:** ~1,587 across 37 files

---

## 1. Root Cause Analysis

Coverage gaps fall into **7 distinct categories**. Each requires a different testing strategy.

### Category A: LspClient Production Implementation — No Integration Tests (430 lines)
**Files:** `pathfinder-lsp/src/client/mod.rs` (33.3% coverage)
**Root Cause:** `LspClient` spawns real child processes, manages JSON-RPC transport, and performs the `initialize` handshake with real LSP servers. Tests only cover pure parsing helpers (`parse_definition_response`, etc.) — all async methods in `impl Lawyer for LspClient` are untested because no test harness exists to mock at the process/transport boundary.

### Category B: LSP Process Lifecycle — Unsafe + OS-Specific Code (113 lines)
**Files:** `pathfinder-lsp/src/client/process.rs` (41.8% coverage)
**Root Cause:** `spawn_and_initialize`, `send`, `shutdown`, and `apply_linux_process_hardening` require real OS process spawning. The `pre_exec`/`prctl` code is `#[cfg(target_os = "linux")]` + `#[allow(unsafe_code)]`. No test exercises process creation, crash recovery, or the idle timeout supervisor.

### Category C: Batch Edit Operations — No End-to-End Tests (171 lines)
**Files:** `pathfinder/src/server/tools/edit/batch.rs` (40.0% coverage, **worst in project**)
**Root Cause:** `replace_batch_impl` and its helpers (`validate_batch_occ`, `apply_batch_edits`, `apply_single_batch_edit`) are only tested indirectly. The existing `handler_tests.rs` covers individual edit operations (replace_body, delete, insert) but never exercises the batch path which applies multiple edits atomically.

### Category D: Tool Registration Boilerplate — `#[tool_handler]` Macro Code (50 lines)
**Files:** `pathfinder/src/server.rs` lines 64-97
**Root Cause:** The `#[tool_handler]` and `#[tool_router]` proc macros generate dispatch code that calls the `async fn tool_name()` wrapper methods. These wrappers are thin delegation calls (`self.xxx_impl(params).await`). Coverage tools mark them as uncovered because unit tests call `_impl` methods directly via `with_engines()` — they bypass the macro-generated dispatch layer.

### Category E: Server Constructor — Requires Real LspClient (30 lines)
**Files:** `pathfinder/src/server.rs` lines 188-233
**Root Cause:** `PathfinderServer::new()` calls `LspClient::new()` which performs real filesystem I/O for language detection. Tests use `with_engines()` or `with_all_engines()` which inject mocks. The constructor path (LspClient creation, warm_start, fallback to NoOpLawyer) is never exercised.

### Category F: Navigation/Deep-Context Tools — LSP-Dependent Paths (94 lines)
**Files:** `pathfinder/src/server/tools/navigation.rs` (86.7% coverage)
**Root Cause:** `analyze_impact_impl`, `read_with_deep_context_impl`, and `get_definition_impl` have code paths that interact with the `Lawyer` trait for LSP features. Tests cover the "degraded mode" (NoOpLawyer) paths but miss the LSP-available branches and error-recovery paths.

### Category G: Tree-Sitter Surgeon — Edge Cases in Multi-Language Parsing (73 lines)
**Files:** `pathfinder-treesitter/src/treesitter_surgeon.rs` (80.5% coverage)
**Root Cause:** Error branches in `extract_symbol_body`, `replace_symbol_body`, `insert_before`, `insert_after`, and language-specific parsing paths (Vue zones, Go methods) are not exercised. Tests focus on Rust and Python but skip Go, TypeScript, and Vue-specific edge cases.

---

## 2. Inventory of Uncovered Files

Sorted by impact (uncovered lines × criticality):

| # | File | Uncovered | Total | Coverage | Category | Priority |
|---|------|-----------|-------|----------|----------|----------|
| 1 | `pathfinder-lsp/src/client/mod.rs` | 430 | 645 | 33.3% | A | P0 |
| 2 | `pathfinder/src/server/tools/edit/batch.rs` | 171 | 285 | 40.0% | C | P0 |
| 3 | `pathfinder-lsp/src/client/process.rs` | 113 | 194 | 41.8% | B | P1 |
| 4 | `pathfinder/src/server/tools/navigation.rs` | 94 | 801 | 86.7% | F | P1 |
| 5 | `pathfinder/src/server/tools/edit/text_edit.rs` | 83 | 326 | 74.5% | C | P1 |
| 6 | `pathfinder-treesitter/src/treesitter_surgeon.rs` | 73 | 380 | 80.5% | G | P1 |
| 7 | `pathfinder-treesitter/src/symbols.rs` | 67 | 1392 | 95.2% | G | P2 |
| 8 | `pathfinder/src/server.rs` | 50 | 1128 | 95.6% | D,E | P2 |
| 9 | `pathfinder/src/server/tools/edit/handlers.rs` | 45 | 230 | 80.4% | C | P2 |
| 10 | `pathfinder-treesitter/src/repo_map.rs` | 36 | 473 | 92.4% | G | P2 |
| 11 | `pathfinder/src/server/tools/edit/validation.rs` | 36 | 240 | 85.0% | C | P2 |
| 12 | `pathfinder-common/src/file_watcher.rs` | 31 | 121 | 74.4% | misc | P2 |
| 13 | `pathfinder-lsp/src/client/transport.rs` | 30 | 158 | 81.0% | B | P2 |
| 14 | `pathfinder/src/main.rs` | 29 | 35 | 17.1% | D | P3 |
| 15 | `pathfinder/src/server/tools/source_file.rs` | 29 | 132 | 78.0% | F | P2 |
| 16 | `pathfinder-search/src/ripgrep.rs` | 27 | 581 | 95.4% | misc | P3 |
| 17 | `pathfinder/src/server/tools/file_ops.rs` | 27 | 227 | 88.1% | misc | P3 |
| 18 | `pathfinder-lsp/src/client/detect.rs` | 20 | 307 | 93.5% | A | P3 |
| 19 | `pathfinder/src/server/helpers.rs` | 20 | 192 | 89.6% | misc | P3 |
| 20 | `pathfinder-lsp/src/client/protocol.rs` | 19 | 109 | 82.6% | B | P3 |
| 21 | `pathfinder-treesitter/src/vue_zones.rs` | 18 | 225 | 92.0% | G | P3 |
| 22 | `pathfinder-treesitter/src/cache.rs` | 17 | 174 | 90.2% | misc | P3 |
| 23 | `pathfinder-common/src/types.rs` | 16 | 205 | 92.2% | misc | P3 |
| 24 | `pathfinder-common/src/git.rs` | 15 | 137 | 89.1% | misc | P3 |
| 25 | `pathfinder/src/server/tools/repo_map.rs` | 13 | 82 | 82.2% | misc | P3 |
| 26 | `pathfinder-treesitter/src/language.rs` | 12 | 151 | 92.1% | G | P3 |
| 27 | `pathfinder-lsp/src/no_op.rs` | 11 | 126 | 91.3% | misc | P3 |
| 28 | `pathfinder-common/src/config.rs` | 10 | 133 | 92.5% | misc | P3 |
| 29 | `pathfinder-treesitter/src/error.rs` | 9 | 22 | 59.1% | misc | P3 |
| 30 | `pathfinder-common/src/sandbox.rs` | 8 | 294 | 97.3% | misc | P3 |
| 31 | `pathfinder-lsp/src/mock.rs` | 6 | 313 | 98.1% | misc | P3 |
| 32 | `pathfinder-treesitter/src/parser.rs` | 6 | 53 | 88.7% | misc | P3 |
| 33 | `pathfinder-common/src/error.rs` | 5 | 403 | 98.8% | misc | P3 |
| 34 | `pathfinder-treesitter/src/mock.rs` | 4 | 37 | 89.2% | misc | P3 |
| 35 | `pathfinder/src/server/tools/search.rs` | 4 | 185 | 97.8% | misc | P3 |
| 36 | `pathfinder-common/src/normalize.rs` | 2 | 114 | 98.2% | misc | P3 |
| 37 | `pathfinder-lsp/src/client/capabilities.rs` | 1 | 76 | 98.7% | misc | P3 |

**By Crate:**
| Crate | Uncovered Lines | Files |
|-------|----------------|-------|
| `pathfinder-lsp` | 630 | 8 |
| `pathfinder` (server + main) | 601 | 11 |
| `pathfinder-treesitter` | 242 | 9 |
| `pathfinder-common` | 87 | 7 |
| `pathfinder-search` | 27 | 1 |

---

## 3. Remediation Plan — Work Packages

### WP-1: LspClient Test Harness (Category A+B) — P0/P1
**Impact:** 573 uncovered lines (36% of all gaps)
**Estimated effort:** 3-4 days
**Assign to:** Senior engineer with async/Tokio experience

#### Step 1.1: Create `MockTransport` abstraction

Create a new file `crates/pathfinder-lsp/src/client/test_transport.rs`:

```rust
//! Test-only transport that simulates LSP JSON-RPC communication
//! without spawning real processes.
//!
//! Usage:
//!   let transport = MockTransport::new();
//!   transport.enqueue_response("textDocument/definition", expected_response);
//!   let client = LspClient::from_transport(transport);
```

The `MockTransport` must:
- Accept a `Vec<(method_name, response_json)>` queue
- Implement the same read/write interface as the real transport (`transport.rs`)
- Support configurable delays (for timeout testing)
- Support simulating crashes (dropping the reader side)

#### Step 1.2: Add `LspClient::from_parts()` test constructor

Add to `crates/pathfinder-lsp/src/client/mod.rs`:

```rust
#[cfg(test)]
impl LspClient {
    /// Create an LspClient with pre-populated process state for testing.
    /// Bypasses real process spawning entirely.
    pub fn from_test_parts(
        language_id: &str,
        process: ManagedProcess,
        reader_handle: JoinHandle<()>,
    ) -> Self {
        // ... construct with processes HashMap pre-filled
    }
}
```

#### Step 1.3: Test all `Lawyer for LspClient` methods

Create `crates/pathfinder-lsp/src/client/lawyer_tests.rs`:

Test each method in the `impl Lawyer for LspClient` block:
1. `goto_definition` — success path (mock response with Location), empty response, error response
2. `call_hierarchy_prepare` — success, unsupported capability, empty results
3. `call_hierarchy_incoming` — success with parsed items, empty
4. `call_hierarchy_outgoing` — success with parsed items, empty
5. `did_open` — success, NoLspAvailable (bad extension), notify failure
6. `did_change` — success, NoLspAvailable
7. `did_close` — success, NoLspAvailable
8. `pull_diagnostics` — success with diagnostics, empty, timeout
9. `pull_workspace_diagnostics` — success, empty
10. `range_formatting` — success with edits, no edits available
11. `capability_status` — returns per-language status
12. `did_change_watched_files` — success

For each test:
```rust
#[tokio::test]
async fn test_lsp_client_goto_definition_success() {
    let transport = MockTransport::new();
    transport.enqueue_response(
        "textDocument/definition",
        json!({
            "uri": "file:///workspace/src/main.rs",
            "range": { "start": { "line": 9, "character": 0 }, "end": { "line": 9, "character": 5 } }
        }),
    );
    let client = LspClient::from_test_parts("rust", transport);
    let result = client.goto_definition(Path::new("/workspace"), Path::new("src/main.rs"), 10, 1).await;
    assert!(result.is_ok());
}
```

#### Step 1.4: Test process lifecycle

Create `crates/pathfinder-lsp/src/client/process_tests.rs`:

1. `test_spawn_and_initialize_success` — spawn a simple echo process, verify handshake
2. `test_spawn_and_initialize_timeout` — verify timeout error on hung process
3. `test_shutdown_kills_process` — verify process is terminated
4. `test_crash_recovery_exponential_backoff` — simulate crash, verify retry timing
5. `test_max_restart_marks_unavailable` — 3 failures → `ProcessEntry::Unavailable`
6. `test_recovery_cooldown_elapsed` — unavailable state with old timestamp → retry

For `apply_linux_process_hardening`: this is `#[cfg(target_os = "linux")]` unsafe code.
Mark with `#[cfg(test)]` attribute and add a simple compilation/smoke test:

```rust
#[cfg(target_os = "linux")]
#[test]
fn test_linux_process_hardening_does_not_panic() {
    let mut cmd = tokio::process::Command::new("echo");
    apply_linux_process_hardening(&mut cmd);
    // If we got here, the prctl call didn't crash
}
```

---

### WP-2: Batch Edit End-to-End Tests (Category C) — P0
**Impact:** 171 uncovered lines in batch.rs + 83 in text_edit.rs + 45 in handlers.rs = 299 lines
**Estimated effort:** 2-3 days
**Assign to:** Engineer familiar with the edit system

#### Step 2.1: Create batch edit test file

Create `crates/pathfinder/src/server/tools/edit/tests/batch_tests.rs` and register in `mod.rs`.

#### Step 2.2: Test matrix for `replace_batch_impl`

Each test creates a temp workspace with source files, calls `replace_batch` via `PathfinderServer`, and verifies the result.

| Test Name | Setup | Batch Contents | Expected |
|-----------|-------|---------------|----------|
| `test_batch_replace_body_single` | 1 file, 2 functions | 1 replace_body | Body updated, hash correct |
| `test_batch_replace_body_multi` | 1 file, 3 functions | 2 replace_body edits | Both bodies updated atomically |
| `test_batch_mixed_edit_types` | 1 file | 1 replace_body + 1 insert_after | Both applied |
| `test_batch_text_targeting` | Vue file with template | 1 text edit (old_text) | Template modified |
| `test_batch_mixed_semantic_and_text` | Vue file | 1 semantic + 1 text | Both applied atomically |
| `test_batch_occ_version_mismatch` | File on disk | Wrong base_version | VERSION_MISMATCH, no changes |
| `test_batch_partial_failure_rollback` | 1 file | 2 edits, 2nd has bad semantic_path | No changes on disk |
| `test_batch_empty_edits` | 1 file | Empty vec | Error or no-op |
| `test_batch_file_not_found` | Nonexistent path | 1 edit | FILE_NOT_FOUND |
| `test_batch_sandbox_denied` | `.git/` path | 1 edit | ACCESS_DENIED |
| `test_batch_normalize_whitespace` | HTML content | text edit with normalize=true | Matched despite spacing |
| `test_batch_large_multi_edit` | 10-function file | 5 edits | All applied in correct order |
| `test_batch_text_not_found` | File exists | old_text not in file | TEXT_NOT_FOUND, rollback |

#### Step 2.3: Cover uncovered `text_edit.rs` paths

Add tests for these uncovered regions in `text_edit.rs`:
- L111, L117-120: `resolve_text_edit` — no-match branch with context window
- L153-154, L157: Whitespace normalization edge cases
- L184-189: `context_line` clamping at file boundaries
- L341-439: `apply_text_edit` — all edit types (replace, insert_before, insert_after, delete) with text targeting

#### Step 2.4: Cover uncovered `handlers.rs` paths

- L281-301: `insert_after_impl` / `insert_before_impl` error paths
- L513-663: `replace_body_impl` / `replace_full_impl` with real TreeSitter surgeon (not mock)

Template for each test:
```rust
#[tokio::test]
async fn test_batch_replace_body_multi_atomic() {
    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    let surgeon = Arc::new(TreeSitterSurgeon::new(100));
    let server = PathfinderServer::with_all_engines(
        ws, config, sandbox,
        Arc::new(MockScout::default()),
        surgeon,
        Arc::new(MockLawyer::default()),
    );

    // Write a Rust file with 3 functions
    let source = r#"
fn foo() -> i32 { 1 }
fn bar() -> i32 { 2 }
fn baz() -> i32 { 3 }
"#;
    let path = "src/lib.rs";
    fs::create_dir_all(ws_dir.path().join("src")).expect("dir");
    fs::write(ws_dir.path().join(path), source).expect("write");
    let hash = VersionHash::compute(source.as_bytes());

    let result = server.replace_batch(Parameters(ReplaceBatchParams {
        filepath: path.to_owned(),
        base_version: hash.as_str().to_owned(),
        edits: vec![
            BatchEdit { semantic_path: Some("src/lib.rs::foo".into()), edit_type: Some(EditType::ReplaceBody), new_code: Some("42".into()), ..Default::default() },
            BatchEdit { semantic_path: Some("src/lib.rs::baz".into()), edit_type: Some(EditType::ReplaceBody), new_code: Some("99".into()), ..Default::default() },
        ],
    })).await;

    assert!(result.is_ok());
    let updated = fs::read_to_string(ws_dir.path().join(path)).expect("read");
    assert!(updated.contains("42"));
    assert!(updated.contains("99"));
    // bar should be unchanged
    assert!(updated.contains("2"));
}
```

---

### WP-3: Navigation Tool LSP Paths (Category F) — P1
**Impact:** 94 uncovered lines + 29 in source_file.rs = 123 lines
**Estimated effort:** 1-2 days

#### Step 3.1: MockLawyer-based navigation tests

Create tests in `crates/pathfinder/src/server/tools/navigation.rs` `mod tests` that use `MockLawyer` (via `with_all_engines`) instead of `NoOpLawyer`:

1. **`get_definition_impl` with LSP available** — MockLawyer returns a `DefinitionLocation`, verify response contains file/line/column
2. **`get_definition_impl` with LSP error** — MockLawyer returns `LspError::Timeout`, verify graceful degradation
3. **`analyze_impact_impl` with call hierarchy** — MockLawyer returns prepare + incoming/outgoing items, verify formatted response
4. **`analyze_impact_impl` — LSP unavailable** — MockLawyer returns `NoLspAvailable`, verify `degraded: true`
5. **`read_with_deep_context_impl` with LSP** — MockLawyer returns definition + call hierarchy, verify dependencies in response
6. **`read_with_deep_context_impl` — no LSP** — Verify `degraded: true, degraded_reason: "no_lsp"`

These must use `PathfinderServer::with_all_engines()` with a configured `MockLawyer`.

#### Step 3.2: Source file tool tests

Cover `source_file.rs` uncovered paths:
- L33-75: `read_source_file_impl` — unsupported language, missing file, sandbox denied
- L111-166: AST symbol extraction edge cases (empty file, single-line file, deeply nested symbols)

---

### WP-4: Tree-Sitter Surgeon Edge Cases (Category G) — P1/P2
**Impact:** 73 + 67 + 36 + 18 + 17 = 211 lines
**Estimated effort:** 2-3 days

#### Step 4.1: Multi-language surgeon tests

Add tests to `crates/pathfinder-treesitter/src/treesitter_surgeon.rs` `mod tests`:

1. **Go method extraction** — `func (s *Server) Handle() { ... }` — extract and replace body
2. **Go function extraction** — `func main() { ... }` — replace full
3. **TypeScript class method** — `class Foo { bar() { ... } }` — replace body
4. **TypeScript arrow function** — `const fn = () => { ... }` — extract body
5. **Vue SFC** — `<script setup>` block symbol extraction
6. **Vue template zone** — Extract template content
7. **Python decorator + function** — `@decorator\ndef fn():` — replace full preserves decorator
8. **Insert before/after in Go** — Insert function between two existing Go functions
9. **Insert before/after in TypeScript** — Insert method between two existing methods
10. **Error: symbol not found** — Non-existent semantic path → `SymbolNotFound`
11. **Error: bare file** — File with no parseable AST → `BareFileRejected`
12. **Edge: empty function body** — `fn foo() {}` — replace body with content
13. **Edge: nested impl blocks** — Rust `impl Foo { fn bar() { impl Baz {} } }`

#### Step 4.2: Symbols parser edge cases

Add to `crates/pathfinder-treesitter/src/symbols.rs` `mod tests`:
- Lines 68-69, 111: Language-specific parsing branches (Go, Python, TypeScript)
- Lines 218-251: Multiline function signatures
- Lines 440-647: Nested class/impl symbol extraction
- Lines 711-788: Comment and decorator handling
- Lines 943-1147: Language-specific identifier patterns

#### Step 4.3: Repo map edge cases

Add to `crates/pathfinder-treesitter/src/repo_map.rs` `mod tests`:
- Lines 199-210: Token budget exhaustion and truncation
- Lines 340-396: File-level filtering, extension filtering

#### Step 4.4: Cache, parser, and vue_zones edge cases

- `cache.rs` L104-237: Cache eviction, concurrent access
- `parser.rs` L31-44: Unsupported language fallback
- `vue_zones.rs` L94-265: Template/style/script zone boundary parsing

---

### WP-5: Server Constructor & Tool Registration (Category D+E) — P2
**Impact:** 50 lines in server.rs + 29 in main.rs = 79 lines
**Estimated effort:** 1 day

#### Step 5.1: Integration test for `PathfinderServer::new()`

Create `crates/pathfinder/tests/test_server_new.rs`:

```rust
#[tokio::test]
async fn test_server_new_with_empty_workspace() {
    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    // Empty workspace → NoOpLawyer fallback
    let server = PathfinderServer::new(ws, PathfinderConfig::default()).await;
    // Verify server functions in degraded mode
    let result = server.get_repo_map(/* ... */).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_server_new_with_rust_workspace() {
    let ws_dir = tempdir().expect("temp dir");
    // Write Cargo.toml + src/lib.rs
    // Verify LspClient initializes for rust-analyzer
    // This test is #[ignore] by default (requires rust-analyzer installed)
}
```

#### Step 5.2: Tool dispatch integration test

Test the macro-generated dispatch path through `ServerHandler`:

```rust
#[tokio::test]
async fn test_tool_dispatch_search_codebase() {
    // Use the tool_router directly to invoke search_codebase
    // This exercises the #[tool_handler] generated code
}
```

#### Step 5.3: main.rs coverage

For `main.rs` (17.1% coverage) — this is the binary entry point. Use one of:
- **Option A:** Extract the body of `main()` into a `run()` function in `lib.rs` that can be tested
- **Option B:** Accept as uncovered (entry point boilerplate) and add `#[allow(dead_code)]` + `#![allow(unused)]` pragmas with a coverage exclusion comment

Recommended: Option A. Refactor:
```rust
// main.rs
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    pathfinder::run().await
}

// lib.rs
pub async fn run() -> anyhow::Result<()> {
    // ... current main.rs body ...
}
```

---

### WP-6: Remaining Smaller Gaps (misc) — P3
**Impact:** ~400 lines across 20+ files
**Estimated effort:** 3-4 days
**Assign to:** Any engineer, can be parallelized

These are straightforward unit test additions. Each file needs 1-5 targeted tests.

#### 6.1: `pathfinder-common/src/file_watcher.rs` (31 lines)
- L37-80: File watcher lifecycle (start, receive events, stop)
- L139: Recursive directory watch
- L221-223: File change debouncing

#### 6.2: `pathfinder-common/src/git.rs` (15 lines)
- L28-47: `get_changed_files` — mock git command output
- L72-102: Various git operations with mocked output
- L210-211: Git unavailable fallback

#### 6.3: `pathfinder-search/src/ripgrep.rs` (27 lines)
- L30-46: Ripgrep binary detection / path resolution
- L154-247: Search result parsing edge cases
- L335-467: File type filtering, glob expansion
- L555, L742, L752: Error handling paths

#### 6.4: `pathfinder/src/server/helpers.rs` (20 lines)
- L20-26: `io_error_data` / `pathfinder_to_error_data` formatting
- L139-154: `check_occ` version mismatch error construction
- L258, L322: Helper function edge cases

#### 6.5: `pathfinder/src/server/tools/file_ops.rs` (27 lines)
- L78-93: `write_file_impl` — create missing directories, append mode
- L251-294: `create_file_impl` — various error paths
- L322-574: `delete_file_impl` / `read_file_impl` edge cases

#### 6.6: `pathfinder-lsp/src/client/transport.rs` (30 lines)
- L38-126: Frame reading/writing (Content-Length header parsing)
- L180-203: Connection lifecycle (close, error propagation)

#### 6.7: `pathfinder-lsp/src/client/protocol.rs` (19 lines)
- L46-105: Request ID generation, JSON-RPC message construction
- L165: Response parsing edge cases

#### 6.8: `pathfinder-lsp/src/client/detect.rs` (20 lines)
- L59-400: Language detection from file extensions, config-based overrides
- Individual language mapping functions

#### 6.9: `pathfinder-lsp/src/no_op.rs` (11 lines)
- L82-223: Individual NoOpLawyer method bodies — already have partial test coverage, need to cover remaining methods (`did_close`, `capability_status`, `range_formatting`)

#### 6.10: `pathfinder-common/src/types.rs` (16 lines)
- L48-450: Edge cases in `VersionHash`, `SemanticPath`, `WorkspaceRoot` constructors

#### 6.11: `pathfinder-common/src/config.rs` (10 lines)
- L74-85: Config validation / default construction
- L210-308: Config parsing from TOML

#### 6.12: `pathfinder-treesitter/src/error.rs` (9 lines)
- L46-58: Error Display impl branches

#### 6.13: Remaining files (< 8 lines each)
Quick one-liner tests for:
- `pathfinder-common/src/sandbox.rs` (8 lines)
- `pathfinder-lsp/src/mock.rs` (6 lines)
- `pathfinder-treesitter/src/mock.rs` (4 lines)
- `pathfinder-treesitter/src/parser.rs` (6 lines)
- `pathfinder-common/src/error.rs` (5 lines)
- `pathfinder/src/server/tools/search.rs` (4 lines)
- `pathfinder-common/src/normalize.rs` (2 lines)
- `pathfinder-lsp/src/client/capabilities.rs` (1 line)

---

## 4. Execution Order

```
Week 1-2: WP-1 (LspClient harness) — blocks WP-3 and WP-5
Week 1-2: WP-2 (Batch edit tests) — independent, can run in parallel
Week 2-3: WP-3 (Navigation tests) — depends on WP-1 MockLawyer pattern
Week 2-3: WP-4 (TreeSitter edge cases) — independent
Week 3:   WP-5 (Server constructor) — depends on WP-1 for MockLawyer pattern
Week 3-4: WP-6 (Remaining gaps) — can start anytime, parallelizable
```

**Parallelization:** WP-1 and WP-2 can run simultaneously. WP-4 is fully independent. WP-6 can be split across 2-3 engineers.

---

## 5. Testing Conventions

All new tests MUST follow these rules:

### 5.1 File Placement
- Unit tests: `#[cfg(test)] mod tests { }` at bottom of the source file
- Integration tests: `crates/<crate>/tests/test_<name>.rs`
- Test helpers shared across files: `crates/<crate>/src/test_helpers.rs` with `#[cfg(test)] pub mod test_helpers`

### 5.2 Naming Convention
```
test_<unit>_<scenario>_<expected_outcome>

Examples:
test_goto_definition_with_valid_response_returns_location
test_replace_batch_with_version_mismatch_returns_error
test_extract_symbol_body_for_go_method_returns_body
```

### 5.3 Test Structure (AAA Pattern)
```rust
#[tokio::test]
async fn test_<name>() {
    // ── Arrange ──
    let ws_dir = tempdir().expect("temp dir");
    let server = /* ... */;

    // ── Act ──
    let result = server.tool_name(/* ... */).await;

    // ── Assert ──
    assert!(result.is_ok(), "Expected success, got {:?}", result.err());
}
```

### 5.4 Mock Usage
- Use `MockLawyer` for LSP-dependent server tests
- Use `MockScout` for search-dependent server tests
- Use `MockSurgeon` for surgeon-dependent server tests
- For real TreeSitter tests, use `TreeSitterSurgeon::new(100)` (not mock)

### 5.5 Test Isolation
- Each test creates its own `tempdir()`
- No shared mutable state between tests
- No tests that depend on external tools (rust-analyzer, etc.) without `#[ignore]`

### 5.6 Coverage Verification Command
After completing a work package, run:
```bash
cargo llvm-cov --workspace --summary-only 2>&1 | grep "<file_name>"
```

Target per file after remediation:
- P0/P1 files: 90%+ line coverage
- P2 files: 85%+ line coverage
- P3 files: 80%+ line coverage

---

## 6. Acceptance Criteria

- [ ] Overall line coverage >= 90% (currently 83.78%)
- [ ] Overall function coverage >= 95% (currently 82.49%)
- [ ] No file below 75% line coverage (currently 5 files below this)
- [ ] `batch.rs` coverage >= 85% (currently 40.0%)
- [ ] `client/mod.rs` coverage >= 80% (currently 33.3%)
- [ ] `client/process.rs` coverage >= 75% (currently 41.8%)
- [ ] All new tests pass: `cargo test --workspace`
- [ ] No regressions: `cargo clippy --workspace -- -D warnings`
- [ ] Coverage report generated and compared: `cargo llvm-cov --workspace --summary-only`

---

## 7. Files to Exclude from Coverage

These should have `#[cfg(test)]` coverage exclusions or be accepted as permanently uncovered:

| File | Lines | Reason |
|------|-------|--------|
| `pathfinder/src/main.rs` | 29 | Binary entry point, refactor to `lib.rs::run()` for testability |
| `pathfinder-lsp/src/client/process.rs` L92 | 1 | `#[cfg(target_os = "linux")]` unsafe `prctl` — OS-specific, requires root or specific kernel |
| `pathfinder-lsp/src/client/mod.rs` L1163-1366 | ~200 | `idle_timeout_task` and `reader_supervisor_task` — background async tasks requiring real process lifecycle |

For permanently uncovered code, add inline comments:
```rust
// COVERAGE-EXCLUDED: Runs in background task, tested via integration tests
```

---

## 8. Regression Prevention

### 8.1 CI Gate
Add to CI pipeline:
```yaml
- name: Coverage Check
  run: |
    cargo llvm-cov --workspace --summary-only 2>&1 | tee coverage.txt
    # Parse and fail if overall coverage drops below 90%
    python3 scripts/check_coverage.py coverage.txt 90
```

### 8.2 PR Checklist
Add to PR template:
```
- [ ] New code has test coverage (run `cargo llvm-cov --workspace --summary-only`)
- [ ] Coverage has not decreased for modified files
```

### 8.3 Pre-commit Hook (optional)
```bash
#!/bin/bash
# Verify modified files have test coverage
git diff --name-only --cached | grep -E '\.rs$' | while read f; do
  if [[ ! "$f" =~ test ]]; then
    test_file="${f%.rs}_tests.rs"
    if [[ ! -f "$test_file" ]]; then
      echo "WARNING: $f modified but no corresponding test file found"
    fi
  fi
done
```
