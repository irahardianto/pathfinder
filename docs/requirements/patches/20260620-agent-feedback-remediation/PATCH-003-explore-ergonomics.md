# PATCH-003: Explore Ergonomics — suggested_max_tokens & Hint Text

Date: 2026-06-20
Source: 7 independent agent assessment reports
Status: Implemented

## Problem Statement

The explore tool's max_tokens hint is only a freeform text string containing
the word "approximately". The previous session (conversation 1348cb13)
claimed to add a `suggested_max_tokens` structured field (commit 0329753)
but this field does NOT exist in the current `GetRepoMapMetadata` struct.

Current state in `repo_map.rs`:

```rust
hint = Some(format!(
    "Repository map is incomplete (coverage: {}%, {}/{} files). \
     To scan more files, increase max_tokens from {} to approximately {}.",
    result.coverage_percent, result.files_scanned, result.files_in_scope,
    max_tokens, suggested,
));
```

The `suggested` value (rounded to nearest 4000, capped at 100000) is
computed but buried inside natural language. No machine-readable numeric
field exists. Both text channel AND structured `hint` field contain the
same freeform string.

---

## DELIVERABLE A: Add suggested_max_tokens Structured Field

Priority: P2
Effort: Low (30 minutes)
Risk: Low (additive, no breaking changes)

**Steps**:

1. In the types file containing `GetRepoMapMetadata` (check both
   `crates/pathfinder-common/src/types.rs` and
   `crates/pathfinder/src/server/types.rs`):
   - Add field:
   ```rust
   #[serde(skip_serializing_if = "Option::is_none")]
   pub suggested_max_tokens: Option<u32>,
   ```

2. In `crates/pathfinder-core/src/tools/repo_map.rs` (or
   `crates/pathfinder/src/server/tools/repo_map.rs`):
   - Where the suggested value is computed (around lines 326-332,
     rounding to nearest 4000, capping at 100000):
   - Store the computed value in a variable
   - Set `metadata.suggested_max_tokens = Some(suggested)` when
     coverage < 100%
   - Set `metadata.suggested_max_tokens = None` when coverage == 100%

3. Add tests:
   - `test_explore_suggested_max_tokens_present_when_truncated`
   - `test_explore_suggested_max_tokens_none_when_full_coverage`
   - `test_explore_suggested_max_tokens_rounded_to_4000`
   - `test_explore_suggested_max_tokens_capped_at_100000`

**Files to modify**:
- Types file containing `GetRepoMapMetadata`
- `repo_map.rs` (explore tool implementation)

**Acceptance**:
- Explore response includes `suggested_max_tokens: 36000` (numeric) when
  coverage < 100%
- `suggested_max_tokens` absent when coverage == 100%
- Value rounded up to nearest 4000, capped at 100000

---

## DELIVERABLE B: Fix Hint Text Wording

Priority: P3
Effort: 5 minutes
Risk: None

**Problem**: Hint text says "to approximately {suggested}". The value is
already rounded up to nearest 4000 — it's not approximate. It's a
concrete recommendation.

**Steps**:

1. In `repo_map.rs`, the format string for the hint (around line 333-341):
   - Change `"to approximately {suggested}"` to
     `"to at least {suggested}"`
   - Full new text:
     ```
     Repository map is incomplete (coverage: X%, N/M files).
     To scan more files, increase max_tokens from {current}
     to at least {suggested}.
     ```

**Files to modify**:
- `repo_map.rs` (hint format string)

**Acceptance**:
- Hint text says "at least" not "approximately"
- Value is actionable — agent can use it directly

---

## Dependency Order

A must be done before B (A adds the field, B changes the text).
Both are independent of other patches.

## Verification Plan

```bash
cargo test -p pathfinder-core  # or relevant crate
cargo clippy -- -D warnings
```

Manual verification:
- Call explore with low max_tokens on a medium repo
- Verify response JSON has `suggested_max_tokens` numeric field
- Verify hint text reads "at least" not "approximately"

Total effort: ~35 minutes
