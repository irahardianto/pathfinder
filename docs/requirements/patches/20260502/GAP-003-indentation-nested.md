# GAP-003: Fix dedent_then_reindent for Nested Blocks

## Group: B (High) — Silent Correctness Bugs
## Depends on: Nothing

## Objective

When `replace_body` is used to replace a function body containing multi-line nested
structures (if-else, match arms), the `dedent_then_reindent` pipeline produces
over-indented output. The inner lines of the nested structure get `target_column +
original_relative_indent` instead of the correct `target_column + relative_indent`.

Example from the Rust report:
```
// Expected (all at consistent indent):
let greeting = if name.is_empty() {
    "Hello, stranger!".to_owned()
} else {
    format!("Hello, {}!", name)
};
greeting

// Actual (inner lines over-indented):
let greeting = if name.is_empty() {
        "Hello, stranger!".to_owned()    // ← 8 spaces instead of 4
    } else {
        format!("Hello, {}!", name)      // ← 8 spaces instead of 4
    };
    greeting                              // ← 4 spaces instead of 0
```

## Root Cause Analysis

The pipeline is:
1. `dedent(code)` — finds `min_indent` of all non-empty lines, strips that many chars
2. `reindent(dedented, target_column)` — prepends `target_column` spaces to every non-empty line

The problem: `dedent` calculates `min_indent` across ALL lines. For the nested if-else:
```
let greeting = if name.is_empty() {    // indent = 4 (min)
        "Hello, stranger!".to_owned() // indent = 8
    } else {                          // indent = 4
```

`min_indent = 4`. After dedent, the "Hello" line is at column 4 (8-4=4), which is correct
as a relative indent within the if block. But then `reindent` adds `target_column` (4)
to EVERY line, giving:
- Line 1: 4+0 = 4 ✓
- Line 2: 4+4 = 8 ← should be 4+4 = 8... actually wait, this IS correct if target_column is the function body indent (4).

Actually, re-reading the code more carefully:

The issue is that the agent's `new_code` parameter for `replace_body` already includes
some indentation, and the pipeline doesn't handle this correctly when the code contains
nested blocks.

When an agent provides:
```rust
let greeting = if name.is_empty() {
    "Hello, stranger!".to_owned()
} else {
    format!("Hello, {}!", name)
};
greeting
```

This code has min_indent = 0 (the first and last lines start at column 0).
So `dedent` is a no-op. Then `reindent` adds `target_column` (body_indent_column = 4)
to every line, giving the correct result.

But if the agent provides the code WITH some leading indentation:
```rust
    let greeting = if name.is_empty() {
        "Hello, stranger!".to_owned()
    } else {
        format!("Hello, {}!", name)
    };
    greeting
```

Now min_indent = 4. Dedent strips 4 from all lines. The result:
```
let greeting = if name.is_empty() {
    "Hello, stranger!".to_owned()   // was 8, now 4 ✓
} else {                            // was 4, now 0 ✓
    format!("Hello, {}!", name)     // was 8, now 4 ✓
};                                  // was 4, now 0 ✓
greeting                            // was 4, now 0 ✓
```

Then reindent with target_column=4:
```
    let greeting = if name.is_empty() {
        "Hello, stranger!".to_owned()   // 4+4=8 ← OVER-INDENTED
    } else {                            // 4+0=4 ✓
        format!("Hello, {}!", name)     // 4+4=8 ← OVER-INDENTED
    };                                  // 4+0=4 ✓
    greeting                            // 4+0=4 ← OVER-INDENTED (should be 0)
```

The root cause: `reindent` adds `target_column` to ALL lines, including lines that
should be at the SAME indent level as the top-level statement (like `} else {` and `};`
and `greeting`). These lines should be at `target_column` (body indent), but their
content after dedent is at column 0, and adding `target_column` gives the right result.
The INNER lines are at relative column 4 after dedent, and adding `target_column` (4)
gives column 8, which is over-indented.

Wait — actually, column 8 IS correct for the inner lines of an if-else inside a
function body at column 4. The function body is at column 4, the if-else contents
are at column 8. That's standard Rust formatting.

The ACTUAL problem is with the LAST line: `greeting` after dedent is at column 0,
then reindent adds 4 → column 4. But `greeting` is a top-level statement in the
function body and SHOULD be at column 4... which IS correct.

So where's the bug? Let me re-read the report more carefully:

```
// Actual (inner lines over-indented):
let greeting = if name.is_empty() {
        "Hello, stranger!".to_owned()
    } else {
```

The if-else block starts at column 0 (no indent), but its contents are at column 8
and the `} else {` at column 4. That means `target_column` was applied to the
OUTER if-else, and then the inner content got target_column + 4 = 8.

But if the function body indent is 4, the top-level `let greeting` should be at
column 4, not column 0. This suggests the issue is that `replace_body` is treating
the body as if `target_column = 0` when it should be `target_column = 4`.

Actually, the deeper issue might be in `resolve_body_range` → `detect_body_indent`.

## Revised Root Cause

Looking at `resolve_body_range`:
```rust
let body_indent_column = Self::detect_body_indent(&source, start_byte, end_byte, is_brace_block, fallback_indent);
```

