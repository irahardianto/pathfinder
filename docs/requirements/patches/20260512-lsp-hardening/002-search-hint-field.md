# 002: Add Zero-Result Hint to `search_codebase`

**Epic**: 1 — Quick Wins
**Status**: ✅ Complete (2026-05-12)
**Severity**: Low
**Risk**: Low — additive field, no behavioral change to existing consumers

---

## Problem

When `search_codebase` is called with `filter_mode=code_only` (the default) and all ripgrep matches fall inside comments or string literals, the response returns `returned_count: 0` with no explanation. Agents interpret this as "the symbol does not exist in the codebase" and stop searching.

In reality, the symbol exists — it was just hidden by the filter. The agent should retry with `filter_mode=all` to see the matches. Without a signal, agents waste time on dead-end investigation or falsely report missing symbols.

### Concrete Example

```
Query: "DEPRECATED_CONSTANT"
filter_mode: code_only
Result: 0 matches (agent concludes symbol doesn't exist)

Actual state: 5 matches in comments like `// DEPRECATED_CONSTANT was removed in v2`
```

---

## Solution

Added an optional `hint` field to `SearchCodebaseResponse` that is populated when:
1. `returned_count == 0` (filter removed everything)
2. `raw_match_count > 0` (ripgrep found matches before filtering)
3. `filter_mode != All` (a filter was actively applied)

The hint message includes the active filter mode and the raw count, explicitly suggesting `filter_mode='all'`.

### Fields Added

```rust
// In SearchCodebaseResponse:
#[serde(skip_serializing_if = "Option::is_none")]
pub hint: Option<String>,
```

### Hint Format

```
0 matches with filter_mode=CodeOnly but 5 match(es) exist with filter_mode=all.
Retry with filter_mode='all' to include comments and strings.
```

### Files Modified

| File | Change |
|------|--------|
| `crates/pathfinder/src/server/types.rs` | Added `hint: Option<String>` to `SearchCodebaseResponse` |
| `crates/pathfinder/src/server/tools/search.rs` | Populated `hint` based on filter/count conditions; added `hint_emitted` to tracing span |

---

## Acceptance Criteria

- [x] `hint` field is `None` when `returned_count > 0`
- [x] `hint` field is `None` when `filter_mode = All` (no filtering applied)
- [x] `hint` field is `Some(...)` when `returned_count == 0 && raw_match_count > 0 && filter_mode != All`
- [x] Hint message includes the active `filter_mode` name
- [x] Hint message includes the `raw_match_count`
- [x] Hint message explicitly suggests `filter_mode='all'`
- [x] Field is `skip_serializing_if = "Option::is_none"` (absent from JSON when not set)
- [x] `hint_emitted` boolean logged in tracing span

---

## Tests

| Test | File | Description |
|------|------|-------------|
| `test_search_hint_populated_when_filter_removes_all_results` | `search.rs` | Rust file with symbol only in comment; `code_only` filter → hint present with `filter_mode='all'` suggestion |
| `test_search_hint_absent_when_no_filter_applied` | `search.rs` | Rust file with code match; `filter_mode=All` → hint absent |

---

## Verification

```bash
cargo test -p pathfinder-mcp -- tools::search::tests::test_search_hint
# 2 passed, 0 failed

cargo clippy -p pathfinder-mcp -- -D warnings
# 0 warnings
```
