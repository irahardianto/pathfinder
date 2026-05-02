# LSP-HEALTH-001: lsp_health Status Discrepancy and LSP Lifecycle Bugs

**Date**: 2026-05-02
**Severity**: P0 (blocks all LSP-dependent features for non-Rust languages)
**Status**: Open
**Affects**: Pathfinder v0.4.0, all non-Rust language servers (gopls, tsserver, pyright)

---

## Finding Validation Summary

| Report Finding | Real? | Severity | Worth Fixing? | Root Cause Confirmed? |
|---|---|---|---|---|
| Bug 1: lsp_health never reports ready | YES | P0 | YES | YES |
| Bug 2: Orphaned gopls from host IDE | YES | P1 | YES | YES (partial fix) |
| Bug 3: Pyright symlink storm | YES | P2 | YES (user-side only) | YES |

---

## Bug 1: lsp_health Status Never Reaches "ready" for Non-Rust Languages (P0)

### Root Cause Analysis

The status determination logic in `crates/pathfinder/src/server/tools/navigation.rs` lines 1270-1278:

```rust
let (status_str, uptime) = if status.indexing_complete == Some(true) {
    ("ready", ...)
} else if status.indexing_complete == Some(false) {
    ("warming_up", ...)
} else if status.uptime_seconds.is_some() {
    ("starting", ...)
} else {
    ("unavailable", ...)
};
```

This logic gates "ready" entirely on `indexing_complete == Some(true)`. The `indexing_complete` flag is only set by `progress_watcher_task` (`crates/pathfinder-lsp/src/client/mod.rs` line 1592) when it receives a `$/progress` notification with `kind == "end"`.

**The problem chain:**

1. **Rust (rust-analyzer)**: rust-analyzer emits `$/progress` with `WorkDoneProgressEnd` after indexing. This works. Status correctly becomes "ready".

2. **Go (gopls)**: gopls uses `window/workDoneProgress/create` to create progress tokens but its `$progress` notifications use a **different token format** or may not emit `WorkDoneProgressEnd` for the initial indexing phase in the same way. The progress watcher never sees `kind == "end"` -> `indexing_complete` stays `false` -> status stays "warming_up" forever.

3. **TypeScript (typescript-language-server)**: Same issue as gopls. tsserver's progress reporting doesn't match the watcher's expectations. However, the report confirms `get_definition` paradoxically works for TS while `lsp_health` says "warming_up" — proving the LSP is functional but the health signal is wrong.

4. **Python (pyright)**: Pyright reports `diagnostics_strategy: DiagnosticsStrategy::None` (no `diagnosticProvider` and no `textDocumentSync` detected as push-capable). The `validation_status_from_parts` function in `crates/pathfinder-lsp/src/client/mod.rs` line 162 maps `DiagnosticsStrategy::None` to a status where `validation: false` and `reason: "LSP connected but does not support diagnostics"`. This means `indexing_complete` is `Some(false)` but the overall capability profile says "not supporting diagnostics" — which is misleading since pyright DOES support `definitionProvider` (the core navigation feature).

**There IS a probe-based fallback** (PATCH-006, lines 1312-1332) that fires when `uptime > 10s` and status is still "warming_up". It calls `probe_language_readiness()` which does a real `goto_definition` call. However, this probe:
- Only fires on `lsp_health` tool calls (not proactively)
- The probe for `find_probe_file()` only looks at hardcoded paths like `src/main.rs`, `main.go`, `src/index.ts` — in monorepos with non-standard layouts (e.g., `apps/backend/cmd/main.go`, `tools/fath-factory/src/...`), the probe finds NO candidate file and returns `false`
- The probe opens/closes files on each `lsp_health` call, which is wasteful

### Additional Same-Type Occurrences in Codebase

1. **`validation_status_from_parts` gates validation on diagnostics, not navigation** (`mod.rs:141-175`): The function treats `DiagnosticsStrategy::None` as a second-class status even when `definition_provider: true` and `call_hierarchy_provider: true`. This causes pyright (which has `diagnostics_strategy: None` but `definition_provider: true`) to report `validation: false`, which feeds into the "unavailable" perception.

2. **`compute_degraded_tools`** (`navigation.rs:1407-1420`): Uses `supports_diagnostics != Some(true)` AND `diagnostics_strategy != "push"` to mark `validate_only` as degraded. For pyright (push diagnostics through `textDocumentSync` but no explicit `diagnosticProvider`), this incorrectly marks validation as degraded.

