# 008: Call-Site Aware Grep for `analyze_impact`

**Epic**: 3 — Richer Grep Fallbacks
**Status**: ✅ Complete (2026-05-12)
**Severity**: Medium
**Risk**: Medium — adds tree-sitter enrichment to grep fallback path
**Depends on**: 001 (grep_reference_fallback extracted)

---

## Problem

The current `grep_reference_fallback` (extracted in spec 001) performs a plain text search for the symbol name across source files. This produces false positives from:

1. **Comments**: `// TODO: refactor my_function` matches `my_function`
2. **String literals**: `log("calling my_function")` matches `my_function`
3. **Type annotations**: `fn foo(bar: MyStruct)` matches `MyStruct` — this is a reference, but not a call site
4. **Import statements**: `use crate::my_function;` — valid reference but not a call site

The existing `search_codebase` infrastructure already solves this problem through tree-sitter node classification (`filter_mode=code_only`), but `grep_reference_fallback` bypasses it by calling `scout.search()` directly.

---

## Proposed Solution

Enhance `grep_reference_fallback` to leverage the tree-sitter enrichment pipeline:

### Option A: Use `search_codebase_impl` Internally (Preferred)

Instead of calling `scout.search()` directly, call the full `search_codebase_impl` with `filter_mode=CodeOnly`:

```rust
async fn grep_reference_fallback(
    &self,
    symbol_name: &str,
    definition_path: &str,
    files_referenced: &mut HashSet<String>,
) -> Option<Vec<ImpactReference>> {
    let params = SearchCodebaseParams {
        query: symbol_name.to_owned(),
        filter_mode: FilterMode::CodeOnly,
        max_results: 20,
        path_glob: "**/*".to_owned(),
        // ... other fields
    };

    let result = self.search_codebase_impl(params).await.ok()?;
    // ... map result.matches to ImpactReference, filter definition file
}
```

This reuses the existing tree-sitter enrichment, `enclosing_semantic_path` resolution, and known-file deduplication.

### Option B: Inline Tree-sitter Classification

Call `scout.search()` then `surgeon.classify_node_at_position()` for each match. More control but duplicates logic from `search_codebase_impl`.

**Recommendation**: Option A — composition over duplication.

### Files to Modify

| File | Change |
|------|--------|
| `crates/pathfinder/src/server/tools/navigation.rs` | Update `grep_reference_fallback` to use `search_codebase_impl` with `filter_mode=CodeOnly` |

---

## Acceptance Criteria

- [ ] Grep fallback results exclude matches in comments
- [ ] Grep fallback results exclude matches in string literals
- [ ] Code-only matches are preserved (function calls, type references)
- [ ] Definition file still excluded
- [ ] Result cap still 10
- [ ] `enclosing_semantic_path` populated for each result (from tree-sitter enrichment)
- [ ] No performance regression: enrichment must complete within 2s for typical workspaces
- [ ] Falls back to raw grep if tree-sitter enrichment fails (resilient)

---

## Test Plan

| Test | Description |
|------|-------------|
| `test_grep_fallback_excludes_comment_matches` | Symbol in comment only → filtered out |
| `test_grep_fallback_excludes_string_matches` | Symbol in string literal only → filtered out |
| `test_grep_fallback_keeps_code_matches` | Symbol in function call → included |
| `test_grep_fallback_includes_semantic_path` | Result has `enclosing_semantic_path` set |

---

## Verification

```bash
cargo test -p pathfinder-mcp -- grep_fallback
cargo clippy -p pathfinder-mcp -- -D warnings
```

---

## Impact on Agents

Before: Agent receives 10 "references" including comments, strings, imports → must manually filter.
After: Agent receives 10 code-only references with semantic paths → can directly navigate to callers.
