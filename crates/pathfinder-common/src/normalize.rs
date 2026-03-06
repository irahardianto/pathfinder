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
    let after_lang = after_open
        .find('\n')
        .map_or("", |pos| &after_open[pos + 1..]);

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
            // Strip outer braces and trim whitespace adjacent to them
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
#[must_use]
pub fn normalize_for_body_replace(input: &str) -> String {
    let step1 = strip_markdown_fences(input);
    let step2 = strip_outer_braces(step1);
    normalize_line_endings(step2).into_owned()
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
        assert_eq!(result, " return 42; ");
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
}