3. **Capabilities detection for push diagnostics** (`capabilities.rs:102-108`): The push detection logic `caps.get("textDocumentSync").is_some_and(|v| !v.is_null())` is too aggressive — it checks for ANY textDocumentSync presence, but pyright-langserver may report textDocumentSync without actually pushing diagnostics in the way Pathfinder expects.

### Fix Required

**Two-phase readiness model:**
- Phase 1: `ready` when `initialize` handshake completes + `definitionProvider: true` (navigation tools usable)
- Phase 2: `indexing_complete` as an optional additional signal (not a gate)

**Separate navigation readiness from diagnostics readiness:**
- `lsp_health` should report two independent status signals: `navigation_ready` (definition, call hierarchy) and `validation_ready` (diagnostics)
- An LSP can be `navigation_ready: true` but `validation_ready: false` — agents should still be able to use `get_definition`, `analyze_impact`, etc.

---

## Bug 2: Orphaned gopls from Host IDE (P1)

### Root Cause Analysis

The concurrent LSP detection (`detect_concurrent_lsp` in `mod.rs:525-567`) detects concurrent instances correctly. However:

1. **Isolation only applies to Rust**: `spawn_lsp_child` (`process.rs:175`) only sets `CARGO_TARGET_DIR` when `language_id == "rust"`. For gopls, the concurrent instance is detected but NO isolation is applied. Two gopls instances share the same `go.sum` cache and build artifacts, causing lock contention and slow indexing.

2. **No GOPATH/GOCACHE isolation for gopls**: Unlike rust-analyzer (which gets `CARGO_TARGET_DIR`), gopls has no equivalent environment variable set to isolate its cache. The Go module cache (`$GOMODCACHE`) and build cache (`$GOCACHE`) are shared between the IDE's gopls and Pathfinder's gopls.

3. **Warning is insufficient**: The log says "Isolating build artifacts" but for gopls it doesn't actually isolate anything. The message is misleading.

### Additional Same-Type Occurrences

- **No isolation for typescript-language-server**: If an IDE has its own tsserver running, Pathfinder's tsserver shares the same `.tsbuildinfo` files. No detection or isolation exists for TypeScript.
- **No isolation for pyright**: Pyright uses its own cache in `.pyright/` but concurrent instances may still conflict on `.pyc` files or type stub caches.

### Fix Required

1. Add `GOCACHE` and `GOMODCACHE` environment variable overrides for gopls when concurrent instances detected
2. Fix the warning message to be accurate about what's actually being isolated
3. Consider adding a `--remote=auto` flag to gopls args to connect to the existing instance instead of spawning a new one (gopls supports `gopls serve` in daemon mode)

---

## Bug 3: Pyright Symlink Storm (P2)

### Root Cause Analysis

This is a user-side issue. Pyright crawls the workspace tree and encounters recursive symlinks in `pytest-of-irahardianto/` directories. Pyright correctly skips them but produces 141 lines of log noise and adds unnecessary crawl time.

### Fix Required (User-Side Only)

1. Add `pytest-of-*` to `.gitignore` and `pyrightconfig.json` exclude patterns
2. Clean up existing pytest temp directories

---

## Remediation Plan

### Task 1: Decouple Navigation Readiness from Indexing Completion (P0)

**File**: `crates/pathfinder-lsp/src/types.rs`
**File**: `crates/pathfinder/src/server/tools/navigation.rs`
**File**: `crates/pathfinder-lsp/src/client/mod.rs`

**Step 1.1**: Add `navigation_ready` field to `LspLanguageStatus`

In `crates/pathfinder-lsp/src/types.rs`, add to `LspLanguageStatus`:

```rust
/// Whether the LSP is ready for navigation operations (get_definition, analyze_impact).
/// True once initialize handshake completes with definitionProvider: true.
/// Independent of indexing_complete.
#[serde(skip_serializing_if = "Option::is_none")]
pub navigation_ready: Option<bool>,
```

**Step 1.2**: Update `validation_status_from_parts` to set `navigation_ready`

In `crates/pathfinder-lsp/src/client/mod.rs`, update `validation_status_from_parts`:

