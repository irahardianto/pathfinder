# PATCH-001: Split `edit.rs` into Focused Submodules

**Status:** Planned  
**Priority:** Low (structural, no behavioral change)  
**Estimated Effort:** 1–2 hours  
**Prerequisite:** None  
**PR Strategy:** Standalone PR — zero logic changes, purely structural

---

## Problem Statement

`crates/pathfinder/src/server/tools/edit.rs` is **~3200 lines** — the largest file in the
entire codebase. It contains the complete edit pipeline: 7 tool handlers, 5 batch
resolvers, LSP validation infrastructure, OCC/TOCTOU guards, text-edit resolution,
normalization helpers, and ~800 lines of tests.

While the internal decomposition (Phase 1–3 of the Qlty remediation) successfully
reduced per-function complexity and eliminated structural duplication, the file itself
remains a single module. This creates:

1. **Cognitive overhead** — Contributors must navigate 3200 lines to find relevant code.
2. **Git conflict risk** — Any two concurrent edit-tool PRs will conflict in the same file.
3. **IDE performance** — Large single files degrade Rust Analyzer responsiveness.

## Proposed Module Structure

```
crates/pathfinder/src/server/tools/
├── edit/
│   ├── mod.rs              ← Re-exports, shared types
│   ├── handlers.rs         ← Individual tool _impl functions
│   ├── batch.rs            ← Batch edit pipeline
│   ├── text_edit.rs        ← Text-based edit resolution
│   ├── validation.rs       ← LSP validation pipeline
│   └── tests.rs            ← All #[cfg(test)] code
└── (edit.rs removed)
```

### Module Responsibilities

#### `mod.rs` (~80 lines)
- `FinalizeEditParams` struct
- `ResolvedEdit` struct (both variants)
- `InsertEdge` enum
- `ValidationOutcome` struct
- Re-exports for `PathfinderServer` impl blocks

#### `handlers.rs` (~600 lines)
- `replace_body_impl`
- `replace_full_impl`
- `insert_before_impl`
- `insert_after_impl`
- `delete_symbol_impl`
- `validate_only_impl`
- `resolve_insert_position`

#### `batch.rs` (~350 lines)
- `replace_batch_impl`
- `validate_batch_occ`
- `resolve_single_batch_edit`
- `resolve_semantic_batch_edit`
- `resolve_batch_replace_body`
- `resolve_batch_replace_full`
- `resolve_batch_insert_before`
- `resolve_batch_insert_after`
- `resolve_batch_delete`
- `apply_sorted_edits`

#### `text_edit.rs` (~200 lines)
- `resolve_text_edit`
- `build_line_starts`
- `compute_search_window`
- `collapse_whitespace`
- `collapse_and_match`
- `ResolvedEditFree` struct

#### `validation.rs` (~250 lines)
- `run_lsp_validation`
- `lsp_error_to_skip_reason`
- `build_validation_outcome`
- `finalize_edit`
- `flush_edit_with_toctou`
- `hash_file_content`
- `resolve_hash_for_full_or_bare`
- `resolve_hash_for_symbol_range`
- `resolve_version_hash_for_edit_type`

#### `tests.rs` (~800 lines)
- `UnsupportedDiagLawyer` mock
- `make_server_dyn`, `make_server`, `make_body_range` helpers
- All `#[tokio::test]` functions

## Implementation Steps

Each step must produce a green build (`cargo test --workspace && cargo clippy`):

1. **Create `edit/mod.rs`** — Move shared types, add `mod` declarations
2. **Create `edit/validation.rs`** — Move validation pipeline (no external callers, clean cut)
3. **Create `edit/text_edit.rs`** — Move free functions (no `self`, easiest to split)
4. **Create `edit/batch.rs`** — Move batch pipeline (`impl PathfinderServer` block)
5. **Create `edit/handlers.rs`** — Move remaining tool handlers
6. **Create `edit/tests.rs`** — Move all `#[cfg(test)]` code
7. **Delete `edit.rs`** — Replaced by `edit/` directory
8. **Verify:** `cargo test --workspace`, `cargo clippy`, `cargo fmt --check`
9. **Commit:** Single squashed commit, PR title: `refactor(edit): split monolithic edit.rs into submodules`

## Risk Mitigation

- **Visibility:** All functions are either `pub(crate)` methods on `PathfinderServer` or
  private free functions. Splitting into submodules within the same crate does not change
  visibility boundaries.
- **`impl PathfinderServer`:** Multiple files can contain `impl` blocks for the same type
  in Rust. Each submodule gets its own `impl PathfinderServer { ... }` block.
- **Test isolation:** Tests access handlers through `PathfinderServer` methods. Moving tests
  to a separate file within the same module preserves all access.

## Acceptance Criteria

- [ ] `edit.rs` no longer exists as a single file
- [ ] All 6 submodules are < 800 lines each
- [ ] `cargo test --workspace` passes with identical test count
- [ ] `cargo clippy --all-targets --all-features` produces 0 warnings
- [ ] No `pub` visibility changes (all items remain `pub(crate)` or private)
- [ ] Git diff shows 0 lines of logic change (only `mod`/`use` additions)
