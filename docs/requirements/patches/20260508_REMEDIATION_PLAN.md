# Pathfinder MCP Tools — Agent Ergonomics Remediation Plan

Date: 2026-05-08
Source: Cross-referenced findings from 2 independent agent reports (Go/Vue/Python project + Pathfinder's own Rust codebase)

---

## Finding Triage

Each finding from both reports was verified against the Pathfinder source code. Findings are categorized:

- CONFIRMED: Real issue found in code, causes agent friction, worth remediating
- NOT Pathfinder: Issue is in external dependencies or infrastructure, not Pathfinder code
- ALREADY FIXED: Issue was already addressed in current code
- BY DESIGN: Intentional behavior, not a bug

---

## CONFIRMED Findings (Worth Remediating)

### F1. total_matches / returned_count inconsistency in search_codebase

Report: "total_matches: 1 with returned_count: 0 and empty matches array"
Code location: `crates/pathfinder/src/server/tools/search.rs::search_codebase_impl`
Root cause: `total_matches` comes directly from ripgrep (raw count BEFORE filter_mode filtering). After filter_mode drops matches, `returned_count` can be 0 while `total_matches` is still the pre-filter number. The `SearchCodebaseResponse` docstring documents this behavior, but agents interpret `total_matches: 1, returned_count: 0` as a bug.
Impact: HIGH — agents waste reasoning cycles deciding if the search is broken, may retry or abandon.
Other occurrences: Same pattern exists in `SearchResultGroup.total_matches` which correctly reflects post-filter count. Inconsistency between the two `total_matches` fields.

### F2. Stdlib/external noise in analyze_impact and read_with_deep_context outgoing results

Report: "depth=2 outgoing produces 30-50 references, most are Vec::push, String::clone"
Code location: `crates/pathfinder/src/server/tools/navigation.rs::bfs_call_hierarchy` (line ~1037), `append_outgoing_deps` (line ~258)
Root cause: `is_workspace_file()` filters absolute paths and node_modules/vendor, but Rust's rust-analyzer returns stdlib paths as relative-looking paths (e.g., `alloc::vec::push` maps to `alloc/vec.rs` or similar). The filter only checks if path starts with `/` or has `:\` at position 1. Rust stdlib paths returned by rust-analyzer may appear as relative paths but belong to the sysroot.
Impact: HIGH — agents waste 40-60% of output tokens on noise. `analyze_impact` depth>0 is almost unusable for Rust.
Other occurrences: Same filter is used in both `bfs_call_hierarchy` and `append_outgoing_deps`. No `project_only` parameter exists anywhere.

### F3. No "did you mean" in get_definition error responses

Report: "Silent failure mode. Wrong semantic path -> empty or error, but no guidance"
Code location: `crates/pathfinder-treesitter/src/symbols.rs::did_you_mean` exists and works, `crates/pathfinder/src/server/tools/navigation.rs::get_definition_impl`
Root cause: `get_definition_impl` returns `PathfinderError::SymbolNotFound` with `did_you_mean: vec![]` (empty vec). It never calls `symbols::did_you_mean()` to populate suggestions. The grep fallback in `fallback_definition_grep` attempts pattern matching but doesn't extract the symbol list from the file to build suggestions.
Impact: HIGH — agents' most common failure mode is wrong path format. `did_you_mean` exists but isn't wired through.
Other occurrences: `read_symbol_scope_impl` DOES get `did_you_mean` from the tree-sitter surgeon error. `get_definition` bypasses tree-sitter symbol resolution — it reads scope for position only, then queries LSP.

### F4. Inconsistent degraded_reason strings

Report: "no_lsp, lsp_warmup_grep_fallback, unsupported_language_filter_bypassed — different naming conventions"
Code locations: Multiple files in `crates/pathfinder/src/server/tools/`
Root cause: degraded_reason is a free-form `String`, not an enum. Each tool handler invents its own naming convention. There are at least 8 distinct strings across navigation.rs alone.
Impact: MEDIUM — agents need to match strings to decide action. Inconsistent naming forces agents to treat them as opaque text rather than structured signals.
All degraded_reason values found in code:
- `"no_lsp"` — navigation.rs (multiple)
- `"lsp_warmup_empty_unverified"` — navigation.rs
- `"lsp_warmup_grep_fallback"` — navigation.rs (get_definition grep fallback after LSP None)
- `"no_lsp_grep_fallback"` — navigation.rs (analyze_impact when no LSP)
- `"lsp_timeout_grep_fallback"` — navigation.rs
- `"lsp_error_grep_fallback"` — navigation.rs
- `"grep_fallback_file_scoped"` — navigation.rs
- `"grep_fallback_impl_scoped"` — navigation.rs
- `"grep_fallback_global"` — navigation.rs
- `"unsupported_language_filter_bypassed"` — search.rs
- `"unsupported_language"` — search.rs

### F5. get_repo_map default max_tokens too low for monorepos

Report: "max_tokens: 16000 truncated aggressively. Files truncated with [TRUNCATED - NO CLASSES EXTRACTED]"
Code location: `crates/pathfinder/src/server/types.rs::default_max_tokens` (returns 16000)
Root cause: Default is a fixed 16000 tokens. No auto-scaling based on project size. Monorepos (20+ crates like Pathfinder itself) need 24000+. Agents must manually tune every time.
Impact: MEDIUM — agents get incomplete repo maps on first call, then waste a second call tuning. Friction on every new session.
Other occurrences: `max_tokens_per_file` default is 2000 which also truncates large files.

### F6. No file-size preview before read operations

Report: "No indication of file size before reading. Agent must guess."
Code locations: `read_source_file`, `read_file` tools
Root cause: No metadata-only mode to preview file size/line count before reading content. `read_file` reads the full file before returning `total_lines` in metadata.
Impact: LOW-MEDIUM — agents can overshoot context budgets on large files. Currently mitigated by `start_line`/`end_line` pagination but agents don't know the file is large until after reading.

### F7. analyze_impact has no token/output budget control

Report: "analyze_impact with depth=2 returned 58 references. No max_output_tokens parameter."
Code location: `crates/pathfinder/src/server/tools/navigation.rs::analyze_impact_impl`, `AnalyzeImpactParams`
Root cause: `AnalyzeImpactParams` has only `semantic_path` and `max_depth`. No way to cap output. BFS can produce unbounded results.
Impact: MEDIUM — agents can overflow context on large codebases. Only mitigation is lower `max_depth` which loses information.

### F8. read_with_deep_context has no output budget control

Report: "For large functions with many calls, the output could be enormous."
Code location: `crates/pathfinder/src/server/tools/navigation.rs::read_with_deep_context_impl`, `ReadWithDeepContextParams`
Root cause: Same as F7. `ReadWithDeepContextParams` has only `semantic_path`. Dependencies list is unbounded.
Impact: MEDIUM — same token overflow risk as F7.

---

## NOT Pathfinder Findings (External Issues)

### X1. Go LSP (gopls) reliability issues

Report: "get_definition never worked for Go. LSP_ERROR on every attempt."
Root cause: This is a gopls infrastructure issue, not Pathfinder. The Go LSP is detected and started correctly by Pathfinder. gopls version mismatches, memory pressure, or configuration issues in the FATH project environment cause the failures. Pathfinder's grep fallback handles this gracefully.
Action: Document known gopls issues in SKILL.md. No code changes needed.

### X2. get_repo_map returning image data on filtered calls

Report: "The second call (with visibility=public + include_extensions) returned image data"
Root cause: Verified Pathfinder code — `get_repo_map_impl` ALWAYS uses `Content::text()`. The image rendering happens in the MCP transport layer (rmcp crate), not in Pathfinder. This is a known rmcp issue with certain response sizes.
Action: Track as external dependency issue. No Pathfinder code changes needed.

### X3. Rust impl block #2 disambiguator

Report: "impl impl PathfinderServer#2 — the #2 disambiguator is not documented"
Root cause: Verified `merge_rust_impl_blocks` — the `#N` suffix is correctly generated to disambiguate multiple impl blocks for the same type. This is a documentation issue, not a bug. The SKILL.md should explain this pattern.
Action: Update SKILL.md documentation. No code changes needed.

---

## ALREADY FIXED Findings

### A1. lsp_health false "ready" state

Report: "Reports 'ready' when Go LSP can't serve requests"
Current code: `lsp_health_impl` has a probe-based verification system with:
  - `probe_language_readiness()` that actually sends `goto_definition` to verify
  - `ProbeCacheEntry` with TTL for negative results
  - Liveness probe for "ready" languages (`LIVENESS_PROBE_INTERVAL_SECS = 120`)
  - `navigation_ready` two-phase model (capability gate vs indexing)
  - Graceful downgrade from "ready" to "degraded" on probe failure
Status: Already fixed. The probe-verified system is comprehensive.

### A2. LSP_ERROR with no diagnostics

Report: "LSP_ERROR contains zero diagnostic info"
Current code: `PathfinderError::LspError` includes the full message. `PathfinderError::hint()` generates context-aware hints including:
  - Timeout messages → "LSP timed out... Workaround: use search_codebase + read_symbol_scope"
  - Connection lost → "LSP process crashed or disconnected"
  - Generic → includes original error message + "Workaround: use search_codebase"
Status: Already fixed. Error messages include diagnostics and actionable hints.

### A3. Token budget defaults documentation

Report: "No indication of which files were truncated vs fully rendered"
Current code: `GetRepoMapMetadata` includes `files_truncated` and `coverage_percent`. The tool description in server.rs documents `max_tokens` and `max_tokens_per_file` controls.
Status: Already addressed via metadata fields.

### A4. Document lifecycle (did_open/did_close leak)

Report: "Implied by LSP errors — files not in buffer"
Current code: IW-3 fix implemented with RAII `DocumentGuard`. Every `get_definition_impl`, `analyze_impact_impl`, `read_with_deep_context_impl` uses `_doc_guard` that fires `did_close` on drop. Tests verify open/close symmetry.
Status: Already fixed.

---

## BY DESIGN Findings

### D1. Grep fallback non-determinism for structs

Report: "Same struct, same session: 0 refs then 10 refs"
Root cause: The grep fallback searches for the symbol name as plain text. First attempt may have been during LSP warmup (returned empty because `call_hierarchy_prepare` returned Ok([]) and probe was Ok(None)). Second attempt was after warmup completed and grep was used as fallback. This is documented behavior — grep results are heuristic.
Status: By design. The `degraded: true` + `degraded_reason` honestly signals the limitation.

### D2. total_matches reflecting pre-filter count

Report: "total_matches is buggy"
Root cause: `SearchCodebaseResponse.total_matches` documents that it reflects ripgrep's raw count BEFORE filter_mode filtering. `returned_count` reflects post-filter count. This is intentional — agents can compare the two to understand filter impact. However, the naming is confusing (see F1).
Status: By design, but confusing (addressed in F1).

---

## Remediation Plan

Tasks are ordered by impact (HIGH first). Each task is self-contained with exact file locations, expected changes, and verification steps.

---

### TASK-1: Fix total_matches semantic confusion in search_codebase

Priority: HIGH
Files:
  - `crates/pathfinder/src/server/tools/search.rs`
  - `crates/pathfinder/src/server/types.rs`
  - `crates/pathfinder/src/server/tools/search.rs::tests`

Problem:
  `total_matches` is the ripgrep raw count (pre-filter). After `filter_mode` filtering, `returned_count` can be 0 while `total_matches` is positive. Agents see this as a bug.

Solution:
  1. Rename the field to `raw_match_count` (pre-filter ripgrep count) in `SearchCodebaseResponse`
  2. Change `total_matches` to equal `returned_count` (post-filter count) — this is what agents expect
  3. Add a `filtered_count` field = `raw_match_count - total_matches` so agents know how many were filtered
  4. Update `SearchResultGroup.total_matches` to be consistent (it already is post-filter)
  5. Update docstrings on all three fields

Changes:
  In `types.rs` `SearchCodebaseResponse`:
  - Rename `total_matches` to `raw_match_count` with doc: "Raw match count from ripgrep BEFORE filter_mode filtering"
  - Add `total_matches: usize` that equals `returned_count` (post-filter) with doc: "Total matches in this response (after filter_mode filtering). Equals `matches.len()`."
  - Add `filtered_count: usize` with doc: "Matches removed by filter_mode. `raw_match_count - total_matches`."

  In `search.rs` `search_codebase_impl`:
  - Set `raw_match_count: result.total_matches`
  - Set `total_matches: returned_count`
  - Set `filtered_count: result.total_matches.saturating_sub(returned_count)`

  In `SearchResultGroup`:
  - `total_matches` stays as-is (already post-filter, confirmed correct)

Verification:
  - Run existing test `test_search_codebase_filter_mode_code_only_drops_comments`
  - Assert `total_matches == 2` (post-filter), `raw_match_count == 3` (pre-filter), `filtered_count == 1`
  - Add new test: `test_search_codebase_total_matches_equals_returned_count_after_filter`
  - Run `cargo test -p pathfinder`

---

### TASK-2: Add project_only filter to analyze_impact and read_with_deep_context

Priority: HIGH
Files:
  - `crates/pathfinder/src/server/types.rs` (AnalyzeImpactParams, ReadWithDeepContextParams)
  - `crates/pathfinder/src/server/tools/navigation.rs` (bfs_call_hierarchy, append_outgoing_deps)

Problem:
  BFS traversal at depth > 0 includes stdlib references (Vec::push, String::clone, etc.) that waste 40-60% of output tokens. Rust's rust-analyzer returns stdlib paths as paths within the Rust sysroot which may look relative but aren't in the workspace.

Solution:
  1. Add `project_only: Option<bool>` parameter to `AnalyzeImpactParams` (default: true)
  2. Add `project_only: Option<bool>` parameter to `ReadWithDeepContextParams` (default: true)
  3. Improve `is_workspace_file()` to check if the file actually exists in the workspace (not just path pattern matching)
  4. Pass `project_only` flag through to `bfs_call_hierarchy` and `append_outgoing_deps`

Changes:
  In `types.rs`:
  ```rust
  // Add to AnalyzeImpactParams:
  #[serde(default)]
  pub project_only: Option<bool>,

  // Add to ReadWithDeepContextParams:
  #[serde(default)]
  pub project_only: Option<bool>,
  ```

  In `navigation.rs`:
  - Add `workspace_root: &Path` parameter to `is_workspace_file` and change implementation to:
    ```rust
    fn is_workspace_file(file: &str, workspace_root: &Path) -> bool {
        // Existing heuristic checks (absolute paths, node_modules, vendor)
        if file.starts_with('/') || file.starts_with('\\') { return false; }
        if file.len() >= 2 && file.chars().nth(1) == Some(':') { return false; }
        if file.contains("node_modules/") || file.contains("vendor/") { return false; }
        // NEW: verify file exists within workspace
        let resolved = workspace_root.join(file);
        resolved.exists()
    }
    ```
  - Update all call sites (8 occurrences) to pass `workspace_root`
  - Add `project_only` parameter to `bfs_call_hierarchy` and `append_outgoing_deps`
  - When `project_only == Some(true)` (default), apply `is_workspace_file` filter
  - When `project_only == Some(false)`, skip the filter (include all references)

  In tool descriptions (server.rs):
  - Update `analyze_impact` description to mention `project_only` parameter
  - Update `read_with_deep_context` description to mention `project_only` parameter

Verification:
  - Run `cargo test -p pathfinder`
  - Existing tests should pass (default behavior unchanged)
  - Add test: `test_analyze_impact_bfs_filters_stdlib_paths`
  - Add test: `test_analyze_impact_project_only_false_includes_external`
  - Manual verification: run `analyze_impact` on a Rust function with depth=2 and confirm no stdlib noise

---

### TASK-3: Wire did_you_mean into get_definition error responses

Priority: HIGH
Files:
  - `crates/pathfinder/src/server/tools/navigation.rs` (get_definition_impl)
  - `crates/pathfinder/src/server/tools/helpers.rs` (if needed)

Problem:
  `symbols::did_you_mean()` exists and works in tree-sitter but `get_definition_impl` returns `SymbolNotFound` with empty `did_you_mean` vec. When the LSP fails and grep fallback finds nothing, the agent gets zero guidance.

Solution:
  1. When `get_definition_impl` reaches the SYMBOL_NOT_FOUND error, extract the symbol name from the semantic path
  2. Call `self.surgeon.read_source_file()` on the file from the semantic path to get symbols
  3. Use `symbols::did_you_mean()` to compute suggestions
  4. Populate the `did_you_mean` vec in the error

Changes:
  In `navigation.rs` `get_definition_impl`:
  - Before returning the final `SYMBOL_NOT_FOUND` error, add:
    ```rust
    // Try to provide did_you_mean suggestions
    let suggestions = self.compute_did_you_mean(&semantic_path).await;
    return Err(pathfinder_to_error_data(&PathfinderError::SymbolNotFound {
        semantic_path: params.semantic_path,
        did_you_mean: suggestions,
    }));
    ```

  Add helper method:
    ```rust
    async fn compute_did_you_mean(
        &self,
        semantic_path: &SemanticPath,
    ) -> Vec<String> {
        // Extract symbol chain, call surgeon.extract_symbols, then did_you_mean
        // If anything fails, return empty vec (graceful degradation)
    }
    ```

Verification:
  - Run `cargo test -p pathfinder`
  - Add test: `test_get_definition_returns_did_you_mean_on_failure`
  - Manual: query `get_definition("src/auth.rs::logn")` and verify suggestions include "login"

---

### TASK-4: Standardize degraded_reason into an enum

Priority: MEDIUM
Files:
  - `crates/pathfinder-common/src/types.rs` (new enum)
  - `crates/pathfinder/src/server/types.rs` (update response types)
  - `crates/pathfinder/src/server/tools/navigation.rs` (all handlers)
  - `crates/pathfinder/src/server/tools/search.rs` (search handler)
  - `crates/pathfinder/.pi/skills/pathfinder/SKILL.md` (update docs)

Problem:
  `degraded_reason` is a free-form `String`. 11+ distinct string values across the codebase with inconsistent naming. Agents can't reliably parse or branch on these.

Solution:
  1. Create a `DegradedReason` enum in `pathfinder-common/src/types.rs`
  2. Implement `Display` for human-readable output
  3. Replace `Option<String>` with `Option<DegradedReason>` in all response types
  4. Update all tool handlers to use enum variants

Changes:
  In `pathfinder-common/src/types.rs`:
  ```rust
  #[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
  #[serde(rename_all = "snake_case")]
  pub enum DegradedReason {
      NoLsp,
      LspWarmupEmptyUnverified,
      LspWarmupGrepFallback,
      LspTimeoutGrepFallback,
      LspErrorGrepFallback,
      NoLspGrepFallback,
      GrepFallbackFileScoped,
      GrepFallbackImplScoped,
      GrepFallbackGlobal,
      UnsupportedLanguageFilterBypassed,
      UnsupportedLanguage,
  }
  ```

  In response types (`types.rs`):
  - `SearchCodebaseResponse.degraded_reason: Option<DegradedReason>`
  - `ReadWithDeepContextMetadata.degraded_reason: Option<DegradedReason>`
  - `GetDefinitionResponse.degraded_reason: Option<DegradedReason>`
  - `AnalyzeImpactMetadata.degraded_reason: Option<DegradedReason>`
  - `GetRepoMapMetadata.degraded_reason: Option<DegradedReason>`

  In tool handlers:
  - Replace all `Some("no_lsp".to_owned())` with `Some(DegradedReason::NoLsp)`
  - Replace all string matches similarly

Verification:
  - `cargo test -p pathfinder -p pathfinder-common`
  - All existing tests must pass
  - Add test: verify serde serialization produces `snake_case` strings (backward compatible with current agents)
  - Verify JSON output unchanged: `assert_eq!(serde_json::to_string(&DegradedReason::NoLsp)?, "\"no_lsp\"")`

---

### TASK-5: Auto-scale get_repo_map max_tokens based on project size

Priority: MEDIUM
Files:
  - `crates/pathfinder/src/server/tools/repo_map.rs` (get_repo_map_impl)
  - `crates/pathfinder-treesitter/src/repo_map.rs` (generate_skeleton_text)

Problem:
  Default `max_tokens: 16000` is too low for monorepos. Agents waste a round-trip discovering this and re-calling with higher values.

Solution:
  1. Before generating the skeleton, count source files in the project
  2. If file count exceeds threshold (e.g., 20 files), auto-scale `max_tokens`
  3. Formula: `max(16000, min(file_count * 800, 48000))`
  4. Log the auto-scaling so agents know what happened

Changes:
  In `repo_map.rs` `get_repo_map_impl`:
  - Before calling `generate_skeleton_text`, add file counting:
    ```rust
    // Auto-scale token budget for large projects
    let effective_max_tokens = if params.max_tokens == default_max_tokens() {
        // Only auto-scale when the user didn't explicitly set a value
        let source_file_count = count_source_files(/* ... */).await;
        if source_file_count > 20 {
            let scaled = (source_file_count * 800).clamp(16000, 48000);
            tracing::info!(
                tool = "get_repo_map",
                source_file_count,
                auto_scaled_tokens = scaled,
                "auto-scaling max_tokens for large project"
            );
            scaled
        } else {
            params.max_tokens
        }
    } else {
        params.max_tokens // Respect explicit user setting
    };
    ```
  - Pass `effective_max_tokens` to `generate_skeleton_text` instead of `params.max_tokens`
  - Include `max_tokens_used` in the metadata so agents know the effective budget

Verification:
  - `cargo test -p pathfinder`
  - Add test with mock workspace containing 50+ files
  - Verify `max_tokens_used` is scaled up
  - Verify explicit `max_tokens` values are respected (not auto-scaled)

---

### TASK-6: Add max_references parameter to analyze_impact

Priority: MEDIUM
Files:
  - `crates/pathfinder/src/server/types.rs` (AnalyzeImpactParams)
  - `crates/pathfinder/src/server/tools/navigation.rs` (bfs_call_hierarchy)

Problem:
  BFS traversal can return unbounded references. No output budget control. Agents can overflow context.

Solution:
  1. Add `max_references: Option<u32>` to `AnalyzeImpactParams` (default: 50)
  2. Track reference count during BFS traversal
  3. Stop traversal when limit is reached
  4. Include `references_truncated: bool` in metadata

Changes:
  In `types.rs`:
  ```rust
  pub struct AnalyzeImpactParams {
      pub semantic_path: String,
      pub max_depth: u32,
      /// Maximum total references (incoming + outgoing) to return.
      /// Prevents context overflow on large codebases. Default: 50.
      #[serde(default)]
      pub max_references: Option<u32>,
  }
  ```

  In `navigation.rs`:
  - Add `max_references` parameter to `bfs_call_hierarchy`
  - Track cumulative count across both incoming and outgoing BFS passes
  - Stop early when limit reached
  - Set `references_truncated: true` in metadata

  In `AnalyzeImpactMetadata`:
  - Add `references_truncated: bool` field

Verification:
  - `cargo test -p pathfinder`
  - Add test: `test_analyze_impact_respects_max_references`
  - Add test: `test_analyze_impact_default_max_references_is_50`
  - Manual: run with `max_references: 5` and verify truncation

---

### TASK-7: Add max_dependencies parameter to read_with_deep_context

Priority: MEDIUM
Files:
  - `crates/pathfinder/src/server/types.rs` (ReadWithDeepContextParams)
  - `crates/pathfinder/src/server/tools/navigation.rs` (resolve_lsp_dependencies, append_outgoing_deps)

Problem:
  Same as TASK-6 but for read_with_deep_context. Dependencies list is unbounded.

Solution:
  1. Add `max_dependencies: Option<u32>` to `ReadWithDeepContextParams` (default: 30)
  2. Cap the dependencies vec after population
  3. Include `dependencies_truncated: bool` in metadata

Changes:
  Mirror TASK-6 pattern but for `resolve_lsp_dependencies` and `append_outgoing_deps`.

Verification:
  - `cargo test -p pathfinder`
  - Add test: `test_read_with_deep_context_respects_max_dependencies`

---

### TASK-8: Update SKILL.md documentation for edge cases

Priority: LOW-MEDIUM
Files:
  - `crates/pathfinder/.pi/skills/pathfinder/SKILL.md`

Problem:
  Several agent-facing patterns are undocumented:
  - Rust `#N` impl block disambiguator
  - `total_matches` vs `returned_count` semantics (until TASK-1 is done)
  - When to use `project_only` parameter
  - Cross-file method resolution patterns in Go

Solution:
  Update SKILL.md with:
  1. "Rust Impl Blocks" section explaining `#N` disambiguator
  2. "Search Result Counts" section explaining total vs returned
  3. "Stdlib Noise" section recommending `project_only: true` (after TASK-2)
  4. "Cross-File Methods" section with search_codebase workaround for Go
  5. Known limitation: Go LSP reliability

Verification:
  - Read SKILL.md and verify all sections are accurate
  - Cross-reference with actual tool descriptions in server.rs

---

## Implementation Order

Phase 1 (HIGH impact, self-contained):
1. TASK-3: Wire did_you_mean into get_definition errors
2. TASK-1: Fix total_matches semantic confusion
3. TASK-2: Add project_only filter

Phase 2 (MEDIUM impact, parameter additions):
4. TASK-4: Standardize degraded_reason enum
5. TASK-5: Auto-scale max_tokens
6. TASK-6: Add max_references to analyze_impact
7. TASK-7: Add max_dependencies to read_with_deep_context

Phase 3 (Documentation):
8. TASK-8: Update SKILL.md

---

## Verification Checklist

After all tasks are complete, verify the agent experience:

1. Session start: `lsp_health` → honest status with probe_verified
2. Project exploration: `get_repo_map` → auto-scaled tokens, no truncation
3. Symbol search: `search_codebase` → consistent total_matches, no 0/1 confusion
4. Symbol reading: `read_symbol_scope` → works every time
5. Deep context: `read_with_deep_context` → project-only deps, capped output
6. Navigation: `get_definition` → did_you_mean suggestions on failure
7. Impact analysis: `analyze_impact` → no stdlib noise, capped output
8. Error recovery: every error has actionable hint + did_you_mean where applicable
9. Degraded mode: every degraded response has structured reason enum
10. Token budget: all tools respect their output limits

Run full test suite: `cargo test --workspace`
