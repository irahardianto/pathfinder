# BATCH-04: Navigation Remaining Coverage

Scope: `crates/pathfinder/src/server/tools/navigation/`
Files: health.rs, references.rs, overview.rs, mod.rs
Est. Uncovered Lines: ~49
Complexity: MEDIUM
Priority: 4 (depends on BATCH-02 shared test helpers)

---

## Files in Scope

| File | Lines | Uncovered Lines | Purpose |
|---|---|---|---|
| `health.rs` | 2161 | ~18 | LSP health check and diagnostics |
| `references.rs` | 2134 | ~9 | find_all_references implementation |
| `overview.rs` | 1039 | ~8 | symbol_overview implementation |
| `mod.rs` | 1634 | ~14 | Shared navigation dispatch and helpers |

---

## Uncovered Line Ranges -- health.rs (~18 lines)

### Block H1: Health check execution (lines 473, 498, 503, 525, 530, 554-556, 573-577, 580-582, 608, 612, 618, 671, 674-679)
```
473     health check timeout handling
498     server health response parsing
503     health check retry
525     capability-based health check
530     degraded health state
554-556 partial capability detection
573-577 server initialization timeout
580-582 server startup failure
608     server readiness check
612     readiness timeout
618     health state caching
671     cache invalidation
674-677 background health monitor
679     health monitor shutdown
```
Why uncovered: Health checks involve async polling, timeouts, and background monitoring tasks. Tests cover synchronous health check but not async monitoring.
Strategy:
- Test health check timeout (set very short timeout)
- Test background health monitor with mock that alternates healthy/unhealthy
- Test cache invalidation on configuration change
- Test degraded state detection

---

## Uncovered Line Ranges -- references.rs (~9 lines)

### Block RF1: Reference finding (lines 69, 74-75, 81, 103-104, 110, 159-160, 167, 173-174, 181, 1305)
```
69      find_references entry with project_only filter
74-75   reference filtering by workspace scope
81      reference deduplication
103-104 LSP reference response parsing
110     reference location resolution
159-160 grep fallback for references
167     grep result filtering
173-174 reference symbol matching
181     cross-file reference aggregation
1305    reference result formatting edge case
```
Why uncovered: Reference finding has LSP and grep paths. Tests cover LSP happy path.
Strategy:
- Test grep fallback when LSP returns empty results
- Test workspace scope filtering (references in node_modules excluded)
- Test reference deduplication across files
- Test cross-file reference aggregation

---

## Uncovered Line Ranges -- overview.rs (~8 lines)

### Block O1: Symbol overview (lines 108-109, 114-119, 153-154, 159, 177-178, 183-188, 208-209, 215-219, 243)
```
108-109 overview parameter validation
114-119 multi-source aggregation (source + callers + references)
153-154 caller count limits
159     caller result truncation
177-178 reference count limits
183-188 reference aggregation and dedup
208-209 source extraction fallback
215-219 partial source extraction
243     empty overview result
```
Why uncovered: Symbol overview aggregates results from multiple tools (read_symbol, find_callers, find_references). Tests cover individual tools but not the aggregation layer.
Strategy:
- Test with symbol that has callers + references (full aggregation)
- Test with symbol that has no callers (partial aggregation)
- Test with symbol in generated code (source extraction fallback)
- Test empty result when symbol doesn't exist

---

## Uncovered Line Ranges -- mod.rs (~14 lines)

### Block M1: Navigation dispatch (lines 83, 88-105, 128, 137, 147, 501-506, 510-518, 524-527, 529-539, 543-544, 557, 573-576, 582-584, 586-589, 591, 596, 601-606, 1030)
```
83      tool dispatch routing
88-103  unknown tool handling
105     tool name normalization
128     parameter extraction
137     parameter validation
147     parameter default application
501-506 navigation cache lookup
510-518 cache miss handling
524-527 cache population
529-539 cache invalidation on file change
543-544 stale cache detection
557     cache entry serialization
573-576 navigation result caching
582-584 cache size limit
586-589 cache eviction
591     LRU eviction strategy
596     cache statistics
601-606 cache warming
1030    navigation telemetry
```
Why uncovered: Navigation dispatch has caching layer and parameter routing. Tests bypass dispatch and call tool implementations directly.
Strategy:
- Test dispatch routing for each navigation tool
- Test cache hit/miss behavior
- Test cache invalidation on file change events
- Test cache size limit and eviction
- Test parameter defaults and validation

---

## Shared Test Infrastructure

All files share `test_helpers.rs` which provides:
- `make_server()` -- creates test server with mocked LSP
- `MockLawyer` -- mock LSP client
- `MockScout` -- mock search engine

Extend `test_helpers.rs` if needed for new test scenarios:
- Cache manipulation helpers
- File change event simulation
- Timeout configuration helpers

---

## Delivery Breakdown

### BATCH-04a: health.rs Async Health Monitoring (est. 18 lines covered)
Scope: Block H1
Files: health.rs
Tests: Add to inline `mod tests`
Cases:
- Health check timeout
- Background health monitor with alternating states
- Cache invalidation
- Degraded state detection
- Server startup failure handling

### BATCH-04b: references.rs Grep Fallback (est. 9 lines covered)
Scope: Block RF1
Files: references.rs
Tests: Add to inline `mod tests`
Cases:
- Grep fallback when LSP empty
- Workspace scope filtering
- Reference deduplication
- Cross-file aggregation

### BATCH-04c: overview.rs Aggregation (est. 8 lines covered)
Scope: Block O1
Files: overview.rs
Tests: Add to inline `mod tests`
Cases:
- Full aggregation (callers + references)
- Partial aggregation (no callers)
- Source extraction fallback
- Empty result

### BATCH-04d: mod.rs Dispatch and Cache (est. 14 lines covered)
Scope: Block M1
Files: mod.rs
Tests: Add to inline `mod tests`
Cases:
- Dispatch routing for each tool
- Cache hit/miss
- Cache invalidation
- Cache eviction
- Parameter validation and defaults

---

## Estimated Impact

| Sub-batch | Lines Covered | Cumulative LCV |
|---|---|---|
| BATCH-04a | ~18 | +0.15% |
| BATCH-04b | ~9 | +0.08% |
| BATCH-04c | ~8 | +0.07% |
| BATCH-04d | ~14 | +0.12% |
| **Total** | **~49** | **+0.42%** |

---

## Validation

```bash
cargo test -p pathfinder -- navigation
cargo clippy -p pathfinder -- -D warnings
```
