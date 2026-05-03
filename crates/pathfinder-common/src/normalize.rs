//! Input normalization for Pathfinder edit tools.
//!
//! Implements PRD §3.4 step 0: sanitize LLM-generated `new_code` before
//! insertion into the AST edit pipeline.
//!
//! All functions are pure and allocation-minimal.

use std::borrow::Cow;

/// Strip markdown code fences from LLM output.
///
/// Many LLMs wrap `new_code` in triple-backtick fences even when instructed
/// not to. This function detects the pattern and strips the outer fence,
/// leaving only the interior content.
///
/// Handles both:
/// - `` ```lang\ncode\n``` `` (with language tag)
/// - `` ```\ncode\n``` `` (no language tag)
///
/// Returns the original string unchanged if no fences are detected.
#[must_use]
pub fn strip_markdown_fences(input: &str) -> &str {
    let trimmed = input.trim();

    // Must start with ```
    let Some(after_open) = trimmed.strip_prefix("```") else {
        return input;
    };

    // Must end with ``` (possibly with trailing whitespace before in original)
    if !trimmed.ends_with("```") {
        return input;
    }

    // Strip the optional language tag on the opening line
    let after_lang = after_open.split_once('\n').map_or("", |(_, rest)| rest);

    // Strip the closing ```
    let Some(body) = after_lang.strip_suffix("```") else {
        return input;
    };

    // Trim one trailing newline before the closing fence
    body.strip_suffix('\n').unwrap_or(body)
}

/// Strip outermost braces from code that wraps its body in `{ ... }`.
///
/// LLMs are heavily trained to produce syntactically-complete code and
/// frequently wrap `new_code` in `{ ... }` despite being instructed not to.
/// This function detects that pattern and strips only the outermost matching
/// braces, preventing the `{{ ... }}` double-brace failure mode.
///
/// Rules:
/// - Both the first non-whitespace char must be `{` and the last must be `}`
/// - The braces must be a matching pair (not just any leading/trailing chars)
/// - Interior content is returned trimmed of any whitespace adjacent to the braces
///
/// Returns the original string unchanged if no outer brace wrapping is found.
#[must_use]
pub fn strip_outer_braces(input: &str) -> &str {
    let trimmed = input.trim();

    if !(trimmed.starts_with('{') && trimmed.ends_with('}')) {
        return input;
    }

    // Verify matching — walk the string, track depth
    let mut depth: i32 = 0;
    let mut close_pos = None;
    for (i, ch) in trimmed.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close_pos = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }

    // The outer `{` must match the final `}`
    match close_pos {
        Some(pos) if pos == trimmed.len() - 1 => {
            // Safety: slicing at byte positions 1 and `len-1` is correct here because
            // `{` and `}` are both single-byte ASCII characters. The slice
            // cannot fall on a multi-byte boundary since we're only indexing into
            // positions adjacent to these ASCII delimiters.
            &trimmed[1..trimmed.len() - 1]
        }
        _ => input,
    }
}

/// Normalize line endings: `\r\n` → `\n`.
///
/// Returns a `Cow::Borrowed` when no CRLF sequences are present (zero-copy
/// fast path). Allocates a `String` only when normalization is needed.
#[must_use]
pub fn normalize_line_endings(input: &str) -> Cow<'_, str> {
    if input.contains("\r\n") {
        Cow::Owned(input.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(input)
    }
}

/// Run all input normalizations required before inserting body content.
///
/// Applies in PRD-specified order:
/// 1. Strip markdown fences
/// 2. Strip outer braces (for `replace_body`)
/// 3. Normalize CRLF → LF
/// 4. Anchor to column 0 (GAP-003: prevent over-indentation for nested blocks)
#[must_use]
pub fn normalize_for_body_replace(input: &str) -> String {
    let step1 = strip_markdown_fences(input);
    let step2 = strip_outer_braces(step1);
    let step3 = normalize_line_endings(step2).into_owned();
    anchor_to_column_zero(&step3)
}

