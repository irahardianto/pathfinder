# GAP-001: Handle LspError::Timeout in Navigation Tools with Grep Fallback

## Group: A (Critical) — LSP Timeout Resilience
## Depends on: Nothing

## Objective

When an LSP request times out (10s for definition/callHierarchy, 30s for diagnostics),
the navigation tools (`get_definition`, `read_with_deep_context`, `analyze_impact`) currently
return hard errors. The grep-based degraded fallback code EXISTS but only matches
`Err(LspError::NoLspAvailable)`. The `Err(LspError::Timeout)` variant hits a generic
`Err(e)` match arm that logs a warning and returns an error with no fallback.

This is the root cause of the "100% timeout rate" reported in both agent evaluations.
Fixing these match arms unblocks all three navigation tools when the LSP is slow but
the rest of the system (tree-sitter, ripgrep) is functional.

## Scope

| File | Function | Change |
|------|----------|--------|
| `crates/pathfinder/src/server/tools/navigation.rs` | `get_definition_impl` | Add `LspError::Timeout` to the fallback match arm |
| `crates/pathfinder/src/server/tools/navigation.rs` | `resolve_lsp_dependencies` | Add `LspError::Timeout` to the degraded match arm |
| `crates/pathfinder/src/server/tools/navigation.rs` | `analyze_impact_impl` | Add `LspError::Timeout` to the grep-fallback match arm |

## Current Code

### 1. get_definition_impl (lines ~345-395)

The LSP error handling has three arms:
- `Ok(Some(def))` → success
- `Ok(None)` → retry + grep fallback (works correctly)
- `Err(LspError::NoLspAvailable)` → grep fallback (works correctly)
- `Err(e)` → hard error, NO fallback ← **BUG: Timeout lands here**

```rust
// Line ~345 in navigation.rs — the problematic match
Err(LspError::NoLspAvailable) => {
    // Degraded mode — LSP not available. Use a grep-based heuristic...
    if let Some(mut def) = self.fallback_definition_grep(&semantic_path).await {
        def.degraded_reason = Some("no_lsp_grep_fallback: ...".to_owned());
        return Ok(Json(def));
    }
    Err(pathfinder_to_error_data(&PathfinderError::NoLspAvailable { ... }))
}
Err(e) => {
    // THIS IS WHERE Timeout LANDS — no fallback!
    tracing::warn!(...);
    Err(pathfinder_to_error_data(&PathfinderError::LspError {
        message: e.to_string(),
    }))
}
```

### 2. resolve_lsp_dependencies (lines ~50-128)

Used by `read_with_deep_context`:
```rust
// Line ~120 in navigation.rs
Err(LspError::NoLspAvailable | LspError::UnsupportedCapability { .. }) => {}
Err(e) => {
    // Timeout lands here — logged and swallowed, stays degraded
    tracing::warn!(...);
}
```

This one is partially OK — the function already returns degraded=true by default,
so a Timeout here degrades gracefully BUT without attempting grep fallback for
dependency resolution. Since there's no grep equivalent for call hierarchy
outgoing deps, this is acceptable. The function degrades correctly for deep context.

### 3. analyze_impact_impl (lines ~1050-1060)

```rust
// Line ~1053 in navigation.rs
Err(LspError::NoLspAvailable | LspError::UnsupportedCapability { .. }) => {
    // Grep-based reference search fallback...
    let search_result = self.scout.search(&SearchParams { ... }).await;
    ...
}
Err(e) => {
    // Timeout lands here — no grep fallback!
    tracing::warn!(...);
}
```

## Target Code

### 1. get_definition_impl — change the error match

Replace the two-branch error handling with a combined fallback:

