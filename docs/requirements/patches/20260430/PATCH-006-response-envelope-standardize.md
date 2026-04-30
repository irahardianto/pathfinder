# PATCH-006: Standardize All Tool Response Envelopes

## Group: B (High) — Search & Response Fixes

## Objective

Standardize the response format across all 19 tools so agents can write consistent parsing
logic. Currently:
- `write_file` returns plain string `"File successfully written"` in text content
- `delete_file` returns `{ success: true }` with no version_hash
- `get_repo_map` sometimes saves to file vs. returning inline (framework behavior)
- Most edit tools return structured JSON via `structured_content`

The fix: ensure every tool returns structured JSON in `structured_content` with consistent
fields.

## Severity: MEDIUM — agents need inconsistent parsing logic

## Scope

| # | File | Tool | Current | Target |
|---|------|------|---------|--------|
| 1 | `file_ops.rs` | `write_file` | Plain string in content | Structured metadata (already has `structured_content`) |
| 2 | `file_ops.rs` | `delete_file` | `{ success: true }` no version_hash | Add `version_hash` (see PATCH-007) |

Note: `get_repo_map` "image" responses are a client-side rendering behavior, not a server
bug. The server always returns `Content::text()`. No server-side fix is possible.

## Step 1: Improve write_file text content

**File:** `crates/pathfinder/src/server/tools/file_ops.rs`

**Find (~line 517):**
```rust
        let mut res = CallToolResult::success(vec![rmcp::model::Content::text(
            "File successfully written",
        )]);
        res.structured_content = serialize_metadata(&metadata);
        Ok(res)
```

**Replace with:**
```rust
        let mut res = CallToolResult::success(vec![rmcp::model::Content::text(format!(
            "File written: {} (version {})",
            params.filepath, metadata.new_version_hash
        ))]);
        res.structured_content = serialize_metadata(&metadata);
        Ok(res)
```

This gives agents actionable information in the text content (filename + version hash)
while maintaining the structured metadata for programmatic consumption.

## EXCLUSIONS — Do NOT Modify These

- `delete_file` — handled separately in PATCH-007
- Tool descriptions/schema — those are correct, just the response values need updating
- `get_repo_map` image rendering — client-side behavior, not a server bug

## Verification

```bash
# 1. Confirm write_file text content includes version
grep -A3 'Content::text' crates/pathfinder/src/server/tools/file_ops.rs | grep 'version'

# 2. Run file_ops tests
cargo test -p pathfinder file_ops

# 3. Full verification
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```
