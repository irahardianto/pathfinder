# PATCH-002: Trait Method Resolution

Date: 2026-06-22
Source: Pathfinder report (Rust stack) — "trait method semantic paths don't resolve in trace"
Priority: P0 — correctness bug; Rust traits with signature-only methods are untraceable
Status: Spec — awaiting implementation

## Problem Statement

When an agent calls `trace(semantic_path="crates/foo.rs::Scout.search")`
where `Scout` is a Rust trait and `search` is a signature-only method
declaration (`fn search(&self);` with no body), the call fails with
`SYMBOL_NOT_FOUND`.

v1 SPIKE-A (commit `c3c873a`) was claimed to fix this but addressed a
DIFFERENT problem — stripping `super::`/`crate::`/`self::` path prefixes
from impl block names in tree-sitter extraction. The trait method
resolution problem has a separate root cause.

### Root Cause 1: `function_signature_item` Not Extracted

File: `crates/pathfinder-treesitter/src/language.rs:126-135`

```rust
Self::Rust => &LanguageNodeTypes {
    function_kinds: &["function_item"],
    class_kinds: &["struct_item", "enum_item", "trait_item", "type_item"],
    method_kinds: &[],
    impl_kinds: &["impl_item"],
    ...
}
```

`function_kinds` contains ONLY `function_item`. In tree-sitter-rust 0.23.3,
trait method declarations WITHOUT bodies are `function_signature_item`
nodes (grammar ends with `;`), while `function_item` nodes have bodies
(`field('body', $.block)`).

A `function_signature_item` is in NEITHER `function_kinds` NOR
`class_kinds` → `determine_symbol_kind` returns `None` → the node is
skipped. Trait method signatures are invisible to the symbol tree.

Extraction flow (`symbols.rs:88-120` `determine_symbol_kind`):
```rust
if self.types.function_kinds.contains(&kind) {   // "function_item" only
    return Some(SymbolKind::Function);
}
if self.types.class_kinds.contains(&kind) {
    return Some(refine_class_kind(node));   // trait_item -> Interface
}
```

A `function_signature_item` matches neither → returns `None` → skipped.

Consequence: `resolve_symbol_chain` (`treesitter_surgeon.rs:108-112`)
finds `Scout` (Interface) at segment 1, then looks for `search` in
`Scout.children` — but `search` was never extracted → returns `None` →
`SurgeonError::SymbolNotFound`.

Edge case: if the trait method HAS a default body (`fn search() { ... }`),
it IS a `function_item` and resolves. The common case (signature-only
trait methods) fails.

### Root Cause 2: `trace` Never Calls `goto_implementation`

File: `crates/pathfinder/src/server/tools/navigation/impact.rs:492-626`

`find_callers_callees_impl` has no special handling for trait vs struct
methods. Flow: `read_symbol_scope_enriched` → `call_hierarchy_prepare` →
`bfs_call_hierarchy`. It never calls `goto_implementation`.

`goto_implementation` EXISTS in the Lawyer trait
(`lawyer_impl.rs:264-330`) and IS wired into `find_all_references`
(`references.rs:304-312`), but NOT into `trace`. And even
`find_all_references` fails first at `read_symbol_scope_enriched`
(`references.rs:254-256`) for the same `function_signature_item` reason —
`goto_implementation` is unreachable for trait methods.

### Root Cause 3: `did_you_mean` Unreliable for Cross-File Impls

File: `crates/pathfinder-treesitter/src/symbols.rs:1001-1061` and
`crates/pathfinder/src/server/tools/navigation/mod.rs:544-603`

`did_you_mean` collects semantic paths from the SAME FILE only and does
fuzzy Levenshtein matching. For target `Scout.search`,
`MockScout.search` (Levenshtein 4, threshold 5) matches IF `MockScout`
is in the same file. Cross-file impls are invisible.

`enrich_did_you_mean` (`mod.rs:544-603`) does cross-file search via
`find_symbol_impl`, but ONLY when same-file suggestions are EMPTY
(line 567):
```rust
if suggestions.is_empty() {
    ... cross-file find_symbol_impl ...
}
```

