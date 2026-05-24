# Epic 4: Workflow Composition

**Priority**: P2
**Theme**: Enable composed multi-tool workflows that reduce ceremony
**Specs**: 4
**Estimated effort**: 3-4 days

---

## Problem Statement

Common agent workflows require 5+ sequential tool calls:

1. **Impact analysis**: get_repo_map -> read_symbol_scope -> search_codebase for each caller -> read_symbol_scope for each
2. **Symbol discovery**: get_repo_map -> search_codebase -> manually build semantic_path -> get_definition
3. **Everything about X**: read_symbol_scope + analyze_impact + search_codebase (3 separate calls)

`find_symbol` (already shipped) and `read_files` (already shipped) reduce some ceremony.
This epic adds the remaining compositions.

---

## Spec 4.1: Add `symbol_overview` composite tool

### Problem
Agents frequently need "everything about a symbol" — its source, callers, callees, type
signature, and where it's tested. This currently requires 3-5 separate tool calls that
return different data shapes the agent must mentally merge.

### Root Cause
No composite tool exists that combines read + impact + search in one call.

### Files
- `crates/pathfinder/src/server.rs` — new tool registration
- `crates/pathfinder/src/server/tools/navigation.rs` — implementation
- `crates/pathfinder/src/server/types.rs` — response types

### Changes

1. Add `symbol_overview` tool:

```rust
#[tool(
    name = "symbol_overview",
    description = "Get comprehensive information about a symbol in one call: source code, 
    callers, callees, references, and test coverage. Combines read_symbol_scope + 
    find_callers_callees + find_all_references into a single response. 
    Use for initial analysis before refactoring.
    IMPORTANT: semantic_path MUST include file path + '::'."
)]
```

2. Implementation calls existing tools internally:
   - `surgeon.read_symbol_scope()` for source code
   - `analyze_impact_impl()` for callers/callees (with depth=2, budget=20)
   - `find_all_references_impl()` for references (with max=20)
   - Returns combined result

3. Response type:

```rust
pub struct SymbolOverviewResponse {
    pub source: Option<SymbolSource>,
    pub impact: Option<ImpactSummary>,
    pub references: Option<Vec<ReferenceLocation>>,
    pub degraded: bool,
    pub degraded_reason: Option<DegradedReason>,
    pub lsp_readiness: Option<String>,
}
```

4. Text output format:
```
SYMBOL: file.rs::function_name (20 lines)
CALLERS: 3 direct (file_a.rs::caller1, file_b.rs::caller2, ...)
CALLEES: 5 (format!, parse, validate, save, notify)
REFERENCES: 12 total across 4 files
DEGRADED: no (LSP-backed, authoritative)
```

### Test Plan
- Call `symbol_overview` on a well-known function
- Verify response includes source + callers + callees + references
- Call during LSP warmup -> verify degraded response with partial results
- Verify performance is bounded (no unbounded BFS or reference enumeration)

### Acceptance Criteria
- Single call returns source + impact + references
- Degraded mode returns source + search_codebase results (no callers/callees)
- Total response is bounded (max 20 refs, max depth 2)
- Performance under 10s for typical symbols

---

## Spec 4.2: Add `analyze_impact_and_test_coverage` workflow helper

### Problem
Agents doing TDD need to know both who calls a function AND what tests cover it.
Currently this is a manual multi-step process.

### Root Cause
No test-awareness in any tool. `find_all_references` can find test references but
agents must filter manually.

### Files
- `crates/pathfinder/src/server/types.rs` — add `tests_only` filter to existing types
- `crates/pathfinder/src/server/tools/navigation.rs` — use SymbolKind::Test when available

### Changes

1. Add `include_test_coverage: bool` parameter to `find_callers_callees`:
   - When true, also run `find_all_references` filtered to test files
   - Return test references in a separate `test_callers` field

2. In `AnalyzeImpactMetadata`, add:
```rust
pub test_callers: Option<Vec<ImpactReference>>,
pub test_coverage_status: String, // "found" | "not_found" | "unknown_degraded"
```

