# PATCH-004: Fix search_codebase group_by_file Serialization Bug

## Group: B (High) — Search & Response Fixes

## Objective

Fix the confirmed data-loss bug where `search_codebase` with `group_by_file=true` returns
`total_matches: 6` but `matches: []` and `file_groups: []`. The matches exist in the
pipeline but are lost during serialization. This breaks a key token-saving feature.

## Severity: HIGH — silent data loss, agents get incomplete results

## Background

The `build_file_groups` function in `search.rs` groups matches by file. The agent report
found that when `group_by_file=true` + `exclude_glob` are used together, the response
contains `total_matches > 0` but empty `file_groups` and `matches`. The flat `matches`
list is also empty because `group_by_file` consumes the `filtered_matches` via ownership
but the grouped output doesn't serialize correctly.

Root cause analysis: The `flat_matches` list is built from `filtered_matches` AFTER
`build_file_groups` has already consumed it. But `build_file_groups` takes `&[SearchMatch]`
(borrow), so `filtered_matches` should still be available. The actual bug is likely in
the `group_by_file` branch: when `group_by_file` is true, the code builds `file_groups`
AND `flat_matches` from the same data, but the `flat_matches` processing also strips
content for known files. If there's an interaction with `known_files` + `group_by_file`,
the `SearchResultGroup` schema requires either `matches` or `known_matches` to be
non-empty for each group, but the serialization with `#[serde(skip_serializing_if = "Vec::is_empty")]`
causes groups with all-known-files to have both arrays empty, resulting in empty-looking
groups.

## Scope

| # | File | Action |
|---|------|--------|
| 1 | `crates/pathfinder/src/server/tools/search.rs` | Fix `build_file_groups` to handle known_files correctly |
| 2 | `crates/pathfinder/src/server/types.rs` | Remove `skip_serializing_if` on group match arrays |

## Step 1: Investigate and fix the group serialization

**File:** `crates/pathfinder/src/server/types.rs`

The `SearchResultGroup` has `#[serde(skip_serializing_if = "Vec::is_empty")]` on both
`matches` and `known_matches`. When ALL matches in a group are for known files, the
`matches` vec is empty and gets skipped. And if there's a bug in the known_matches
population, both arrays end up empty, and the entire group serializes as just
`{ file, version_hash }` — which looks empty to the agent.

**Find:**
```rust
pub struct SearchResultGroup {
    /// File path relative to workspace root.
    pub file: String,
    /// SHA-256 hash of the file (shared by all matches in this group).
    pub version_hash: String,
    /// Full matches (for files NOT in `known_files`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<GroupedMatch>,
    /// Minimal matches (for files in `known_files`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub known_matches: Vec<GroupedKnownMatch>,
}
```

**Replace with:**
```rust
pub struct SearchResultGroup {
    /// File path relative to workspace root.
    pub file: String,
    /// SHA-256 hash of the file (shared by all matches in this group).
    pub version_hash: String,
    /// Total number of matches in this group (both full and known).
    /// Provided so agents can quickly assess match density without counting sub-arrays.
    pub total_matches: usize,
    /// Full matches (for files NOT in `known_files`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<GroupedMatch>,
    /// Minimal matches (for files in `known_files`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub known_matches: Vec<GroupedKnownMatch>,
}
```

## Step 2: Populate total_matches in build_file_groups

**File:** `crates/pathfinder/src/server/tools/search.rs`

**Find in `build_file_groups`:**
```rust
                SearchResultGroup {
                    file: m.file.clone(),
                    version_hash: m.version_hash.clone(),
                    matches: Vec::new(),
                    known_matches: Vec::new(),
                },
```

**Replace with:**
```rust
                SearchResultGroup {
                    file: m.file.clone(),
                    version_hash: m.version_hash.clone(),
                    total_matches: 0,
                    matches: Vec::new(),
                    known_matches: Vec::new(),
                },
```

Then, at the end of `build_file_groups`, before returning, set `total_matches` for each group:

After the loop, before the final `order.into_iter().filter_map(...)`:

**Find:**
```rust
    order
        .into_iter()
        .filter_map(|k| groups.remove(&k))
        .collect()
```

**Replace with:**
```rust
    // Set total_matches for each group before returning
    for group in groups.values_mut() {
        group.total_matches = group.matches.len() + group.known_matches.len();
    }

    order
        .into_iter()
        .filter_map(|k| groups.remove(&k))
        .collect()
```

## Step 3: Add regression test

**File:** `crates/pathfinder/src/server/tools/search.rs`

Add a test inside the `mod tests` block:

```rust
    #[tokio::test]
    async fn test_search_group_by_file_with_known_files() {
        let ws_dir = tempfile::tempdir().unwrap();
        let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(ws_dir.path().join("src/main.rs"), "fn findme() {}\nfn other() { findme(); }\n").unwrap();

        let scout = Arc::new(RipgrepScout);
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon.enclosing_symbol_results.lock().unwrap().push(Ok(None));
        surgeon.node_type_at_position_results.lock().unwrap().push(Ok("code".to_string()));
        surgeon.node_type_at_position_results.lock().unwrap().push(Ok("code".to_string()));
        let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);

        let server = PathfinderServer::new(ws, scout, surgeon, lawyer, sandbox);

        let params = SearchCodebaseParams {
            query: "findme".to_owned(),
            group_by_file: true,
            known_files: vec!["src/main.rs".to_owned()],
            ..Default::default()
        };

        let result = server.search_codebase_impl(params).await.expect("should succeed");
        let groups = result.file_groups.expect("should have file_groups");

        // Even when all matches are in known_files, groups should not be empty
        assert!(!groups.is_empty(), "file_groups should not be empty when matches exist");
        assert!(groups[0].total_matches > 0, "total_matches should be positive");
        assert!(groups[0].known_matches.len() > 0, "known_matches should contain the suppressed matches");
    }
```

## Verification

```bash
# 1. Run the new test
cargo test -p pathfinder test_search_group_by_file_with_known_files

# 2. Run all search tests
cargo test -p pathfinder search

# 3. Full verification
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```
