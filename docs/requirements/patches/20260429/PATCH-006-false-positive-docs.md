# PATCH-006: False Positive Documentation

## Status: COMPLETED (2026-04-29)

## Objective

Three findings from the audit were investigated and confirmed to be **false positives** — the code is correct by design. However, the reasoning is non-obvious. Add clarifying comments so future auditors (human or AI) do not re-investigate or incorrectly "fix" these.

No behavior changes. Comments only.

## Severity: LOW — Prevents future confusion and incorrect "fixes"

---

## Scope

| # | File | Function | Finding | Status |
|---|------|----------|---------|--------|
| 1 | `crates/pathfinder-common/src/normalize.rs` | `strip_outer_braces` | F3.2a — byte-slice indexing | False positive: chars are ASCII |
| 2 | `crates/pathfinder-treesitter/src/vue_zones.rs` | `byte_to_point` | F3.2b — byte-based column | Correct: tree-sitter uses byte columns |
| 3 | `crates/pathfinder-common/src/types.rs` | `WorkspaceRoot::resolve` | F3.4a — no symlink resolution | Mitigated: Sandbox is the security boundary |
| 4 | `crates/pathfinder-common/src/sandbox.rs` | `Sandbox::new` | F1.2b — blocking I/O in async | Acceptable: one-time startup cost |

---

## Task 6.1: Document `strip_outer_braces` byte indexing

**File:** `crates/pathfinder-common/src/normalize.rs`
**Function:** `strip_outer_braces`

Find the slice operation that was flagged: `&trimmed[1..trimmed.len() - 1]` or `&trimmed[1..close_pos]`.

Add a comment immediately above that slice:

```rust
// Safety: slicing at byte positions 0 and `close_pos` is correct here because
// `{` and `}` are both single-byte ASCII characters. `char_indices()` provides
// the byte offset of `}`, and `{` is always at byte 0. No multi-byte boundary
// can fall inside these delimiters.
```

---

## Task 6.2: Document `byte_to_point` column semantics

**File:** `crates/pathfinder-treesitter/src/vue_zones.rs`
**Function:** `byte_to_point`

Find the function (it computes `Point { row, column }` from a byte offset).

Add or replace the doc comment on the function:

```rust
/// Convert a byte offset in `source` to a tree-sitter [`Point`] (row, column).
///
/// # Column semantics
///
/// `column` is **byte-based**, not character-based. This is intentional and
/// correct: tree-sitter's `Point` uses byte offsets for column positions, not
/// Unicode character counts. Changing this to character-based would break
/// [`set_included_ranges`] interop with the tree-sitter C API.
fn byte_to_point(source: &[u8], byte: usize) -> Point {
```

Keep the function body unchanged.

---

## Task 6.3: Document `WorkspaceRoot::resolve` security model

**File:** `crates/pathfinder-common/src/types.rs`
**Function:** `WorkspaceRoot::resolve`

Find the existing doc comment (or the `pub fn resolve` line if no doc comment exists). Replace with or add:

```rust
/// Resolve a relative path against this workspace root.
///
/// # Path traversal protection
///
/// Strips `Component::ParentDir` (`..`) and `Component::RootDir` from the
/// input to prevent trivial traversal attacks. This is defense-in-depth:
/// the authoritative security boundary is [`Sandbox::check`], which runs
/// after resolution.
///
/// # Symlink behavior
///
/// Symlinks are **not** resolved. A symlink inside the workspace could point
/// outside it. This is accepted because the `Sandbox` validates the resolved
/// path against allowed roots, and Pathfinder does not follow symlinks in
/// any read/write path without a subsequent sandbox check.
pub fn resolve(&self, relative: &Path) -> PathBuf {
```

Keep the function body unchanged.

---

## Task 6.4: Document `Sandbox::new` blocking I/O

**File:** `crates/pathfinder-common/src/sandbox.rs`
**Function:** `Sandbox::new`

Find the line that calls `.exists()` on the `.pathfinderignore` path:

```rust
        let user_ignore = if ignore_path.exists() {
```

Add a comment immediately above `let ignore_path = ...` (or immediately before the `exists()` line if they're combined):

```rust
        // `.exists()` is a synchronous stat(2) syscall. This is intentional:
        // `Sandbox::new` is called once at server startup, not on the hot path.
        // If Pathfinder is ever embedded in a multi-tenant async host, this
        // should move into `tokio::task::spawn_blocking`.
        let ignore_path = workspace_root.join(".pathfinderignore");
```

---

## Verification

```bash
# 1. Confirm each comment was added
grep -n 'single-byte ASCII' crates/pathfinder-common/src/normalize.rs
grep -n 'byte-based.*tree-sitter\|tree-sitter.*byte' crates/pathfinder-treesitter/src/vue_zones.rs
grep -n 'Symlink' crates/pathfinder-common/src/types.rs
grep -n 'stat(2)\|spawn_blocking' crates/pathfinder-common/src/sandbox.rs

# Expected: at least 1 result each

# 2. No behavior changes
cargo test --all
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
```

## Completion Criteria

- [ ] `strip_outer_braces` has comment explaining ASCII-byte safety
- [ ] `byte_to_point` doc comment explains byte-based column semantics
- [ ] `WorkspaceRoot::resolve` doc comment explains symlink non-resolution
- [ ] `Sandbox::new` has comment explaining one-time blocking I/O
- [ ] Zero behavior changes (no function body modifications)
- [ ] All existing tests continue to pass
