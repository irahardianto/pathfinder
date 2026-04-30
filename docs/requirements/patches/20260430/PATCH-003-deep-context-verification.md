# PATCH-003: Add Verification Probe to resolve_lsp_dependencies

## Group: A (Critical) — Column-1 Root Cause Fix
## Depends on: PATCH-001, PATCH-002

## Objective

Fix the "dishonest degraded" bug in `resolve_lsp_dependencies`. When `call_hierarchy_prepare`
returns `Ok([])` (empty), the current code sets `degraded = false` without verifying that the
LSP is actually warm. This causes `read_with_deep_context` to return 0 dependencies with
`degraded: false`, leading agents to believe the symbol truly has no callees when in fact
the LSP just didn't resolve it.

## Severity: HIGH — agents make wrong refactoring decisions based on false data

## Background

`analyze_impact` already has a verification probe: when `call_hierarchy_prepare` returns
empty, it calls `goto_definition` at the same position. If `goto_definition` succeeds,
the LSP is warm and zero callers is genuine. If it fails, the result is marked degraded.

`resolve_lsp_dependencies` lacks this probe. It trusts `Ok([])` as confirmed-zero.

After PATCH-002, the column positioning will be correct, so the probe will work reliably.

## Scope

| # | File | Line(s) | Action |
|---|------|---------|--------|
| 1 | `crates/pathfinder/src/server/tools/navigation.rs` | ~80-85 | Add verification probe after empty call_hierarchy_prepare |

## Step 1: Add verification probe

**File:** `crates/pathfinder/src/server/tools/navigation.rs`

**Find:**
```rust
            Ok(_) => {
                engines.push("lsp");
                degraded = false;
                degraded_reason = None;
            }
```

This is inside `resolve_lsp_dependencies`, in the match on `lsp_result`.

**Replace with:**
```rust
            Ok(_) => {
                // Empty call hierarchy — verify LSP is actually warm.
                // Mirror the probe logic from analyze_impact_impl: if goto_definition
                // can resolve the symbol, the LSP is indexed and zero deps is genuine.
                // If goto_definition also returns None, the LSP is still warming up
                // and the empty result is unreliable.
                let probe = self
                    .lawyer
                    .goto_definition(
                        self.workspace_root.path(),
                        &semantic_path.file_path,
                        u32::try_from(start_line + 1).unwrap_or(1),
                        u32::try_from(name_column + 1).unwrap_or(1),
                    )
                    .await;

                if matches!(probe, Ok(Some(_))) {
                    // LSP is warm — definition resolved → confirmed zero dependencies
                    engines.push("lsp");
                    degraded = false;
                    degraded_reason = None;
                } else {
                    // LSP returned empty but can't resolve the symbol → warmup or bad position
                    engines.push("lsp");
                    degraded = true;
                    degraded_reason = Some("lsp_warmup_empty_unverified".to_owned());
                }
            }
```

Note: This requires `name_column` parameter which was added in PATCH-002.

## EXCLUSIONS — Do NOT Modify These

- `analyze_impact_impl` — already has the probe, no change needed
- Any other match arms in `resolve_lsp_dependencies`
- The `Ok(items) if !items.is_empty()` arm — that works correctly

## Verification

```bash
# 1. Confirm the probe exists in resolve_lsp_dependencies
grep -A5 'Ok(_) =>' crates/pathfinder/src/server/tools/navigation.rs | head -20

# Expected: should show the probe logic (goto_definition call)

# 2. Run existing tests
cargo test -p pathfinder read_with_deep_context

# 3. Full verification
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

## Expected Impact

When LSP is cold:
- Before: `degraded: false, dependencies: []` — agent trusts this as "no callees"
- After: `degraded: true, degraded_reason: "lsp_warmup_empty_unverified", dependencies: []` — agent knows data is unreliable

When LSP is warm and symbol genuinely has no callees:
- Before: `degraded: false, dependencies: []` — correct
- After: `degraded: false, dependencies: []` — still correct (probe confirms LSP is warm)
