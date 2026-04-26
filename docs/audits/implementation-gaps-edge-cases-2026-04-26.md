# Implementation Gaps and Edge Cases Review

**Date:** 2026-04-26
**Reviewer:** AI Agent (Pathfinder MCP)
**Scope:** Full codebase audit for implementation gaps and unhandled edge cases

## Summary

- **Files reviewed:** 50+ source files across all crates
- **Issues found:** 8 total (0 critical, 2 major, 4 minor, 2 nit)

## Critical Issues

None identified.

## Major Issues

### [ARCH] M1: Cache race condition documented but unaddressed

**Location:** `crates/pathfinder-treesitter/src/cache.rs:41-47`

**Issue:** The comment in `AstCache` acknowledges a race condition:

```rust
// NOTE: Concurrent requests for the same file may race through the slow path
// simultaneously, resulting in redundant parsing work. For v1 (local MCP
// server, low concurrency) this is acceptable. A singleflight /
// `tokio::sync::OnceCell` approach would eliminate it if contention becomes
// measurable.
```

**Impact:** In high-concurrency scenarios (multiple agents working simultaneously), multiple requests for the same uncached file will:
1. Each perform a full disk read
2. Each perform a full parse
3. Each try to insert into the LRU cache (last one wins)
4. Waste CPU and I/O resources

**Recommendation:** Implement singleflight pattern using `tokio::sync::OnceCell` or a dedicated singleflight crate:

```rust
use tokio::sync::OnceCell;

pub struct AstCache {
    entries: Mutex<LruCache<PathBuf, CacheEntry>>,
    vue_entries: Mutex<LruCache<PathBuf, MultiZoneEntry>>,
    // Add singleflight locks for in-flight parses
    in_flight: Mutex<HashMap<PathBuf, Arc<OnceCell<Result<(Tree, Arc<[u8]>), SurgeonError>>>>>,
}
```

**Priority:** Medium - documented as acceptable for v1, but should be tracked for v2.

---

### [RES] M2: LSP process cleanup on graceful shutdown not guaranteed

**Location:** `crates/pathfinder-lsp/src/client/mod.rs:1330-1367`

**Issue:** The `idle_timeout_task` runs in an infinite loop with no cancellation token. When the main server shuts down (via `tokio::main` completion), this background task is aborted without:
1. Graceful LSP shutdown requests (shutdown + exit)
2. Proper cleanup of child processes
3. Flushing of pending diagnostics

**Impact:**
- LSP child processes may become orphaned
- Zombie processes if parent terminates abnormally
- Potential resource leaks (file descriptors, memory)

**Current code:**
```rust
async fn idle_timeout_task(
    processes: Arc<RwLock<HashMap<String, ProcessEntry>>>,
    dispatcher: Arc<RequestDispatcher>,
) {
    loop {
        tokio::time::sleep(IDLE_CHECK_INTERVAL).await;
        // ... idle timeout logic
    }
}
```

**Recommendation:** Add cancellation support:

```rust
async fn idle_timeout_task(
    processes: Arc<RwLock<HashMap<String, ProcessEntry>>>,
    dispatcher: Arc<RequestDispatcher>,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                // Graceful shutdown of all processes
                tracing::info!("LSP: shutting down all processes");
                let mut guard = processes.write().await;
                for (_, entry) in guard.drain() {
                    if let ProcessEntry::Running(mut state) = entry {
                        shutdown(&mut state.process, &dispatcher).await;
                    }
                }
                break;
            }
            _ = tokio::time::sleep(IDLE_CHECK_INTERVAL) => {
                // ... existing idle timeout logic
            }
        }
    }
}
```

**Priority:** Medium - graceful shutdown is best practice for process management.

## Minor Issues

### [PAT] M3: Unsafe code lacks SAFETY comment

**Location:** `crates/pathfinder-lsp/src/client/process.rs:314-320`

**Issue:** The `apply_linux_process_hardening` function uses `unsafe` but lacks a `// SAFETY:` comment documenting why it's safe.

**Current code:**
```rust
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn apply_linux_process_hardening(cmd: &mut tokio::process::Command) {
    unsafe {
        cmd.pre_exec(|| {
            let _ = libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
}
```

**Recommendation:** Add safety documentation:

