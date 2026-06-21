# SPIKE-A: Semantic Path `super::` Inconsistency — Findings

## Status: RESOLVED

## Problem

When Rust code uses path-qualified types in `impl` blocks — `impl super::MyStruct`,
`impl crate::Config`, or `impl self::Handler` — the tree-sitter symbol extraction
preserved the full path prefix in the impl block's name and semantic path.

This caused two failures:

1. **Merge failure**: `merge_rust_impl_blocks_recursive` matches impl names against
   struct/enum names. An impl named `super::MyStruct` never matches a struct named
   `MyStruct`, so methods stay orphaned as children of a synthetic impl node instead
   of merging under the struct.

2. **Lookup failure**: Agents write semantic paths like `MyStruct.method`, but the
   impl node's semantic path is `super::MyStruct.method` — `SYMBOL_NOT_FOUND`.

## Root Cause

`extract_impl_block` (symbols.rs:761) reads the raw type node text and passes it
through `strip_generics` (removes `<T>`) but never strips Rust path prefixes.

Tree-sitter for `impl super::MyStruct<T>` produces a `type` node with text
`super::MyStruct<T>`. After `strip_generics` this becomes `super::MyStruct`.
The `super::` prefix survives into the symbol name and semantic path.

## Fix

Added `strip_path_prefix` function that extracts the final segment after the last
`::` delimiter:

```rust
fn strip_path_prefix(type_name: &str) -> &str {
    match type_name.rfind("::") {
        Some(idx) => &type_name[idx + 2..],
        None => type_name,
    }
}
```

Call chain in `extract_impl_block`:
```rust
// Before:
let type_name = strip_generics(type_name).to_string();
// After:
let type_name = strip_path_prefix(strip_generics(type_name)).to_string();
```

This handles all path prefixes: `super::`, `crate::`, `self::`, and multi-segment
paths like `std::fmt::Display`.

## Scope Limitation

Cross-scope merging is architecturally unsupported: `impl super::MyStruct` inside
`mod sub {}` cannot merge with the `MyStruct` struct at the parent scope, because
`merge_rust_impl_blocks_recursive` operates within a single scope level. The prefix
stripping ensures the orphaned impl node is still named correctly (`MyStruct`, not
`super::MyStruct`), enabling future cross-scope resolution if needed.

## Tests Added

| Test | Validates |
|------|-----------|
| `test_impl_block_with_super_prefix_strips_name` | Cross-scope: prefix stripped from name |
| `test_impl_block_with_super_prefix_same_scope_merges` | Same-scope: methods merge correctly |
| `test_impl_block_with_crate_prefix_merges_correctly` | `crate::` prefix stripped + merge |
| `test_impl_block_with_self_prefix_merges_correctly` | `self::` prefix stripped + merge |

## Files Changed

- `crates/pathfinder-treesitter/src/symbols.rs` — added `strip_path_prefix`, integrated into `extract_impl_block`
- `crates/pathfinder-treesitter/src/symbols_test.rs` — 4 new tests
