# GFB-001: Agent Feedback Triage and Remediation

Date: 2026-06-07
Source: 10+ independent agent session reports from Pathfinder MCP usage
Status: Active

## 1. Triage Summary

Agent feedback collected over multiple sessions was cross-referenced against the
current codebase. Many previously-reported issues have been resolved. This
document captures only findings that remain valid and actionable.

### Stale Name Cleanup

The tool was renamed from `analyze_impact` to `find_callers_callees` but
internal references were not fully updated:

- `health.rs:714` — `tool: "analyze_impact"` in `compute_degraded_tools()`
- `health.rs:319` — `tool: "analyze_impact"` in `lsp_health_impl()` missing-language path
- `health.rs:1485-1496` — test references `"analyze_impact"` in assertions

Internal struct names (`AnalyzeImpactParams`, `AnalyzeImpactMetadata`,
`analyze_impact_impl`) are acceptable as implementation details but should
eventually be renamed for consistency. The user-facing tool name is the priority.

### Already Resolved (Do Not Reimplement)

| Item | Implementation |
|---|---|
| find_symbol tool | `server/tools/find_symbol.rs` (738 lines) |
| read_files batch tool | `server/tools/read_files.rs` (1163 lines) |
| symbol_overview composite | `navigation/overview.rs` (1041 lines) |
| Actionable degraded guidance | `ActionableGuidance` with retry_after_seconds, fallback_tool, trust_level |
| filter_mode hint on 0 results | `search.rs:169-180` |
| LSP status in get_repo_map | `lsp_status: HashMap` in metadata |
| results_truncated + next_offset | Present on search response |
| coverage_percent | `files_searched`/`files_in_scope`/`coverage_percent` |
| duration_ms on all responses | All 11 response types |
| VersionHash::compute_from_raw | Pre-allocated hex, no format!() |
| Rc<str> for file path sharing | `MatchCollector` at `ripgrep.rs:122` |
| file-start index for backfill | Range-based at `ripgrep.rs:505` |
| Shared exclusion constants | `ALWAYS_EXCLUDED_DIRS` in pathfinder-common |
| UTF-8 validation truncation | `safe_truncate_bytes` at `ripgrep.rs:70` |
| get_or_parse_vue_preloaded | `cache.rs:469` |
| Running token counter O(n) | `current_tokens` accumulator |
| did_you_mean two-phase | Exact match at `symbols.rs:975` |
| Vue template/style symbol extraction | `symbols.rs:1133`, `symbols.rs:1462` |
| WorkspaceRoot::resolve &Path | Already takes `&Path` |
| Cross-file did_you_mean | `enrich_did_you_mean` searches workspace |
| Separator auto-correction | `try_separator_correction` |
| Bounded parallelism | `buffer_unordered(32)` |
| Java LSP support | JavaPlugin with jdtls |
| is_definition on search results | `enrich_matches` in `search.rs:279` |

### Discarded (Not Worth Pursuing)

| Item | Reason |
|---|---|
| Sandbox pattern trie | Under 20 patterns. Linear scan is fine. |
| SmolStr for symbol names | Not a measured bottleneck. Premature. |
| Search result caching | Rarely repeated queries. Low hit rate. |
| Memory-mapped file I/O | OS page cache sufficient. Benchmark first. |
| Stream walk_files | Vec fine for typical repos. Measure first. |
| Cross-language data flow tracing | Hard problem. Out of scope. |
| Workflow API server-side | symbol_overview already composes. |
| Shorter tool name aliases | Cosmetic. Agents don't care. |
| path_glob on get_repo_map | `path` param scopes to subdirectory. |
| Staleness detection | `version_hash` per file already enables this. |

---

## 2. Deliverables

Progressive bytesized tasks. Each is independently testable and shippable.

### DELIVERABLE A: Stale Name Cleanup
Priority: Trivial
Effort: 15 minutes
Risk: None

**Task**: Rename `analyze_impact` references in user-facing output to
`find_callers_callees`.

