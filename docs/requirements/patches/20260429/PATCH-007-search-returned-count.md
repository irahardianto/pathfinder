# PATCH-007: Search Response `returned_count` Field

## Status: COMPLETED (2026-04-29)

## Objective

`SearchCodebaseResponse.total_matches` reflects the ripgrep match count **before** `filter_mode` filtering. After filtering, `matches.len()` can be significantly smaller. An agent that sees `total_matches: 50` but only 3 items in `matches` cannot tell whether results were truncated (ripgrep hit `max_results`) or filtered out (filter_mode removed them). This creates a false truncation signal.

Adding a `returned_count` field makes the distinction explicit:
- `total_matches` — raw ripgrep count before filtering and truncation
- `returned_count` — count of matches returned after filtering
- `truncated` — true when `total_matches == max_results` (ripgrep hit its cap)

## Severity: LOW — Misleading metadata in search responses

---

## Scope

| # | File | Change |
|---|------|--------|
| 1 | `crates/pathfinder/src/server/types.rs` | ADD `returned_count` field to `SearchCodebaseResponse` |
| 2 | `crates/pathfinder/src/server/tools/search.rs` | SET `returned_count = flat_matches.len()` before construction |

---

## Task 7.1: Add `returned_count` to `SearchCodebaseResponse`

**File:** `crates/pathfinder/src/server/types.rs`

Find `SearchCodebaseResponse`:

```rust
pub struct SearchCodebaseResponse {
    /// List of search matches.
    pub matches: Vec<pathfinder_search::SearchMatch>,
    /// Total number of matches found.
    pub total_matches: usize,
    /// Indicates if the match list was truncated.
    pub truncated: bool,
```

**Replace with:**

```rust
pub struct SearchCodebaseResponse {
    /// List of search matches.
    pub matches: Vec<pathfinder_search::SearchMatch>,
    /// Raw match count from ripgrep **before** `filter_mode` filtering or truncation.
    ///
    /// When `filter_mode` is `"comments_only"` or `"code_only"`, matches that do not
    /// pass the filter are excluded from `matches` but still counted here.
    /// Compare with `returned_count` to understand how many matches were filtered.
    pub total_matches: usize,
    /// Number of matches actually returned in this response (after filter_mode filtering).
    ///
    /// `returned_count == matches.len()`. Provided as a convenience field so agents
    /// do not need to count `matches` themselves.
    pub returned_count: usize,
    /// Indicates if the match list was truncated by `max_results`.
    ///
    /// When `true`, ripgrep reached `max_results` and stopped searching. Some files
    /// may not have been searched at all. Increase `max_results` to retrieve more.
    pub truncated: bool,
```

---

## Task 7.2: Set `returned_count` in the tool handler

**File:** `crates/pathfinder/src/server/tools/search.rs`
**Function:** `search_codebase_impl`

Find the line:
```rust
                let returned_count = flat_matches.len();
```

This variable already exists. Now find the `SearchCodebaseResponse` construction block (around line 113):

```rust
                Ok(Json(SearchCodebaseResponse {
                    matches: flat_matches,
                    total_matches: result.total_matches,
                    truncated: result.truncated,
                    file_groups,
                    degraded,
                    degraded_reason,
                }))
```

**Replace with:**

```rust
                Ok(Json(SearchCodebaseResponse {
                    matches: flat_matches,
                    total_matches: result.total_matches,
                    returned_count,
                    truncated: result.truncated,
                    file_groups,
                    degraded,
                    degraded_reason,
                }))
```

> **NOTE:** `returned_count` is already computed as `let returned_count = flat_matches.len();` a few lines above. No new computation needed.

---

## New Tests

Add to the search tests in `crates/pathfinder/src/server/tools/search.rs` or the server test module:

```rust
    #[tokio::test]
    async fn test_search_returned_count_matches_response_length() {
        // Verify returned_count == matches.len() always
        let response = /* call search_codebase with known query */;
        assert_eq!(response.returned_count, response.matches.len());
    }

    #[tokio::test]
    async fn test_search_returned_count_less_than_total_when_filtered() {
        // When filter_mode=comments_only on code-only files, returned_count < total_matches
        let response = /* search with filter_mode=CommentsOnly in a code-heavy file */;
        // If any matches were filtered, returned_count should be < total_matches
        // (or equal if nothing was filtered — that's fine too)
        assert!(response.returned_count <= response.total_matches);
    }
```

---

## Verification

```bash
# 1. Confirm field added to struct
grep -n 'returned_count' crates/pathfinder/src/server/types.rs

# Expected: 2 matches (field definition + doc comment)

# 2. Confirm field set in handler
grep -n 'returned_count' crates/pathfinder/src/server/tools/search.rs

# Expected: 2 matches (existing let + new struct field)

# 3. Full verification
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

## Completion Criteria

- [ ] `SearchCodebaseResponse` has `returned_count: usize` field
- [ ] Field has doc comment explaining the distinction from `total_matches`
- [ ] `returned_count` is set to `flat_matches.len()` in `search_codebase_impl`
- [ ] New test validates `returned_count == matches.len()`
- [ ] `cargo test --all` passes
- [ ] `cargo clippy` passes with zero warnings
