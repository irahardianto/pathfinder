# BATCH-01 Deferred Items: Architectural Gaps for Future Implementation

> These items were identified during BATCH-01 implementation review as valid gaps
> that require non-trivial architectural changes beyond test-only modifications.
> They are documented here with sufficient context for future implementation.

---

## D-1: ProcessSpawner Trait for Spawn Logic Testing

### Problem

`spawn_lsp_child()` (process.rs:137 lines) is a concrete function that calls
`tokio::process::Command` directly. Cannot test spawn argument construction,
PATH lookup, environment setup, or working directory resolution without
actually spawning real processes.

### Current State

- `spawn_lsp_child(binary, args, workspace_root, language_id, isolate_target_dir)`
  builds a `tokio::process::Command`, sets up env vars, configures stdio pipes,
  and spawns a child process.
- Only testable path: `test_spawn_lsp_child_nonexistent_binary` -- tests spawn
  failure when binary doesn't exist.
- Cannot test:
  - Binary path resolution (which::which fallback)
  - PATH lookup ordering
  - Spawn environment setup (RUSTC_WRAPPER, CARGO_TARGET_DIR isolation)
  - Working directory resolution
  - Process group setup
  - Stdio pipe configuration (stdin/stdout/stderr)
  - Linux prctl(PR_SET_PDEATHSIG) hardening

### Proposed Solution

Extract a `ProcessSpawner` trait:

```rust
// crates/pathfinder-lsp/src/client/process.rs

#[async_trait]
pub(crate) trait ProcessSpawner: Send + Sync {
    async fn spawn(
        &self,
        binary: &str,
        args: &[String],
        workspace_root: &Path,
        language_id: &str,
        isolate_target_dir: bool,
    ) -> Result<tokio::process::Child, std::io::Error>;
}

struct RealProcessSpawner;

#[async_trait]
impl ProcessSpawner for RealProcessSpawner {
    async fn spawn(...) -> Result<Child, std::io::Error> {
        // Move current spawn_lsp_child body here
    }
}
```

Then inject via LspClient or make it a parameter of `start_process()`.

### Scope

- Files affected: process.rs, lifecycle.rs (start_process, spawn_and_initialize)
- Risk: Medium -- changes core spawn path, needs integration test validation
- Estimated effort: 2-3 hours

### Coverage Impact

Would cover ~40 currently untestable lines in `spawn_lsp_child()` plus
`spawn_and_initialize()` argument construction paths.

---

## D-2: LspClient::new() Integration Test with Filesystem Fixtures

### Problem

All tests bypass `LspClient::new()` using test helpers:

- `client_no_languages()` -- empty descriptors, no detection
- `client_with_descriptors()` -- pre-built descriptors, no detection
- `make_running_client()` -- pre-built with FakeTransport, no detection

`LspClient::new()` calls `detect_languages()` which does real filesystem I/O
(marker file detection, binary resolution via which::which). Not mockable
without architectural change.

### Current State

```rust
// lifecycle.rs:46-77
pub async fn new(workspace_root: &Path, config: ...) -> io::Result<Self> {
    let detection_result = detect_languages(workspace_root, &config).await?;
    // ... real filesystem I/O happens here
}
```

Cannot test:
- Config validation branches
- Descriptor initialization from detection results
- Timer setup for idle_timeout_task
- Error branch for invalid/missing workspace root
- Full init path with languages (warm_start integration)

### Proposed Solution

Option A: Extract `detect_languages` behind a trait (similar to D-1):

```rust
#[async_trait]
pub(crate) trait LanguageDetector: Send + Sync {
    async fn detect(&self, root: &Path, config: &PathfinderConfig)
        -> Result<DetectionResult, io::Error>;
}
```

Option B: Create tempdir-based integration tests that set up real marker files:

```rust
#[tokio::test]
async fn test_new_with_rust_project() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"");
    // ... but requires rust-analyzer on PATH
}
```

Option B is simpler but limited by binary availability on the test system.

### Scope

- Files affected: lifecycle.rs, detect.rs (if Option A), mod.rs
- Risk: Medium (Option A -- affects public API), Low (Option B -- additive only)
- Estimated effort: Option A: 3-4 hours, Option B: 1-2 hours

### Coverage Impact

Would cover `new()` body (lifecycle.rs:46-77) plus `warm_start()` integration.
Currently this code is only exercised in integration tests.

---

## D-3: Malformed Response Injection for Protocol Error Testing

### Problem

`read_message()` in transport.rs handles Content-Length parsing, header parsing,
and JSON deserialization. FakeTransport only produces valid JSON via
`set_response()` and `set_error()`. Cannot test protocol-level error paths.

### Current State

`read_message()` flow (transport.rs):
1. Read header line by line until blank line
2. Parse `Content-Length` from headers
3. Read exactly N bytes from body
4. Parse body as JSON
5. Return `Ok(Value)` or `Err(LspError::Protocol/ConnectionLost)`

Error paths not tested at unit level:
- Invalid UTF-8 in headers
- Missing Content-Length header
- Non-numeric Content-Length value
- Body shorter than Content-Length (partial read)
- Invalid JSON in body
- Empty body

### Proposed Solution

Option A: Create a `MockStdout` that wraps a `Cursor<Vec<u8>>`:

```rust
struct MockStdout {
    data: Vec<u8>,
    pos: usize,
}

impl MockStdout {
    fn write_lsp_message(content: &str) -> Self {
        let body = content.as_bytes();
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        let mut data = header.into_bytes();
        data.extend_from_slice(body);
        Self { data, pos: 0 }
    }

    fn write_raw(bytes: &[u8]) -> Self {
        Self { data: bytes.to_vec(), pos: 0 }
    }
}

impl AsyncRead for MockStdout { ... }
```