**Steps**:
1. In `crates/pathfinder/src/server/tools/navigation/health.rs` line 714:
   Change `tool: "analyze_impact".to_owned()` to
   `tool: "find_callers_callees".to_owned()`
2. In same file line 319: Change `tool: "analyze_impact".to_owned()` to
   `tool: "find_callers_callees".to_owned()`
3. In same file lines 1485-1496: Update test assertions from
   `"analyze_impact"` to `"find_callers_callees"`
4. Run `cargo test -p pathfinder -- test_health_shows_degraded_tools`

**Acceptance**: `lsp_health` response shows `find_callers_callees` instead of
`analyze_impact` in `degraded_tools`.

---

### DELIVERABLE B: find_all_references Grep Fallback
Priority: P0 (every agent report flagged this)
Effort: Medium (2-3 hours)
Risk: Low (reuse existing grep_reference_fallback from impact.rs)

**Problem**: `find_all_references` returns `references: None` when LSP is
unavailable. Zero fallback. Agent gets nothing and must manually call
`search_codebase`.

**Steps**:

B1. In `crates/pathfinder/src/server/tools/navigation/references.rs`, add a
    new function `grep_references_fallback` after the existing error handling
    (after line ~387):

```rust
async fn grep_references_fallback(
    &self,
    symbol_name: &str,
    file_path: &str,
    params: &FindAllReferencesParams,
) -> (Vec<ReferenceLocation>, Option<DegradedReason>, ActionableGuidance)
```

B2. Implementation:
    - Call `self.search_codebase_impl` with:
      - `query: format!(r"\b{}\b", regex::escape(symbol_name))`
      - `filter_mode: CodeOnly`
      - `is_regex: true`
      - `path_glob: None` (search all source files)
      - `max_results: params.max_results.unwrap_or(50)`
      - `offset: params.offset.unwrap_or(0)`
    - Filter out matches where `file == file_path` and the match line matches
      a definition pattern for that language (to exclude the definition itself)
    - Convert `SearchMatch` to `ReferenceLocation` using
      `file`, `line`, `column`, `enclosing_semantic_path`
    - Set `direction: "reference_heuristic"` to distinguish from LSP results

B3. Wire into `find_all_references_impl`:
    - After all LSP error paths (NoLspAvailable, Timeout, other errors)
    - Call `self.grep_references_fallback(...)` instead of returning `None`
    - Set `degraded: true`, `degraded_reason: GrepFallback`
    - Include `actionable_guidance` with `trust_level: "heuristic"`

B4. Update response types:
    - `FindAllReferencesMetadata` already has `degraded` and
      `degraded_reason` fields. No type changes needed.

B5. Add test:
    - Test with `MockLawyer` that returns error on `references()`
    - Verify references are returned with `degraded: true`
    - Verify definition file is excluded from results
    - Verify pagination works with offset/limit

**Files to modify**:
- `crates/pathfinder/src/server/tools/navigation/references.rs`
- No type changes needed

**Acceptance**:
- `find_all_references` returns results when LSP is down
- Results tagged `degraded: true` with `source: "grep_fallback"`
- Definition site excluded
- Pagination via offset/limit works

---

### DELIVERABLE C: Vue Grep Fallback Patterns
Priority: P0 (Vue-heavy projects get no grep fallback quality)
Effort: Medium (2-3 hours)
Risk: Low

**Problem**: Vue files fall through to bare `\b{name}\b` in ALL grep fallbacks
(definition, deep_context, impact). This means `const handleClick = () => {}`
in `<script setup>` is never matched as a definition.

**Steps**:

C1. In `crates/pathfinder/src/server/tools/navigation/mod.rs`, add `"vue"`
    arm to `definition_patterns()` (around line 380):

