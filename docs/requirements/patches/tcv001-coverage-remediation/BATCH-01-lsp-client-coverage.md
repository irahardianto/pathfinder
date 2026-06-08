# BATCH-01: pathfinder-lsp Client Coverage

Scope: `crates/pathfinder-lsp/src/client/`
Est. Uncovered Lines: ~134 (largest offender group)
Complexity: HIGH
Priority: 1 (start here -- biggest impact)

---

## Files in Scope

| File | Lines | Uncovered Lines | Type |
|---|---|---|---|
| `client/lifecycle.rs` | 2003 | ~52 | LSP client lifecycle management |
| `client/detect.rs` | 1963 | ~41 | Language server detection and configuration |
| `client/process.rs` | 1168 | ~40 | Process spawning, supervision, I/O |
| `client/capabilities.rs` | 635 | ~1 | Capability negotiation |
| `plugin.rs` | 634 | ~2 | Plugin state management |

---

## Uncovered Line Ranges -- lifecycle.rs (~52 lines)

### Block L1: Constructor and init (lines 46-77)
```
46-50   LspClient::new() config validation branch
52-55   LspClient::new() descriptor initialization
59-60   LspClient::new() timer setup
62-72   LspClient::new() full init path with languages
74-77   LspClient::new() error branch for invalid config
```
Why uncovered: Tests use `client_no_languages()` / `client_with_descriptors()` helper constructors that bypass `new()`.
Strategy: Create tests that call `LspClient::new()` directly with various config combinations. Mock the transport layer.

### Block L2: Request/notify dispatch (lines 79, 129, 146, 190-196, 203, 210-212, 226-227)
```
79      request() method entry
129     notify() method entry
146     ensure_process() call in request path
190-191 timeout branch in request
193     timeout error formatting
195-196 retry after timeout
203     capability check branch
210-212 response validation
226-227 error response handling
```
Why uncovered: Requires live JSON-RPC I/O with a connected process.
Strategy: Use `FakeLspTransport` (already in codebase) to simulate responses. Extend it to cover timeout and error scenarios.

### Block L3: Document management (lines 291-292, 348-366, 421-428, 468, 493-503, 511-521)
```
291-292 did_open notification dispatch
348-356 open_document retry logic
362     open_document timeout
364-366 open_document error path
421-422 did_change notification
425-426 change versioning
428     change error handling
468     did_close notification
493-494 document close cleanup
497-503 close error handling
511-521 close and re-open sequence
```
Why uncovered: Document lifecycle needs a running LSP process to receive notifications.
Strategy: Use `FakeLspTransport` to simulate document sync. Create a test fixture that opens/closes documents through the fake transport.

### Block L4: Process lifecycle (lines 528-630, 678-688)
```
528-532 process shutdown initiation
534-538 kill signal dispatch
541-548 force kill path
554-559 shutdown timeout
562-567 cleanup after shutdown
569-570 state transition
572-573 process state assertion
576-580 respawn detection
583-584 cooldown handling
594-596 idle timeout trigger
598     idle timer reset
602-604 process restart
607-628 full restart lifecycle
630     restart completion
678-688 background task teardown
```
Why uncovered: Process supervision involves async background tasks and OS-level process management.
Strategy: Use `FakeLspTransport` with controlled lifecycle events. Simulate process death and verify respawn behavior.

### Block L5: Error handling (lines 695-750, 805-844, 865-942, 899, 941-942)
```
695-707 error recovery paths
709     error state transition
719     error callback
740     error logging
747-750 error cleanup
805-809 connection error handling
813-814 retry after connection error
839-844 error state propagation
865-868 transport error
870-872 error categorization
874-878 error retry logic
880-886 error escalation
890-894 fatal error path
899     error state finalization
941-942 background task error
951-953 supervisor task error
```
Why uncovered: Error paths require simulating transport failures, process crashes, and timeout conditions.
Strategy: Create `FailingTransport` that produces controlled failures at specific points. Test each error recovery path individually.