Then call `read_message()` directly with `BufReader<MockStdout>`.

Option B: Add a `set_raw_response` method to FakeTransport that bypasses
JSON parsing in send() and writes raw bytes. This is less clean because
FakeTransport doesn't use the `read_message` path at all.

### Recommended Approach

Option A is correct -- test `read_message()` directly, not through FakeTransport.
The reader task (`start_reader_task`) uses `BufReader<ChildStdout>`. A `MockStdout`
implementing `AsyncRead` lets us inject any byte sequence.

### Scope

- Files affected: transport.rs (tests only), new mock_stdout.rs helper
- Risk: Low -- additive test-only changes
- Estimated effort: 2-3 hours

### Coverage Impact

Would cover transport.rs `read_message()` error branches (~10-15 uncovered lines).
Current transport.rs coverage: 89.49% line coverage.

---

## D-4: MAX_CONSECUTIVE_READER_ERRORS Threshold Testing

### Problem

The reader task in `start_reader_task()` (process.rs:594-661) maintains a
`consecutive_io_errors` counter. After `MAX_CONSECUTIVE_READER_ERRORS = 5`
consecutive non-ConnectionLost IO errors, it calls `cancel_for_language()` and
breaks out of the read loop.

Cannot test through FakeTransport because FakeTransport doesn't use the reader
task at all -- it dispatches responses synchronously in `send()`.

### Current State

```rust
// process.rs:633-658
Err(e) => {
    consecutive_io_errors += 1;
    if consecutive_io_errors >= MAX_CONSECUTIVE_READER_ERRORS {
        dispatcher.cancel_for_language(&language_id);
        break;
    }
}
```

Also untested:
- `malformed_message_count` increment on Protocol errors
- Logging categories ("io_error" vs "malformed_message")
- `consecutive_io_errors` reset to 0 on successful read

### Proposed Solution

Extract the reader loop body into a testable function:

```rust
fn handle_reader_result(
    result: Result<Value, LspError>,
    consecutive_io_errors: &mut u32,
    malformed_count: &mut u32,
) -> ReaderAction {
    match result {
        Ok(_) => {
            *consecutive_io_errors = 0;
            ReaderAction::Continue
        }
        Err(LspError::ConnectionLost) => ReaderAction::CancelAndBreak,
        Err(LspError::Protocol(_)) => {
            *malformed_count += 1;
            ReaderAction::Continue
        }
        Err(_) => {
            *consecutive_io_errors += 1;
            if *consecutive_io_errors >= MAX_CONSECUTIVE_READER_ERRORS {
                ReaderAction::CancelAndBreak
            } else {
                ReaderAction::Continue
            }
        }
    }
}

enum ReaderAction {
    Continue,
    CancelAndBreak,
}
```

Then `start_reader_task` calls `handle_reader_result()` in the loop. Tests can
call it directly with crafted error values.

Combined with D-3's MockStdout, could also test the full loop by feeding
a sequence of errors via AsyncRead.

### Scope

- Files affected: process.rs
- Risk: Low -- pure refactor of loop body into a function
- Estimated effort: 1-2 hours

### Coverage Impact

Would cover the `consecutive_io_errors` threshold branch (~8 uncovered lines)
plus malformed message counting (~3 uncovered lines).

---

## D-5: Delayed Error Response via FakeTransport

### Status Note

Fully addressed by the `response_delay` feature added to FakeTransport
during BATCH-01 fix cycle. However, delayed error responses are NOT supported
-- when `response_delay` is active and the response has an error, the error
is dispatched immediately via the dispatcher AND returned directly from `send()`,
completely bypassing the delay mechanism.

The current behavior is intentional and creates an asymmetry:

- **Success + delay**: `send()` returns `Ok(())`, response arrives via oneshot after delay
- **Error + delay**: `send()` returns `Err(LspError::Protocol)` immediately, delay ignored
  - Error is also dispatched through the dispatcher immediately (no delay)
  - The delayed spawn task is never used for errors

This asymmetry is acceptable for current timeout tests because we only need to
validate timeout behavior for successful responses, not error paths. Error responses
arrive synchronously via the `send()` return, so timeout behavior cannot be tested for errors.

If asynchronous error arrival testing is needed in the future, a new
`set_delayed_error()` method could be added that returns `Ok(())` from `send()`
and dispatches the error via the delayed task instead of returning `Err` immediately.

### Scope

- Files affected: fake_transport.rs
- Risk: Low -- test-only infrastructure
- Estimated effort: 0.5-1 hour

---

## Summary

| ID | Item | Risk | Effort | Coverage Gain |
|----|------|------|--------|---------------|
| D-1 | ProcessSpawner trait | Medium | 2-3h | ~40 lines |
| D-2 | LspClient::new() fixtures | Medium/Low | 1-4h | ~30 lines |
| D-3 | Malformed response (MockStdout) | Low | 2-3h | ~15 lines |
| D-4 | Reader error threshold extraction | Low | 1-2h | ~11 lines |
| D-5 | Delayed error response refinement | Low | 0.5-1h | edge cases |

### Recommended Priority

1. D-4 (highest ROI -- simple refactor, meaningful gap)
2. D-3 (clear scope, isolated to transport.rs)
3. D-2 Option B (tempdir fixtures -- low risk)
4. D-1 (most invasive, defer unless spawn bugs surface)
5. D-5 (refinement only, not critical)

### Dependencies

- D-3 and D-4 are independent of each other
- D-4 can be combined with D-3 (use MockStdout to feed errors to reader loop)
- D-1 is independent but touches the same files as D-2
- D-5 is independent
