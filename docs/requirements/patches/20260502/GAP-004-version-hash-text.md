# GAP-004: Append version_hash to Text Output of Read Tools

## Group: B (High) — Silent Correctness Bugs
## Depends on: Nothing

## Objective

Both reports identified that `read_source_file` and `read_symbol_scope` do not surface
`version_hash` in their text output. The hash IS present in `structured_content` (JSON
metadata), but agents consuming the text response cannot extract it. This forces agents
to make an additional `read_file` call just to get the hash needed for subsequent edits.

This creates:
1. Extra tool calls and latency
2. Risk of VERSION_MISMATCH errors when agents guess wrong hashes
3. Confusion when `insert_into` fails because the agent used the wrong hash from
   a previous `read_source_file` call

The fix: append `version_hash: <hash>` at the end of text output, matching the
pattern already used by `read_file`.

## Scope

| File | Function | Change |
|------|----------|--------|
| `crates/pathfinder/src/server/tools/source_file.rs` | `read_source_file_impl` | Append version_hash to text output |
| `crates/pathfinder/src/server/tools/symbols.rs` | `read_symbol_scope_impl` | Append version_hash to text output |

## Current Code

### read_source_file_impl

```rust
let mut contents = Vec::new();
if let Some(text) = final_content {
    contents.push(Content::text(text));
}
let mut result = CallToolResult::success(contents);
result.structured_content = serialize_metadata(&metadata);
```

### read_symbol_scope_impl

```rust
let mut result = CallToolResult::success(vec![Content::text(scope.content)]);
result.structured_content = serialize_metadata(&metadata);
```

### read_file (the model to follow)

```rust
// In read_file_impl, the hash is appended to the text:
let text = format!("{content}\n---\nversion_hash: {}", version_hash.short());
```

## Target Code

### read_source_file_impl

```rust
let mut contents = Vec::new();
if let Some(text) = final_content {
    let with_hash = format!("{text}\n---\nversion_hash: {}", metadata.version_hash);
    contents.push(Content::text(with_hash));
}
```

### read_symbol_scope_impl

```rust
let text_with_hash = format!(
    "{}\n---\nversion_hash: {}",
    scope.content,
    scope.version_hash.short()
);
let mut result = CallToolResult::success(vec![Content::text(text_with_hash)]);
```

## Design Notes

1. The `---\nversion_hash: <hash>` format matches `read_file` exactly, so agents
   already have parsing logic for this.

2. The hash is appended AFTER the content, not before, so it doesn't affect symbol
   rendering or line numbers.

3. For `read_source_file` with `detail_level=symbols`, the tree text gets the hash
   appended. This is fine — the hash is clearly separated by `---`.

4. The `structured_content` continues to include the hash as before — this change is
   purely additive.

## Exclusions

- Do NOT remove version_hash from `structured_content` — that would break agents
  consuming structured content.
- Do NOT change `read_file` — it already has the correct format.
- Do NOT add version_hash to error responses — that's a separate concern (GAP-008).

## Verification

```bash
cargo test -p pathfinder --lib -- test_read_source_file
cargo test -p pathfinder --lib -- test_read_symbol_scope
```

After fix, verify manually:
```
# Call read_source_file and check text output ends with "---\nversion_hash: <hash>"
# Call read_symbol_scope and check text output ends with "---\nversion_hash: <hash>"
```

## Tests

### Test 1: test_read_source_file_includes_version_hash_in_text
```rust
// Call read_source_file_impl
// Parse the text output
// Verify it contains "---\nversion_hash: " followed by a 7-char hash
```

### Test 2: test_read_symbol_scope_includes_version_hash_in_text
```rust
// Call read_symbol_scope_impl
// Parse the text output
// Verify it contains "---\nversion_hash: " followed by a 7-char hash
```
