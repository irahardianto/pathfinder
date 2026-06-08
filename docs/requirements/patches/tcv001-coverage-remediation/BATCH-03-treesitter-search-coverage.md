# BATCH-03: treesitter repo_map + search ripgrep Coverage

Scope: `crates/pathfinder-treesitter/src/repo_map.rs`, `crates/pathfinder-search/src/ripgrep.rs`
Est. Uncovered Lines: ~31
Complexity: MEDIUM
Priority: 3

---

## File 1: repo_map.rs (pathfinder-treesitter)

Total lines: 1259
Uncovered lines: ~19

### Block R1: Repo map generation (lines 129, 205, 210-220, 240-259, 289-301)
```
129     get_repo_map entry with include_imports config
205     import filtering logic
210-214 third-party import detection
216-220 import path normalization
240-241 visibility filtering (public vs all)
244-254 symbol visibility resolution
259     nested symbol depth tracking
289     file-level symbol aggregation
292     duplicate symbol filtering
301     sort stability check
```
Why uncovered: Repo map has many configuration combinations (visibility, imports, depth). Tests cover default config.
Strategy: Test with non-default configurations:
- `include_imports: "all"` (currently tested with "none" and "third_party")
- `visibility: "all"` vs `visibility: "public"`
- Files with deeply nested symbol hierarchies
- Files with duplicate symbol names across modules

### Block R2: Tree walking edge cases (lines 367-374, 427-438, 453-464, 534-539)
```
367     symbol kind resolution for rare node types
369     fallback kind mapping
374     anonymous node handling
427-429 multi-line symbol signature
433-438 truncated signature for large symbols
453-455 generic parameter extraction
459-464 lifetime annotation handling
534-535 doc comment association
537     trailing doc comment
539     doc comment continuation
```
Why uncovered: Tree walking handles many AST node types. Uncommon types (macros, trait impls, const generics) aren't in test fixtures.
Strategy: Create test fixture files with:
- Macros with complex bodies
- Trait implementations with generics
- Functions with lifetime parameters
- Multi-line function signatures
- Types with const generics

### Block R3: Output formatting (lines 539)
```
539     output truncation when map exceeds max_tokens
```
Why uncovered: Test fixtures are small, truncation never triggers.
Strategy: Create a large test fixture or set low `max_tokens` to trigger truncation.

---

## File 2: ripgrep.rs (pathfinder-search)

Total lines: 1547
Uncovered lines: ~12

### Block RG1: Search execution (lines 102, 133, 227, 233, 440, 478, 562-565, 582-584, 612, 631, 964, 993)
```
102     ripgrep binary path resolution
133     argument construction for complex queries
227     regex pattern escaping
233     multi-pattern search
440     file type filtering
478     gitignore resolution
562-565 search result batching
582-584 result deduplication
612     context line extraction
631     match position calculation
964     large result set handling
993     search cancellation
```
Why uncovered: Ripgrep wrapper delegates to the `rg` binary. Tests use mock searcher but don't cover all ripgrep argument combinations.
Strategy: Test argument construction paths:
- Multi-pattern queries (query with `|`)
- File type filtering (include/exclude glob)
- Gitignore respect toggle
- Context line configuration
- Large result set pagination
- Search cancellation mid-stream

---

## Existing Test Infrastructure

### pathfinder-treesitter
- Inline tests in `test_impl.rs` and `test_symbols.rs`
- Integration tests in `tests/` directory
- Test fixtures for Rust, Java, and top-level source files

### pathfinder-search
- `mock.rs` provides `MockSearcher`
- Benchmarks in `benches/search_bench.rs`
- No dedicated test file -- tests are inline in `ripgrep.rs` or use mock

---

## Delivery Breakdown

### BATCH-03a: repo_map.rs Configuration Variants (est. 10 lines covered)
Scope: Block R1
Files: repo_map.rs
Tests: Add to `tests/test_impl.rs`
Cases:
- include_imports="all" configuration
- visibility="all" configuration
- Files with deeply nested symbols
- Files with duplicate symbol names

### BATCH-03b: repo_map.rs AST Edge Cases (est. 8 lines covered)
Scope: Blocks R2, R3
Files: repo_map.rs
Tests: Add to `tests/test_impl.rs`
Cases:
- Macro definitions
- Trait impls with generics
- Functions with lifetimes
- Multi-line signatures
- Output truncation (low max_tokens)

### BATCH-03c: ripgrep.rs Argument Construction (est. 8 lines covered)
Scope: Block RG1
Files: ripgrep.rs
Tests: Add to inline `mod tests` or create new test file
Cases:
- Multi-pattern query
- File type filter combinations
- Context line configuration
- Gitignore toggle
- Result deduplication

---

## Test Code Patterns

```rust
// Pattern: repo_map with import configuration
#[test]
fn test_repo_map_include_all_imports() {
    let source = r#"use std::collections::HashMap; fn main() {}"#;
    let result = get_repo_map(source, "test.rs", RepoMapConfig {
        include_imports: ImportMode::All,
        ..Default::default()
    });
    assert!(result.contains("use std::collections::HashMap"));
}

// Pattern: ripgrep multi-pattern query
#[test]
fn test_search_multi_pattern() {
    let searcher = MockSearcher::new();
    let results = searcher.search("foo|bar", &SearchConfig {
        path_glob: Some("**/*.rs"),
        ..Default::default()
    });
    // Verify both patterns matched
}
```

---

## Estimated Impact

| Sub-batch | Lines Covered | Cumulative LCV |
|---|---|---|
| BATCH-03a | ~10 | +0.08% |
| BATCH-03b | ~8 | +0.07% |
| BATCH-03c | ~8 | +0.07% |
| **Total** | **~31** | **+0.22%** |

---

## Validation

```bash
cargo test -p pathfinder-treesitter
cargo test -p pathfinder-search
cargo clippy -p pathfinder-treesitter -p pathfinder-search -- -D warnings
```