```rust
// SAFETY:
// - `prctl(PR_SET_PDEATHSIG, SIGKILL)` is a well-documented Linux syscall
// - The closure returns `io::Result<()>` and doesn't access any borrowed data
// - This runs in the child process before exec, so no shared state concerns
// - Failure is ignored (`let _ =`) as this is a best-effort hardening measure
unsafe {
    cmd.pre_exec(|| {
        let _ = libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
        Ok(())
    });
}
```

**Priority:** Low - code is safe, but missing documentation.

---

### [TEST] M4: No integration test for batch edit overlap detection

**Location:** `crates/pathfinder/src/server/tools/edit/batch.rs:153-166`

**Issue:** The `apply_sorted_edits` function includes overlap detection logic:

```rust
// Ensure no overlapping edits
for i in 1..resolved_edits.len() {
    let (prev_idx, _, prev) = &resolved_edits[i - 1];
    let (curr_idx, curr_path, curr) = &resolved_edits[i];
    if curr.end_byte > prev.start_byte {
        // ... return error
    }
}
```

However, there are no integration tests that verify this overlap detection works correctly for:
- Multiple edits in the same file with overlapping byte ranges
- Edge case: exact boundary overlap (end_byte == start_byte)
- Large numbers of edits that might trigger integer overflow in byte arithmetic

**Recommendation:** Add test cases:
```rust
#[tokio::test]
async fn test_replace_batch_detects_overlapping_edits() {
    // Create edits that overlap in byte range
    let params = ReplaceBatchParams {
        filepath: "src/test.rs".into(),
        base_version: "hash".into(),
        edits: vec![
            BatchEdit {
                semantic_path: "src/test.rs::func_a".into(),
                edit_type: "replace_body".into(),
                new_code: Some("new body a".into()),
                // ... edit 1 targets lines 10-20
            },
            BatchEdit {
                semantic_path: "src/test.rs::func_b".into(),
                edit_type: "replace_body".into(),
                new_code: Some("new body b".into()),
                // ... edit 2 targets lines 15-25 (overlaps with edit 1)
            },
        ],
        ignore_validation_failures: false,
    };
    // Expect: INVALID_TARGET error with overlap message
}
```

**Priority:** Low - code logic appears correct, but test coverage is missing.

---

### [OBS] M5: File watcher event loss not detected or logged

**Location:** `crates/pathfinder-common/src/file_watcher.rs:35-55`

**Issue:** The file watcher uses `unbounded_channel` for event delivery and silently ignores send failures:

```rust
// Best-effort send — if receiver is dropped, we stop
let _ = tx.send(fe);
```

If the channel becomes full (unlikely with unbounded) or the receiver is dropped, events are silently lost. This could lead to:
- Cache inconsistency (cached AST not invalidated when file changes)
- Stale data returned to agents

**Recommendation:** Log when events are dropped:

```rust
if let Err(_) = tx.send(fe) {
    tracing::warn!(
        path = %path.display(),
        "file watcher: channel send failed - receiver dropped, cache may be stale"
    );
}
```

**Priority:** Low - unbounded channel rarely drops, but logging improves observability.

---

### [ERR] M6: UTF-8 validation failures skip all validation silently

**Location:** `crates/pathfinder/src/server/tools/edit/validation.rs:268-277`

**Issue:** When file content is invalid UTF-8, validation is skipped with a generic "utf8_error" reason:

```rust
let validation_outcome = match (original_str, new_str) {
    (Ok(orig), Ok(new)) => {
        self.run_lsp_validation(/* ... */).await
    }
    _ => ValidationOutcome {
        validation: EditValidation::skipped(),
        skipped: true,
        skipped_reason: Some("utf8_error".to_owned()),
        should_block: false,
    },
};
```

This silently proceeds with the edit despite invalid UTF-8, which could:
- Produce corrupted files
- Cause downstream tools (compilers, formatters) to fail
- Confuse agents when errors appear later

**Recommendation:** Block edits that would introduce invalid UTF-8:

