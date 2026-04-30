# PATCH-010: Fix insert_after Missing Blank Line Before Doc Comments

## Group: C (Medium) — Validation & Fallback Improvements

## Objective

Fix the formatting bug where `insert_after` concatenates the closing brace of the target
symbol with the doc comment of the inserted code, missing the required blank line separator.

Example of the bug:
```rust
}
}/// Ensures the string ends with a newline character.
```

Should be:
```rust
}

/// Ensures the string ends with a newline character.
```

## Severity: MEDIUM — generates syntactically valid but idiomatically wrong code

## Background

The `resolve_insert_after` method in `handlers.rs` computes `before_sep` (separator between
the existing code and the inserted code). The separator logic:

```rust
let before_sep = if before.ends_with(b"\n\n")
    || after.starts_with(b"\n\n")
    || (before.ends_with(b"\n") && after.starts_with(b"\n"))
{
    ""
} else if before.ends_with(b"\n") {
    "\n"
} else {
    "\n\n"
};
```

The problem: `before` is the source up to `insert_byte`, which for `insert_after` is the
byte right after the closing brace. If the source is `}\n`, then `before` ends with `\n`
(but not `\n\n`), and `after` starts with whatever follows. The condition `before.ends_with(b"\n")`
matches, so `before_sep = "\n"`, adding only one newline. But the inserted code starts with
`///` (doc comment), which needs a blank line before it for idiomatic Rust.

The issue is that `before_sep` only adds one `\n`, resulting in `}\n///` instead of `}\n\n///`.

The fix: when the inserted code starts with a doc comment (`///`, `//!`, `/**`, etc.),
ensure there's a blank line before it regardless of the existing spacing.

## Scope

| # | File | Action |
|---|------|--------|
| 1 | `crates/pathfinder/src/server/tools/edit/handlers.rs` | Fix `before_sep` logic in `resolve_insert_after` |

## Step 1: Fix the before_sep logic

**File:** `crates/pathfinder/src/server/tools/edit/handlers.rs`

**Find in `resolve_insert_after`:**
```rust
        let before_sep = if before.ends_with(b"\n\n")
            || after.starts_with(b"\n\n")
            || (before.ends_with(b"\n") && after.starts_with(b"\n"))
        {
            ""
        } else if before.ends_with(b"\n") {
            "\n"
        } else {
            "\n\n"
        };
```

**Replace with:**
```rust
        // Doc comments need a blank line before them for idiomatic formatting.
        // Detect if the inserted code starts with a doc comment marker.
        let inserted_starts_doc_comment = indented
            .bytes()
            .next()
            .is_some_and(|b| b == b'/')
            && (indented.starts_with("///")
                || indented.starts_with("//!")
                || indented.starts_with("/**")
                || indented.starts_with("/*!" ));

        let before_sep = if before.ends_with(b"\n\n")
            || after.starts_with(b"\n\n")
            || (before.ends_with(b"\n") && after.starts_with(b"\n"))
        {
            if inserted_starts_doc_comment && !before.ends_with(b"\n\n") {
                "\n" // Add one more newline to create blank line before doc comment
            } else {
                ""
            }
        } else if before.ends_with(b"\n") {
            if inserted_starts_doc_comment {
                "\n\n" // Blank line before doc comment
            } else {
                "\n"
            }
        } else {
            "\n\n"
        };
```

## EXCLUSIONS — Do NOT Modify These

- `insert_before` — already handles spacing correctly per the agent report
- `insert_into` — different code path with its own separator logic
- `normalize_blank_lines` — this runs AFTER the separator and can't fix a missing separator
- Python/YAML/TOML files — `is_whitespace_significant_file` already skips
  `normalize_blank_lines` for these. The doc comment detection only triggers for `/`
  which doesn't apply to Python docstrings.

## Verification

```bash
# 1. Build
cargo build --all

# 2. Run edit handler tests
cargo test -p pathfinder insert_after

# 3. Full verification
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

## Expected Impact

Inserting code after a function when the new code starts with `///`:
- Before: `}\n}/// Doc comment` — missing blank line
- After: `}\n\n}/// Doc comment` — proper blank line before doc comment