If same-file `did_you_mean` found ANY fuzzy match (e.g. the trait `Scout`
itself), cross-file search is skipped. The cross-file search also queries
by the bare last-segment name (`search`), which returns many unrelated
symbols, not specifically Scout impls.

### Agent Impact

Any Rust codebase using traits with signature-only methods (the common
case) cannot trace trait methods. Agents get `SYMBOL_NOT_FOUND` and must
manually search for impl methods, then trace each individually. This is
the #1 friction source reported by the Pathfinder assessment.

---

## DELIVERABLE A: Extract `function_signature_item` as a Symbol

Priority: P0
Effort: Medium (1 hour)
Risk: Low (additive — new node kind recognized, existing extraction unchanged)

**Steps**:

1. In `crates/pathfinder-treesitter/src/language.rs:127`, add
   `"function_signature_item"` to Rust `function_kinds`:

   ```rust
   Self::Rust => &LanguageNodeTypes {
       function_kinds: &["function_item", "function_signature_item"],
       class_kinds: &["struct_item", "enum_item", "trait_item", "type_item"],
       ...
   }
   ```

2. In `crates/pathfinder-treesitter/src/symbols.rs`, verify
   `determine_symbol_kind` now classifies `function_signature_item` as
   `SymbolKind::Function`. It should, since the check is:
   ```rust
   if self.types.function_kinds.contains(&kind) {
       return Some(SymbolKind::Function);
   }
   ```

3. In `extract_impl_block` (`symbols.rs:804`), the method loop already
   checks `types.function_kinds.contains(&item.kind())`:
   ```rust
   for item in body.named_children(&mut body_cursor) {
       if !types.function_kinds.contains(&item.kind()) {
           continue;
       }
       ...
   }
   ```
   Adding `function_signature_item` to `function_kinds` means impl blocks
   with signature-only methods (rare but possible in trait impls with
   default bounds) will also extract those. This is correct behavior.

4. Verify that `function_signature_item` nodes have a `name` field
   (they do per tree-sitter-rust grammar — same `field('name', ...)`
   as `function_item`).

5. Verify that `read_symbol_scope` / `surgeon` can extract the source
   range of a `function_signature_item` node. The surgeon uses
   `node.byte_range()` which works on any named node.

6. Add a `SymbolKind` sub-variant or flag to distinguish signature-only
   methods from methods with bodies, if needed for `trace` to decide
   whether to call `goto_implementation`. Options:
   - Option A: Add `SymbolKind::FunctionSignature` (breaking — new enum
     variant)
   - Option B: Add `has_body: bool` field to `ExtractedSymbol` (additive)
   - Option C: In `trace`, detect trait/interface parent kind and call
     `goto_implementation` regardless of whether the method has a body

   **Recommended: Option C** — simplest, handles both signature-only and
   default-body trait methods (default body exists on the trait, but the
   actual call sites are in impls).

**Files to modify**:
- `crates/pathfinder-treesitter/src/language.rs` — add to `function_kinds`
- `crates/pathfinder-treesitter/src/symbols_test.rs` — tests for extraction

**Acceptance**:
- `explore(detail="symbols")` on a file with `trait Scout { fn search(&self); }`
  shows `search` as a child of `Scout`
- `inspect(semantic_path="crates/foo.rs::Scout.search")` returns the
  signature source code
- `read(detail_level="symbols")` on a trait file shows signature-only methods

---

## DELIVERABLE B: Wire `goto_implementation` into `trace`

Priority: P0
Effort: Medium (1.5 hours)
Risk: Medium (changes trace flow for trait methods — needs careful testing)

**Problem**: Even after Deliverable A, `trace(semantic_path="...::Scout.search")`
would trace the trait method's own callers/callees, not the impl methods'
callers/callees. For trait methods, the agent typically wants to know
"who calls any implementation of this trait method?" — which requires
expanding to impls first.

**Design**: When `trace(scope="callers")` resolves a symbol whose parent
is a trait/interface (SymbolKind::Interface), call `goto_implementation`
to find all impl methods, then BFS the call hierarchy of EACH impl method,
merging results.

**Steps**:

