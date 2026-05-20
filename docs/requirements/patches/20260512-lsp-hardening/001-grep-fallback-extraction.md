# 001: Extract `grep_reference_fallback` Helper

**Epic**: 1 — Quick Wins
**Status**: ✅ Complete (2026-05-12)
**Severity**: Medium
**Risk**: Low — pure refactor, behavior-preserving

---

## Problem

`analyze_impact_impl` in `navigation.rs` contained three identical ~35-line blocks implementing grep-based reference search as a fallback when LSP call hierarchy is unavailable. Each block appeared in a different match arm:

1. **`LspWarmupEmptyUnverified`** — LSP returned empty call hierarchy items, goto_definition probe failed → LSP likely still indexing
2. **`NoLspAvailable | UnsupportedCapability`** — no LSP process running for the language
3. **`LspError::Timeout`** — LSP timed out responding to call hierarchy request

All three blocks performed identical logic:
- Extract last segment of `semantic_path.symbol_chain` as `symbol_name`
- Run `scout.search()` with `max_results: 20`, `is_regex: false`
- Filter to `is_source_file()` matches, exclude definition file
- Cap at 10 results
- Map to `ImpactReference { direction: "incoming_heuristic", depth: 0 }`
- Set `incoming = Some(refs)` and `degraded_reason` to the branch-specific variant

Only `degraded_reason` differed between branches. This duplication meant any bug fix or enhancement to the grep fallback logic had to be applied three times.

---

## Solution

Extracted a single private method on `PathfinderServer`:

```rust
async fn grep_reference_fallback(
    &self,
    symbol_name: &str,
    definition_path: &str,
    files_referenced: &mut HashSet<String>,
) -> Option<Vec<ImpactReference>>
```

Each match arm now calls `self.grep_reference_fallback(...)` and sets its own `DegradedReason` variant.

### Files Modified

| File | Change |
|------|--------|
| `crates/pathfinder/src/server/tools/navigation.rs` | Added `grep_reference_fallback` method; replaced 3 inline blocks with single-line calls |

### Lines Changed

- **Added**: ~60 lines (helper method + doc comments)
- **Removed**: ~100 lines (3 × 35-line duplicate blocks)
- **Net**: −40 lines

---

## Acceptance Criteria

- [x] `grep_reference_fallback` is a private `async fn` on `PathfinderServer`
- [x] All three match arms (`LspWarmupEmptyUnverified`, `NoLspAvailable`, `Timeout`) call the helper
- [x] Each arm still sets its own `DegradedReason` variant
- [x] Results are filtered to `is_source_file()` only
- [x] Definition file is excluded from results
- [x] Results capped at 10
- [x] All `direction` values are `"incoming_heuristic"`
- [x] `files_referenced` set is updated by the helper

---

## Tests

| Test | File | Description |
|------|------|-------------|
| `test_grep_reference_fallback_finds_references` | `navigation.rs` | 2-file workspace; verifies refs found, tagged `incoming_heuristic`, definition file excluded, `files_referenced` updated |
| `test_grep_reference_fallback_excludes_definition_file` | `navigation.rs` | Single-file workspace; verifies definition file never appears in results |

---

## Verification

```bash
cargo test -p pathfinder-mcp -- tools::navigation::tests::test_grep_reference_fallback
# 2 passed, 0 failed

cargo clippy -p pathfinder-mcp -- -D warnings
# 0 warnings
```