```rust
"vue" => vec![
    // Named function declarations
    format!(r"(?:export\s+)?(?:async\s+)?function\s+{escaped}\s*\("),
    // const/let/var assignments (ref, reactive, computed, arrow functions)
    format!(r"(?:export\s+)?(?:const|let|var)\s+{escaped}\s*[=:]"),
    // defineProps, defineEmits, defineExpose macros
    format!(r"(?:const|let)\s+{escaped}\s*=\s*(?:defineProps|defineEmits|defineExpose|defineModel|withDefaults)\("),
],
```

C2. In same file `call_pattern_full()` (line ~187), add `"vue"` to the group
    alongside `"typescript"`, `"javascript"`, `"python"`:
    - Vue `<script setup>` uses same call patterns as TS (method chains,
      dot notation, `this.method()`)

C3. In same file `keywords_for_language()` (line ~231), add `"vue"` arm:
    ```rust
    "vue" => [
        "defineProps", "defineEmits", "defineExpose", "defineModel",
        "withDefaults", "ref", "reactive", "computed", "watch",
        "watchEffect", "onMounted", "onUnmounted", "provide", "inject",
        "toRef", "toRefs", "useSlots", "useAttrs", "useTemplateRef",
        // Vue template directives (not keywords per se, but common false positives)
        "template", "script", "style", "setup",
    ].iter().copied().collect(),
    ```

C4. In `crates/pathfinder/src/server/tools/navigation/deep_context.rs`,
    add `"vue"` arm to `resolve_candidate_via_grep()` (around line 199):
    - Use same patterns as TypeScript since Vue `<script>` is TS/JS

C5. Add tests:
    - Test `definition_patterns("vue", "handleClick", None)` matches
      `const handleClick = () => {}`
    - Test `definition_patterns("vue", "count", None)` matches `const count = ref(0)`
    - Test `call_pattern_full` for vue extracts `service.validate()` calls

**Files to modify**:
- `crates/pathfinder/src/server/tools/navigation/mod.rs`
- `crates/pathfinder/src/server/tools/navigation/deep_context.rs`

**Acceptance**:
- `get_definition` grep fallback finds `const x = () => {}` in `.vue` files
- `read_with_deep_context` grep fallback extracts callees from `<script setup>`
- `find_callers_callees` grep fallback finds references in `.vue` files

---

### DELIVERABLE D: Python LSP Detection Fix
Priority: P0 (pyright installed but Python LSP shows unavailable)
Effort: Low (1 hour)
Risk: Low

**Problem**: Pathfinder only checks `pyright-langserver`, never `pyright`.
Also `ruff-lsp` is deprecated since ruff 0.4.0 (May 2024). The install hint
is misleading.

**Steps**:

D1. In `crates/pathfinder-lsp/src/plugin.rs`, update
    `PythonPlugin::lsp_candidates()` (lines ~192-210):
    - Add `("pyright", &["--stdio"])` as second candidate
    - Replace `("ruff-lsp", &[])` with `("ruff", &["server", "--stdio"])`
    - Update `install_hint` to mention `pyright-langserver` specifically

D2. In `crates/pathfinder-lsp/src/client/detect.rs`, update the hardcoded
    `python_lsp_candidates` (lines ~691-696) to match plugin.rs exactly.
    NOTE: This is a redundancy that should be eliminated — detect.rs should
    import from plugin.rs. For this deliverable, just keep them in sync.

D3. Add test:
    - Test that `PythonPlugin::lsp_candidates()` returns 5 candidates
    - Test that second candidate is `("pyright", &["--stdio"])`
    - Test that `ruff` candidate uses `["server", "--stdio"]` args

**Files to modify**:
- `crates/pathfinder-lsp/src/plugin.rs`
- `crates/pathfinder-lsp/src/client/detect.rs`

**Acceptance**:
- `pyright` (not just `pyright-langserver`) is detected as valid Python LSP
- `ruff server --stdio` works as fallback
- Install hint is accurate about which binary is needed

---

### DELIVERABLE E: Java Grep Pattern Completeness
Priority: P1 (Java projects get poor grep fallback)
Effort: Low (1-2 hours)
Risk: Low

