# PATCH-007: Add version_hash to delete_file Response

## Group: B (High) — Search & Response Fixes

## Objective

Add a `version_hash` field to the `DeleteFileResponse` so agents can chain delete operations
with other file operations consistently. Currently `create_file` returns `version_hash` but
`delete_file` does not, creating an asymmetric API.

## Severity: LOW — minor inconsistency, but agents expect uniform OCC fields

## Scope

| # | File | Action |
|---|------|--------|
| 1 | `crates/pathfinder/src/server/types.rs` | Add `version_hash` to `DeleteFileResponse` |
| 2 | `crates/pathfinder/src/server/tools/file_ops.rs` | Populate `version_hash` in `delete_file_impl` |

## Step 1: Update DeleteFileResponse

**File:** `crates/pathfinder/src/server/types.rs`

**Find:**
```rust
/// The response for `delete_file`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct DeleteFileResponse {
    /// Whether the file deletion succeeded.
    pub success: bool,
}
```

**Replace with:**
```rust
/// The response for `delete_file`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct DeleteFileResponse {
    /// Whether the file deletion succeeded.
    pub success: bool,
    /// Short hash of the file version that was deleted (for audit/OCC chain).
    pub version_hash: String,
}
```

## Step 2: Populate version_hash in delete_file_impl

**File:** `crates/pathfinder/src/server/tools/file_ops.rs`

The `current_hash` is already computed during OCC check. Use it in the response.

**Find (near the end of `delete_file_impl`, before the return):**
```rust
        Ok(Json(DeleteFileResponse { success: true }))
```

**Replace with:**
```rust
        Ok(Json(DeleteFileResponse {
            success: true,
            version_hash: current_hash.short().to_owned(),
        }))
```

## Verification

```bash
# 1. Confirm version_hash field exists
grep -n 'version_hash' crates/pathfinder/src/server/types.rs | grep Delete

# 2. Confirm it's populated
grep -n 'version_hash' crates/pathfinder/src/server/tools/file_ops.rs | grep delete

# 3. Build and test
cargo test -p pathfinder file_ops
cargo test --all
```