And `detect_body_indent` for brace blocks:
```rust
// Find first non-empty line INSIDE the block (after the `{`)
for line in lines {
    if !line.trim().is_empty() {
        return line.len() - line.trim_start().len();
    }
}
```

This returns the indent of the FIRST non-empty line inside the block, which IS the
correct body indent. For a function like:
```rust
fn greet(name: &str) -> String {
    let greeting = if name.is_empty() {
```
detect_body_indent returns 4 (the indent of `let greeting`).

Then `dedent_then_reindent(new_code, 4)` runs on the agent's provided code.

If the agent provides code starting at column 0 (no leading whitespace), dedent is
a no-op (min_indent=0), and reindent adds 4 to every line → correct.

If the agent provides code starting at column 4 (with body-level whitespace), dedent
strips 4 from all lines → correct, then reindent adds 4 → correct.

If the agent provides code with INCONSISTENT indentation (some lines at 0, some at 4),
dedent uses min_indent=0 → no-op, reindent adds 4 to everything → the lines that
were already at 4 become 8 → BUG.

**This is the actual bug**: when the agent's `new_code` has lines at different indent
levels, and the minimum is 0, lines that were at the body indent level get doubled.

## Fix Strategy

The fix should be in `normalize_for_body_replace` (which already runs before dedent).
It should strip leading whitespace to column 0 for the first non-empty line, making
the code "anchored" at column 0. Then dedent is a no-op for clean code, and reindent
correctly adds `target_column`.

Alternatively, improve `dedent` to be smarter about the base indent level. The current
`min_indent` approach is mathematically correct but doesn't account for the intent that
the code should be treated as starting at column 0.

## Scope

| File | Function | Change |
|------|----------|--------|
| `crates/pathfinder-common/src/normalize.rs` | `normalize_for_body_replace` | Ensure code is anchored at column 0 before dedent |
| `crates/pathfinder-common/src/indent.rs` | `dedent_then_reindent` | Add validation/assertion |
| `crates/pathfinder-common/src/indent.rs` | tests | Add test for nested block case |

## Current Code

```rust
// crates/pathfinder-common/src/normalize.rs
pub fn normalize_for_body_replace(code: &str) -> String {
    let code = strip_markdown_fences(code);
    let code = strip_outer_braces(&code);
    let code = normalize_line_endings(&code);
    code
}
```

## Target Code

```rust
pub fn normalize_for_body_replace(code: &str) -> String {
    let code = strip_markdown_fences(code);
    let code = strip_outer_braces(&code);
    let code = normalize_line_endings(&code);
    // Ensure the code block starts at column 0 by removing any leading
    // whitespace from the first non-empty line. This prevents dedent_then_reindent
    // from producing over-indented output when the agent provides code with
    // inconsistent leading indentation.
    anchor_to_column_zero(&code)
}

/// Strip leading whitespace from the first non-empty line and adjust all
/// subsequent lines by the same amount. This ensures the code block starts
/// at column 0 while preserving relative indentation.
fn anchor_to_column_zero(code: &str) -> String {
    let first_indent = code
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .unwrap_or(0);

    if first_indent == 0 {
        return code.to_owned();
    }

    // Dedent by the first line's indent — this anchors at column 0
    // while preserving all relative indentation within the block.
    dedent_by(code, first_indent)
}

fn dedent_by(code: &str, columns: usize) -> String {
    code.lines()
        .map(|line| {
            let expanded = expand_tabs(line);
            if expanded.len() >= columns && !expanded.trim().is_empty() {
                expanded[columns..].to_owned()
            } else if expanded.trim().is_empty() {
                String::new() // blank lines stay blank
            } else {
                expanded.trim_start().to_owned()
            }
        })
        .collect::<Vec<String>>()
        .join("\n")
}
```

## Exclusions

- Do NOT change `reindent` — it's correct.
- Do NOT change `detect_body_indent` — it correctly detects the target column.
- Do NOT change `dedent` — the existing `min_indent` approach is correct for general use.
  The fix belongs in the normalization step, not the dedent algorithm.

## Verification

```bash
cargo test -p pathfinder-common -- dedent reindent normalize
cargo test -p pathfinder-common -- test_anchor_to_column_zero_nested_if_else
```

## Tests

### Test 1: test_anchor_to_column_zero_nested_if_else
```rust
let input = "    let x = if cond {\n        val1\n    } else {\n        val2\n    };\n    result";
let result = anchor_to_column_zero(input);
// First line starts at column 4, so everything shifts left by 4
assert!(result.starts_with("let x = if cond {"));
assert!(result.contains("    val1")); // 4 spaces (was 8, shifted by 4)
```

### Test 2: test_anchor_preserves_relative_indent
```rust
let input = "    line1\n        nested\n    line3";
let result = anchor_to_column_zero(input);
assert_eq!(result, "line1\n    nested\nline3");
```

### Test 3: test_replace_body_nested_if_else_indentation
Integration test that runs the full `dedent_then_reindent(anchor(normalize(code)), target_column=4)`
pipeline and verifies the output has consistent indentation.