**Problem**: Java patterns miss constructors, records, annotated methods,
primitive return types.

**Steps**:

E1. In `crates/pathfinder/src/server/tools/navigation/mod.rs`, update the
    `"java"` arm of `definition_patterns()` (lines ~413-421):

Add these patterns:
```rust
// Constructor (class name followed by parens, no return type)
format!(r"(?:public|protected|private)?\s*(?:\w+(?:<[^>]+>)?\s+)*{parent}\s*\("),
// Record type
format!(r"(?:public|protected|private)?\s*(?:final\s+)?record\s+{escaped}\s*[<(]"),
// Sealed class/interface with permits
format!(r"(?:public|protected|private)?\s*(?:sealed\s+)?(?:abstract\s+)?(?:class|interface)\s+{escaped}\s"),
// Method with annotations (e.g., @Bean, @Override)
format!(r"(?:@\w+(?:\([^)]*\))?\s+)*(?:(?:public|private|protected|static|final|abstract|synchronized|native|default)\s+)*(?:[\w<>\[\],\s?]+)\s+{escaped}\s*\("),
```

E2. In `crates/pathfinder/src/server/tools/navigation/deep_context.rs`,
    update Java pattern in `resolve_candidate_via_grep()` (line ~210):

Change from `[A-Z][a-zA-Z0-9_]*\s+{candidate}` to:
```rust
format!(r"(?:void|boolean|int|long|double|float|short|byte|char|[A-Z][a-zA-Z0-9_]*(?:<[^>]+>)?(?:\[\])?)\s+{candidate}\s*\(")
```

E3. Add annotation prefix to Java method pattern:
```rust
format!(r"(?:@\w+(?:\([^)]*\))?\s+)*(?:void|boolean|int|long|double|float|short|byte|char|[A-Z][a-zA-Z0-9_]*(?:<[^>]+>)?(?:\[\])?)\s+{candidate}\s*\(")
```

E4. Add tests for:
    - Constructor: `public MyClass(String name)` resolves via grep
    - Record: `public record Person(String name)` resolves
    - Annotated: `@Bean public MyService myService()` resolves
    - Primitive return: `void process()` resolves

**Files to modify**:
- `crates/pathfinder/src/server/tools/navigation/mod.rs`
- `crates/pathfinder/src/server/tools/navigation/deep_context.rs`

**Acceptance**:
- Constructors found by `get_definition` grep fallback
- Records found by `get_definition` grep fallback
- Annotated methods found
- Primitive return type methods found

---

### DELIVERABLE F: Outgoing Deps in find_callers_callees Grep Fallback
Priority: P1
Effort: Medium (2 hours)
Risk: Low

**Problem**: `find_callers_callees` grep fallback only finds incoming refs.
Outgoing is `None`. `extract_call_candidates` exists but is not used here.

**Steps**:

F1. In `crates/pathfinder/src/server/tools/navigation/impact.rs`, after the
    existing `grep_reference_fallback` for incoming refs (line ~116):

    - Read the symbol's source via `self.read_symbol_scope_enriched(...)`
    - Call `extract_call_candidates(&source, language)` from `mod.rs`
    - For each candidate, call `resolve_candidate_via_grep(...)` to find
      its definition
    - Deduplicate by semantic_path
    - Populate the `outgoing` field

F2. The `extract_call_candidates` function is in `mod.rs` (lines ~186-209).
    It takes source text and language, returns `Vec<String>` of candidate
    function/method names.

F3. The `resolve_candidate_via_grep` function is in `deep_context.rs`
    (lines ~192-331). It takes a candidate name and language, returns
    `Option<ResolvedDependency>`.

F4. Wire results into the existing `ImpactSummary` struct:
    - `outgoing: Some(vec![...])` instead of `outgoing: None`
    - Tag each entry with `direction: "outgoing_heuristic"`

