# Group C: Observability — Research Log

## Date: 2026-05-01

## Patches
- PATCH-005: Surface Per-Language Capabilities in lsp_health
- PATCH-006: Add Probe-Based Readiness to lsp_health

## Objective

Improve `lsp_health` response to give agents the information they need to choose their tool strategy at session start, rather than discovering limitations through failed tool calls.

## Current State

### LspLanguageStatus (pathfinder-lsp/src/types.rs)
```rust
pub struct LspLanguageStatus {
    pub validation: bool,
    pub reason: String,
    pub indexing_complete: Option<bool>,
    pub uptime_seconds: Option<u64>,
    pub diagnostics_strategy: Option<String>,
}
```

### LspLanguageHealth (pathfinder/src/server/types.rs)
```rust
pub struct LspLanguageHealth {
    pub language: String,
    pub status: String,
    pub uptime: Option<String>,
}
```

### DetectedCapabilities (pathfinder-lsp/src/client/capabilities.rs)
```rust
pub struct DetectedCapabilities {
    pub definition_provider: bool,
    pub call_hierarchy_provider: bool,
    pub formatting_provider: bool,
    pub diagnostics_strategy: DiagnosticsStrategy,
    pub workspace_diagnostic_provider: bool,
}
```

## Key Findings

### PATCH-005: Per-Language Capabilities

**What's Missing:**
- `LspLanguageStatus` doesn't expose individual capabilities (definition, call_hierarchy, diagnostics, formatting)
- `LspLanguageHealth` doesn't include capability fields
- Agents can't determine which tools will degrade until they try them

**What's Available:**
- `DetectedCapabilities` has all the data we need in `LspClient::capability_status()`
- `ProcessEntry::to_validation_status()` already maps process state to `LspLanguageStatus`

**Implementation Path:**
1. Extend `LspLanguageStatus` with capability fields
2. Extend `LspLanguageHealth` with strategy and capability fields  
3. Update `validation_status_from_parts()` to populate capabilities from `DetectedCapabilities`
4. Wire up in `lsp_health_impl()` to map from status to health response

### PATCH-006: Probe-Based Readiness

**Problem:**
- Some LSPs (gopls, tsserver) don't emit `$/progress` notifications
- `indexing_complete` stays `None` or `false` even when LSP is ready
- Status shows `warming_up` indefinitely

**Solution:**
- When `status == "warming_up"` and `uptime > 10s`, fire a lightweight probe
- Probe: `goto_definition` on line 1 column 1 of a known file
- If probe succeeds → upgrade status to `"ready"`, set `probe_verified = true`
- If probe fails → keep `"warming_up"`, `probe_verified = false`

**Implementation Path:**
1. Add `probe_verified` field to `LspLanguageHealth`
2. Add `probe_language_readiness()` helper to `navigation.rs`
3. Add `find_probe_file()` helper to locate well-known files per language
4. In `lsp_health_impl()`, check for stale `warming_up` and fire probe
5. Add helper to parse uptime strings back to seconds

## Architecture Decisions

### Capability Surface (PATCH-005)
- Use `Option<bool>` for capability flags to distinguish "unsupported" from "unknown"
- When process hasn't started (lazy), all capabilities are `None`
- When process is running, capabilities reflect actual LSP server capabilities

### Probe Design (PATCH-006)
- Probe only when `status == "warming_up"` AND `uptime > 10s`
- 10s threshold avoids unnecessary probes during normal warmup
- Probe timeout: 3s (quick check, not blocking)
- Use `goto_definition` because it's fast and indicates LSP is processing requests
- Any `Ok` result (even `Ok(None)`) means LSP is alive — only `Err` means not ready

### File Candidates for Probe
- Rust: `src/main.rs`, `src/lib.rs`
- Go: `main.go`, `cmd/main.go`
- TypeScript: `src/index.ts`, `index.ts`, `src/main.ts`
- Python: `src/__init__.py`, `main.py`, `setup.py`

## Dependencies

- PATCH-005: No dependencies
- PATCH-006: Depends on PATCH-005 (needs capability fields to exist)

## Risk Assessment

**PATCH-005:** LOW
- Purely additive changes
- No behavior modification
- Backward compatible (new fields are optional)

**PATCH-006:** LOW  
- Adds optimistic status upgrade (false positive unlikely)
- Probe failure keeps original status (safe fallback)
- Adds ~500ms latency for stale warming_up cases only

## Verification Plan

### PATCH-005
- `test_lsp_health_includes_diagnostics_strategy`
- `test_lsp_health_shows_push_for_go`
- `test_lsp_health_shows_pull_for_rust`
- `test_lsp_health_shows_capabilities`
- Manual: call `lsp_health` and verify all fields populated

### PATCH-006
- `test_lsp_health_probe_upgrades_warming_up_to_ready`
- `test_lsp_health_probe_keeps_warming_up_when_probe_fails`
- `test_lsp_health_no_probe_for_recently_started`
- `test_lsp_health_no_probe_for_already_ready`
- Manual: start gopls, wait 15s, call `lsp_health` → should show `ready`

## References
- 00-INDEX.md: Group C overview
- PATCH-001: DiagnosticsStrategy enum (already implemented)
- capabilities.rs: DetectedCapabilities definition
- types.rs: LspLanguageStatus, LspLanguageHealth definition
- navigation.rs: lsp_health_impl implementation