```rust
let validation_outcome = match (original_str, new_str) {
    (Ok(orig), Ok(new)) => {
        self.run_lsp_validation(/* ... */).await
    }
    (Err(_), Ok(_)) => {
        // Original was invalid but new is valid - allow (fixing corruption)
        ValidationOutcome {
            validation: EditValidation::skipped(),
            skipped: true,
            skipped_reason: Some("original_invalid_utf8_fixed".to_owned()),
            should_block: false,
        }
    }
    (Ok(_), Err(_)) => {
        // Original valid but new invalid - BLOCK this
        let err = PathfinderError::IoError {
            message: "new content contains invalid UTF-8".to_owned(),
        };
        return Err(pathfinder_to_error_data(&err));
    }
    (Err(_), Err(_)) => {
        // Both invalid - skip validation but warn
        tracing::warn!("both original and new content are invalid UTF-8");
        ValidationOutcome {
            validation: EditValidation::skipped(),
            skipped: true,
            skipped_reason: Some("both_invalid_utf8".to_owned()),
            should_block: false,
        }
    }
};
```

**Priority:** Low - most files are UTF-8, but blocking invalid UTF-8 prevents corruption.

## Nit Issues

### [NIT] N1: Unused variable in tests

**Location:** `crates/pathfinder-lsp/src/no_op.rs` (multiple test functions)

**Issue:** Several test functions create a `lawyer` variable that is never used:

```rust
#[tokio::test]
async fn test_did_open_no_lsp() {
    let lawyer = NoOpLawyer;
    // lawyer is never used
}
```

**Recommendation:** Prefix with underscore:
```rust
let _lawyer = NoOpLawyer;
```

**Priority:** Nit - clean-up only.

---

### [NIT] N2: Clippy warnings in test code

**Issue:** Multiple clippy warnings in test code:
- Single-character string constants used as patterns
- `format!(...)` appended to existing `String`
- Unnecessary hashes around raw string literals
- Missing backticks in documentation

**Recommendation:** Run `cargo clippy --fix` on test code:
```bash
cargo clippy --fix --tests --allow-dirty --allow-staged
```

**Priority:** Nit - doesn't affect production code.

## Edge Cases Handled Well

The following edge cases are already properly handled:

1. **TOCTOU (Time-of-check-time-of-use) races** in `flush_edit_with_toctou` - re-reads file before write to detect concurrent modifications
2. **Path traversal attacks** - blocked by sandbox with explicit `ParentDir` check
3. **LSP process crashes** - reader supervisor task detects and removes dead processes
4. **Cache invalidation** - file watcher + manual invalidation on edits
5. **Empty files** - handled by tree-sitter parser and edit tools
6. **Concurrent file watchers** - uses unbounded channel with graceful degradation
7. **Vue SFC multi-zone parsing** - degraded mode when template/style zones fail
8. **Large files** - chunked reading in `hash_file` to bound memory
9. **Unicode paths** - uses `PathBuf` and `to_string_lossy()` appropriately
10. **Symbol resolution conflicts** - `did_you_mean` suggestions for ambiguous symbols

## Missing Features (Not Bugs)

These are intentional scope limitations, not bugs:

1. **No multi-workspace support** - single workspace per server instance
2. **No remote file access** - all operations within local workspace
3. **No code completion** - only navigation, search, and edit tools
4. **No refactorings** - only edits, not automated refactorings
5. **No project-wide rename** - would require cross-file symbol tracking
6. **No LSP workspace symbols** - only document symbols and workspace diagnostics

## Recommendations Summary

### High Priority (Fix Before Next Release)
- None

### Medium Priority (Fix In Next Sprint)
1. [M1] Implement singleflight for cache to prevent redundant parsing
2. [M2] Add cancellation token to LSP idle timeout task for graceful shutdown

### Low Priority (Technical Debt)
1. [M3] Add SAFETY comment to `apply_linux_process_hardening`
2. [M4] Add integration tests for batch edit overlap detection
3. [M5] Log file watcher event drops
4. [M6] Block edits that introduce invalid UTF-8

### Nit (Clean-up)
1. [N1] Fix unused variable warnings in tests
2. [N2] Run `cargo clippy --fix` on test code

## Conclusion

The codebase is in good shape with:
- Comprehensive error handling and validation
- Proper OCC (optimistic concurrency control)
- Security-conscious sandbox implementation
- Well-documented edge cases in comments

The two major issues (cache race, LSP shutdown) are documented as acceptable for v1 but should be tracked for v2. No critical security or data integrity issues found.
