# PATCH-002: Use name_column in All Navigation LSP Calls

## Group: A (Critical) — Column-1 Root Cause Fix
## Depends on: PATCH-001

## Objective

Replace the hardcoded `1` (column 1) in all LSP navigation calls with the `name_column`
from `SymbolScope`. This is the fix that actually unblocks `get_definition`,
`analyze_impact`, and `read_with_deep_context`.

## Severity: CRITICAL — direct fix for the Column-1 Bug

## Scope

| # | File | Line(s) | Function | Current | Target |
|---|------|---------|----------|---------|--------|
| 1 | `crates/pathfinder/src/server/tools/navigation.rs` | ~208 | `get_definition_impl` | `, 1,` | `, u32::try_from(symbol_scope.name_column + 1).unwrap_or(1),` |
| 2 | `crates/pathfinder/src/server/tools/navigation.rs` | ~257 | `get_definition_impl` (retry) | `, 1,` | `, u32::try_from(symbol_scope.name_column + 1).unwrap_or(1),` |
| 3 | `crates/pathfinder/src/server/tools/navigation.rs` | ~62 | `resolve_lsp_dependencies` | `, 1,` | `, u32::try_from(name_column + 1).unwrap_or(1),` |
| 4 | `crates/pathfinder/src/server/tools/navigation.rs` | ~861 | `analyze_impact_impl` | `, 1, // Column 1` | `, u32::try_from(scope.name_column + 1).unwrap_or(1),` |
| 5 | `crates/pathfinder/src/server/tools/navigation.rs` | ~912 | `analyze_impact_impl` (probe) | `, 1,` | `, u32::try_from(scope.name_column + 1).unwrap_or(1),` |

Note: `name_column` is 0-indexed (from tree-sitter). LSP expects 1-indexed column.
Hence `name_column + 1`.

## Step 1: Fix get_definition_impl

**File:** `crates/pathfinder/src/server/tools/navigation.rs`

**Find (first goto_definition call, ~line 208):**
```rust
        let lsp_result = self
            .lawyer
            .goto_definition(
                self.workspace_root.path(),
                &semantic_path.file_path,
                // Convert 0-indexed start_line from SymbolScope to 1-indexed for Lawyer
                u32::try_from(symbol_scope.start_line + 1).unwrap_or(1),
                1, // Column 1 — start of the identifier line
            )
            .await;
```

**Replace with:**
```rust
        let lsp_result = self
            .lawyer
            .goto_definition(
                self.workspace_root.path(),
                &semantic_path.file_path,
                // Convert 0-indexed start_line from SymbolScope to 1-indexed for Lawyer
                u32::try_from(symbol_scope.start_line + 1).unwrap_or(1),
                // Position cursor on the symbol's name identifier (e.g., the 'd' in 'dedent'),
                // not the 'pub' keyword. rust-analyzer requires this for symbol resolution.
                u32::try_from(symbol_scope.name_column + 1).unwrap_or(1),
            )
            .await;
```

**Find (retry goto_definition call, ~line 257):**
```rust
                let retry_lsp_result = self
                    .lawyer
                    .goto_definition(
                        self.workspace_root.path(),
                        &semantic_path.file_path,
                        u32::try_from(symbol_scope.start_line + 1).unwrap_or(1),
                        1,
                    )
                    .await;
```

**Replace with:**
```rust
                let retry_lsp_result = self
                    .lawyer
                    .goto_definition(
                        self.workspace_root.path(),
                        &semantic_path.file_path,
                        u32::try_from(symbol_scope.start_line + 1).unwrap_or(1),
                        u32::try_from(symbol_scope.name_column + 1).unwrap_or(1),
                    )
                    .await;
```

## Step 2: Fix analyze_impact_impl

**Find (call_hierarchy_prepare, ~line 861):**
```rust
        let lsp_result = self
            .lawyer
            .call_hierarchy_prepare(
                self.workspace_root.path(),
                &semantic_path.file_path,
                u32::try_from(scope.start_line + 1).unwrap_or(1),
                1, // Column 1
            )
            .await;
```

