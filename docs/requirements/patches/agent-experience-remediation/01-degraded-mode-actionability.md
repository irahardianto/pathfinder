# Epic 1: Degraded Mode Actionability

**Priority**: P0
**Theme**: Agents cannot make good decisions when degraded mode is ambiguous
**Specs**: 8
**Estimated effort**: 2-3 days

---

## Problem Statement

When Pathfinder tools return `degraded: true`, agents face 4 unanswered questions:
1. Are these results trustworthy enough to use?
2. Should I retry with built-in tools?
3. What functionality is actually missing?
4. How long until I can retry?

The current response includes `degraded: true` + `degraded_reason: "lsp_warmup_grep_fallback"`
but no actionable guidance. Agents must read documentation to understand what each reason means.

---

## Spec 1.1: Add `actionable_next_step` to degraded responses

### Problem
6 response types have `degraded: bool` + `degraded_reason: Option<DegradedReason>` but no
machine-readable guidance on what to do next.

### Root Cause
DegradedReason has 12 variants. Each maps to different agent behavior:
- `LspWarmupEmptyUnverified` -> retry after 10-30 seconds
- `NoLsp` -> use search_codebase as permanent fallback
- `GrepFallback*` -> results are heuristic, verify manually
- `UnsupportedLanguage` -> use read_file instead

Currently agents must hardcode this mapping or read skill docs.

### Files
- `crates/pathfinder/src/server/types.rs` — add `ActionableGuidance` struct
- `crates/pathfinder-common/src/types.rs` — add method to DegradedReason

### Changes

1. Add `ActionableGuidance` struct to `types.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ActionableGuidance {
    pub retry_recommended: bool,
    pub retry_after_seconds: Option<u32>,
    pub fallback_tool: Option<String>,
    pub trust_level: String,
    pub permanent: bool,
}
```

2. Add `fn guidance(&self) -> ActionableGuidance` to `DegradedReason`:

```
NoLsp -> { retry: false, fallback: "search_codebase", trust: "partial", permanent: true }
LspWarmupEmptyUnverified -> { retry: true, after: 15, trust: "unreliable", permanent: false }
LspWarmupGrepFallback -> { retry: true, after: 30, fallback: "search_codebase", trust: "heuristic", permanent: false }
LspTimeoutGrepFallback -> { retry: true, after: 10, fallback: "search_codebase", trust: "heuristic", permanent: false }
LspErrorGrepFallback -> { retry: false, fallback: "search_codebase", trust: "heuristic", permanent: true }
NoLspGrepFallback -> { retry: false, fallback: "search_codebase", trust: "heuristic", permanent: true }
GrepFallback* -> { retry: false, trust: "heuristic", permanent: true }
UnsupportedLanguageFilterBypassed -> { retry: false, fallback: "read_file", trust: "partial", permanent: true }
UnsupportedLanguage -> { retry: false, fallback: "read_file", trust: "none", permanent: true }
GitError -> { retry: true, after: 5, trust: "partial", permanent: false }
```

3. Add `actionable_guidance: Option<ActionableGuidance>` to all 6 response types that have degraded fields:
   - `SearchCodebaseResponse`
   - `GetRepoMapMetadata`
   - `ReadWithDeepContextMetadata`
   - `GetDefinitionResponse`
   - `AnalyzeImpactMetadata`
   - `FindAllReferencesMetadata`

4. Populate in all tool handlers when setting `degraded = true`.

### Test Plan
- Unit test: `DegradedReason::NoLsp.guidance().fallback_tool == "search_codebase"`
- Unit test: `DegradedReason::LspWarmupEmptyUnverified.guidance().retry_after_seconds == Some(15)`
- Integration test: call `analyze_impact` during warmup, verify `actionable_guidance` in response
- Verify backward compat: `actionable_guidance` is `None` when `degraded == false`

### Acceptance Criteria
- Every degraded response includes `actionable_guidance` with populated fields
- `retry_after_seconds` is `None` only for permanent degradation (NoLsp, unsupported language)
- `fallback_tool` always names a specific tool, never generic text
- All existing tests pass unchanged

---

## Spec 1.2: Add degraded guidance to text output prefix

### Problem
Only `read_with_deep_context` and `analyze_impact` prepend degraded text warnings.
Other tools (get_definition, search_codebase, get_repo_map) show degraded only in
structured_content. Agents that only read text output miss the signal.

### Root Cause
Text prefix was added to 2 tools in prior remediation but not all. Inconsistent.

