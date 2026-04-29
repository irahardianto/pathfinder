# PATCH-002: Diagnostic Diffing Deduplication

## Status: COMPLETED (2026-04-29)

## Objective

Eliminate redundant HashMap construction in `collect_introduced` and `collect_resolved`. Both functions internally rebuild count maps that are already computed by the caller (`diff_diagnostics`). Pass the pre-built maps as parameters instead.

## Severity: LOW — Performance improvement, code quality

---

## Scope

| # | File | Function | Action |
|---|------|----------|--------|
| 1 | `crates/pathfinder/src/server/tools/diagnostics.rs` | `collect_introduced` | Change signature, remove internal HashMap |
| 2 | `crates/pathfinder/src/server/tools/diagnostics.rs` | `collect_resolved` | Change signature, remove internal HashMap |
| 3 | `crates/pathfinder/src/server/tools/diagnostics.rs` | `diff_diagnostics` | Update call sites |

This patch modifies **1 file only**: `crates/pathfinder/src/server/tools/diagnostics.rs`

---

## Current Code (verbatim)

### `diff_diagnostics` (lines 37-48)
```rust
pub(crate) fn diff_diagnostics(pre: &[LspDiagnostic], post: &[LspDiagnostic]) -> DiagnosticDiff {
    let pre_counts = build_counts(pre);
    let post_counts = build_counts(post);

    let introduced = collect_introduced(post, &pre_counts);
    let resolved = collect_resolved(pre, &post_counts);

    DiagnosticDiff {
        introduced,
        resolved,
    }
}
```

### `collect_introduced` (lines 79-103)
```rust
fn collect_introduced(
    post: &[LspDiagnostic],
    pre_counts: &HashMap<DiagKey, usize>,
) -> Vec<LspDiagnostic> {
    // Build how many of each key appear post but not pre
    let mut post_counts_local: HashMap<DiagKey, usize> = HashMap::new();
    for d in post {
        *post_counts_local.entry(diag_key(d)).or_insert(0) += 1;
    }

    let mut result = Vec::new();
    let mut emitted: HashMap<DiagKey, usize> = HashMap::new();
    for d in post {
        let key = diag_key(d);
        let pre = *pre_counts.get(&key).unwrap_or(&0);
        let post_count = *post_counts_local.get(&key).unwrap_or(&0);
        let excess = post_count.saturating_sub(pre);
        let done = *emitted.get(&key).unwrap_or(&0);
        if done < excess {
            result.push(d.clone());
            *emitted.entry(key).or_insert(0) += 1;
        }
    }
    result
}
```

### `collect_resolved` (lines 106-129)
```rust
fn collect_resolved(
    pre: &[LspDiagnostic],
    post_counts: &HashMap<DiagKey, usize>,
) -> Vec<LspDiagnostic> {
    let mut pre_counts_local: HashMap<DiagKey, usize> = HashMap::new();
    for d in pre {
        *pre_counts_local.entry(diag_key(d)).or_insert(0) += 1;
    }

    let mut result = Vec::new();
    let mut emitted: HashMap<DiagKey, usize> = HashMap::new();
    for d in pre {
        let key = diag_key(d);
        let post = *post_counts.get(&key).unwrap_or(&0);
        let pre_count = *pre_counts_local.get(&key).unwrap_or(&0);
        let excess = pre_count.saturating_sub(post);
        let done = *emitted.get(&key).unwrap_or(&0);
        if done < excess {
            result.push(d.clone());
            *emitted.entry(key).or_insert(0) += 1;
        }
    }
    result
}
```

---

## Target Code

### Replace `diff_diagnostics` with:
```rust
pub(crate) fn diff_diagnostics(pre: &[LspDiagnostic], post: &[LspDiagnostic]) -> DiagnosticDiff {
    let pre_counts = build_counts(pre);
    let post_counts = build_counts(post);

    let introduced = collect_introduced(post, &pre_counts, &post_counts);
    let resolved = collect_resolved(pre, &pre_counts, &post_counts);

    DiagnosticDiff {
        introduced,
        resolved,
    }
}
```

### Replace `collect_introduced` with:
```rust
/// Collect diagnostics in `post` that appear **more often** than in `pre`.
///
/// Each element in the returned vec is a representative `LspDiagnostic` for
/// one excess occurrence. Both count maps are passed in from the caller to
/// avoid redundant HashMap construction.
fn collect_introduced(
    post: &[LspDiagnostic],
    pre_counts: &HashMap<DiagKey, usize>,
    post_counts: &HashMap<DiagKey, usize>,
) -> Vec<LspDiagnostic> {
    let mut result = Vec::new();
    let mut emitted: HashMap<DiagKey, usize> = HashMap::new();
    for d in post {
        let key = diag_key(d);
        let pre = *pre_counts.get(&key).unwrap_or(&0);
        let post_count = *post_counts.get(&key).unwrap_or(&0);
        let excess = post_count.saturating_sub(pre);
        let done = *emitted.get(&key).unwrap_or(&0);
        if done < excess {
            result.push(d.clone());
            *emitted.entry(key).or_insert(0) += 1;
        }
    }
    result
}
```

### Replace `collect_resolved` with:
```rust
/// Collect diagnostics in `pre` that appear **more often** than in `post`.
///
/// Both count maps are passed in from the caller to avoid redundant HashMap
/// construction.
fn collect_resolved(
    pre: &[LspDiagnostic],
    pre_counts: &HashMap<DiagKey, usize>,
    post_counts: &HashMap<DiagKey, usize>,
) -> Vec<LspDiagnostic> {
    let mut result = Vec::new();
    let mut emitted: HashMap<DiagKey, usize> = HashMap::new();
    for d in pre {
        let key = diag_key(d);
        let post = *post_counts.get(&key).unwrap_or(&0);
        let pre_count = *pre_counts.get(&key).unwrap_or(&0);
        let excess = pre_count.saturating_sub(post);
        let done = *emitted.get(&key).unwrap_or(&0);
        if done < excess {
            result.push(d.clone());
            *emitted.entry(key).or_insert(0) += 1;
        }
    }
    result
}
```

---

## Tests

No test changes needed. The existing 8 tests in `diagnostics.rs::tests` all go through `diff_diagnostics()` and do not call `collect_introduced` or `collect_resolved` directly. They should all pass unchanged.

---

## Verification

```bash
# 1. Confirm no internal HashMap construction remains
grep -n 'counts_local' crates/pathfinder/src/server/tools/diagnostics.rs

# Expected: ZERO results

# 2. Full verification
cargo test -p pathfinder --lib -- diagnostics
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
```

## Completion Criteria

- [ ] `collect_introduced` takes 3 params: `post`, `pre_counts`, `post_counts`
- [ ] `collect_resolved` takes 3 params: `pre`, `pre_counts`, `post_counts`
- [ ] No `_local` HashMap variables remain in either function
- [ ] All 8 existing diagnostic tests pass
- [ ] `cargo clippy` passes with zero warnings