For all branches where `running == true` and the capability is known, set:
```rust
navigation_ready: Some(supports_definition),
```

For the `!running` branch:
```rust
navigation_ready: None,
```

For the lazy-start branch in `capability_status`:
```rust
navigation_ready: None,
```

**Step 1.3**: Update `lsp_health_impl` to use two-phase readiness

In `crates/pathfinder/src/server/tools/navigation.rs`, replace the status determination block (lines 1270-1278) with:

```rust
let (status_str, uptime) = if status.navigation_ready == Some(true) {
    // Navigation is functional — report ready regardless of indexing
    // indexing_complete is an optional additional signal
    ("ready", status.uptime_seconds.map(format_uptime))
} else if status.indexing_complete == Some(false) || status.navigation_ready == Some(false) {
    // Process running but not yet functional
    ("warming_up", status.uptime_seconds.map(format_uptime))
} else if status.uptime_seconds.is_some() {
    ("starting", status.uptime_seconds.map(format_uptime))
} else {
    ("unavailable", None)
};
```

**Step 1.4**: Add `indexing_status` field to `LspLanguageHealth` response

In `crates/pathfinder/src/server/types.rs`, add to `LspLanguageHealth`:

```rust
/// Background indexing status: "complete", "in_progress", or None (not reported).
/// Independent of overall status — an LSP can be "ready" for navigation while
/// still indexing in the background.
#[serde(skip_serializing_if = "Option::is_none")]
pub indexing_status: Option<String>,
```

In the `lsp_health_impl` loop, populate:
```rust
indexing_status: if status.indexing_complete == Some(true) {
    Some("complete".to_owned())
} else if status.indexing_complete == Some(false) {
    Some("in_progress".to_owned())
} else {
    None
},
```

**Step 1.5**: Add tests

In `crates/pathfinder-lsp/src/client/mod.rs` tests, add:

```rust
#[test]
fn test_validation_status_navigation_ready_with_no_diagnostics() {
    // Pyright scenario: definition_provider=true, diagnostics_strategy=None
    let status = validation_status_from_parts(
        "pyright",
        true,   // running
        DiagnosticsStrategy::None,
        true,   // supports_definition
        true,   // supports_call_hierarchy
        false,  // supports_formatting
        false,  // indexing_complete
        5,      // uptime_seconds
    );
    assert_eq!(status.navigation_ready, Some(true));
    assert!(!status.validation); // diagnostics not available
    assert_eq!(status.indexing_complete, Some(false));
}
```

In `crates/pathfinder/src/server/tools/navigation.rs` tests, add tests for:
- Language with `navigation_ready: Some(true)` but `indexing_complete: Some(false)` -> status "ready"
- Language with `navigation_ready: Some(false)` and `indexing_complete: Some(false)` -> status "warming_up"
- Language with `navigation_ready: None` -> status "starting"

### Task 2: Fix Pyright Diagnostics Detection (P0)

**File**: `crates/pathfinder-lsp/src/client/capabilities.rs`

**Step 2.1**: Investigate pyright's actual capabilities

Pyright DOES support push diagnostics via `textDocument/publishDiagnostics`. The issue is that `pyright-langserver` may report `textDocumentSync` differently than expected. The current detection:

```rust
let has_push = if has_pull {
    false
} else {
    caps.get("textDocumentSync").is_some_and(|v| !v.is_null())
};
```

Check if pyright returns `textDocumentSync` as an object with `openClose: true, change: 2`. If it does, this should detect push correctly. If pyright omits `textDocumentSync` entirely, then `has_push = false` and `diagnostics_strategy = None`.

**Step 2.2**: If pyright truly has no diagnostics capability in its response, ensure `navigation_ready` is still `true`

The fix in Task 1 already handles this — pyright with `definition_provider: true` will have `navigation_ready: Some(true)` and status "ready" even if `diagnostics_strategy: None`.

### Task 3: Improve Probe Reliability (P0)

**File**: `crates/pathfinder/src/server/tools/navigation.rs`

**Step 3.1**: Improve `find_probe_file` for monorepo layouts

Replace the hardcoded candidate list with a workspace scan:

