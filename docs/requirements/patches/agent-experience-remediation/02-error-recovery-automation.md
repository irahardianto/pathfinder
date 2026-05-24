# Epic 2: Error Recovery Automation

**Priority**: P1
**Theme**: Automate error recovery for the most common agent failure modes
**Specs**: 6
**Estimated effort**: 2-3 days

---

## Problem Statement

SYMBOL_NOT_FOUND is the most common error agents encounter. It accounts for an estimated
40%+ of all agent friction with Pathfinder. The error fires for 5 distinct root causes,
but agents get the same error for all of them. Current recovery requires the agent to:
1. Read the error
2. Decide what went wrong (typo? wrong file? wrong separator? missing symbol?)
3. Choose a recovery strategy (search_codebase? read_source_file? retry with suggestion?)
4. Execute the recovery manually

This is decision fatigue at its worst. Pathfinder should automate the common cases.

---

## The 5 SYMBOL_NOT_FOUND Root Causes

| Cause | Example | Frequency | Auto-fixable? |
|-------|---------|-----------|---------------|
| Typo in symbol name | `src/auth.rs::logn` instead of `login` | High | Yes (did_you_mean) |
| Wrong file | `src/auth.rs::login` but login is in `src/service.rs` | High | Yes (cross-file search) |
| Separator confusion | `src/auth.rs::tests::test_login` instead of `tests.test_login` | Medium | Yes (detect and suggest) |
| Bare file path | `src/main.rs` without symbol target | Medium | Yes (already addressed in Epic 1.5) |
| Impl block with lifetimes | `src/types.rs::Context::process` but tree-sitter can't merge | Low | Partial (grep fallback) |

---

## Spec 2.1: Populate did_you_mean in ALL semantic-path tools

### Problem
Only `read_symbol_scope` (via tree-sitter surgeon) and `get_definition` (via `compute_did_you_mean`)
populate did_you_mean suggestions. `read_with_deep_context`, `analyze_impact`, and
`find_all_references` return empty did_you_mean on symbol resolution failure.

### Root Cause
`compute_did_you_mean()` is only called in `get_definition_impl` (navigation.rs:959).
The other tools propagate the raw SurgeonError without enrichment.

### Files
- `crates/pathfinder/src/server/tools/navigation.rs`

### Changes

1. In `read_with_deep_context_impl`, when `surgeon.read_symbol_scope()` fails with
   SurgeonError::SymbolNotFound:
   - Extract the semantic path and call `self.compute_did_you_mean(&semantic_path).await`
   - Replace the error with enriched `did_you_mean`

2. In `analyze_impact_impl`, same pattern at the tree-sitter resolution point.

3. In `find_all_references_impl`, same pattern.

4. Each site follows the same pattern already used in `get_definition_impl`:
   ```rust
   let suggestions = self.compute_did_you_mean(&semantic_path).await;
   Err(pathfinder_to_error_data(&PathfinderError::SymbolNotFound {
       semantic_path: params.semantic_path,
       did_you_mean: suggestions,
   }))
   ```

### Test Plan
- Call `read_with_deep_context` with a close typo (e.g., "logn" instead of "login")
  -> verify did_you_mean contains "login"
- Call `analyze_impact` with same typo -> verify did_you_mean populated
- Call `find_all_references` with same typo -> verify did_you_mean populated
- Verify existing get_definition behavior unchanged

### Acceptance Criteria
- All 5 semantic-path tools return did_you_mean on SYMBOL_NOT_FOUND
- Suggestions are populated from actual file symbols (not empty)
- Graceful: if compute_did_you_mean fails, return empty vec (not an error cascade)

---

## Spec 2.2: Add separator confusion detection to did_you_mean

### Problem
Agents frequently use `::` for all nesting (e.g., `file::tests::test_login`) when the
correct format uses `.` for symbol nesting (e.g., `file::tests.test_login`). The current
`did_you_mean` doesn't detect this pattern because the Levenshtein distance is too large.