3. Test file detection heuristic:
   - Rust: files ending in `_test.rs` or containing `mod tests`
   - Go: files ending in `_test.go`
   - Python: files starting with `test_` or containing test functions
   - TypeScript: files ending in `.test.ts` or `.spec.ts`

4. When test_callers are found, text output includes:
```
TEST COVERAGE: 3 test functions cover this symbol
  - src/auth_test.rs::test_login_valid
  - src/auth_test.rs::test_login_invalid
  - tests/integration.rs::test_auth_flow
```

### Test Plan
- Call `find_callers_callees` with `include_test_coverage=true` on a tested function
- Verify test_callers are returned in separate field
- Call on an untested function -> verify "no test coverage found"
- Call during degraded mode -> verify "test coverage unknown"

### Acceptance Criteria
- `include_test_coverage` parameter returns test-specific callers
- Test files detected by language-specific heuristics
- Degraded mode honestly reports "unknown" for test coverage
- No performance regression when `include_test_coverage=false` (default)

---

## Spec 4.3: Add `search_codebase` prefer_definition hint

### Problem
`enclosing_semantic_path` sometimes points to the wrong ancestor node (e.g., test module
instead of the actual function). This happens because tree-sitter picks the innermost
enclosing scope, which may be misleading.

### Root Cause
Tree-sitter's `enclosing_symbol` finds the deepest ancestor, not the most semantically
relevant one. For definitions, the function/struct/class node is more useful than the
module/test block that contains it.

### Files
- `crates/pathfinder/src/server/tools/search.rs` — enrichment logic
- `crates/pathfinder/src/server/types.rs` — add `is_definition: bool` to search matches

### Changes

1. Add `is_definition: Option<bool>` to search match response type:
   - True when the match is at a definition position (fn, struct, class, etc.)
   - Computed by checking if the match position coincides with a symbol definition line

2. In `enrich_matches`, also compute `is_definition`:
```rust
let is_definition = symbol.as_ref().map_or(false, |sym| {
    sym.start_line == match_line
});
```

3. When `is_definition == true`, prefer the symbol's own semantic_path over the
   enclosing scope's path. This gives "file.rs::MyClass" instead of "file.rs::tests"
   for a definition inside a test module.

### Test Plan
- Search for a function name that has both a definition and a call site
- Verify the definition match has `is_definition: true`
- Verify the call site match has `is_definition: false`
- Verify `enclosing_semantic_path` points to the function for definition matches

### Acceptance Criteria
- `is_definition` field appears on search matches
- Definition matches have `is_definition: true`
- `enclosing_semantic_path` accuracy improves for definition matches

---

## Spec 4.4: Add pagination support to find_all_references

### Problem
`find_all_references` can return hundreds of references. There's no pagination mechanism.
If the result is large, agents get truncated output with no way to fetch more.

### Root Cause
No `offset` or `cursor` parameter exists. The tool returns all references at once or
truncates silently.

### Files
- `crates/pathfinder/src/server/types.rs` — `FindAllReferencesParams`, `FindAllReferencesMetadata`
- `crates/pathfinder/src/server/tools/navigation.rs` — implementation

### Changes

1. Add to `FindAllReferencesParams`:
```rust
pub max_results: Option<u32>,  // default 50
pub offset: Option<u32>,       // default 0
```

2. Add to `FindAllReferencesMetadata`:
```rust
pub total_references: usize,    // total found (may exceed returned count)
pub returned_count: usize,      // references actually returned
pub truncated: bool,            // true when total > returned
```

3. In implementation, apply offset + limit to reference list before returning.

### Test Plan
- Call with `max_results=5` on a widely-referenced symbol -> verify 5 returned + truncated=true
- Call with `offset=5, max_results=5` -> verify next page
- Verify default behavior unchanged (returns all, no truncation for reasonable counts)

### Acceptance Criteria
- Pagination parameters work correctly
- `truncated` flag indicates when more results exist
- Default behavior unchanged for existing callers

---

## Execution Order

```
Spec 4.3 (search is_definition hint) -> 2 hours
Spec 4.4 (find_all_references pagination) -> 2 hours
Spec 4.1 (symbol_overview composite tool) -> 6 hours
Spec 4.2 (test coverage workflow) -> 4 hours
```

Total: ~14 hours across 3-4 sessions