### Files
- `crates/pathfinder/src/server/tools/navigation.rs` — get_definition text output
- `crates/pathfinder/src/server/tools/search.rs` — search_codebase text output
- `crates/pathfinder/src/server/tools/repo_map.rs` — get_repo_map text output

### Changes

1. Standardize degraded prefix format across ALL tools:

```
DEGRADED ({reason}) — {trust_level} results.
{fallback_tool: use search_codebase for authoritative results}
{retry: retry after ~{N}s for better results}
```

2. Apply to `get_definition_impl` text output:
   - After line 1085 (text format): if degraded, prepend standardized prefix

3. Apply to `search_codebase_impl` text output:
   - After line 196 (response building): if degraded, prepend standardized prefix

4. Apply to `get_repo_map_impl` text output:
   - After line 276 (result building): if degraded, prepend standardized prefix

### Test Plan
- Call each tool with LSP unavailable -> verify text prefix in output
- Verify prefix includes reason, trust level, and fallback tool name
- Verify prefix is absent when `degraded == false`

### Acceptance Criteria
- All 6 LSP-dependent tools show degraded text prefix when degraded
- Prefix format is consistent (same fields, same order)
- Prefix includes actionable guidance (not just "degraded")

---

## Spec 1.3: Add `missing_capabilities` to lsp_health response

### Problem
`lsp_health` reports `supports_call_hierarchy: true` based on LSP capability advertisement,
but runtime calls fail for certain symbol types (interfaces, Spring proxies, macro-generated
code, impl blocks with lifetimes). Agents see "ready" and trust LSP tools, then get
degraded results.

### Root Cause
Capability advertisement is static (from LSP initialize response). Runtime behavior varies
per symbol type. There's no "runtime capability" concept.

### Files
- `crates/pathfinder/src/server/types.rs` — add field to `LspLanguageHealth`
- `crates/pathfinder/src/server/tools/navigation.rs` — populate from probe results

### Changes

1. Add `known_limitations: Vec<String>` to `LspLanguageHealth`:

```rust
/// Known limitations discovered during probe or runtime.
/// Populated from past degraded-mode encounters in this session.
/// Agents should treat these as warnings, not hard blocks.
pub known_limitations: Vec<String>,
```

2. When a tool returns degraded for a reason that indicates a runtime limitation
   (not just warmup), store the limitation in the server state and surface it in
   subsequent `lsp_health` calls.

3. Seed with known limitations per LSP:
   - rust-analyzer: "Impl blocks with complex lifetimes may not resolve in call hierarchy"
   - gopls: "Interface methods may return empty call hierarchy. Use search_codebase as fallback."
   - typescript-language-server: "May timeout on large files. Retry after 30s."
   - pyright/pylsp: "Decorator-generated methods may not appear in call hierarchy."