F5. Add test:
    - Test with function that calls other functions
    - Verify outgoing refs appear in grep fallback mode
    - Verify deduplication works

**Files to modify**:
- `crates/pathfinder/src/server/tools/navigation/impact.rs`

**Acceptance**:
- `find_callers_callees` grep fallback returns both incoming AND outgoing
- Outgoing tagged as `direction: "outgoing_heuristic"`
- Deduplication by semantic_path works

---

### DELIVERABLE G: Graceful Fallback for Unsupported Languages
Priority: P1
Effort: Low (1-2 hours)
Risk: Low

**Problem**: `read_source_file` errors on `.sql`, `.yaml`, `.toml` files
instead of returning raw content.

**Steps**:

G1. In `crates/pathfinder/src/server/tools/source_file.rs`, catch
    `SurgeonError::UnsupportedLanguage` before returning error:

    - Detect unsupported language
    - Read raw file content via `std::fs::read_to_string`
    - Return response with:
      - `source`: raw content
      - `language`: file extension (not "unknown")
      - `detail_level`: "source_only"
      - `symbols`: empty/None
      - Metadata: `unsupported_language: Some(true)`

G2. Add `unsupported_language: Option<bool>` to `ReadSourceFileMetadata`
    in `types.rs`.

G3. Add test:
    - Call `read_source_file` on a `.sql` file
    - Verify raw content returned (not error)
    - Verify `unsupported_language: true` in metadata

**Files to modify**:
- `crates/pathfinder/src/server/tools/source_file.rs`
- `crates/pathfinder/src/server/types.rs`

**Acceptance**:
- `.sql` files return raw content instead of error
- Metadata indicates `unsupported_language: true`

---

### DELIVERABLE H: TypeScript Glob Fix
Priority: P1
Effort: Trivial (5 minutes)
Risk: None

**Problem**: `language_to_file_glob("typescript")` returns `**/*.ts` only,
missing `.tsx` files.

**Steps**:

H1. In `crates/pathfinder/src/server/tools/navigation/mod.rs` line ~354:
    Change `"**/*.ts"` to `"**/*.{ts,tsx}"`

H2. Run existing tests. Add test if none covers this case.

**Files to modify**:
- `crates/pathfinder/src/server/tools/navigation/mod.rs`

**Acceptance**:
- `language_to_file_glob("typescript")` returns `"**/*.{ts,tsx}"`

---

### DELIVERABLE I: Search Coverage Exclusion Reasons
Priority: P2
Effort: Medium (2-3 hours)
Risk: Low

**Problem**: `files_searched < files_in_scope` with no explanation why.
Agents cannot decide whether to re-scope or accept.

**Steps**:

I1. In `crates/pathfinder-search/src/types.rs`, add to `SearchResult`:
    ```rust
    pub binary_skipped: usize,
    pub gitignored_skipped: usize,
    pub other_skipped: usize,
    ```

I2. In `crates/pathfinder-search/src/ripgrep.rs`, track skipped files
    during `walk_files()`:
    - When WalkBuilder skips a file, categorize why
    - Binary detection: check extension against known binary extensions
    - Gitignored: WalkBuilder respects .gitignore, count entries it skips
    - Increment counters

I3. In `crates/pathfinder/src/server/types.rs`, add to
    `SearchCodebaseResponse`:
    ```rust
    pub binary_skipped: usize,
    pub gitignored_skipped: usize,
    pub other_skipped: usize,
    ```

I4. In `crates/pathfinder/src/server/tools/search.rs`, map from
    `SearchResult` to response.

I5. Add test:
    - Repo with mixed binary/source files
    - Verify breakdown shows binary count
    - Verify coverage_percent reflects accurate ratio

**Files to modify**:
- `crates/pathfinder-search/src/types.rs`
- `crates/pathfinder-search/src/ripgrep.rs`
- `crates/pathfinder/src/server/types.rs`
- `crates/pathfinder/src/server/tools/search.rs`