### Root Cause
`did_you_mean()` in `symbols.rs:930` computes Levenshtein distance against full semantic
paths. Replacing `::` with `.` changes multiple characters, exceeding the distance threshold.

### Files
- `crates/pathfinder-common/src/error.rs` — `hint()` method
- `crates/pathfinder-treesitter/src/symbols.rs` — `did_you_mean()` (optional enhancement)

### Changes

1. In `PathfinderError::SymbolNotFound::hint()`, add separator confusion detection:

```rust
// Detect separator confusion: :: where . should be used
let chain = semantic_path.split("::").nth(1).unwrap_or("");
if chain.contains("::") {
    // Agent used :: for symbol nesting within the chain
    let corrected = format!("{}::{}",
        semantic_path.split("::").next().unwrap_or(""),
        chain.replace("::", ".")
    );
    return Some(format!(
        "Symbol chain uses wrong separator. Use '.' for nested symbols, not '::'. \
         Try: {corrected}{}",
        separator_hint.unwrap_or("")
    ));
}
```

2. Additionally, in `compute_did_you_mean()`, try the corrected path:
   - If `chain.contains("::")`, also search for the `.` variant
   - If found, include it in did_you_mean suggestions

### Test Plan
- Call `read_symbol_scope("src/test.rs::tests::test_login")` -> verify hint says "use '.' for nested symbols"
- Verify suggested correction is `src/test.rs::tests.test_login`
- Call with correct `.` separator -> verify no false positive detection

### Acceptance Criteria
- `::` within symbol chain triggers separator confusion hint
- Hint includes the corrected semantic path
- No false positives on paths that correctly use `::` (none should, since `::` is only at file boundary)

---

## Spec 2.3: Add cross-file symbol search on SYMBOL_NOT_FOUND

### Problem
When an agent specifies the wrong file (e.g., `src/auth.rs::login` but login is in
`src/service.rs`), the error gives no indication which file contains the symbol. The
agent must manually search_codebase to find it.

### Root Cause
All symbol resolution is file-scoped. No workspace-wide fallback exists.

### Files
- `crates/pathfinder/src/server/tools/navigation.rs` — add cross-file search
- `crates/pathfinder/src/server/helpers.rs` — optional helper

### Changes

1. When `compute_did_you_mean()` returns empty results (no similar symbols in the
   specified file), attempt a workspace search:

```rust
async fn compute_did_you_mean_with_crossfile(
    &self,
    semantic_path: &SemanticPath,
) -> Vec<String> {
    // First try same-file suggestions
    let suggestions = self.compute_did_you_mean(semantic_path).await;
    if !suggestions.is_empty() {
        return suggestions;
    }
    
    // Cross-file search: use search_codebase to find the symbol name
    let chain = semantic_path.symbol_chain.as_ref();
    if let Some(chain) = chain {
        let base_name = chain.segments.last().map(|s| s.name.as_str()).unwrap_or("");
        if !base_name.is_empty() {
            let results = self.search_codebase_impl(SearchCodebaseParams {
                query: base_name.to_string(),
                path_glob: format!("**/*.{}", language_extension),
                filter_mode: FilterMode::CodeOnly,
                max_results: Some(5),
                ..Default::default()
            }).await;
            
            if let Ok(response) = results {
                return response.matches.iter()
                    .filter_map(|m| m.enclosing_semantic_path.clone())
                    .take(3)
                    .collect();
            }
        }
    }
    
    vec![]
}
```

2. Replace `compute_did_you_mean` calls with `compute_did_you_mean_with_crossfile`
   in all 5 semantic-path tools.

3. When cross-file suggestions are found, the hint should say:
   "Symbol not found in specified file but found elsewhere. Did you mean: ..."

### Test Plan
- Call `read_symbol_scope("src/wrong_file.rs::existing_symbol")` where symbol exists in a different file
- Verify did_you_mean includes paths from the correct file
- Verify performance: cross-file search should not add more than 2s latency
- Verify empty suggestions when symbol truly doesn't exist

