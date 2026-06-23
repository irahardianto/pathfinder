# PATCH-006: find_symbol Did-You-Mean

Date: 2026-06-22
Source: Bank-of-Anthos report (Java/Python stack) — "Consider Adding 'Did You Mean' to Symbol Search"
Priority: P1 — ergonomic friction; agents get 0 results with no recovery path
Status: Spec — awaiting implementation
Depends on: PATCH-002 (shared did_you_mean cross-file helper)

## Problem Statement

`search(mode="symbol")` returns 0 results with no suggestions when the
symbol name doesn't match exactly. The `locate` tool has `compute_did_you_mean`
(`definition.rs:258`) and `enrich_did_you_mean` (`mod.rs:544`) that provide
fuzzy suggestions, but `find_symbol` does not call either.

v1 documented a fallback pattern in SKILL.md ("use search(mode='text')
for broader search") but did not add the feature. Agents must manually
fall back to text search with no guidance on what the correct symbol
name might be.

### Root Cause

File: `crates/pathfinder/src/server/tools/find_symbol.rs`

`find_symbol_impl` runs ripgrep + tree-sitter enrichment, then returns
`FindSymbolResponse`:

```rust
pub struct FindSymbolResponse {
    pub symbols: Vec<FoundSymbol>,
    pub total_found: u32,
    pub search_strategy: String,
    pub duration_ms: Option<u64>,
}
```

When `symbols` is empty, there is no `did_you_mean` field, no hint, no
suggestion. The agent gets:
```json
{
  "symbols": [],
  "total_found": 0,
  "search_strategy": "ripgrep+treesitter",
  "duration_ms": 5
}
```

Meanwhile, `locate` (definition.rs:258) calls `compute_did_you_mean` which
returns suggestions like:
```
Hint: Symbol not found in the specified file.
Use search(mode="symbol", query="NonExistentFunction") to locate...
Did you mean: MockScout.search, RipgrepScout.search?
```

And `enrich_did_you_mean` (`mod.rs:544-603`) does cross-file search when
same-file suggestions are empty.

### Agent Impact

Agents searching for a symbol with a slightly wrong name (typo, wrong
scope, partial name) get 0 results and must:
1. Guess alternative names
2. Fall back to `search(mode="text")` with the bare name
3. Manually scan results to find the correct semantic path

This wastes multiple round-trips. The `locate` tool already has the
machinery to suggest alternatives — `find_symbol` just doesn't use it.

---

## DELIVERABLE A: Add `did_you_mean` Field to `FindSymbolResponse`

Priority: P1
Effort: Low (20 minutes)
Risk: Low (additive field)

**Steps**:

1. In `crates/pathfinder/src/server/types.rs`, add to
   `FindSymbolResponse`:

   ```rust
   /// Suggested alternative symbol paths when `symbols` is empty.
   ///
   /// Populated using fuzzy matching and cross-file search when the
   /// query matches no exact symbol. Each entry is a full semantic path
   /// (`file::symbol`) that can be used directly with `inspect` or
   /// `trace`.
   ///
   /// Absent when `symbols` is non-empty (exact match found).
   /// Absent when no suggestions could be found.
   #[serde(skip_serializing_if = "Option::is_none")]
   pub did_you_mean: Option<Vec<String>>,
   ```

2. Initialize `did_you_mean: None` in existing `find_symbol_impl` return
   paths where symbols are found.

**Files to modify**:
- `crates/pathfinder/src/server/types.rs` — add field

**Acceptance**:
- `FindSymbolResponse` has `did_you_mean: Option<Vec<String>>`
- Field is `None` (absent from JSON) when symbols are found
- Field is `None` when no suggestions available
- Field is `Some(vec![...])` when suggestions are found

---

## DELIVERABLE B: Extract `enrich_did_you_mean` into Shared Helper

Priority: P1
Effort: Medium (1 hour)
Risk: Low (refactor, no behavior change)

**Problem**: `enrich_did_you_mean` (`mod.rs:544-603`) is a method on
`PathfinderServer` in the navigation module. `find_symbol.rs` is in a
different module (`tools/`). The helper needs to be accessible from both.

**Steps**:

1. In `crates/pathfinder/src/server/tools/navigation/mod.rs`, make
   `enrich_did_you_mean` `pub(crate)` instead of private:

   Before:
   ```rust
   async fn enrich_did_you_mean(
   ```

   After:
   ```rust
   pub(crate) async fn enrich_did_you_mean(
   ```

2. If the method relies on navigation-module-specific types, extract the
   core logic into a standalone function that accepts generic parameters.
   Alternatively, move the method to a shared module
   (`crate::server::helpers` or a new `crate::server::tools::shared`).

3. Verify that `compute_did_you_mean` in
   `crates/pathfinder-treesitter/src/symbols.rs` is already `pub` (it
   is used by the surgeon). If not, make it `pub`.

4. Ensure the cross-file search logic (the `find_symbol_impl` call inside
   `enrich_did_you_mean`) works when called from `find_symbol.rs`. Watch
   for re-entrancy: `enrich_did_you_mean` calls `find_symbol_impl`, which
   would now also call `enrich_did_you_mean` if it returns empty results.
   Add a guard to prevent infinite recursion:

   ```rust
   // In find_symbol_impl, only call did_you_mean on the FIRST search,
   // not on the recursive cross-file search inside enrich_did_you_mean.
   // Pass a flag or use a separate internal function.
   ```

   Recommended approach: split `find_symbol_impl` into:
   - `find_symbol_impl` (public entry point, calls did_you_mean on miss)
   - `find_symbol_impl_inner` (internal, no did_you_mean — used by
     `enrich_did_you_mean` for cross-file search)

**Files to modify**:
- `crates/pathfinder/src/server/tools/navigation/mod.rs` — visibility
- `crates/pathfinder/src/server/tools/find_symbol.rs` — split into
  public + internal functions

**Acceptance**:
- `enrich_did_you_mean` is callable from `find_symbol.rs`
- No infinite recursion (enrich → find_symbol → enrich → ...)
- Existing navigation did_you_mean behavior unchanged

---

## DELIVERABLE C: Call did_you_mean from find_symbol_impl

Priority: P1
Effort: Low (30 minutes)
Risk: Low

**Steps**:

1. In `crates/pathfinder/src/server/tools/find_symbol.rs`, modify
   `find_symbol_impl` to call `enrich_did_you_mean` when `symbols` is
   empty:

   ```rust
   // After search results are collected:
   if result.symbols.is_empty() && !params.query.is_empty() {
       // Try did_you_mean suggestions
       let suggestions = self
           .compute_symbol_did_you_mean(&params.query, &params.path_glob)
           .await;

       if !suggestions.is_empty() {
           return Ok(Json(SearchCodebaseResponse {
               symbols: vec![],
               total_found: 0,
               search_strategy: result.search_strategy,
               duration_ms: Some(start.elapsed().as_millis() as u64),
               did_you_mean: Some(suggestions),
           }));
       }
   }

   // Normal return (with or without results)
   Ok(Json(SearchCodebaseResponse {
       ...
       did_you_mean: None,
   }))
   ```

2. Implement `compute_symbol_did_you_mean` — a variant of
   `enrich_did_you_mean` that works for the search use case:

   ```rust
   /// Compute did-you-mean suggestions for a failed symbol search.
   /// Uses tree-sitter fuzzy matching on the query, then cross-file
   /// search if no same-file matches.
   async fn compute_symbol_did_you_mean(
       &self,
       query: &str,
       path_glob: &str,
   ) -> Vec<String> {
       // 1. Try fuzzy matching against all known symbols in the workspace
       //    Use tree-sitter symbol index if available, or search with
       //    a broader query

       // 2. Levenshtein distance matching (similar to compute_did_you_mean
       //    in symbols.rs but across the whole workspace, not one file)

       // 3. If fuzzy matching finds nothing, try prefix/substring matching:
       //    search(mode="text", query=query) and collect enclosing_symbol_paths

       // Return top 5 suggestions as semantic paths
   }
   ```

   Note: The cross-file search in `enrich_did_you_mean` (mod.rs:567-597)
   already does a `find_symbol_impl` call by bare name. Reuse that logic
   but with the query from the failed search.

3. Add a `hint` when did_you_mean is populated but symbols is empty:

   ```rust
   hint: Some("No exact symbol match found. Check did_you_mean for \
               suggested alternative paths.".to_string()),
   ```

   Note: `FindSymbolResponse` doesn't currently have a `hint` field. Add
   one:
   ```rust
   #[serde(skip_serializing_if = "Option::is_none")]
   pub hint: Option<String>,
   ```

**Files to modify**:
- `crates/pathfinder/src/server/tools/find_symbol.rs` — call did_you_mean
- `crates/pathfinder/src/server/types.rs` — add `hint` to `FindSymbolResponse`

**Acceptance**:
- `search(mode="symbol", query="Scout")` when `Scout` doesn't exist but
  `ScoutService` does → `did_you_mean: ["...::ScoutService"]`
- `search(mode="symbol", query="seach")` (typo) when `search` exists →
  `did_you_mean: ["...::search"]`
- `search(mode="symbol", query="exact_match")` when found →
  `did_you_mean: None`
- `hint` populated when `did_you_mean` is non-empty

---

## DELIVERABLE D: Tests

Priority: P1
Effort: Low (30 minutes)
Risk: None

**Steps**:

Add tests to `crates/pathfinder/src/server/tools/find_symbol_test.rs`:

1. `test_find_symbol_exact_match_no_did_you_mean`
   - Search for an existing symbol
   - Assert: `symbols` non-empty, `did_you_mean` is `None`

2. `test_find_symbol_typo_returns_did_you_mean`
   - Search for `seach` (typo of `search`)
   - Assert: `symbols` empty, `did_you_mean` is `Some(vec![...])`
   - Assert: `did_you_mean` contains `search` semantic path
   - Assert: `hint` is `Some(...)`

3. `test_find_symbol_no_match_no_suggestions`
   - Search for a completely non-existent symbol
   - Assert: `symbols` empty, `did_you_mean` is `None` (no suggestions
     found)
   - Assert: `hint` is `None`

4. `test_find_symbol_partial_name_match`
   - Search for `Scout` when `ScoutService` and `ScoutManager` exist
   - Assert: `did_you_mean` contains `ScoutService` and `ScoutManager`
     semantic paths

5. `test_find_symbol_no_recursion`
   - Verify that did_you_mean cross-file search doesn't trigger
     recursive did_you_mean calls
   - This is more of an implementation verification — check that
     `find_symbol_impl_inner` doesn't call `compute_symbol_did_you_mean`

**Files to modify**:
- `crates/pathfinder/src/server/tools/find_symbol_test.rs`

**Acceptance**:
- All 5 tests pass
- Tests cover: exact match, typo, no match, partial match, no recursion

---

## Dependency Order

```
PATCH-002 (trait did_you_mean improvement) → B (extract shared helper)
A (add field) → C (call did_you_mean) → D (tests)
B (extract helper) → C (call did_you_mean)
```

PATCH-002 improves `enrich_did_you_mean` for trait methods. This patch
extracts it as a shared helper and calls it from `find_symbol`. Doing
PATCH-002 first ensures the cross-file fallback is already improved.

A and B are independent — can be done in parallel.
C depends on both A and B.
D depends on C.

## Verification Plan

```bash
cargo test -p pathfinder find_symbol
cargo clippy -- -D warnings
```

Manual verification:
- `search(mode="symbol", query="NonExistent")` on any project
- Verify response has `did_you_mean` field with suggestions (or None if
  truly no matches)
- `search(mode="symbol", query="health")` (partial name)
- Verify `did_you_mean` includes `lsp_health_impl`, `compute_degraded_tools`,
  etc. (symbols containing "health")
- `search(mode="symbol", query="search")` (exact match exists)
- Verify `symbols` is non-empty and `did_you_mean` is None

Total effort: ~1.5 hours