**Acceptance**:
- `search_codebase` response includes breakdown of why files were skipped
- Agents can distinguish binary vs gitignored vs other exclusions

---

### DELIVERABLE J: Repo Map Truncated Paths
Priority: P2
Effort: Low (1-2 hours)
Risk: Low

**Problem**: `get_repo_map` only reports `files_truncated: usize` count.
No list of which files were cut.

**Steps**:

J1. In `crates/pathfinder-treesitter/src/repo_map.rs`:
    - Add `truncated_paths: Vec<String>` to `RepoMapResult`
    - In `generate_skeleton_text`, collect file paths when they exceed
      `max_tokens_per_file`

J2. In `crates/pathfinder/src/server/types.rs`:
    - Add `truncated_paths: Vec<String>` to `GetRepoMapMetadata`

J3. In `crates/pathfinder/src/server/tools/repo_map.rs`:
    - Map from `RepoMapResult` to response metadata

J4. Add test:
    - Call `get_repo_map` with low `max_tokens_per_file`
    - Verify truncated file paths listed

**Files to modify**:
- `crates/pathfinder-treesitter/src/repo_map.rs`
- `crates/pathfinder/src/server/types.rs`
- `crates/pathfinder/src/server/tools/repo_map.rs`

**Acceptance**:
- `get_repo_map` response lists which files were truncated

---

### DELIVERABLE K: Content-Hash Cache Invalidation
Priority: P2
Effort: Medium (2 hours)
Risk: Low

**Problem**: Cache uses mtime for invalidation. Unreliable on Docker/CI
mounts where mtime may change without content change.

**Steps**:

K1. In `crates/pathfinder-treesitter/src/cache.rs`, modify invalidation:
    - Keep mtime as fast-path (unchanged mtime = guaranteed same content)
    - When mtime differs: compute content hash, compare with stored hash
    - If hash matches despite mtime change: keep cache, update stored mtime
    - If hash differs: invalidate

K2. Add test:
    - Touch a file (changes mtime, not content)
    - Verify cache hit (no re-parse)

**Files to modify**:
- `crates/pathfinder-treesitter/src/cache.rs`

**Acceptance**:
- Cache hit when mtime changes but content unchanged
- Cache miss when content changes

---

### DELIVERABLE L: Compiled Regex Caching
Priority: P3 (profile first)
Effort: Medium (2-3 hours)
Risk: Low

**Prerequisite**: Profile `find_symbol` (which calls search multiple times
with same pattern). If search latency is under 200ms, skip this.

**Steps**:

L1. Add `lru` crate to `crates/pathfinder-search/Cargo.toml`

L2. Add `LruCache<String, RegexMatcher>` field to `RipgrepScout` or as
    a static/thread-local. Wrap in `Mutex`. Cap at 32 entries.

L3. In `build_matcher`, check cache before compiling.

L4. Benchmark before/after with `find_symbol` workload.

**Files to modify**:
- `crates/pathfinder-search/Cargo.toml`
- `crates/pathfinder-search/src/ripgrep.rs`

**Acceptance**:
- Repeated searches with same pattern avoid re-compilation
- No regression on unique-pattern searches

---

## 3. Dependency Order

```
A (standalone, no deps)
|
B (standalone, no deps)
|
C (standalone, no deps)     D (standalone, no deps)     E (standalone, no deps)
|                           |                           |
+--- F (depends on C for Vue call candidates being available)
    |
    G (standalone)          H (standalone)
    |
    I (standalone)          J (standalone)              K (standalone)
    |
    L (standalone, profile first)
```

B, C, D, E can be done in parallel.
F should wait for C (Vue call pattern support).
G, H, I, J, K can be done in any order after B-F.
L is last resort, profile first.

## 4. Suggested Implementation Order

Batch 1 (quick wins, same session): A, D, H
Batch 2 (core grep improvements): B, C, E
Batch 3 (grep completeness): F, G
Batch 4 (transparency): I, J, K
Batch 5 (perf): L (only if profiling warrants)