1. In `crates/pathfinder/src/server/tools/navigation/impact.rs`,
   `find_callers_callees_impl`, after `read_symbol_scope_enriched`
   resolves the symbol scope, check if the parent symbol is a trait/
   interface:

   ```rust
   // After resolving symbol_scope:
   let is_trait_method = symbol_scope
       .parent_kind
       .as_ref()
       .is_some_and(|k| k == "interface" || k == "trait");
   ```

   Note: `symbol_scope` would need to expose the parent symbol's kind.
   Check if `SourceSymbol` or the surgeon's return type includes parent
   kind. If not, add it to the scope resolution output.

2. If `is_trait_method` is true, call `goto_implementation`:

   ```rust
   if is_trait_method {
       let impls = self.lawyer.goto_implementation(
           self.workspace_root.path(),
           &semantic_path.file_path,
           u32::try_from(symbol_scope.start_line + 1).unwrap_or(1),
           u32::try_from(symbol_scope.name_column + 1).unwrap_or(1),
       ).await;

       match impls {
           Ok(impl_locations) if !impl_locations.is_empty() => {
               // For each impl location, resolve semantic path and BFS
               let mut all_incoming = Vec::new();
               let mut all_outgoing = Vec::new();
               for impl_loc in &impl_locations {
                   // Resolve impl_loc to a semantic path
                   let impl_path = ...;
                   // BFS each impl method
                   let (inc, out) = self.bfs_call_hierarchy(...).await;
                   all_incoming.extend(inc);
                   all_outgoing.extend(out);
               }
               // Dedup by semantic_path
               dedup_references(&mut all_incoming);
               dedup_references(&mut all_outgoing);
               incoming = Some(all_incoming);
               outgoing = Some(all_outgoing);
               degraded = false; // LSP-based, not grep
               degraded_reason = None;
               resolution_strategy = Some("lsp_call_hierarchy_with_impl_expansion".to_string());
           }
           _ => {
               // No impls found or goto_implementation unavailable
               // Fall through to normal BFS on the trait method itself
           }
       }
   }
   ```

3. Add `resolution_strategy: "lsp_call_hierarchy_with_impl_expansion"` to
   indicate that results were merged from multiple impl methods. Agents
   can use this to understand the result shape.

4. Add a `hint` when trait method expansion is used:
   ```
   "Symbol is a trait/interface method. Results include callers of all
    implementations found via goto_implementation. N implementations
    found across M files."
   ```

