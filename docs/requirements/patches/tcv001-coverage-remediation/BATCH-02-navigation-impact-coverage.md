# BATCH-02: Navigation impact.rs Coverage

Scope: `crates/pathfinder/src/server/tools/navigation/impact.rs`
Est. Uncovered Lines: ~30
Complexity: HIGH (3219 lines, dense logic)
Priority: 2 (largest single navigation file)

---

## File Overview

`impact.rs` implements `analyze_impact` / `find_callers_callees` -- the call graph traversal tool. It performs BFS traversal of call hierarchies with LSP-powered and grep-fallback paths.

Total lines: 3219
Test infrastructure: `test_helpers.rs` provides `make_server()` and mock setup.

---

## Uncovered Line Ranges

### Block I1: Entry and dispatch (lines 70-77, 112, 161-177)
```
70-71   analyze_impact_impl parameter validation
77      error return for invalid params
112     dispatch to specific analysis strategy
161     max_depth clamping
166     depth limit enforcement
171     traversal strategy selection
177     result aggregation entry
```
Why uncovered: Parameter validation edge cases and dispatch branches not exercised by existing tests.
Strategy: Test with boundary values -- max_depth=0, max_depth exceeding limit, invalid semantic paths.

### Block I2: BFS Traversal core (lines 306, 316-324, 351-358, 493-497, 527, 535, 541, 543-547, 549-553, 555, 581-585, 649, 657, 701-702, 707, 753-754, 819-820)
```
306     BFS queue initialization
316-319 visited set insertion
321     cycle detection
324     depth tracking
351-352 edge case: empty caller list
354     caller result filtering
358     caller deduplication
493-495 outgoing edge resolution
497     callee deduplication
527     traversal result merging
535     incoming caller merge
541     merge conflict resolution
543-547 partial graph handling
549     incomplete result flag
551-553 fallback to grep
555     grep result integration
581-583 grep match parsing
585     grep result validation
649     depth-first fallback path
657     DFS termination condition
701-702 recursive call edge case
707     recursion depth limit
753-754 call hierarchy error recovery
819-820 cross-reference resolution
```
Why uncovered: BFS/DFS traversal has many branches for cycle detection, depth limits, partial graphs, and error recovery. Tests cover happy path but not edge cases.
Strategy: Create targeted test cases for each branch:
- Cycle in call graph (A calls B calls A)
- Depth limit reached mid-traversal
- Empty caller/callee results at each level
- Partial graph where some nodes fail to resolve
- Grep fallback triggered when LSP fails mid-traversal

### Block I3: Result formatting (lines 1199, 1506, 2326)
```
1199    result serialization edge case
1506    large result truncation
2326    output formatting for empty result set
```
Why uncovered: Output formatting edge cases.
Strategy: Test with empty results, results exceeding truncation limit, and unusual symbol types.

---

## Existing Tests to Extend

The file already has test coverage via `test_helpers.rs`. Look for existing test patterns:

1. `test_analyze_impact_*` -- happy path tests
2. `test_find_callers_callees_*` -- mock-based call hierarchy tests

Extend these with edge case variants rather than creating entirely new test structures.

---

## Delivery Breakdown

### BATCH-02a: BFS Edge Cases (est. 18 lines covered)
Scope: Block I2 (BFS traversal branches)
Files: impact.rs
Tests: Add to inline `mod tests`
Cases:
- Call graph with cycle (A->B->A)
- Depth limit 0 and depth limit 1
- Empty callee list at intermediate node
- Partial graph where one node fails to resolve
- Grep fallback when LSP returns error mid-BFS

### BATCH-02b: Entry/Dispatch and Result Formatting (est. 12 lines covered)
Scope: Blocks I1, I3
Files: impact.rs
Tests: Add to inline `mod tests`
Cases:
- Invalid semantic path format
- max_depth boundary values (0, 1, max)
- Empty result set formatting
- Result exceeding truncation limit
- Unusual symbol types (macro, trait impl)

---

## Test Code Patterns

```rust
// Pattern: Cycle detection in BFS
#[tokio::test]
async fn test_bfs_cycle_detection() {
    let server = make_server().await;
    // Setup: function_a calls function_b which calls function_a
    // Verify: traversal completes without infinite loop
    // Verify: both directions recorded
}

// Pattern: Grep fallback mid-traversal
#[tokio::test]
async fn test_bfs_grep_fallback_partial() {
    let server = make_server().await;
    // Setup: mock LSP returns error for one symbol, succeeds for others
    // Verify: grep fallback activates for failed symbol only
    // Verify: final result includes both LSP and grep results
}
```

---

## Estimated Impact

| Sub-batch | Lines Covered | Cumulative LCV |
|---|---|---|
| BATCH-02a | ~18 | +0.15% |
| BATCH-02b | ~12 | +0.1% |
| **Total** | **~30** | **+0.25%** |

---

## Validation

```bash
cargo test -p pathfinder -- navigation::impact
cargo clippy -p pathfinder -- -D warnings
```
