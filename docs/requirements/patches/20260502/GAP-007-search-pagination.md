# GAP-007: Add Offset Pagination to search_codebase

## Group: D (Low) — Agent Quality of Life
## Depends on: Nothing

## Objective

Both reports identified that `search_codebase` silently drops matches beyond `max_results`.
The response includes `truncated: true` and `total_matches: 11` vs `returned_count: 6`,
but there's no way to retrieve the remaining 5 matches. Agents must reformulate their
query or guess at more specific patterns.

Add an `offset` parameter that allows agents to paginate through results.

## Scope

| File | Struct/Function | Change |
|------|----------------|--------|
| `crates/pathfinder-search/src/types.rs` | `SearchParams` | Add `offset: usize` field |
| `crates/pathfinder-search/src/ripgrep.rs` | `MatchCollector` | Skip first `offset` matches |
| `crates/pathfinder/src/server/tools/search.rs` | handler | Pass offset from params |
| `crates/pathfinder/src/server/tools/types.rs` | `SearchParams` schema | Add offset field |

## Current Code

```rust
// SearchParams in pathfinder-search/src/types.rs
pub struct SearchParams {
    pub workspace_root: PathBuf,
    pub query: String,
    pub is_regex: bool,
    pub max_results: usize,
    pub path_glob: String,
    pub exclude_glob: String,
    pub context_lines: usize,
}
```

```rust
// MatchCollector::new
fn new(
    matcher: RegexMatcher,
    matches: &'a Mutex<Vec<SearchMatch>>,
    total_count: &'a Mutex<usize>,
    max_results: usize,
    context_lines: usize,
) -> Self { ... }
```

```rust
// MatchCollector::matched — truncation logic
fn matched(&mut self, ...) -> Result<bool> {
    // ... increment total_count ...
    let current = self.current_match_count();
    if current >= self.max_results {
        self.truncated = true;
        return Ok(false); // ← drops the match
    }
    // ... store the match ...
}
```

## Target Code

### 1. Add offset to SearchParams

```rust
pub struct SearchParams {
    pub workspace_root: PathBuf,
    pub query: String,
    pub is_regex: bool,
    pub max_results: usize,
    pub offset: usize,        // NEW: skip first N matches
    pub path_glob: String,
    pub exclude_glob: String,
    pub context_lines: usize,
}
```

### 2. Add offset to MatchCollector

```rust
struct MatchCollector<'a> {
    // ... existing fields ...
    offset: usize,            // NEW
    skipped: usize,           // NEW: count of matches skipped due to offset
}

impl<'a> MatchCollector<'a> {
    fn new(..., offset: usize, ...) -> Self {
        Self {
            // ... existing ...
            offset,
            skipped: 0,
        }
    }
}
```

### 3. Modify matched() to skip offset matches

```rust
fn matched(&mut self, ...) -> Result<bool> {
    // ... increment total_count ...
    *self.total_count.lock().unwrap() += 1;

    let current = self.current_match_count();

    // Skip matches before the offset
    if self.skipped < self.offset {
        self.skipped += 1;
        return Ok(true); // Continue searching, but don't store
    }

    if current >= self.max_results {
        self.truncated = true;
        return Ok(false);
    }
    // ... store the match ...
}
```

### 4. Default offset to 0

In the MCP tool handler, default `offset` to 0 when not provided:

```rust
let offset = params.offset.unwrap_or(0);
```

### 5. Add hint to truncated response

When results are truncated, include the offset to use for the next page:

```rust
let next_offset = if result.truncated {
    Some(offset + result.matches.len())
} else {
    None
};
```

Include `next_offset` in the response metadata.

## Exclusions

- Do NOT add cursor-based pagination (stateful) — offset-based is simpler and stateless.
- Do NOT change the `SearchResult` struct significantly — just add `offset` to metadata.
- Do NOT add infinite scroll — agents should use `max_results` + `offset` explicitly.

## Verification

```bash
cargo test -p pathfinder-search -- test_search_offset_pagination
cargo test -p pathfinder-search -- test_search_offset_beyond_results
```

## Tests

### Test 1: test_search_offset_pagination
```rust
// 10 files, each with "needle", max_results=3
// offset=0 → matches 1-3
// offset=3 → matches 4-6
// offset=6 → matches 7-9
// offset=9 → match 10
```

### Test 2: test_search_offset_beyond_results
```rust
// 5 matches total, offset=10 → empty results, total_matches=5
```

### Test 3: test_search_offset_with_truncation_hint
```rust
// 20 matches, max_results=5, offset=0 → truncated=true, next_offset=5
```
