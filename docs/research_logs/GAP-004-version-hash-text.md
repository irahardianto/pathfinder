# Research Log: GAP-004 - Append version_hash to Text Output of Read Tools

## Date
2026-05-03

## Objective
Append `version_hash` to the text output of `read_source_file` and `read_symbol_scope` tools to match the pattern used by `read_file`.

## Problem
The `version_hash` is present in `structured_content` (JSON metadata) but not in the text output. This forces agents to:
1. Make an additional `read_file` call to get the hash
2. Risk VERSION_MISMATCH errors when guessing hashes
3. Experience confusion when `insert_into` fails due to wrong hashes

## Solution Approach
Follow the pattern already used by `read_file`:
```rust
let full_content = format!("{}\n---\nversion_hash: {}", content, version_hash.short());
```

## Implementation Plan
1. Update `read_source_file_impl()` to append version_hash to text output
2. Update `read_symbol_scope_impl()` to append version_hash to text output
3. Add tests to verify version_hash is in text output

## Files to Modify
- `crates/pathfinder/src/server/tools/source_file.rs` - Update `read_source_file_impl()`
- `crates/pathfinder/src/server/tools/symbols.rs` - Update `read_symbol_scope_impl()`

## Dependencies
None - self-contained output format change

## Completion Criteria
- All tests pass
- version_hash appears in text output with format "---\nversion_hash: <hash>"
- structured_content still contains version_hash (no regression)
- Tests verify hash is present in text output