### Test Plan
- Call `lsp_health` -> verify `known_limitations` is populated for each language
- Verify limitations are honest (not claiming capabilities that don't exist)

### Acceptance Criteria
- `lsp_health` response includes `known_limitations` for each language
- Limitations are populated from known issues per LSP server
- Empty vec when no known limitations

---

## Spec 1.4: Improve SYMBOL_NOT_FOUND hint for wrong-file cases

### Problem
When an agent provides the wrong file in a semantic path (e.g., `src/auth.ts::AuthService.login`
but login is in `src/service.ts`), the error says SYMBOL_NOT_FOUND with no hint about which
file might contain the symbol. `did_you_mean` returns empty because no similar symbols exist
in the specified file.

### Root Cause
`compute_did_you_mean` only extracts symbols from the specified file. When the file has no
similar symbols, suggestions are empty. The hint says "Use search_codebase" but that's generic.

### Files
- `crates/pathfinder-common/src/error.rs` — `hint()` method for `SymbolNotFound`

### Changes

1. In `PathfinderError::SymbolNotFound::hint()`, when `did_you_mean` is empty:

```rust
Self::SymbolNotFound { semantic_path, did_you_mean } => {
    if did_you_mean.is_empty() {
        let parts: Vec<&str> = semantic_path.split("::").collect();
        let symbol_name = parts.last().unwrap_or(&semantic_path);
        // Check if separator confusion (. vs ::)
        let dot_segments: Vec<&str> = symbol_name.split('.').collect();
        let base_name = dot_segments[0];
        Some(format!(
            "Symbol not found in the specified file. \
             Use find_symbol(name=\"{}\") to locate the correct file, \
             or search_codebase(query=\"{}\") to search the entire workspace.{}",
            base_name,
            base_name,
            separator_hint.unwrap_or("")
        ))
    } else {
        // existing did_you_mean logic
    }
}
```

### Test Plan
- Call `read_symbol_scope` with wrong file path -> verify hint mentions find_symbol and search_codebase
- Call with correct file but wrong symbol name -> verify existing did_you_mean logic still works
- Call with separator confusion (.) -> verify separator hint still appears

### Acceptance Criteria
- SYMBOL_NOT_FOUND with empty did_you_mean suggests find_symbol by name
- Suggestion includes the base symbol name (no parent qualifiers)
- Existing did_you_mean behavior unchanged when suggestions exist

---

## Spec 1.5: Distinguish bare file from symbol-not-found errors

### Problem
When an agent passes a bare file path (e.g., `src/main.rs`) to `read_symbol_scope`, the
error is `SYMBOL_NOT_FOUND` with empty `did_you_mean`. This is semantically wrong — the
file exists, there's just no symbol specified. The agent thinks the file doesn't exist.

### Root Cause
`require_symbol_target()` in helpers.rs:118 returns `PathfinderError::SymbolNotFound`
with empty did_you_mean. Should return `PathfinderError::InvalidSemanticPath` with
clear message.

### Files
- `crates/pathfinder/src/server/helpers.rs` — `require_symbol_target()`

### Changes

1. Change `require_symbol_target()` to return `InvalidSemanticPath`:

```rust
pub(crate) fn require_symbol_target(semantic_path: &SemanticPath) -> Result<&SymbolChain, ErrorData> {
    semantic_path.symbol_chain.as_ref().ok_or_else(|| {
        pathfinder_to_error_data(&PathfinderError::InvalidSemanticPath {
            input: semantic_path.to_string(),
            issue: "Bare file path without symbol target. This tool requires a symbol chain (e.g., src/main.rs::main or src/auth.ts::AuthService.login). Use read_source_file or read_file for full file content.".to_string(),
        })
    })
}
```

2. Update `PathfinderError::InvalidSemanticPath::hint()` to include the suggested tools:
   - "Use read_source_file(filepath=...) for source files with AST metadata"
   - "Use read_file(filepath=...) for raw file content"

### Test Plan
- Call `read_symbol_scope("src/main.rs")` -> verify error is `INVALID_SEMANTIC_PATH` not `SYMBOL_NOT_FOUND`
- Verify error message mentions `read_source_file` and `read_file`
- Verify existing symbol-targeted calls still work

### Acceptance Criteria
- Bare file paths return `INVALID_SEMANTIC_PATH` (error code), not `SYMBOL_NOT_FOUND`
- Error message explicitly names the correct alternative tools
- All existing tests that expect `SYMBOL_NOT_FOUND` for bare paths are updated

---

## Spec 1.6: Add file existence check before symbol resolution

### Problem
When an agent passes a semantic path with a nonexistent file (e.g., `src/auht.rs::login`),
tree-sitter attempts to parse the file, fails, and returns SYMBOL_NOT_FOUND. This wastes
time and gives an unclear error.

### Root Cause
No early validation in `read_symbol_scope_impl` or `treesitter_surgeon.rs`. The sandbox
check validates access but not existence.

### Files
- `crates/pathfinder/src/server/tools/symbols.rs` — add existence check
- `crates/pathfinder/src/server/tools/navigation.rs` — add for all navigation tools

### Changes

1. In `read_symbol_scope_impl` (symbols.rs), after parsing semantic path:

```rust
let semantic_path = parse_semantic_path(&params.semantic_path)?;
let file_path = workspace_root.join(&semantic_path.file_path);
if !file_path.exists() {
    return Err(pathfinder_to_error_data(&PathfinderError::FileNotFound {
        path: file_path,
    }));
}
```

2. Apply same pattern to `get_definition_impl`, `read_with_deep_context_impl`,
   `analyze_impact_impl`, `find_all_references_impl`.

3. Update `PathfinderError::FileNotFound::hint()` to suggest:
   - "Use search_codebase to find the correct file path"
   - "Use get_repo_map to see all files in the project"

### Test Plan
- Call `read_symbol_scope("nonexistent.rs::foo")` -> verify `FILE_NOT_FOUND` error
- Verify error is returned immediately (no tree-sitter parsing attempted)
- Call with existing file + wrong symbol -> verify still returns `SYMBOL_NOT_FOUND` with did_you_mean

### Acceptance Criteria
- Nonexistent file paths return `FILE_NOT_FOUND` immediately
- Error suggests `search_codebase` or `get_repo_map` for path discovery
- Existing symbol resolution unchanged for existing files

---

## Spec 1.7: Add `warm_start_complete` signal to degraded tool responses

### Problem
When LSP tools return degraded early in a session, agents don't know if it's because LSP
is still warming up (transient) or because LSP doesn't support the operation (permanent).
They must call `lsp_health` separately to check.

### Root Cause
`warm_start_complete` exists in `LspHealthResponse` but not in individual tool responses.
Agents that skip `lsp_health` have no way to know warmup status.

### Files
- `crates/pathfinder/src/server/types.rs` — add field to response types
- `crates/pathfinder/src/server/tools/navigation.rs` — populate from lawyer state

### Changes

1. Add `warm_start_in_progress: Option<bool>` to navigation response metadata types:
   - `GetDefinitionResponse`
   - `ReadWithDeepContextMetadata`
   - `AnalyzeImpactMetadata`
   - `FindAllReferencesMetadata`

2. Populate from `self.lawyer.is_warm_start_complete()` in each tool handler.

3. When `warm_start_in_progress == Some(true)` AND `degraded == true`, the degraded text
   prefix should say "LSP still warming up" (transient) instead of generic degraded message.

### Test Plan
- Call navigation tool immediately after LSP start -> verify `warm_start_in_progress` is `true`
- Wait for warmup, call again -> verify `warm_start_in_progress` is `false`
- Verify field is `None` when no LSP is available

### Acceptance Criteria
- All 4 navigation response types include `warm_start_in_progress`
- Value is accurate (matches actual LSP warmup state)
- Text prefix distinguishes warmup degradation from permanent degradation

---

## Spec 1.8: Consolidate degraded status into a single prefix format

### Problem
Different tools format their degraded text prefix differently:
- `read_with_deep_context`: "DEGRADED MODE"
- `analyze_impact`: variable format
- `get_definition`: no prefix (only in metadata)

Agents parsing text output can't reliably extract degradation info.

### Files
- `crates/pathfinder/src/server/tools/navigation.rs` — all tool text outputs
- `crates/pathfinder/src/server/tools/search.rs` — search text output
- `crates/pathfinder/src/server/tools/repo_map.rs` — repo map text output

### Changes

1. Create a shared function for degraded text prefix:

```rust
fn format_degraded_notice(reason: &DegradedReason, guidance: &ActionableGuidance) -> String {
    let mut parts = vec![format!("DEGRADED ({reason})")];
    
    match guidance.trust_level.as_str() {
        "unreliable" => parts.push("results are UNRELIABLE, do not trust empty counts".into()),
        "heuristic" => parts.push("results are heuristic (grep-based), verify manually".into()),
        "partial" => parts.push("results are PARTIAL, some features unavailable".into()),
        "none" => parts.push("results are UNAVAILABLE for this language".into()),
        _ => {}
    }
    
    if let Some(fallback) = &guidance.fallback_tool {
        parts.push(format!("fallback: use {fallback} for authoritative results"));
    }
    
    if let Some(secs) = guidance.retry_after_seconds {
        parts.push(format!("retry after ~{secs}s for LSP-backed results"));
    }
    
    parts.join(" — ")
}
```

2. Replace all existing degraded text formatting with calls to this function.

3. Ensure the prefix is ALWAYS the first line of text output, before any tool-specific content.

### Test Plan
- Visual inspection of text output from each tool in degraded mode
- Verify consistent format: `DEGRADED (reason) — trust description — fallback — retry`
- Verify no duplicate prefixes (some tools had both prefix + inline degraded text)

### Acceptance Criteria
- Single shared function generates degraded text for all tools
- Format is identical across all tools
- No tool has custom degraded text formatting

---

## Execution Order

```
Spec 1.5 (bare file error semantics) -> 1 hour
Spec 1.6 (file existence check) -> 1 hour
Spec 1.4 (SYMBOL_NOT_FOUND hint improvement) -> 30 min
Spec 1.1 (ActionableGuidance struct + population) -> 3 hours
Spec 1.8 (consolidated prefix format) -> 2 hours
Spec 1.2 (text prefix for all tools) -> 1 hour
Spec 1.7 (warm_start_in_progress in responses) -> 1 hour
Spec 1.3 (known_limitations in lsp_health) -> 1 hour
```

Total: ~10.5 hours across 2-3 sessions