5. Handle the case where `goto_implementation` is not supported by the
   LSP server (some servers don't implement `textDocument/implementation`).
   Fall back to normal BFS on the trait method itself with a degraded hint.

**Files to modify**:
- `crates/pathfinder/src/server/tools/navigation/impact.rs` — trait detection + impl expansion
- `crates/pathfinder/src/server/types.rs` — new `resolution_strategy` value
- `crates/pathfinder-treesitter/src/treesitter_surgeon.rs` — expose parent kind in scope resolution

**Acceptance**:
- `trace(semantic_path="...::TraitName.method")` where TraitName is a trait
  returns merged callers from all impl methods
- `resolution_strategy` is `"lsp_call_hierarchy_with_impl_expansion"` when
  impl expansion is used
- When no impls exist: falls back to normal BFS with a hint
- When `goto_implementation` is unsupported: falls back to normal BFS with
  degraded hint

---

## DELIVERABLE C: Improve `did_you_mean` Cross-File Fallback

Priority: P1
Effort: Medium (1 hour)
Risk: Low

**Problem**: `enrich_did_you_mean` (`mod.rs:544-603`) only does cross-file
search when same-file suggestions are empty. If same-file fuzzy match
finds anything (even the trait itself), cross-file search is skipped.

**Steps**:

1. In `crates/pathfinder/src/server/tools/navigation/mod.rs:567`,
   change the condition:

   Before:
   ```rust
   if suggestions.is_empty() {
       // cross-file search
   }
   ```

   After:
   ```rust
   // Always do cross-file search for trait/interface methods
   // to find impl methods that may be in different files
   let needs_cross_file = suggestions.is_empty()
       || suggestions.iter().all(|s| !s.contains('.'));
   // If all same-file suggestions are parent-only (no method separator),
   // they're the trait itself, not impl methods — search cross-file

   if needs_cross_file {
       // cross-file search (existing logic)
   }
   ```

2. Improve the cross-file search query. Currently it searches by the
   bare last-segment name (`search`), which returns many unrelated symbols.
   Instead, search for the method name with `kind=function` filter to
   narrow results:

   ```rust
   let search_params = SearchParams {
       query: base_name.name.clone(),  // "search"
       mode: SearchMode::Symbol,
       kind: Some("function".to_string()),
       max_results: 20,
       ..Default::default()
   };
   ```

3. Filter cross-file results to only include symbols whose semantic path
   contains the method name as the last segment (already done by symbol
   search, but verify).

**Files to modify**:
- `crates/pathfinder/src/server/tools/navigation/mod.rs` — `enrich_did_you_mean`

**Acceptance**:
- When `trace("...::Scout.search")` fails and same file has only `Scout`
  (the trait), cross-file search runs and finds `MockScout.search`,
  `RipgrepScout.search`, etc.
- Cross-file search uses `mode="symbol"` with `kind="function"` to narrow
  results
- Suggestions include impl methods from other files

---

## DELIVERABLE D: Tests for Trait Method Resolution

Priority: P0
Effort: Medium (1 hour)
Risk: None

**Steps**:

Add tests to `crates/pathfinder-treesitter/src/symbols_test.rs`:

1. `test_trait_signature_method_extracted`
   - Input: `trait Scout { fn search(&self); }`
   - Assert: `Scout` has child `search` with kind `Function`

2. `test_trait_with_default_body_method_extracted`
   - Input: `trait Scout { fn search(&self) { default } }`
   - Assert: `Scout` has child `search` with kind `Function`
   - (This already works — regression test)

3. `test_trait_signature_method_in_impl_block`
   - Input: `impl Scout for Mock { fn search(&self) { ... } }`
   - Assert: `Mock` has child `search` with kind `Function`/`Method`
   - (This already works — regression test)

Add tests to `crates/pathfinder/src/server/tools/navigation/impact_test.rs`
(or `mod_test.rs`):

4. `test_trace_trait_method_resolves_and_expands_to_impls`
   - Setup: mock LSP with `goto_implementation` returning 2 impl locations
   - Input: `trace(semantic_path="...::Scout.search")` where Scout is trait
   - Assert: `incoming` contains callers from both impls
   - Assert: `resolution_strategy == "lsp_call_hierarchy_with_impl_expansion"`

5. `test_trace_trait_method_no_impls_falls_back`
   - Setup: mock LSP with `goto_implementation` returning empty
   - Input: `trace(semantic_path="...::Scout.search")`
   - Assert: falls back to normal BFS on trait method
   - Assert: hint mentions "no implementations found"

6. `test_did_you_mean_cross_file_finds_impl_methods`
   - Setup: trait `Scout` in file A, impl `MockScout` in file B
   - Input: `trace("A.rs::Scout.search")` → SYMBOL_NOT_FOUND
   - Assert: `did_you_mean` includes `B.rs::MockScout.search`

**Files to modify**:
- `crates/pathfinder-treesitter/src/symbols_test.rs`
- `crates/pathfinder/src/server/tools/navigation/impact_test.rs` or `mod_test.rs`

**Acceptance**:
- All 6 tests pass
- Tests cover: extraction, trace expansion, no-impls fallback, cross-file did_you_mean

---

## Dependency Order

```
A (extract function_signature_item) → B (wire goto_implementation into trace)
                                      → C (improve did_you_mean cross-file)
                                      → D (tests)
```

A must be first — without extraction, the symbol is invisible and B/C
can't even reach the trait method.

C can be done in parallel with B.

D depends on A, B, C.

## Verification Plan

```bash
cargo test -p pathfinder-mcp-treesitter symbols
cargo test -p pathfinder navigation
cargo clippy -- -D warnings
```

Manual verification on Pathfinder's own codebase:
- `search(mode="symbol", query="Scout")` → find the Scout trait
- `trace(semantic_path="...::Scout.search")` → should resolve and expand
  to MockScout.search and RipgrepScout.search callers
- Verify `resolution_strategy` is `"lsp_call_hierarchy_with_impl_expansion"`

Total effort: ~3 hours
