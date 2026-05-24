# Epic 5: Performance Metrics and Observability

**Priority**: P3
**Theme**: Add timing and quality metrics so agents can make timeout/budget decisions
**Specs**: 3
**Estimated effort**: 1-2 days

---

## Problem Statement

Agents have no way to know:
- How long a tool call took (for timeout decisions)
- Whether results are complete or truncated
- How expensive a follow-up call would be

Adding `duration_ms` to all responses enables agents to:
- Set appropriate timeouts for retry logic
- Understand when LSP queries are slow vs fast
- Make cost-benefit decisions about deeper traversals

---

## Spec 5.1: Add `duration_ms` to all tool responses

### Problem
No tool response includes timing information. Agents can't distinguish "fast response,
authoritative" from "slow response, likely timed out internally."

### Root Cause
Tool handlers don't measure or report their execution time.

### Files
- `crates/pathfinder/src/server/types.rs` — add `duration_ms` to all response types
- `crates/pathfinder/src/server/tools/navigation.rs` — measure and populate
- `crates/pathfinder/src/server/tools/search.rs` — measure and populate
- `crates/pathfinder/src/server/tools/symbols.rs` — measure and populate
- `crates/pathfinder/src/server/tools/source_file.rs` — measure and populate
- `crates/pathfinder/src/server/tools/repo_map.rs` — measure and populate
- `crates/pathfinder/src/server/tools/find_symbol.rs` — measure and populate
- `crates/pathfinder/src/server/tools/file_ops.rs` — measure and populate
- `crates/pathfinder/src/server/tools/read_files.rs` — measure and populate

### Changes

1. Add `duration_ms: u64` to ALL response metadata types:
   - `SearchCodebaseResponse`
   - `GetRepoMapMetadata`
   - `ReadSymbolScopeMetadata`
   - `ReadSourceFileMetadata`
   - `ReadFileMetadata`
   - `GetDefinitionResponse`
   - `ReadWithDeepContextMetadata`
   - `AnalyzeImpactMetadata`
   - `FindAllReferencesMetadata`
   - `FindSymbolResponse`
   - `ReadFilesResponse`

2. In each tool handler, measure elapsed time:

```rust
let start = std::time::Instant::now();
// ... tool logic ...
let duration_ms = start.elapsed().as_millis() as u64;
// ... populate response.duration_ms ...
```

3. Include in text output as a trailing line:
```
[completed in {duration_ms}ms]
```

### Test Plan
- Call each tool and verify `duration_ms` > 0 in response
- Verify text output includes timing line
- Verify timing is reasonable (search < 100ms, LSP tools < 5000ms typically)

### Acceptance Criteria
- All 11 tool responses include `duration_ms`
- Value is accurate (reflects actual wall-clock time)
- Text output includes timing for agent consumption

---

## Spec 5.2: Add `strategy_used` to navigation responses

### Problem
When a navigation tool uses grep fallback, agents can't tell from the response whether
results came from LSP (authoritative) or grep (heuristic). The `degraded` flag helps
but doesn't tell the full story.

### Root Cause
Multiple fallback strategies exist (file-scoped grep, impl-scoped grep, global grep)
but only `degraded_reason` distinguishes them. Agents need to know what strategy was
used to calibrate trust.

### Files
- `crates/pathfinder/src/server/types.rs` — add field to response types
- `crates/pathfinder/src/server/tools/navigation.rs` — populate

### Changes

1. Add `resolution_strategy: Option<String>` to:
   - `GetDefinitionResponse`
   - `AnalyzeImpactMetadata`
   - `FindAllReferencesMetadata`
   - `ReadWithDeepContextMetadata`

2. Values:
   - `"lsp_goto_definition"` — authoritative LSP resolution
   - `"lsp_call_hierarchy"` — authoritative LSP call graph
   - `"lsp_references"` — authoritative LSP references
   - `"grep_file_scoped"` — ripgrep within single file
   - `"grep_impl_scoped"` — ripgrep within impl block files
   - `"grep_global"` — ripgrep across entire workspace
   - `"grep_dependency"` — ripgrep for dependency signatures
   - `"treesitter_direct"` — tree-sitter AST navigation (no LSP)
   - `"treesitter_fallback"` — tree-sitter used after LSP failure

3. Populate in each tool handler based on the code path taken.

### Test Plan
- Call `get_definition` with LSP ready -> verify strategy is "lsp_goto_definition"
- Call `get_definition` with LSP unavailable -> verify strategy is "grep_*"
- Call `read_symbol_scope` -> verify strategy is "treesitter_direct"

### Acceptance Criteria
- All navigation responses include `resolution_strategy`
- Value accurately reflects the code path taken
- Agents can use this to calibrate trust without checking `degraded` separately

---

## Spec 5.3: Add `indexing_progress` to lsp_health when available

### Problem
During LSP warmup, agents have no visibility into how far indexing has progressed.
They only see "warming_up" and must guess how long to wait.

### Root Cause
Some LSP servers report indexing progress via `window/workDoneProgress`. Pathfinder
doesn't surface this information.

### Files
- `crates/pathfinder/src/server/types.rs` — `LspLanguageHealth`
- `crates/pathfinder-lsp/src/types.rs` — `LspLanguageStatus`

### Changes

1. Add `indexing_progress_percent: Option<u8>` to `LspLanguageHealth`.

2. If the LSP reports progress (via workDoneProgress notifications), populate this field.

3. If progress is not reported, set to `None` (don't guess).

4. Include in text output:
```
rust: ready (indexing: 100% complete, uptime: 45s)
typescript: warming_up (indexing: 67%, uptime: 12s)
```

### Test Plan
- Call `lsp_health` during TypeScript LSP warmup
- Verify `indexing_progress_percent` is populated if LSP reports it
- Verify `None` for languages that don't report progress

### Acceptance Criteria
- `indexing_progress_percent` appears in lsp_health response when available
- Text output includes percentage when available
- `None` when LSP doesn't report progress (no guessing)

---

## Execution Order

```
Spec 5.1 (duration_ms in all responses) -> 3 hours
Spec 5.2 (resolution_strategy in navigation) -> 2 hours
Spec 5.3 (indexing progress) -> 2 hours
```

Total: ~7 hours across 1-2 sessions