### Acceptance Criteria
- Empty same-file suggestions trigger cross-file search
- Cross-file results appear as did_you_mean suggestions
- Suggestions include full semantic paths from the correct files
- Performance impact is bounded (max 2s additional latency)

---

## Spec 2.4: Add auto-retry for warmup-induced SYMBOL_NOT_FOUND

### Problem
When LSP is warming up, `get_definition` returns SYMBOL_NOT_FOUND after grep fallback
also fails (because the symbol name isn't distinctive enough for grep). The agent could
retry after warmup completes, but there's no signal to do so.

### Root Cause
`get_definition_impl` tries LSP -> waits 3s -> retries LSP -> grep fallback -> fails.
If the grep fallback also fails, it returns SYMBOL_NOT_FOUND with no indication that
retrying after warmup might succeed.

### Files
- `crates/pathfinder/src/server/tools/navigation.rs` — get_definition_impl

### Changes

1. When get_definition fails with SYMBOL_NOT_FOUND AND `warm_start_in_progress == true`,
   include a special hint in the error response:

```rust
if !self.lawyer.is_warm_start_complete() {
    // LSP still warming - the symbol might exist but grep couldn't find it
    return Err(pathfinder_to_error_data(&PathfinderError::SymbolNotFound {
        semantic_path: params.semantic_path,
        did_you_mean: suggestions,
    }));
    // The hint() for SymbolNotFound should check warm_start state and add:
    // "LSP is still warming up. This symbol may exist but couldn't be found by grep.
    //  Retry after ~15-30 seconds for LSP-backed resolution."
}
```

2. Add `lsp_warming_up: bool` to the error details for SYMBOL_NOT_FOUND when applicable:

```rust
let mut details = serde_json::json!({ "did_you_mean": &did_you_mean });
if !self.lawyer.is_warm_start_complete() {
    details["lsp_warming_up"] = serde_json::json!(true);
    details["retry_recommended"] = serde_json::json!(true);
    details["retry_after_seconds"] = serde_json::json!(15);
}
```

### Test Plan
- Call `get_definition` for a non-grep-friendly symbol during LSP warmup
- Verify error includes `lsp_warming_up: true` in details
- Verify hint says "retry after warmup"
- Call after warmup completes -> verify normal resolution

### Acceptance Criteria
- SYMBOL_NOT_FOUND during warmup includes retry guidance
- Error details include `lsp_warming_up` and `retry_after_seconds`
- No retry guidance when warmup is complete (normal SYMBOL_NOT_FOUND behavior)

---

## Spec 2.5: Improve grep fallback definition patterns for complex cases

### Problem
`definition_patterns()` at navigation.rs:115 covers basic patterns (fn, struct, class)
but misses complex cases that agents frequently encounter:
- Rust: `pub async fn`, `pub(crate) fn`, impl methods with lifetimes
- TypeScript: arrow function assignments (`const foo = () =>`), class expressions
- Python: async defs, class methods, static methods
- Go: methods on generic types, interface satisfaction

### Root Cause
Patterns are hand-crafted regex per language. They cover the common cases but miss
less common (but not rare) definition styles.

### Files
- `crates/pathfinder/src/server/tools/navigation.rs` — `definition_patterns()`

### Changes

1. Expand Rust patterns to cover:
```rust
"rust" => format!(
    r"(?:(?:pub\s*(?:\([^)]*\)\s*)?(?:async\s*)?)?fn\s+{name}\b|\
     (?:pub\s*)?struct\s+{name}\b|\
     (?:pub\s*)?enum\s+{name}\b|\
     (?:pub\s*)?trait\s+{name}\b|\
     (?:pub\s*)?type\s+{name}\b|\
     (?:pub\s*)?mod\s+{name}\b|\
     (?:pub\s*(?:\([^)]*\)\s*)?)?const\s+{name}\b|\
     (?:pub\s*)?static\s+{name}\b|\
     macro_rules!\s+{name}\b"
),
```

2. Expand TypeScript/JavaScript patterns:
```rust
"typescript" | "javascript" | "vue" => format!(
    r"(?:export\s+(?:default\s+)?)?(?:async\s+)?function\s+{name}\b|\
     (?:export\s+(?:default\s+)?)?(?:abstract\s+)?class\s+{name}\b|\
     (?:export\s+)?(?:type|interface|enum)\s+{name}\b|\
     (?:export\s+(?:const|let|var)\s+{name}\s*=\s*(?:async\s+)?(?:\([^)]*\)|[^=])?\s*=>)|\
     (?:export\s+(?:const|let|var)\s+{name}\s*=\s*(?:async\s+)?function)"
),
```

3. Expand Python patterns:
```rust
"python" => format!(
    r"(?:async\s+)?def\s+{name}\b|\
     class\s+{name}\b|\
     {name}\s*=\s*(?:lambda|property|staticmethod|classmethod)"
),
```

4. Expand Go patterns:
```rust
"go" => format!(
    r"func\s+(?:\([^)]*\)\s+)?{name}\b|\
     type\s+{name}\b|\
     (?:const|var)\s+{name}\b|\
     type\s+{name}\s+struct\b|\
     type\s+{name}\s+interface\b"
),
```

### Test Plan
- Test each expanded pattern against real codebase examples
- Verify no false positives (patterns shouldn't match call sites)
- Verify grep fallback finds symbols that previously returned SYMBOL_NOT_FOUND

### Acceptance Criteria
- `pub async fn foo` is found by grep fallback
- `pub(crate) fn foo` is found
- `const foo = () =>` is found in TypeScript
- `async def foo` is found in Python
- `func (t *T[U]) foo` is found in Go
- No regressions in existing grep fallback tests

---

## Spec 2.6: Add error response examples to tool descriptions

### Problem
When agents get errors, the error format is opaque. They don't know what fields are
available (did_you_mean, hint, details) or how to parse them. Tool descriptions only
show happy-path usage.

### Root Cause
Tool descriptions in `#[tool(description = "...")]` don't include error response examples.

### Files
- `crates/pathfinder/src/server.rs` — tool description strings

### Changes

1. Add error format documentation to `read_symbol_scope` description:

```
On SYMBOL_NOT_FOUND: the error includes "did_you_mean" suggestions and a "hint" with 
recovery guidance. Check error.details.did_you_mean for alternative symbol names, and 
error.hint for suggested next steps (e.g., use find_symbol or search_codebase to locate 
the correct path).
```

2. Add similar documentation to `get_definition`, `read_with_deep_context`,
   `analyze_impact`, `find_all_references`.

3. Include the error JSON shape:
```
Error format: { error: "SYMBOL_NOT_FOUND", details: { did_you_mean: ["login", "logout"] }, hint: "..." }
Error format: { error: "FILE_NOT_FOUND", details: { path: "..." }, hint: "..." }
Error format: { error: "INVALID_SEMANTIC_PATH", details: { issue: "..." }, hint: "..." }
```

### Test Plan
- Visual inspection of tool descriptions in MCP tool list
- Verify descriptions include error format examples
- Verify descriptions suggest recovery tools

### Acceptance Criteria
- All 5 semantic-path tools include error format in description
- Error format includes field names (did_you_mean, hint, details)
- Recovery guidance mentions specific alternative tools

---

## Execution Order

```
Spec 2.1 (did_you_mean in all tools) -> 2 hours
Spec 2.2 (separator confusion detection) -> 1 hour
Spec 2.4 (auto-retry hint for warmup) -> 1 hour
Spec 2.5 (improved grep patterns) -> 2 hours
Spec 2.3 (cross-file search) -> 3 hours
Spec 2.6 (error docs in tool descriptions) -> 1 hour
```

Total: ~10 hours across 2-3 sessions