```rust
// BEFORE (two separate arms):
Err(LspError::NoLspAvailable) => {
    // grep fallback...
}
Err(e) => {
    // hard error...
}

// AFTER (three arms, Timeout joins the fallback):
Err(LspError::NoLspAvailable) => {
    // Existing grep fallback (unchanged)...
}
Err(LspError::Timeout { .. }) => {
    // NEW: Timeout also triggers grep fallback
    tracing::info!(
        tool = "get_definition",
        semantic_path = %params.semantic_path,
        "get_definition: LSP timed out — attempting grep-based fallback"
    );

    if let Some(mut def) = self.fallback_definition_grep(&semantic_path).await {
        def.degraded_reason = Some(
            "lsp_timeout_grep_fallback: LSP timed out; result from Ripgrep pattern search — \
             may not be the canonical definition. Verify with read_source_file."
                .to_owned(),
        );
        return Ok(Json(def));
    }

    Err(pathfinder_to_error_data(&PathfinderError::LspError {
        message: "LSP timed out and grep fallback found no match".to_owned(),
    }))
}
Err(e) => {
    // Keep existing behavior for other errors (ConnectionLost, Protocol, Io)
    tracing::warn!(...);
    Err(pathfinder_to_error_data(&PathfinderError::LspError {
        message: e.to_string(),
    }))
}
```

### 2. analyze_impact_impl — add Timeout to fallback arm

```rust
// BEFORE:
Err(LspError::NoLspAvailable | LspError::UnsupportedCapability { .. }) => {
    // grep fallback...
}

// AFTER:
Err(LspError::NoLspAvailable | LspError::UnsupportedCapability { .. } | LspError::Timeout { .. }) => {
    // grep fallback — now also triggered on LSP timeout
    ...
}
```

Update the degraded_reason inside the block to distinguish:
```rust
degraded_reason = Some(
    if matches!(e, Some(LspError::Timeout { .. })) {
        "lsp_timeout_grep_fallback"
    } else {
        "no_lsp_grep_fallback"
    }.to_owned()
);
```

Note: Since the match arm doesn't bind the error, use the existing context:
```rust
Err(LspError::NoLspAvailable | LspError::UnsupportedCapability { .. }) => {
    // existing code, degraded_reason stays "no_lsp_grep_fallback"
}
Err(LspError::Timeout { .. }) => {
    // NEW: same grep search but with "lsp_timeout_grep_fallback" reason
    tracing::info!(...);
    // Copy the same grep search logic...
    degraded_reason = Some("lsp_timeout_grep_fallback".to_owned());
}
```

### 3. resolve_lsp_dependencies — already degrades correctly

No change needed. The function initializes `degraded = true` and `degraded_reason = Some("no_lsp")`.
When `call_hierarchy_prepare` times out, it hits the generic `Err(e)` arm which logs
the warning but leaves degraded=true. This is correct — there's no grep fallback for
outgoing call dependencies. The function returns "DEGRADED MODE (no_lsp)" which is honest.

## Exclusions

- Do NOT change the LSP client timeout values (10s/30s). Those are appropriate.
- Do NOT add retry logic for Timeout errors in navigation tools. The LSP client already
  has retry logic at the process level (3 restart attempts). Tool-level retries add
  latency without reliability benefit.
- Do NOT change the validation module. Validation already handles Timeout via
  `lsp_error_to_skip_reason("lsp_timeout")` which is correct.

## Verification

```bash
# 1. Run existing tests — must still pass
cargo test -p pathfinder --lib -- navigation

# 2. New tests to add (see below)
cargo test -p pathfinder --lib -- test_get_definition_timeout_triggers_grep_fallback
cargo test -p pathfinder --lib -- test_analyze_impact_timeout_triggers_grep_fallback
```

## Tests

### Test 1: get_definition_timeout_triggers_grep_fallback

Add to `crates/pathfinder/src/server/tools/navigation.rs` tests module:

```rust
#[tokio::test]
async fn test_get_definition_timeout_triggers_grep_fallback() {
    // Setup: MockLawyer that returns Timeout for goto_definition
    // + MockScout that returns a match for the symbol name
    // Verify: result is Ok with degraded=true and degraded_reason contains "lsp_timeout_grep_fallback"
}
```

### Test 2: analyze_impact_timeout_triggers_grep_fallback

```rust
#[tokio::test]
async fn test_analyze_impact_timeout_triggers_grep_fallback() {
    // Setup: MockLawyer that returns Timeout for call_hierarchy_prepare
    // + MockScout that returns matches for the symbol name
    // Verify: result is degraded with "lsp_timeout_grep_fallback" reason
    // and incoming references contain the grep matches
}
```
