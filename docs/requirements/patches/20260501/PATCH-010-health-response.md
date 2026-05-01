# PATCH-010: Enrich lsp_health with Diagnostics Strategy Info

## Group: E (Polish) — Response Quality
## Depends on: PATCH-001, PATCH-005

## Objective

Ensure the `lsp_health` response clearly communicates the diagnostics strategy
for each language and what tools are affected. When strategy is "push", include
estimated validation latency. When strategy is "none", include which tools are
degraded. This makes `lsp_health` the single source of truth for agent planning.

## Severity: LOW — polish, not a fix

## Scope

| # | File | Change | Description |
|---|------|--------|-------------|
| 1 | `crates/pathfinder/src/server/types.rs` | Add affected_tools field to LspLanguageHealth | List which tools are degraded |
| 2 | `crates/pathfinder/src/server/tools/navigation.rs` | Populate affected_tools | Compute from capabilities |

## Step 1: Add Affected Tools

**File:** `crates/pathfinder/src/server/types.rs`

```rust
pub struct LspLanguageHealth {
    // ... existing fields ...

    /// Tools that are degraded (using fallback) for this language.
    /// Empty when LSP is fully operational.
    /// Example: ["validate_only", "edit_validation"] when diagnostics unsupported
    pub degraded_tools: Vec<String>,

    /// Approximate validation latency in milliseconds for this language.
    /// None when unknown or not applicable.
    pub validation_latency_ms: Option<u64>,
}
```

## Step 2: Compute Degraded Tools

**File:** `crates/pathfinder/src/server/tools/navigation.rs`

```rust
let mut degraded_tools = Vec::new();

if status.supports_call_hierarchy != Some(true) {
    degraded_tools.push("analyze_impact".to_owned());
    degraded_tools.push("read_with_deep_context".to_owned());
}
if status.supports_diagnostics != Some(true) && status.diagnostics_strategy.as_deref() != Some("push") {
    if status.diagnostics_strategy.as_deref() == Some("pull") {
        // Pull works, not degraded
    } else {
        degraded_tools.push("validate_only".to_owned());
    }
}

let validation_latency_ms = match status.diagnostics_strategy.as_deref() {
    Some("push") => Some(10_000), // ~10s for push collection (5s pre + 5s post)
    Some("pull") => Some(2_000),  // ~2s for pull request
    _ => None,
};
```

## Step 3: Tests

- `test_health_shows_degraded_tools_for_no_diagnostics` — LSP without diagnostics
  -> degraded_tools includes "validate_only"
- `test_health_shows_empty_degraded_when_fully_capable` — all capabilities supported
  -> degraded_tools is empty
- `test_health_shows_push_latency` — push diagnostics language
  -> validation_latency_ms is ~10000

## Verification

```bash
cargo build --all
cargo test --all
grep -n "degraded_tools\|validation_latency" crates/pathfinder/src/server/types.rs
```

## Expected Impact

- lsp_health response tells agents exactly which tools are degraded per language
- Agents can plan fallback strategies (e.g., use search_codebase instead of analyze_impact)
- Validation latency estimate helps agents decide whether to validate
