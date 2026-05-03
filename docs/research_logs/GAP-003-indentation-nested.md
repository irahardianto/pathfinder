# Research Log: GAP-003 - Fix dedent_then_reindent for Nested Blocks

## Date
2026-05-03

## Objective
Fix the over-indentation bug in `replace_body` when the agent provides code with nested structures (if-else, match arms) and inconsistent leading indentation.

## Root Cause
When an agent's `new_code` has lines at different indent levels with `min_indent = 0`, lines that were already at the body indent level get doubled when `reindent` adds `target_column`.

Example:
- Agent provides code with some lines at 0, some at 4 spaces
- `dedent` computes `min_indent = 0` → no-op
- `reindent(target_column=4)` adds 4 to EVERY line
- Lines already at 4 become 8 → over-indented

## Solution Approach
Add a `normalize_for_body_replace` preprocessing step to anchor the code at column 0:
1. Find the first non-empty line's indent
2. Dedent ALL lines by that amount
3. This preserves relative indentation while anchoring at column 0

## Implementation Plan
1. Add `anchor_to_column_zero()` helper function
2. Add `dedent_by()` helper function
3. Update `normalize_for_body_replace()` to call `anchor_to_column_zero()`
4. Add tests for:
   - Nested if-else indentation
   - Relative indent preservation
   - Already-at-column-zero case (no-op)

## Files to Modify
- `crates/pathfinder-common/src/normalize.rs` - Add helper functions and update normalization
- `crates/pathfinder-common/src/indent.rs` - No changes (existing dedent/reindent are correct)

## Dependencies
None - self-contained normalization change

## Completion Criteria
- All tests pass
- Nested block indentation is correct
- Relative indentation is preserved
- Already-at-column-zero code is handled correctly