### Block L6: Advanced features (lines 1209-1373)
```
1209-1222 goto_definition fallback
1355    call_hierarchy error
1373    references error path
```
Why uncovered: These are fallback paths when the primary LSP method fails.
Strategy: Configure `FakeLspTransport` to return errors for specific methods, forcing fallback.

---

## Uncovered Line Ranges -- detect.rs (~41 lines)

### Block D1: Language detection (lines 76, 228-242, 305, 339, 348, 353, 412-458, 470)
```
76      detect_language() entry for unknown extension
228-233 heuristic matching for multi-language files
239-242 language priority resolution
305     extension-based detection fallback
339     filename-based detection
348     shebang-based detection
353     content-sniffing detection
412-416 workspace config detection
420-424 user config override
426-430 language ID normalization
436     detection confidence scoring
442-445 multi-language workspace
447-451 detection result caching
453-454 cache invalidation
458     detection telemetry
470     detection error handling
```
Why uncovered: Language detection has many branches for different file types and detection strategies.
Strategy: Create test fixtures with various file types (Vue, Svelte, embedded languages). Test each detection path.

### Block D2: Client configuration (lines 522-529, 558-565, 588-627, 678, 725-809, 821-831, 883, 912, 1144)
```
522-529 client config resolution for multi-root workspaces
558-565 config precedence (workspace > user > default)
588-597 environment variable config
611     config validation
625-627 config merge strategy
678     config error path
725-733 command template resolution
737-744 argument expansion
782     env var interpolation
803-809 path resolution
821-831 glob pattern matching for exclude
883     nested workspace config
912     config inheritance
1144    config migration path
```
Why uncovered: Configuration resolution has complex precedence rules and many edge cases.
Strategy: Create test matrix of config combinations. Test precedence, merging, and error paths.

---

## Uncovered Line Ranges -- process.rs (~40 lines)

### Block P1: Process spawning (lines 125-126, 133, 140, 142, 150-152, 157, 163, 169-170, 174, 179-180, 183-184, 187-188, 191-192, 256)
```
125-126 spawn argument construction
133     binary path resolution
140     PATH lookup
142     spawn environment setup
150-152 working directory resolution
157     spawn timeout
163     process group setup
169-170 stdout pipe configuration
174     stderr pipe configuration
179-180 stdin pipe configuration
183-184 process creation
187-188 process start validation
191-192 spawn error handling
256     process health check
```
Why uncovered: Process spawning touches OS-level APIs. Tests use fake transport.
Strategy: Extract process spawning into a trait (`ProcessSpawner`). Create mock spawner for tests. Test argument construction and error handling with mock.

### Block P2: I/O handling (lines 261-277, 291-293, 298, 303-309, 311, 339, 341-343, 353-359, 369, 371-373, 382, 384-386, 396-398, 400, 405-406, 408, 412, 422, 435, 443-453, 455, 477, 484-486, 502-504, 594-602, 604-607, 610, 615-616, 618, 624-625, 633, 636-637, 648-649, 655-656, 669-672, 675-678, 681-685, 692-694, 697-699, 703-706, 743-745, 935, 980)
Why uncovered: I/O handling is deeply intertwined with async runtime and process stdout/stderr reading.
Strategy: Use `FakeLspTransport` with injected I/O sequences. Create deterministic test scenarios for message parsing, partial reads, and error handling.

---

## Uncovered Line Ranges -- capabilities.rs (~1 line)

### Block C1: Capability negotiation (lines 265-280)
```
265-280 server capability parsing for completion, hover, etc.
```
Why uncovered: Rare capability combination.
Strategy: Create test with server returning specific capability set.

---

## Uncovered Line Ranges -- plugin.rs (~2 lines)

