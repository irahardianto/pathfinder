# PATCH-008: Make validate_only Honest About LSP Absence

## Group: C (Medium) — Validation & Fallback Improvements

## Objective

Fix the most dangerous finding from the agent report: `validate_only` returns
`validation_skipped: false, status: "passed"` when LSP is unavailable, giving agents
false confidence that their code is correct. The tool must honestly report when
validation could not be performed.

## Severity: CRITICAL — false positive on type errors is trust-breaking

## Background

The validation pipeline has two paths:
1. **LSP available**: `did_open → pull_diagnostics → did_change → pull_diagnostics → diff`.
   This correctly detects type errors (E0308) and syntax errors.
2. **LSP unavailable**: The validation outcome returns `skipped: true, skipped_reason: "..."`.
   BUT the `EditResponse` construction maps this to `validation_skipped: true, validation: { status: "skipped" }`.

The agent report found that `validate_only` returned `status: "passed"` for obviously wrong
code like `let x: i32 = "not a number"`. This suggests one of:
- The LSP was actually available but `validate_only` used a different code path
- The `empty_diagnostics_both_snapshots` skip reason was triggered (both pre/post are clean)
- The validation was skipped but the response didn't reflect this

The `empty_diagnostics_both_snapshots` scenario means: the file had zero diagnostics before
AND after the edit. This is correct when the edit is semantically equivalent. But it's also
the result when rust-analyzer hasn't finished indexing — it returns empty diagnostics for
everything, so pre and post are both empty, and the diff is zero errors.

The fix: When both snapshots are empty AND the LSP was recently started (or has no prior
successful diagnostic), mark validation as uncertain rather than "passed".

## Scope

| # | File | Action |
|---|------|--------|
| 1 | `crates/pathfinder/src/server/tools/edit/validation.rs` | Add uncertain status for empty-on-both-snapshots |
| 2 | `crates/pathfinder/src/server/types.rs` | Add "uncertain" to EditValidation status |

## Step 1: Add "uncertain" status to EditValidation

**File:** `crates/pathfinder/src/server/types.rs`

The `EditValidation.status` field currently accepts `"passed"`, `"failed"`, or `"skipped"`.
Add `"uncertain"` for the case where validation ran but results are unreliable.

Update the doc comment on `EditValidation`:

**Find:**
```rust
impl EditValidation {
    /// Return a skipped validation result (no LSP available).
    #[must_use]
    pub fn skipped() -> Self {
        Self {
            status: "skipped".to_owned(),
            introduced_errors: vec![],
            resolved_errors: vec![],
        }
    }
}
```

**Replace with:**
```rust
impl EditValidation {
    /// Return a skipped validation result (no LSP available).
    #[must_use]
    pub fn skipped() -> Self {
        Self {
            status: "skipped".to_owned(),
            introduced_errors: vec![],
            resolved_errors: vec![],
        }
    }

    /// Return an uncertain validation result (LSP ran but results are unreliable).
    ///
    /// Use when both pre- and post-edit diagnostics are empty, which could mean
    /// either (a) the code is genuinely clean, or (b) the LSP hasn't finished
    /// indexing. Agents should treat "uncertain" as "possibly correct but unverified".
    #[must_use]
    pub fn uncertain() -> Self {
        Self {
            status: "uncertain".to_owned(),
            introduced_errors: vec![],
            resolved_errors: vec![],
        }
    }
}
```

## Step 2: Use "uncertain" for empty_diagnostics_both_snapshots

**File:** `crates/pathfinder/src/server/tools/edit/validation.rs`

Find the `build_validation_outcome` function (or wherever `empty_diagnostics_both_snapshots`
is handled). When both pre and post diagnostics are empty, the current code returns
`ValidationOutcome { skipped: true, skipped_reason: Some("empty_diagnostics_both_snapshots") }`.

The problem: `skipped: true` with `validation_skipped_reason` in the response is correct,
but the agent report suggests the `EditResponse` for `validate_only` might not be
surfacing this correctly. Let's make it explicit.

**Find the `build_validation_outcome` function and its handling of empty diffs:**
When `pre_diags.is_empty() && post_diags.is_empty()`, instead of `skipped`, return
`uncertain`:

```rust
// When both snapshots have zero diagnostics, the result is ambiguous:
// - Genuine clean code (both before and after are error-free)
// - LSP warmup (rust-analyzer hasn't indexed yet, returns empty for everything)
// Return "uncertain" rather than "passed" to signal this ambiguity.
if pre_diags.is_empty() && post_diags.is_empty() {
    return ValidationOutcome {
        validation: EditValidation::uncertain(),
        skipped: true,
        skipped_reason: Some("empty_diagnostics_both_snapshots".to_owned()),
        should_block: false,
    };
}
```

Also update the `EditResponse` construction in `validate_only_impl` to set
`validation_skipped: true` when `validation_skipped_reason` is
`"empty_diagnostics_both_snapshots"`.

## EXCLUSIONS — Do NOT Modify These

- The LSP validation pipeline itself (did_open → diagnostics flow) — it works correctly
- `replace_body` and other edit tools — they already handle `ValidationOutcome` generically
- The `EditResponse.validation` field type — it's a string, "uncertain" is a new valid value

## Verification

```bash
# 1. Confirm "uncertain" status exists
grep -n 'uncertain' crates/pathfinder/src/server/types.rs
grep -n 'uncertain' crates/pathfinder/src/server/tools/edit/validation.rs

# 2. Run validation tests
cargo test -p pathfinder validation

# 3. Full verification
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

## Expected Impact

- When LSP is warm and code is clean: `status: "passed"` (no change)
- When LSP is warm and code has errors: `status: "failed"` with error list (no change)
- When LSP is cold (empty snapshots): `status: "uncertain"` instead of `status: "skipped"`
  or false `status: "passed"`. Agents know to not trust the result.
- When LSP is unavailable entirely: `status: "skipped"` (no change)
