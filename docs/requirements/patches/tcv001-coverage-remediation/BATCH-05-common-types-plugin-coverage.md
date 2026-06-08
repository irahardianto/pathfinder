# BATCH-05: Common Types, Server Types, and Plugin Coverage

Scope: `crates/pathfinder-common/src/types.rs`, `crates/pathfinder/src/server/types.rs`, `crates/pathfinder-lsp/src/plugin.rs`
Est. Uncovered Lines: ~10
Complexity: LOW
Priority: 5 (quick wins, do last or interleave)

---

## Files in Scope

| File | Lines | Uncovered Lines | Purpose |
|---|---|---|---|
| `pathfinder-common/src/types.rs` | 934 | ~7 | Shared type definitions and conversions |
| `pathfinder/src/server/types.rs` | ~1200 | ~1 | Server request/response types |
| `pathfinder-lsp/src/plugin.rs` | 634 | ~2 | Plugin state machine |

---

## Uncovered Line Ranges -- pathfinder-common/src/types.rs (~7 lines)

### Block T1: Type conversions (lines 106, 118, 155, 284-285, 354-355, 366, 494)
```
106     FromStr impl error branch for Visibility
118     Display impl for Visibility
155     Serialize impl for FileRange
284-285 Deserialize impl edge case for optional field
354-355 FromStr for SearchFilter with invalid input
366     Default impl for SearchConfig
494     Hash impl collision handling
```
Why uncovered: These are trait implementations for shared types. Tests cover primary usage but not error branches.
Strategy: Direct unit tests for each trait impl:
- Parse "invalid_visibility" -> verify error
- Parse "invalid_filter" -> verify error
- Deserialize JSON with missing optional fields
- Verify Display output format
- Verify Default values

---

## Uncovered Line Ranges -- pathfinder/src/server/types.rs (~1 line)

### Block ST1: Default impl (lines 1122-1123)
```
1122-1123 Default implementation for request/response type
```
Why uncovered: Trivially testable default implementation.
Strategy: Single unit test verifying default field values.

---

## Uncovered Line Ranges -- pathfinder-lsp/src/plugin.rs (~2 lines)

### Block P1: Plugin state machine (lines 583, 585)
```
583     Invalid state transition error
585     Plugin reload from error state
```
Why uncovered: State machine error paths require forcing invalid transitions.
Strategy:
- Force transition from Running to Initializing (invalid)
- Force reload when in Error state
- Verify error messages are correct

---

## Delivery Breakdown

### BATCH-05a: types.rs Trait Implementations (est. 7 lines covered)
Scope: Block T1
Files: pathfinder-common/src/types.rs
Tests: Add to inline `mod tests`
Cases:
- Visibility FromStr error
- Visibility Display format
- FileRange Serialize edge case
- SearchFilter FromStr error
- SearchConfig Default values
- Deserialize with missing optional field

### BATCH-05b: Server Types + Plugin State (est. 3 lines covered)
Scope: Blocks ST1, P1
Files: pathfinder/src/server/types.rs, pathfinder-lsp/src/plugin.rs
Tests: Add to inline `mod tests`
Cases:
- Default implementation verification
- Invalid state transition
- Reload from error state

---

## Test Code Patterns

```rust
// Pattern: FromStr error branch
#[test]
fn test_visibility_from_str_error() {
    let result = "invalid".parse::<Visibility>();
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("invalid visibility"));
}

// Pattern: Invalid state transition
#[test]
fn test_plugin_invalid_state_transition() {
    let plugin = Plugin::new(PluginState::Running);
    let result = plugin.transition(PluginState::Initializing);
    assert!(result.is_err());
}
```

---

## Estimated Impact

| Sub-batch | Lines Covered | Cumulative LCV |
|---|---|---|
| BATCH-05a | ~7 | +0.06% |
| BATCH-05b | ~3 | +0.03% |
| **Total** | **~10** | **+0.09%** |

---

## Validation

```bash
cargo test -p pathfinder-common -p pathfinder -p pathfinder-lsp -- types plugin
cargo clippy -p pathfinder-common -p pathfinder -p pathfinder-lsp -- -D warnings
```

---

## Why This Batch Exists

Despite being small (~10 lines), these gaps matter:
1. They are trivially fixable -- no infrastructure needed
2. Each one represents a type safety gap (untested error branches)
3. Can be done while waiting for larger batches to review
4. Perfect for interleaving during code review of other batches