/// Run input normalizations for tools that do NOT strip outer braces.
///
/// Used by `replace_full`, `insert_before`, `insert_after`.
/// Applies:
/// 1. Strip markdown fences
/// 2. Normalize CRLF → LF
#[must_use]
pub fn normalize_for_full_replace(input: &str) -> String {
    let step1 = strip_markdown_fences(input);
    normalize_line_endings(step1).into_owned()
}

/// Strip leading whitespace from the first non-empty line and adjust all
/// subsequent lines by the same amount. This ensures the code block starts
/// at column 0 while preserving relative indentation.
///
/// # Example
/// ```
/// use pathfinder_common::normalize::anchor_to_column_zero;
/// let input = "    let x = if cond {\n        val1\n    } else {\n        val2\n    }";
/// let result = anchor_to_column_zero(input);
/// assert_eq!(result, "let x = if cond {\n    val1\n} else {\n    val2\n}");
/// ```
#[must_use]
pub fn anchor_to_column_zero(code: &str) -> String {
    // Check if all lines are empty/whitespace
    let all_empty = code.lines().all(|l| l.trim().is_empty());
    if all_empty {
        return String::new();
    }

    let first_indent = code
        .lines()
        .find(|l| !l.trim().is_empty())
        .map_or(0, |l| l.len() - l.trim_start().len());

    if first_indent == 0 {
        return code.to_owned();
    }

    // Dedent by the first line's indent — this anchors at column 0
    // while preserving all relative indentation within the block.
    code.lines()
        .map(|line| {
            let expanded = crate::indent::expand_tabs(line);
            let leading_spaces = expanded.len() - expanded.trim_start().len();

            if expanded.trim().is_empty() {
                String::new() // blank lines stay blank
            } else if leading_spaces >= first_indent {
                // Line has enough leading spaces, dedent by first_indent
                expanded[first_indent..].to_owned()
            } else {
                // Line has fewer leading spaces, preserve its relative position
                // by keeping it at (leading_spaces) spaces
                // (it will be to the left of the first line's anchor point)
                expanded.trim_start().to_owned()
            }
        })
        .collect::<Vec<String>>()
        .join("\n")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // ── strip_markdown_fences ───────────────────────────────────────────

    #[test]
    fn test_strip_markdown_fences_with_lang() {
        let input = "```rust\nfn hello() {}\n```";
        assert_eq!(strip_markdown_fences(input), "fn hello() {}");
    }

    #[test]
    fn test_strip_markdown_fences_no_lang() {
        let input = "```\nfn hello() {}\n```";
        assert_eq!(strip_markdown_fences(input), "fn hello() {}");
    }

    #[test]
    fn test_strip_markdown_fences_passthrough_no_fences() {
        let input = "fn hello() {}";
        assert_eq!(strip_markdown_fences(input), "fn hello() {}");
    }

    #[test]
    fn test_strip_markdown_fences_passthrough_partial() {
        let input = "```rust\nfn hello() {}";
        // No closing fence — return unchanged
        assert_eq!(strip_markdown_fences(input), input);
    }

    #[test]
    fn test_strip_markdown_fences_opening_only_no_closing() {
        let input = "```rust\nfn main() {}\n// no closing fence";
        let result = strip_markdown_fences(input);
        assert_eq!(result, input); // passthrough — no matching closing fence
    }

    #[test]
    fn test_strip_markdown_fences_multiline() {
        let input = "```typescript\nconst x = 1;\nconst y = 2;\n```";
        assert_eq!(strip_markdown_fences(input), "const x = 1;\nconst y = 2;");
    }

    // ── strip_outer_braces ──────────────────────────────────────────────

    #[test]
    fn test_strip_outer_braces_simple() {
        let input = "{ return 42; }";
        assert_eq!(strip_outer_braces(input), " return 42; ");
    }

    #[test]
    fn test_strip_outer_braces_multiline() {
        let input = "{\n  x := 1\n  return x\n}";
        let result = strip_outer_braces(input);
        assert_eq!(result, "\n  x := 1\n  return x\n");
    }

    #[test]
    fn test_strip_outer_braces_nested_inner_preserved() {
        let input = "{ if (x) { y } }";
        let result = strip_outer_braces(input);
        // Only the outermost braces are stripped
        assert_eq!(result, " if (x) { y } ");
    }

    #[test]
    fn test_strip_outer_braces_not_wrapped() {
        let input = "return 42;";
        assert_eq!(strip_outer_braces(input), "return 42;");
    }

    #[test]
    fn test_strip_outer_braces_unmatched() {
        // First `{` doesn't match the trailing `}` at the very end
        let input = "{ x } something }";
        // The outer `{` matches the `}` at position 4, not at the end
        // → not outer-wrapped, return unchanged
        assert_eq!(strip_outer_braces(input), "{ x } something }");
    }

    // ── normalize_line_endings ──────────────────────────────────────────

    #[test]
    fn test_normalize_crlf_to_lf() {
        let input = "line1\r\nline2\r\nline3";
        let result = normalize_line_endings(input);
        assert_eq!(result.as_ref(), "line1\nline2\nline3");
    }

    #[test]
    fn test_normalize_already_lf_is_borrowed() {
        let input = "line1\nline2";
        let result = normalize_line_endings(input);
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    // ── normalize_for_body_replace ──────────────────────────────────────

    #[test]
    fn test_normalize_full_pipeline_fence_and_braces() {
        let input = "```go\n{ return 42; }\n```";
        let result = normalize_for_body_replace(input);
        // Fence stripped → `{ return 42; }` → outer braces stripped → ` return 42; `
        // Then anchor_to_column_zero → strips leading space → `return 42; `
        // Note: trailing space is preserved from the original input
        assert_eq!(result, "return 42; ");
    }

    #[test]
    fn test_normalize_full_pipeline_plain_code() {
        let input = "x := compute()\nreturn x";
        let result = normalize_for_body_replace(input);
        assert_eq!(result, "x := compute()\nreturn x");
    }

    #[test]
    fn test_normalize_full_pipeline_crlf() {
        let input = "x := 1\r\nreturn x";
        let result = normalize_for_body_replace(input);
        assert_eq!(result, "x := 1\nreturn x");
    }

    #[test]
    fn test_normalize_body_replace_anchors_nested_blocks() {
        // GAP-003: verify that normalize_for_body_replace anchors code to column 0
        // before it's passed to dedent_then_reindent, preventing over-indentation.
        let input = "    let greeting = if name.is_empty() {\n        \"Hello, stranger!\".to_owned()\n    } else {\n        format!(\"Hello, {}!\", name)\n    };\n    greeting";
        let result = normalize_for_body_replace(input);
        // After anchoring, the code should start at column 0
        assert!(
            result.starts_with("let greeting = if name.is_empty() {"),
            "code should be anchored at column 0"
        );
        // Relative indentation should be preserved (nested content at 4 spaces relative to if)
        assert!(
            result.contains("    \"Hello, stranger!\".to_owned()"),
            "nested content should be at column 4 (relative to if block at column 0)"
        );
        assert!(
            result.contains("} else {"),
            "else branch should be at column 0"
        );
    }

    // ── L39-40 uncovered branch: closing fence unreachable after lang-tag split ─

    #[test]
    fn test_strip_markdown_fences_inline_no_newline_returns_input() {
        // Trigger the else-branch at L39: the input starts AND ends with ```
        // but there is no newline after the opening fence.
        // `after_open.split_once('\n')` returns `None` → `after_lang = ""`
        // → `"".strip_suffix("```")` fails → return input unchanged.
        let input = "```code```";
        assert_eq!(
            strip_markdown_fences(input),
            input,
            "inline fence with no newline must be returned unchanged"
        );
    }

    #[test]
    fn test_strip_markdown_fences_only_opening_and_closing_no_body() {
        // Another way to hit L39: opening + closing fences with lang tag but
        // the body between them ends in a word (not ```), yet trimmed ends with ```
        // because the lang-tag line itself is the closing.
        // e.g. "```\n```" → after_open = "\n```", after_lang = "```"
        // after_lang.strip_suffix("```") = Some("") → body = "" → stripped = ""
        let input = "```\n```";
        let result = strip_markdown_fences(input);
        // Body is "", strip_suffix('\n') on "" → unwrap_or("") = ""
        assert_eq!(result, "", "empty-body fence must strip to empty string");
    }

    // ── normalize_for_full_replace (no outer-brace stripping) ────────────────

    #[test]
    fn test_normalize_for_full_replace_does_not_strip_braces() {
        // Unlike `normalize_for_body_replace`, this function must NOT strip
        // outer braces — a full replacement includes the signature.
        let input = "{ return 42; }";
        let result = normalize_for_full_replace(input);
        assert_eq!(
            result, input,
            "normalize_for_full_replace must preserve outer braces"
        );
    }

    #[test]
    fn test_normalize_for_full_replace_strips_fence_and_crlf() {
        // The fence stripping happens before CRLF normalization.
        // Input: fenced block with CRLF line endings, no CRLF before closing fence.
        // After stripping: "func Hello() {}" (CRLF → LF normalized).
        let input = "```go\r\nfunc Hello() {}\n```";
        let result = normalize_for_full_replace(input);
        assert_eq!(result, "func Hello() {}");
    }

    // ── GAP-003: anchor_to_column_zero ───────────────────────────────────────

    #[test]
    fn test_anchor_to_column_zero_nested_if_else() {
        // First line starts at column 4, so everything shifts left by 4
        let input =
            "    let x = if cond {\n        val1\n    } else {\n        val2\n    };\n    result";
        let result = anchor_to_column_zero(input);
        assert!(
            result.starts_with("let x = if cond {"),
            "first line should be at column 0"
        );
        assert!(
            result.contains("    val1"),
            "val1 should be at column 4 (was 8, shifted by 4)"
        );
        assert!(
            result.contains("} else {"),
            "else branch should be at column 0"
        );
        assert!(result.contains("    val2"), "val2 should be at column 4");
        assert!(
            result.ends_with("result"),
            "last line should be at column 0"
        );
    }

    #[test]
    fn test_anchor_preserves_relative_indent() {
        let input = "    line1\n        nested\n    line3";
        let result = anchor_to_column_zero(input);
        assert_eq!(result, "line1\n    nested\nline3");
    }

    #[test]
    fn test_anchor_already_at_column_zero_is_noop() {
        let input = "let x = 1;\nreturn x;";
        let result = anchor_to_column_zero(input);
        assert_eq!(result, input, "already at column 0 should be unchanged");
    }

    #[test]
    fn test_anchor_empty_string_returns_empty() {
        let input = "";
        let result = anchor_to_column_zero(input);
        assert_eq!(result, "");
    }

    #[test]
    fn test_anchor_only_whitespace_returns_empty() {
        let input = "    \n    \n    ";
        let result = anchor_to_column_zero(input);
        assert_eq!(result, "");
    }

    #[test]
    fn test_anchor_blank_lines_preserved() {
        let input = "    line1\n\n    line2";
        let result = anchor_to_column_zero(input);
        assert_eq!(result, "line1\n\nline2", "blank lines should be preserved");
    }

    #[test]
    fn test_anchor_mixed_indentation_uses_first_line() {
        // First line has 4 spaces, so dedent by 4 even though some lines have 0
        // Lines with fewer spaces than the dedent amount get all leading spaces stripped
        let input = "    first\nzero\n    third";
        let result = anchor_to_column_zero(input);
        assert_eq!(result, "first\nzero\nthird");
    }

    // ── GAP-003: dedent_by ─────────────────────────────────────────────────────
}