**Replace with:**
```rust
        let lsp_result = self
            .lawyer
            .call_hierarchy_prepare(
                self.workspace_root.path(),
                &semantic_path.file_path,
                u32::try_from(scope.start_line + 1).unwrap_or(1),
                u32::try_from(scope.name_column + 1).unwrap_or(1),
            )
            .await;
```

**Find (verification probe, ~line 912):**
```rust
                let probe = self
                    .lawyer
                    .goto_definition(
                        self.workspace_root.path(),
                        &semantic_path.file_path,
                        u32::try_from(scope.start_line + 1).unwrap_or(1),
                        1,
                    )
                    .await;
```

**Replace with:**
```rust
                let probe = self
                    .lawyer
                    .goto_definition(
                        self.workspace_root.path(),
                        &semantic_path.file_path,
                        u32::try_from(scope.start_line + 1).unwrap_or(1),
                        u32::try_from(scope.name_column + 1).unwrap_or(1),
                    )
                    .await;
```

## Step 3: Fix resolve_lsp_dependencies

**Find (~line 62):**
```rust
        let lsp_result = self
            .lawyer
            .call_hierarchy_prepare(
                self.workspace_root.path(),
                &semantic_path.file_path,
                u32::try_from(start_line + 1).unwrap_or(1),
                1,
            )
            .await;
```

This function receives `start_line` as a parameter. It also needs `name_column`.
Update the function signature to accept `name_column: usize` and use it.

**Find the function signature (resolve_lsp_dependencies):**
```rust
    async fn resolve_lsp_dependencies(
```

Add `name_column: usize` parameter after `start_line`. Then replace the column:

**Replace column 1:**
```rust
        let lsp_result = self
            .lawyer
            .call_hierarchy_prepare(
                self.workspace_root.path(),
                &semantic_path.file_path,
                u32::try_from(start_line + 1).unwrap_or(1),
                u32::try_from(name_column + 1).unwrap_or(1),
            )
            .await;
```

Also update ALL call sites of `resolve_lsp_dependencies` to pass `name_column`.
Search for `resolve_lsp_dependencies` calls and add `symbol_scope.name_column` (or
the appropriate `name_column` value from the scope).

## Step 4: Update test assertions

The existing mock tests in `navigation.rs` construct `SymbolScope` with hardcoded fields.
Add `name_column: 7` (or appropriate value for the test symbol) to all test `SymbolScope`
constructions. Search for `SymbolScope {` in the test module and add the field.

## EXCLUSIONS — Do NOT Modify These

- `fallback_definition_grep` and related functions — those don't use LSP column positioning
- Any file outside `navigation.rs` for this patch (surgeon/type changes were PATCH-001)
- The verification probe in `resolve_lsp_dependencies` — that's PATCH-003

## Verification

```bash
# 1. Confirm no remaining column-1 hardcoded in navigation LSP calls
grep -n ', 1,$\|, 1)' crates/pathfinder/src/server/tools/navigation.rs | grep -v 'test\|assert\|mock\|Ok(Some\|// '

# Expected: ZERO results for navigation LSP call sites (only test code should remain)

# 2. Confirm name_column is used in all call_hierarchy_prepare and goto_definition calls
grep -n 'name_column' crates/pathfinder/src/server/tools/navigation.rs

# Expected: at least 5 results (the 5 call sites listed above)

# 3. Build and test
cargo build --all
cargo test --all
```

## Expected Impact

With correct column positioning:
- `get_definition`: LSP resolves symbol -> returns definition location (not SYMBOL_NOT_FOUND)
- `analyze_impact`: LSP returns call hierarchy items -> BFS traversal works (not degraded 0/0)
- `read_with_deep_context`: LSP returns dependencies -> deep context is delivered (not 0 deps)