### Block PL1: Plugin state (lines 583, 585)
```
583     plugin state transition error
585     plugin reload path
```
Why uncovered: State machine error paths.
Strategy: Force invalid state transitions in tests.

---

## Test Infrastructure Required

### 1. Extend `FakeLspTransport` (in `fake_transport.rs`)

Add capabilities:
- Controlled failure injection (fail on Nth request)
- Timeout simulation (delay responses)
- Malformed response injection
- Process death simulation
- State inspection (what notifications were sent)

### 2. Create `FailingTransport` (new file or extension)

A transport that:
- Always returns errors for specified methods
- Simulates process crash mid-communication
- Returns malformed JSON
- Times out after configurable delay

### 3. Create `ProcessSpawner` trait (refactor)

Extract `start_process()` spawning logic into a trait:
```rust
trait ProcessSpawner {
    fn spawn(&self, config: &LspConfig) -> Result<Child>;
}
```
Implement `RealProcessSpawner` (current behavior) and `MockProcessSpawner` (returns fake child process).

### 4. Test fixtures

Create test fixture files:
- Multi-language files (`.vue`, `.svelte`) for detect.rs
- Malformed source files for parser error paths
- Config files with various combinations

---

## Delivery Breakdown

### BATCH-01a: lifecycle.rs -- Constructor and Request Paths (est. 15 lines covered)
Scope: Blocks L1, L2
Files: lifecycle.rs
Tests: Add to inline `mod tests`
Approach: Direct constructor tests + FakeLspTransport request/response tests

### BATCH-01b: lifecycle.rs -- Document Management (est. 12 lines covered)
Scope: Block L3
Files: lifecycle.rs
Tests: Add to inline `mod tests`
Approach: Document open/close lifecycle tests via FakeLspTransport

### BATCH-01c: lifecycle.rs -- Process Lifecycle and Error Handling (est. 15 lines covered)
Scope: Blocks L4, L5, L6
Files: lifecycle.rs
Tests: Add to inline `mod tests`
Approach: FailingTransport + controlled lifecycle tests. Refactor process spawning into trait.

### BATCH-01d: detect.rs -- Language Detection (est. 25 lines covered)
Scope: Block D1
Files: detect.rs
Tests: Add to inline `mod tests`
Approach: Test matrix of file types and detection strategies

### BATCH-01e: detect.rs -- Client Configuration (est. 16 lines covered)
Scope: Block D2
Files: detect.rs
Tests: Add to inline `mod tests`
Approach: Config precedence and merging test matrix

### BATCH-01f: process.rs -- Process Spawning and I/O (est. 40 lines covered)
Scope: Blocks P1, P2
Files: process.rs
Tests: Add to inline `mod tests`
Approach: ProcessSpawner trait + MockProcessSpawner. FakeLspTransport I/O sequences.

### BATCH-01g: capabilities.rs + plugin.rs (est. 3 lines covered)
Scope: Blocks C1, PL1
Files: capabilities.rs, plugin.rs
Tests: Add to inline `mod tests`
Approach: Simple unit tests for edge cases

---

## Estimated Impact

| Sub-batch | Lines Covered | LCV Improvement |
|---|---|---|
| BATCH-01a | ~15 | +0.1% |
| BATCH-01b | ~12 | +0.1% |
| BATCH-01c | ~15 | +0.1% |
| BATCH-01d | ~25 | +0.2% |
| BATCH-01e | ~16 | +0.1% |
| BATCH-01f | ~40 | +0.3% |
| BATCH-01g | ~3 | +0.02% |
| **Total** | **~134** | **+0.9%** |

Expected LCV after BATCH-01: ~93.2%

---

## Validation

After each sub-batch:
```bash
cargo test -p pathfinder-lsp
cargo clippy -p pathfinder-lsp -- -D warnings
```

After full batch:
```bash
cargo llvm-cov -p pathfinder-lsp --summary-only
```

Target: pathfinder-lsp crate coverage from current to 95%+.
