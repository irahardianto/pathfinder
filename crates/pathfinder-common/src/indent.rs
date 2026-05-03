//! Tree-sitter indentation pre-pass for Pathfinder edit tools.
//!
//! Implements PRD §3.4 step 5: dedent AI-generated code to column 0, then
//! re-indent it to match the target AST node's starting column.
//!
//! This prevents double-indentation bugs (e.g., LLM outputs 4-space indent,
//! Pathfinder adds 4-space indent → 8-space indent in Python).

/// Expand tabs to spaces using 4-column tab stops.
///
/// Each `\t` is replaced with spaces to advance to the next multiple of 4.
/// This normalises mixed whitespace before measuring byte-based indentation.
pub(crate) fn expand_tabs(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut col = 0usize;
    for ch in s.chars() {
        if ch == '\t' {
            let spaces = 4 - (col % 4);
            for _ in 0..spaces {
                out.push(' ');
            }
            col += spaces;
        } else {
            out.push(ch);
            col += 1;
        }
    }
    out
}

/// Compute the minimum leading whitespace count across all non-empty lines.
///
/// Tabs are expanded to 4-column boundaries before measuring so that
/// tab-indented and space-indented code are compared consistently.
///
/// Empty lines (whitespace-only or empty) are ignored because they do not
/// contribute to meaningful indentation.
#[must_use]
fn min_indent(code: &str) -> usize {
    code.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let expanded = expand_tabs(line);
            expanded.len() - expanded.trim_start().len()
        })
        .min()
        .unwrap_or(0)
}

/// Dedent code to column 0 by stripping the minimum common indentation.
///
/// Tabs are expanded to 4-column boundaries before computing the strip width,
/// ensuring tab-indented input is correctly normalised to all-space indentation.
///
/// Empty lines are preserved (they remain empty strings after stripping
/// leading whitespace equal to or less than the common indent).
#[must_use]
pub fn dedent(code: &str) -> String {
    let strip = min_indent(code);
    if strip == 0 {
        return code.to_owned();
    }

    code.lines()
        .map(|line| {
            let expanded = expand_tabs(line);
            if expanded.len() >= strip {
                expanded[strip..].to_owned()
            } else {
                // Shorter line (e.g., blank line with fewer spaces)
                expanded.trim_start().to_owned()
            }
        })
        .collect::<Vec<String>>()
        .join("\n")
}

/// Re-indent code to a target column by prepending spaces to every line.
///
/// Empty lines are not padded (preserves clean blank lines).
#[must_use]
pub fn reindent(code: &str, target_column: usize) -> String {
    if target_column == 0 {
        return code.to_owned();
    }

    let prefix = " ".repeat(target_column);
    code.lines()
        .map(|line| {
            if line.trim().is_empty() {
                String::default() // preserve clean blank lines
            } else {
                format!("{prefix}{line}")
            }
        })
        .collect::<Vec<String>>()
        .join("\n")
}

/// Full indentation pipeline: dedent to column 0, then re-indent to target.
///
/// This is the canonical operation used by all edit tools (PRD §3.4 step 5).
///
/// # Example
/// ```
/// use pathfinder_common::indent::dedent_then_reindent;
/// let code = "    x := 1\n    return x";
/// let result = dedent_then_reindent(code, 8);
/// // Dedented to: "x := 1\nreturn x"
/// // Re-indented: "        x := 1\n        return x"
/// assert_eq!(result, "        x := 1\n        return x");
/// ```
#[must_use]
pub fn dedent_then_reindent(code: &str, target_column: usize) -> String {
    let dedented = dedent(code);
    reindent(&dedented, target_column)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    // ── dedent ─────────────────────────────────────────────────────────

    #[test]
    fn test_dedent_four_spaces() {
        let code = "    x := 1\n    return x";
        assert_eq!(dedent(code), "x := 1\nreturn x");
    }

    #[test]
    fn test_dedent_mixed_indent_uses_minimum() {
        // Line 1 has 4 spaces, line 2 has 8 spaces → strip 4
        let code = "    x := 1\n        y := 2";
        assert_eq!(dedent(code), "x := 1\n    y := 2");
    }

    #[test]
    fn test_dedent_empty_lines_ignored_in_min() {
        // The blank line between should not reduce the min indent
        let code = "    x := 1\n\n    return x";
        assert_eq!(dedent(code), "x := 1\n\nreturn x");
    }

    #[test]
    fn test_dedent_already_at_column_zero() {
        let code = "x := 1\nreturn x";
        assert_eq!(dedent(code), code);
    }

    #[test]
    fn test_dedent_tab_indented_normalises_to_spaces() {
        // Two tabs at tab-stop 4 = 8 spaces; min_indent should see 8 and strip them.
        let code = "\t\tx := 1\n\t\treturn x";
        let result = dedent(code);
        assert_eq!(result, "x := 1\nreturn x");
    }

    #[test]
    fn test_dedent_single_line() {
        let code = "        return 42;";
        assert_eq!(dedent(code), "return 42;");
    }

    // ── reindent ───────────────────────────────────────────────────────

    #[test]
    fn test_reindent_column_8() {
        let code = "x := 1\nreturn x";
        let result = reindent(code, 8);
        assert_eq!(result, "        x := 1\n        return x");
    }

    #[test]
    fn test_reindent_column_zero_is_noop() {
        let code = "x := 1\nreturn x";
        assert_eq!(reindent(code, 0), code);
    }

    #[test]
    fn test_reindent_blank_lines_not_padded() {
        let code = "x := 1\n\nreturn x";
        let result = reindent(code, 4);
        assert_eq!(result, "    x := 1\n\n    return x");
    }

    // ── dedent_then_reindent ────────────────────────────────────────────

    #[test]
    fn test_dedent_then_reindent_pipeline() {
        let code = "    x := 1\n    return x";
        let result = dedent_then_reindent(code, 8);
        assert_eq!(result, "        x := 1\n        return x");
    }

    #[test]
    fn test_dedent_then_reindent_no_double_indent() {
        // Simulates LLM output that already has 4-space indent
        // Target is also 4 → should result in exactly 4, not 8
        let code = "    return value;";
        let result = dedent_then_reindent(code, 4);
        assert_eq!(result, "    return value;");
    }

    #[test]
    fn test_dedent_then_reindent_multiline_with_blank() {
        let code = "    fn body() {\n\n        return 42;\n    }";
        let result = dedent_then_reindent(code, 4);
        // After dedent: "fn body() {\n\n    return 42;\n}"
        // After reindent(4): "    fn body() {\n\n        return 42;\n    }"
        assert_eq!(result, "    fn body() {\n\n        return 42;\n    }");
    }
}
