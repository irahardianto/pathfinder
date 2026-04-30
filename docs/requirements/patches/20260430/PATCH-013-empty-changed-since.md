# PATCH-013: Structured Empty Result for changed_since

## Group: D (Low) — Agent Self-Adaptation

## Objective

When `get_repo_map` with `changed_since` finds no changed files, return structured data
instead of a bare "tool call completed" with no data. Agents need to distinguish between
"no changes" and "something went wrong".

## Severity: LOW — agents get confused by empty responses, but it doesn't break workflows

## Scope

| # | File | Action |
|---|------|--------|
| 1 | `crates/pathfinder/src/server/tools/repo_map.rs` | Ensure empty changed_files produces a valid skeleton with metadata |
| 2 | `crates/pathfinder-treesitter/src/repo_map.rs` | Handle empty changed_files set gracefully |

## Step 1: Verify and fix the empty changed_since path

**File:** `crates/pathfinder/src/server/tools/repo_map.rs`

The `changed_files` is set via `Ok(files)` from `get_changed_files_since`. When git
returns no changes, `files` is an empty `HashSet`. This is passed to `SkeletonConfig`
via `.with_changed_files(Some(empty_set))`.

In the repo_map generator, the `changed_files` filter currently works as:
```rust
if let Some(changed) = &config.changed_files {
    // Only include files in the changed set
```

When the set is empty, ALL files are excluded, resulting in zero skeleton output.

The fix: when `changed_files` is `Some(empty_set)`, still produce a valid response
with `files_scanned: 0, coverage_percent: 100, changed_files: 0`.

**File:** `crates/pathfinder/src/server/tools/repo_map.rs`

In `get_repo_map_impl`, after computing `changed_files`, add an early return for the
empty case:

**Find:**
```rust
        if !params.changed_since.is_empty() {
            match pathfinder_common::git::get_changed_files_since(
                &pathfinder_common::git::SystemGit,
                self.workspace_root.path(),
                &params.changed_since,
            )
            .await
            {
                Ok(files) => changed_files = Some(files),
```

**Replace with:**
```rust
        if !params.changed_since.is_empty() {
            match pathfinder_common::git::get_changed_files_since(
                &pathfinder_common::git::SystemGit,
                self.workspace_root.path(),
                &params.changed_since,
            )
            .await
            {
                Ok(files) => {
                    if files.is_empty() {
                        // No changes found — return a structured empty result
                        // rather than a skeleton with zero content
                        let metadata = crate::server::types::GetRepoMapMetadata {
                            tech_stack: vec![],
                            files_scanned: 0,
                            files_truncated: 0,
                            files_in_scope: 0,
                            coverage_percent: 100,
                            version_hashes: std::collections::HashMap::new(),
                            visibility_degraded: None,
                            degraded: false,
                            degraded_reason: None,
                            capabilities: RepoCapabilities {
                                edit: true,
                                search: true,
                                lsp: LspCapabilities {
                                    supported: true,
                                    per_language: self.lawyer.capability_status().await,
                                },
                            },
                        };
                        let mut res = CallToolResult::success(vec![rmcp::model::Content::text(
                            "No files changed since the specified ref. No skeleton generated.",
                        )]);
                        res.structured_content = serialize_metadata(&metadata);
                        return Ok(res);
                    }
                    changed_files = Some(files);
                }
```

## Verification

```bash
# 1. Build
cargo build --all

# 2. Run repo_map tests
cargo test -p pathfinder repo_map

# 3. Full verification
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```
