# PATCH-001: Serialization Safety

## Status: COMPLETED (2026-04-29)

## Objective

Replace 7 instances of `serde_json::to_value(...).unwrap_or_default()` with a centralized helper that logs warnings on serialization failure instead of silently degrading to `null`. This ensures AI agents are aware when structured metadata is lost.

## Severity: MEDIUM — Silent data loss affects agent decision-making

---

## Scope

| # | File | Line(s) | Action |
|---|------|---------|--------|
| 1 | `crates/pathfinder/src/server/helpers.rs` | EOF | ADD `serialize_metadata` helper |
| 2 | `crates/pathfinder/src/server/tools/source_file.rs` | 203 | REPLACE |
| 3 | `crates/pathfinder/src/server/tools/navigation.rs` | 468 | REPLACE |
| 4 | `crates/pathfinder/src/server/tools/navigation.rs` | 848 | REPLACE |
| 5 | `crates/pathfinder/src/server/tools/file_ops.rs` | 377 | REPLACE |
| 6 | `crates/pathfinder/src/server/tools/file_ops.rs` | 509 | REPLACE |
| 7 | `crates/pathfinder/src/server/tools/symbols.rs` | 64 | REPLACE |
| 8 | `crates/pathfinder/src/server/tools/repo_map.rs` | 111 | REPLACE |

---

## Step 1: Add Helper Function

**File:** `crates/pathfinder/src/server/helpers.rs`

Add this function BEFORE the `#[cfg(test)]` block at the bottom of the file (i.e., after the `require_symbol_target` function, before the test module):

```rust
// ── Serialization Helpers ───────────────────────────────────────────

/// Serialize metadata to JSON, logging a warning on failure instead of
/// silently degrading to `Value::Null` via `unwrap_or_default()`.
///
/// Returns `Some(Value)` on success, `None` on failure (with a warning log).
pub(crate) fn serialize_metadata<T: serde::Serialize>(metadata: &T) -> Option<serde_json::Value> {
    match serde_json::to_value(metadata) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(
                error = %e,
                type_name = std::any::type_name::<T>(),
                "structured metadata serialization failed; agent will receive null"
            );
            None
        }
    }
}
```

---

## Step 2: Replace All 7 Call Sites

For each file below, find the exact pattern and replace it.

### 2a. `crates/pathfinder/src/server/tools/source_file.rs`

**Add import** at the top of the file (with other `use crate::server::helpers::*` imports):
```rust
use crate::server::helpers::serialize_metadata;
```

**Find (line ~203):**
```rust
                    Some(serde_json::to_value(&metadata).unwrap_or_default());
```

**Replace with:**
```rust
                    serialize_metadata(&metadata);
```

### 2b. `crates/pathfinder/src/server/tools/navigation.rs`

**Add import** at the top (with other helper imports):
```rust
use crate::server::helpers::serialize_metadata;
```

**Find (line ~468):**
```rust
        res.structured_content = Some(serde_json::to_value(metadata).unwrap_or_default());
```

**Replace with:**
```rust
        res.structured_content = serialize_metadata(&metadata);
```

**Find (line ~848):**
```rust
        res.structured_content = Some(serde_json::to_value(metadata).unwrap_or_default());
```

**Replace with:**
```rust
        res.structured_content = serialize_metadata(&metadata);
```

### 2c. `crates/pathfinder/src/server/tools/file_ops.rs`

**Add import:**
```rust
use crate::server::helpers::serialize_metadata;
```

**Find (line ~377):**
```rust
        res.structured_content = Some(serde_json::to_value(metadata).unwrap_or_default());
```

**Replace with:**
```rust
        res.structured_content = serialize_metadata(&metadata);
```

**Find (line ~509):**
```rust
        res.structured_content = Some(serde_json::to_value(metadata).unwrap_or_default());
```

**Replace with:**
```rust
        res.structured_content = serialize_metadata(&metadata);
```

### 2d. `crates/pathfinder/src/server/tools/symbols.rs`

**Add import:**
```rust
use crate::server::helpers::serialize_metadata;
```

**Find (line ~64):**
```rust
                    Some(serde_json::to_value(&metadata).unwrap_or_default());
```

**Replace with:**
```rust
                    serialize_metadata(&metadata);
```

### 2e. `crates/pathfinder/src/server/tools/repo_map.rs`

**Add import:**
```rust
use crate::server::helpers::serialize_metadata;
```

**Find (line ~111):**
```rust
        res.structured_content = Some(serde_json::to_value(metadata).unwrap_or_default());
```

**Replace with:**
```rust
        res.structured_content = serialize_metadata(&metadata);
```

---

## EXCLUSIONS — Do NOT Modify These

These lines also contain `unwrap_or_default()` but are **NOT** serde serialization calls. Do not touch them:

| File | Line(s) | Reason |
|------|---------|--------|
| `edit/handlers.rs` | 581, 604, 698, 744, 789 | `Option<String>::unwrap_or_default()` for `new_code` |
| `edit/batch.rs` | 283 | `Option<&str>::unwrap_or_default()` for `new_code` |
| `ripgrep.rs` | 419-420 | `Mutex::into_inner().unwrap_or_default()` |
| `edit/text_edit.rs` | 309 | `Option<String>::unwrap_or_default()` for diagnostic code |
| `navigation.rs` | 340, 730 | Non-serde `.unwrap_or_default()` (different pattern) |

---

## Step 3: Add Unit Test

**File:** `crates/pathfinder/src/server/helpers.rs`

Add this test inside the existing `mod tests` block:

```rust
    #[test]
    fn test_serialize_metadata_success() {
        use std::collections::HashMap;
        let mut map = HashMap::new();
        map.insert("key", "value");
        let result = super::serialize_metadata(&map);
        assert!(result.is_some());
    }
```

---

## Verification

```bash
# 1. Check no remaining unwrap_or_default on serde_json::to_value in tool handlers
grep -rn 'serde_json::to_value.*unwrap_or_default' crates/pathfinder/src/server/tools/

# Expected: ZERO results (all replaced)

# 2. Confirm helper exists
grep -n 'fn serialize_metadata' crates/pathfinder/src/server/helpers.rs

# Expected: 1 result

# 3. Full verification
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

## Completion Criteria

- [ ] `serialize_metadata` helper function added to `helpers.rs`
- [ ] All 7 call sites replaced
- [ ] No serde `unwrap_or_default` patterns remain in `server/tools/`
- [ ] New unit test passes
- [ ] `cargo test --all` passes
- [ ] `cargo clippy` passes with zero warnings
