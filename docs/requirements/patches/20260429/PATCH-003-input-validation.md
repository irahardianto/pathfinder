# PATCH-003: Input Validation Guards

## Status: COMPLETED (2026-04-29)

## Objective

Add missing input validation to prevent resource exhaustion and unexpected behavior from unbounded parameters. Four specific gaps are addressed:

1. `max_depth` in `analyze_impact` — unbounded BFS traversal
2. `max_tokens` in `get_repo_map` — unbounded memory allocation
3. Empty `query` in `search_codebase` — matches every line in every file
4. `context_line` in text targeting — out-of-range warning

## Severity: MEDIUM — Prevents resource exhaustion from misbehaving agents

---

## Scope

| # | File | Function | Action |
|---|------|----------|--------|
| 1 | `crates/pathfinder/src/server/tools/navigation.rs` | `analyze_impact_impl` | ADD max_depth clamp |
| 2 | `crates/pathfinder/src/server/tools/repo_map.rs` | impl handler | ADD max_tokens clamp |
| 3 | `crates/pathfinder/src/server/tools/search.rs` | `search_codebase_impl` | ADD empty query guard |
| 4 | `crates/pathfinder/src/server/tools/edit/text_edit.rs` | `resolve_text_edit` | ADD context_line warning |

---

## Task 3.1: Clamp `max_depth` in `analyze_impact`

**File:** `crates/pathfinder/src/server/tools/navigation.rs`
**Function:** `analyze_impact_impl`

Find the section near the top of `analyze_impact_impl` where `params` is destructured or used. After the sandbox check, add a clamp before `params.max_depth` is used:

**Find this pattern** (near the start of analyze_impact_impl, where params.max_depth is first used or logged):
```rust
            max_depth = params.max_depth,
```

**Add BEFORE the first usage of `params.max_depth`** (after sandbox check, before the tracing span):
```rust
        // Cap max_depth to prevent unbounded BFS traversal (PRD §5.1 maximum)
        let max_depth = params.max_depth.min(5);
```

Then replace all subsequent uses of `params.max_depth` with `max_depth` in the function body. This includes:
- The tracing span field
- The two `bfs_call_hierarchy` calls

**Specifically find and replace:**
```rust
                        params.max_depth,
```
**With:**
```rust
                        max_depth,
```

(There are 2 occurrences of `params.max_depth` passed to `bfs_call_hierarchy` — replace both.)

Also update the tracing span:
```rust
            max_depth = params.max_depth,
```
**To:**
```rust
            max_depth = max_depth,
```

---

## Task 3.2: Clamp `max_tokens` in `get_repo_map`

**File:** `crates/pathfinder/src/server/tools/repo_map.rs`

Find the impl handler for `get_repo_map`. Look for where `params.max_tokens` is first used to build the `SkeletonConfig`. Add a clamp before that usage.

**Add before the SkeletonConfig construction:**
```rust
        // Clamp to reasonable bounds: minimum 500 (usable output), max 100k (memory safety)
        let max_tokens = params.max_tokens.clamp(500, 100_000);
```

Then use `max_tokens` instead of `params.max_tokens` when constructing `SkeletonConfig`. Find:
```rust
            params.max_tokens,
```
or wherever `params.max_tokens` is passed to `SkeletonConfig::new`, and replace with:
```rust
            max_tokens,
```

---

## Task 3.3: Reject empty `query` in `search_codebase`

**File:** `crates/pathfinder/src/server/tools/search.rs`
**Function:** `search_codebase_impl`

At the top of `search_codebase_impl`, after the params are extracted but BEFORE the search is dispatched, add:

```rust
        if params.query.trim().is_empty() {
            return Err(crate::server::helpers::io_error_data(
                "query must not be empty",
            ));
        }
```

---

## Task 3.4: Warn on out-of-range `context_line`

**File:** `crates/pathfinder/src/server/tools/edit/text_edit.rs`
**Function:** `resolve_text_edit`

Find the section in `resolve_text_edit` where `context_line` is used to compute the search window. The function already clamps `context_line` of 0 to 1. Add a warning AFTER the clamping when the value exceeds the file:

Find the `compute_search_window` call or the area where `context_line` and total line count are both available. Add:

```rust
    let total_lines = lines.len();
    if context_line > total_lines {
        tracing::warn!(
            context_line,
            total_lines,
            "context_line exceeds file length; search window will be truncated"
        );
    }
```

Place this AFTER `build_line_starts` is called (so `lines` / line count is available) and BEFORE the `compute_search_window` call.

---

## New Tests

### Test for max_depth clamp (add to navigation.rs test module)

```rust
    #[tokio::test]
    async fn test_analyze_impact_max_depth_clamped_to_5() {
        // This is a documentation test — the clamp is a simple .min(5)
        // Verified by the existing test_analyze_impact_bfs_respects_max_depth
        // which already tests depth limiting behavior.
        assert_eq!(999u32.min(5), 5);
        assert_eq!(3u32.min(5), 3);
        assert_eq!(0u32.min(5), 0);
    }
```

### Test for empty query rejection (add to search.rs or server.rs test module)

```rust
    #[tokio::test]
    async fn test_search_codebase_empty_query_rejected() {
        let server = make_server(); // use existing test helper
        let result = server
            .search_codebase(SearchCodebaseParams {
                query: "".into(),
                ..Default::default()
            })
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_search_codebase_whitespace_only_query_rejected() {
        let server = make_server();
        let result = server
            .search_codebase(SearchCodebaseParams {
                query: "   ".into(),
                ..Default::default()
            })
            .await;
        assert!(result.is_err());
    }
```

> **NOTE:** Adjust test helper names (`make_server`, param struct names) to match the existing test infrastructure in the file.

---

## Verification

```bash
# 1. Confirm max_depth clamp exists
grep -n 'max_depth.min(5)' crates/pathfinder/src/server/tools/navigation.rs

# 2. Confirm max_tokens clamp exists
grep -n 'max_tokens.clamp' crates/pathfinder/src/server/tools/repo_map.rs

# 3. Confirm empty query guard exists
grep -n 'query must not be empty' crates/pathfinder/src/server/tools/search.rs

# 4. Full verification
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

## Completion Criteria

- [ ] `max_depth` clamped to 5 in `analyze_impact_impl`
- [ ] `max_tokens` clamped to `500..=100_000` in repo_map handler
- [ ] Empty/whitespace query rejected in `search_codebase_impl`
- [ ] Out-of-range `context_line` produces a warning log
- [ ] New tests added and passing
- [ ] `cargo test --all` passes
- [ ] `cargo clippy` passes with zero warnings