```rust
pub(crate) fn find_probe_file(&self, language_id: &str) -> Option<std::path::PathBuf> {
    let extensions: &[&str] = match language_id {
        "rust" => &["rs"],
        "go" => &["go"],
        "typescript" | "javascript" => &["ts", "tsx", "js", "jsx"],
        "python" => &["py"],
        _ => return None,
    };

    // First try well-known paths (fast path)
    let candidates = match language_id {
        "rust" => vec!["src/main.rs", "src/lib.rs"],
        "go" => vec!["main.go", "cmd/main.go"],
        "typescript" | "javascript" => vec![
            "src/index.ts", "index.ts", "src/main.ts",
            "src/index.js", "index.js", "src/main.js",
        ],
        "python" => vec!["src/__init__.py", "main.py", "setup.py", "__init__.py"],
        _ => vec![],
    };

    for candidate in candidates {
        let path = self.workspace_root.path().join(candidate);
        if path.exists() {
            return Some(std::path::PathBuf::from(candidate));
        }
    }

    // Fallback: scan workspace for any file with matching extension (depth 4)
    if let Ok(entries) = std::fs::read_dir(self.workspace_root.path()) {
        // Use a simple recursive scan with depth limit
        self.find_file_by_extension(extensions, 0, 4)
    } else {
        None
    }
}
```

Add a helper method:

```rust
fn find_file_by_extension(
    &self,
    extensions: &[&str],
    current_depth: usize,
    max_depth: usize,
) -> Option<std::path::PathBuf> {
    // Recursive directory scan up to max_depth looking for any file
    // matching the given extensions. Returns the first match.
    // Implementation uses walkdir or manual recursion.
    // ...
}
```

**Step 3.2**: Cache probe results

Add a field to `PathfinderServer` (or use a once-cell) to avoid re-probing on every `lsp_health` call:

```rust
/// Cache of probe results per language to avoid redundant LSP calls.
probe_cache: Arc<DashMap<String, bool>>,
```

On probe success, cache the result. Subsequent `lsp_health` calls use the cached value.

### Task 4: Add gopls Cache Isolation (P1)

**File**: `crates/pathfinder-lsp/src/client/process.rs`

**Step 4.1**: Add gopls-specific isolation

In `spawn_lsp_child`, after the Rust isolation block (line 175), add:

```rust
// gopls isolation: use a separate GOCACHE and GOMODCACHE to avoid
// contention with the IDE's gopls instance.
if isolate_target_dir && language_id == "go" {
    let isolated_cache = project_root.join(".pathfinder").join("gopls-cache");
    cmd.env("GOCACHE", isolated_cache.join("build"));
    cmd.env("GOMODCACHE", isolated_cache.join("mod"));
    tracing::info!(
        language = language_id,
        "LSP: set isolated GOCACHE/GOMODCACHE for gopls to avoid cache contention"
    );
}
```

**Step 4.2**: Add `.pathfinder/` to default `.gitignore` recommendations

When creating the isolated cache directory, ensure it's added to the project's `.gitignore`.

### Task 5: Fix Concurrent LSP Warning Message (P1)

**File**: `crates/pathfinder-lsp/src/client/mod.rs`

**Step 5.1**: Update the warning message to reflect actual isolation

In `detect_concurrent_lsp` (line 553), change the warning to:

```rust
tracing::warn!(
    language = language_id,
    binary = binary_name,
    instances_found = count,
    "LSP: detected {} concurrent instances of {binary_name} on this workspace. \
     {} build artifact isolation will be applied to avoid cache lock contention. \
     First-time indexing may take 30-60s for this workspace.",
    count,
    if language_id == "rust" { "Cargo target directory" }
    else if language_id == "go" { "Go build cache" }
    else { "No" } // Explicit: no isolation for this language yet
);
```

### Task 6: Progress Watcher Fallback Timeout (P2)

**File**: `crates/pathfinder-lsp/src/client/mod.rs`

**Step 6.1**: Add time-based fallback for indexing_complete

After a configurable timeout (default 30s) since `spawned_at`, if no `WorkDoneProgressEnd` has been received but the LSP responded to `initialize` successfully, set `indexing_complete` to `true` as a heuristic.

In `start_process`, after spawning the progress watcher:

