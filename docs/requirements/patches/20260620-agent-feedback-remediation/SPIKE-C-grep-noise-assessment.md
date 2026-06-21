# SPIKE-C: Grep Noise Reduction Assessment — Findings

## Status: ASSESSED — Priority A implemented; Priorities B-D deferred

## Grep Fallback Invocation Points

There are 5 distinct grep fallback paths across 4 modules:

### A) impact.rs — `trace(scope="callers")`
- `grep_reference_fallback` (~L49): Triggered when LSP `call_hierarchy_incoming`
  fails/unavailable. Uses `search_codebase_impl` with TEXT mode, `filter_mode=CodeOnly`.
  Query = bare symbol name (not regex). Takes top 10 of 20 results.

### B) impact.rs — `trace(scope="callees")`
- `grep_outgoing_fallback` (~L130): Triggered when LSP `call_hierarchy_outgoing` fails.
  Extracts call candidates via regex (`extract_call_candidates`), then resolves each
  via `candidate_definition_pattern`. Dedup by enriched `semantic_path`.

### C) references.rs — `trace(scope="references")`
- `grep_references_fallback` (~L34): Uses REGEX mode with `\b{name}\b`.
  Excludes definition line in same file + secondary exclusion via `definition_patterns`.

### D) definition.rs — `locate(semantic_path)`
- `fallback_definition_grep` (~L478): 4-strategy cascade:
  1. `grep_definition_in_file` — language-aware patterns in expected file
  2. `grep_impl_method` — finds `impl Parent` blocks, then method within
  3. `grep_definition_global` — global search with visibility+keyword regex
  4. `grep_symbol_broad` — bare `\b{name}\b` across all files

### E) deep_context.rs — `inspect(include_dependencies=true)`
- `attempt_grep_fallback` (~L227): Same pattern as impact.rs outgoing — extracts
  call candidates, resolves each via `candidate_definition_pattern`.

## Current Filtering Mechanisms

| Mechanism | Where Used | Effect |
|---|---|---|
| `is_source_file` | All fallbacks | Restricts to 12 extensions |
| `is_workspace_file` | Outgoing fallbacks | Excludes node_modules, vendor, stdlib |
| `filter_mode=CodeOnly` | `grep_reference_fallback` | Tree-sitter classifies matches as code/comment/string |
| Definition line exclusion | references.rs | Same-file matches on definition line removed |
| Test/mock dir exclusion | definition.rs strategies 3-4 | `exclude_glob: ["**/{test,tests,mock}*/**"]` |
| Semantic path dedup | impact.rs outgoing, deep_context.rs | Dedup by enriched path |

## Identified Noise Sources

### 1. No Word Boundary in `grep_reference_fallback` (impact.rs)
**Severity: HIGH**. Uses TEXT mode (substring match). Searching for `new` also matches
`new_connection`, `renew`, etc. Compare with references.rs which uses `\b{name}\b` regex.
**Fix: 2-line change** — switch to REGEX mode with `\b{name}\b`.

### 2. No Scope Awareness in Reference Searches
**Severity: MEDIUM**. Bare symbol name search for common names (`new`, `get`, `handle`)
matches every occurrence across the codebase regardless of type/module scope.
No verification that matched occurrences refer to the same semantic entity.

### 3. Call Candidate Extraction is Regex-Only
**Severity: LOW**. `extract_call_candidates` uses regex to find function-call-like
patterns. Can capture false positives from macro invocations and conditional expressions.
Capped at 20 candidates with no priority ordering.

### 4. First-Match Strategy in Definition Resolution
**Severity: LOW**. All definition grep strategies return the first match. Multiple
definitions for the same name (overloads, different modules) may select the wrong one.

### 5. No Import/Dependency Graph Filtering
**Severity: MEDIUM**. No fallback verifies that matching files actually import/depend
on the target module. Unrelated symbols with the same name in different modules match.

## Tree-sitter Post-Filtering Status

Tree-sitter is used for **enrichment** but NOT for **scope filtering**:
- `enrich_matches` → populates `enclosing_semantic_path` and classifies node type
- `filter_mode=CodeOnly` → filters out comment/string matches
- `enclosing_semantic_path` exists and COULD be used for scope validation
  but currently only annotates results — does not filter them

## Recommendations (Priority Order)

### Priority A — Word Boundary Fix (Low effort, HIGH impact)
Fix `grep_reference_fallback` in impact.rs to use REGEX mode with `\b{name}\b`
instead of TEXT mode. This is a 2-line change that eliminates substring false positives.

### Priority B — Tree-sitter Scope Filtering (Medium effort, HIGH impact)
After grep matches are enriched with `enclosing_semantic_path`, compare the target
symbol's parent type against each match's enclosing scope to down-rank or exclude
matches that reference a different symbol with the same name.
**Zero additional parsing cost** — `enrich_matches` already does concurrent Tree-sitter
parsing and the data is already available in `enclosing_semantic_path`.

### Priority C — Confidence Scoring (Medium effort, MEDIUM impact)
Extend existing `confidence: Some("heuristic")` annotation with levels:
- `high` — same file as definition
- `medium` — file imports target module
- `low` — unrelated file with same-name match

### Priority D — Import Graph (High effort, MEDIUM impact)
Build cross-file import graph from Tree-sitter. High implementation cost.
Not recommended as first step.

## Files Analyzed

- `crates/pathfinder/src/server/tools/navigation/impact.rs`
- `crates/pathfinder/src/server/tools/navigation/references.rs`
- `crates/pathfinder/src/server/tools/navigation/definition.rs`
- `crates/pathfinder/src/server/tools/navigation/deep_context.rs`
- `crates/pathfinder/src/server/tools/navigation/mod.rs`
- `crates/pathfinder/src/server/tools/search.rs`
