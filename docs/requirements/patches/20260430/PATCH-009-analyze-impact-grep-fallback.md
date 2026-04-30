# PATCH-009: Add Grep Fallback to analyze_impact When Degraded

## Group: C (Medium) — Validation & Fallback Improvements

## Objective

When `analyze_impact` returns degraded (0/0), it's strictly worse than a simple
`search_codebase` for the same query. Add a grep-based caller analysis fallback
that returns heuristic incoming references when LSP is unavailable.

## Severity: MEDIUM — degraded mode returns zero actionable data

## Background

`analyze_impact` currently:
1. Calls `call_hierarchy_prepare` with the symbol position
2. If LSP returns items, does BFS traversal -> great results
3. If LSP returns empty, does verification probe
4. If degraded, returns `incoming: null, outgoing: null` (agent has zero data)

A grep-based fallback can find files that reference the symbol name, returning
them as `direction: "incoming_heuristic"` candidates. Not as precise as LSP,
but far more useful than null.

## Scope

| # | File | Action |
|---|------|--------|
| 1 | `crates/pathfinder/src/server/tools/navigation.rs` | Add grep fallback in degraded path of `analyze_impact_impl` |

## Step 1: Add grep fallback for degraded analyze_impact

**File:** `crates/pathfinder/src/server/tools/navigation.rs`

Find the degraded path in `analyze_impact_impl` — the branch where the LSP probe
fails and `degraded` remains true. Before returning the response with `incoming: None,
outgoing: None`, add a grep-based heuristic:

```rust
// When LSP is degraded, fall back to ripgrep-based caller analysis.
// This finds files that reference the symbol name, returning them as
// heuristic incoming references. Not as precise as LSP call hierarchy,
// but far more actionable than returning null.
if degraded && incoming.is_none() {
    if let Some(symbol_chain) = &semantic_path.symbol_chain {
        if let Some(symbol_name) = symbol_chain.segments.last() {
            let grep_refs = self.grep_caller_references(
                &symbol_name.name,
                &semantic_path.file_path,
            ).await;

            if !grep_refs.is_empty() {
                incoming = Some(grep_refs);
                degraded_reason = Some(format!(
                    "{}; grep_fallback: incoming references are heuristic (pattern-matched, not call-graph verified)",
                    degraded_reason.as_deref().unwrap_or("degraded")
                ));
            }
        }
    }
}
```

Add the `grep_caller_references` helper method:

```rust
    /// Search the codebase for files that reference a symbol name.
    /// Returns heuristic incoming references (callers) with `direction: "incoming_heuristic"`.
    async fn grep_caller_references(
        &self,
        symbol_name: &str,
        definition_file: &std::path::Path,
    ) -> Vec<crate::server::types::ImpactReference> {
        let pattern = format!(r"\b{symbol_name}\b");
        let search_result = self
            .scout
            .search(&pathfinder_search::SearchParams {
                workspace_root: self.workspace_root.path().to_path_buf(),
                query: pattern,
                is_regex: true,
                max_results: 20,
                path_glob: "**/*".to_owned(),
                exclude_glob: String::default(),
                context_lines: 0,
            })
            .await;

        let Ok(result) = search_result else {
            return vec![];
        };

        result
            .matches
            .into_iter()
            .filter(|m| {
                // Exclude the definition file itself (it's not a caller)
                let m_path = std::path::Path::new(&m.file);
                m_path != definition_file
            })
            .take(10) // Cap at 10 heuristic references
            .filter_map(|m| {
                Some(crate::server::types::ImpactReference {
                    semantic_path: format!("{}::{}", m.file, m.enclosing_semantic_path.as_deref().unwrap_or("unknown")),
                    file: m.file,
                    line: m.line as usize,
                    snippet: m.content,
                    version_hash: m.version_hash,
                    direction: "incoming_heuristic".to_owned(),
                    depth: 0,
                })
            })
            .collect()
    }
```

## EXCLUSIONS — Do NOT Modify These

- The `ImpactReference` struct — already has `direction: String` which supports "incoming_heuristic"
- The non-degraded LSP path — that works correctly
- `read_with_deep_context` — it could also benefit from a grep fallback, but that's a
  different tool with different response semantics. Track separately.

## Verification

```bash
# 1. Confirm grep_caller_references exists
grep -n 'grep_caller_references' crates/pathfinder/src/server/tools/navigation.rs

# 2. Confirm incoming_heuristic direction is used
grep -n 'incoming_heuristic' crates/pathfinder/src/server/tools/navigation.rs

# 3. Run navigation tests
cargo test -p pathfinder navigation

# 4. Full verification
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

## Expected Impact

When LSP is degraded:
- Before: `incoming: null, outgoing: null, degraded: true` — zero actionable data
- After: `incoming: [{...direction: "incoming_heuristic"...}], outgoing: null, degraded: true` —
  agent has candidate callers to investigate, clearly labeled as heuristic