```rust
// Timeout-based fallback: if progress notifications are never received,
// mark indexing as complete after 30 seconds. This prevents permanent
// warming_up status for LSPs that don't emit $/progress.
let timeout_flag = Arc::clone(&indexing_complete);
let timeout_lang = language_id.clone();
tokio::spawn(async move {
    tokio::time::sleep(Duration::from_secs(30)).await;
    if !timeout_flag.load(Ordering::Relaxed) {
        timeout_flag.store(true, Ordering::Relaxed);
        tracing::info!(
            language = %timeout_lang,
            "LSP: no WorkDoneProgressEnd received after 30s — assuming indexing complete (timeout fallback)"
        );
    }
});
```

### Task 7: User-Side Fix — Pytest Symlink Cleanup (P2)

**This is a workspace-specific fix, not a Pathfinder code change.**

**Step 7.1**: Add to workspace `.gitignore`:
```
pytest-of-*
```

**Step 7.2**: Add to `tools/fath-factory/pyrightconfig.json` (or `pyproject.toml`):
```toml
[tool.pyright]
exclude = [
    "pytest-of-*",
    "**/pytest-of-*",
    "**/__pycache__",
]
```

**Step 7.3**: Clean up existing temp dirs:
```bash
find . -type d -name "pytest-of-*" -exec rm -rf {} + 2>/dev/null
```

---

## Verification Test Plan

After applying all fixes, verify:

### Test 1: lsp_health reports "ready" for all initialized languages

```bash
# 1. Kill any orphaned LSP processes
pkill -f "gopls|typescript-language-server|pyright"

# 2. Start Pathfinder
RUST_LOG=debug pathfinder-mcp /path/to/workspace

# 3. From MCP client, call lsp_health at T+5s, T+30s, T+60s
# Expected: all three languages report status="ready" within 5s
# (not "warming_up" or "unavailable")
```

### Test 2: Navigation tools work immediately after lsp_health says "ready"

For each language, call `get_definition` on a known symbol:
- Go: `apps/backend/internal/platform/server/response.go::HandleLogicError`
- TS: `apps/frontend/src/utils/logger.ts::Logger`
- Python: `tools/fath-factory/src/fath_factory/pedagogue.py::generate`

Expected: All return `degraded: false` with a valid definition location.

### Test 3: Pyright reports navigation_ready=true despite diagnostics_strategy=none

Call `lsp_health` with `language: "python"`:
Expected response:
```json
{
  "status": "ready",
  "languages": [{
    "language": "python",
    "status": "ready",
    "navigation_ready": true,
    "indexing_status": "in_progress",
    "diagnostics_strategy": "none",
    "supports_definition": true,
    "supports_call_hierarchy": true,
    "degraded_tools": ["validate_only"]
  }]
}
```

### Test 4: Concurrent gopls isolation works

1. Start VS Code with Go extension on the workspace (launches gopls)
2. Start Pathfinder
3. Check logs for: `"set isolated GOCACHE/GOMODCACHE for gopls"`
4. Verify Pathfinder's gopls initializes without cache lock errors

### Test 5: Probe works in monorepo layouts

1. Open a workspace where Go files are at `apps/backend/cmd/server/main.go` (not `main.go`)
2. Call `lsp_health`
3. Expected: probe finds the file and reports "ready" even if no hardcoded path matches

---

## File Change Summary

| File | Change Type | Task(s) |
|---|---|---|
| `crates/pathfinder-lsp/src/types.rs` | Add `navigation_ready` field | 1.1 |
| `crates/pathfinder-lsp/src/client/mod.rs` | Update `validation_status_from_parts`, add timeout fallback, fix warning | 1.2, 5.1, 6.1 |
| `crates/pathfinder/src/server/types.rs` | Add `indexing_status` to `LspLanguageHealth` | 1.4 |
| `crates/pathfinder/src/server/tools/navigation.rs` | Rewrite status determination, improve probe, add tests | 1.3, 3.1, 3.2 |
| `crates/pathfinder-lsp/src/client/process.rs` | Add gopls isolation env vars | 4.1 |

---

## Execution Order

1. Task 1 (P0) — core status logic fix, unblocks everything
2. Task 2 (P0) — pyright capability detection verification
3. Task 3 (P0) — probe reliability for monorepos
4. Task 6 (P2) — progress watcher timeout fallback
5. Task 4 (P1) — gopls cache isolation
6. Task 5 (P1) — warning message accuracy
7. Task 7 (P2) — user-side pytest cleanup

Tasks 1-3 should be done together as they're interdependent. Task 6 can be done independently. Tasks 4-5 are a pair. Task 7 is standalone.
