use super::{ResolvedEditFree, ValidationOutcome};
use crate::server::helpers::io_error_data;
use crate::server::tools::diagnostics::diff_diagnostics;
use crate::server::types::EditValidation;
use pathfinder_common::error::DiagnosticError;
use rmcp::model::ErrorData;
use std::path::Path;
/// Build a list of byte offsets for the start of every line (0-indexed).
pub(crate) fn build_line_starts(source_str: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(
            source_str
                .char_indices()
                .filter(|(_, c)| *c == '\n')
                .map(|(i, _)| i + 1),
        )
        .collect()
}

/// Compute the search window byte range for a given context line.
pub(crate) fn compute_search_window(
    line_starts: &[usize],
    context_line: u32,
    source_str_len: usize,
) -> (usize, usize) {
    let total_lines = line_starts.len();
    let center = context_line.saturating_sub(1) as usize;
    let window_start_line = center.saturating_sub(25);
    let window_end_line = (center + 25).min(total_lines.saturating_sub(1));

    let window_byte_start = line_starts[window_start_line];
    let window_byte_end = if window_end_line + 1 < total_lines {
        line_starts[window_end_line + 1]
    } else {
        source_str_len
    };

    (window_byte_start, window_byte_end)
}

/// Collapse all runs of whitespace to a single space.
pub(crate) fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = false;
    for ch in s.chars() {
        if ch.is_ascii_whitespace() {
            if !prev_ws {
                out.push(' ');
            }
            prev_ws = true;
        } else {
            out.push(ch);
            prev_ws = false;
        }
    }
    out
}

/// Normalize and match, then map back to original byte positions.
pub(crate) fn collapse_and_match(
    window_text: &str,
    old_text: &str,
    filepath: &std::path::Path,
    context_line: u32,
) -> Result<(usize, usize), pathfinder_common::error::PathfinderError> {
    let normalised_window = collapse_whitespace(window_text);
    let normalised_needle = collapse_whitespace(old_text);

    let norm_match_start = normalised_window
        .find(&normalised_needle[..])
        .ok_or_else(|| pathfinder_common::error::PathfinderError::TextNotFound {
            filepath: filepath.to_path_buf(),
            old_text: old_text.to_owned(),
            context_line,
            actual_content: Some(window_text.to_owned()),
            closest_match: None,
        })?;
    let norm_match_end = norm_match_start + normalised_needle.len();

    // Re-walk the window to find the original byte span
    let mut orig_start: Option<usize> = None;
    let mut orig_end: Option<usize> = None;
    let mut norm_pos = 0usize;
    let mut prev_ws2 = false;

    for (orig_i, ch) in window_text.char_indices() {
        let was_prev_ws = prev_ws2;
        let ch_is_ws = ch.is_ascii_whitespace();

        let norm_char_start = norm_pos;
        if ch_is_ws {
            if !was_prev_ws {
                norm_pos += 1; // the space we emitted
            }
            prev_ws2 = true;
        } else {
            norm_pos += ch.len_utf8();
            prev_ws2 = false;
        }

        if orig_start.is_none() && norm_char_start == norm_match_start {
            orig_start = Some(orig_i);
        }
        if orig_end.is_none() && norm_pos >= norm_match_end {
            orig_end = Some(orig_i + ch.len_utf8());
            break;
        }
    }

    // If the match reached end-of-window.
    if orig_end.is_none() && norm_pos >= norm_match_end {
        orig_end = Some(window_text.len());
    }

    match (orig_start, orig_end) {
        (Some(s), Some(e)) => Ok((s, e)),
        _ => Err(pathfinder_common::error::PathfinderError::TextNotFound {
            filepath: filepath.to_path_buf(),
            old_text: old_text.to_owned(),
            context_line,
            actual_content: Some(window_text.to_owned()),
            closest_match: None,
        }),
    }
}

/// Resolve a text-range edit (E3.1) to a concrete byte span in `source`.
///
/// # Algorithm
///
/// 1. Collect byte offsets for all line starts (0-indexed lines).
/// 2. Compute a search window: lines `(context_line - 1) ± 25` (clamped to file bounds).
/// 3. Extract the UTF-8 text for that window.
/// 4. Search for `old_text` within the window, optionally collapsing `\s+` → `' '`
///    when `normalize_whitespace` is `true`.
/// 5. Map the within-window match offset back to absolute byte positions in `source`.
///
/// Returns a [`ResolvedEditFree`] whose `replacement` is `new_text.as_bytes()`.
/// Returns [`PathfinderError::TextNotFound`] if no match is found.
///
/// # Errors
/// - [`PathfinderError::TextNotFound`] — `old_text` not present in the ±25-line window.
/// - Propagated UTF-8 errors if source is not valid UTF-8 (returns an opaque I/O error).
pub(crate) fn resolve_text_edit(
    source: &[u8],
    old_text: &str,
    context_line: u32,
    new_text: &str,
    normalize_whitespace: bool,
    filepath: &std::path::Path,
) -> Result<ResolvedEditFree, pathfinder_common::error::PathfinderError> {
    // Convert source to UTF-8 — required for line-wise text operations.
    let source_str = std::str::from_utf8(source).map_err(|e| {
        pathfinder_common::error::PathfinderError::IoError {
            message: format!("source file is not valid UTF-8: {e}"),
        }
    })?;

    // Build line starts and compute search window
    let line_starts = build_line_starts(source_str);
    let (window_byte_start, window_byte_end) =
        compute_search_window(&line_starts, context_line, source_str.len());
    let window_text = &source_str[window_byte_start..window_byte_end];

    // Perform the match, with optional whitespace normalisation
    if normalize_whitespace {
        // Use whitespace normalization
        let (start, end) = collapse_and_match(window_text, old_text, filepath, context_line)?;
        Ok(ResolvedEditFree {
            start_byte: window_byte_start + start,
            end_byte: window_byte_start + end,
            replacement: new_text.as_bytes().to_vec(),
        })
    } else {
        // Exact match with optional fuzzy fallback for non-whitespace-significant files
        let Some(abs_start) = window_text.find(old_text) else {
            // Check if this is a whitespace-significant file before attempting fuzzy fallback
            let is_whitespace_significant = filepath
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| matches!(ext, "py" | "yaml" | "yml" | "toml"));

            if is_whitespace_significant {
                return Err(pathfinder_common::error::PathfinderError::TextNotFound {
                    filepath: filepath.to_path_buf(),
                    old_text: old_text.to_owned(),
                    context_line,
                    actual_content: Some(window_text.to_owned()),
                    closest_match: find_closest_match(window_text, old_text),
                });
            }

            // Retry with whitespace normalization as a fallback
            tracing::warn!(
                filepath = %filepath.display(),
                context_line,
                old_text_len = old_text.len(),
                "text_edit: exact match failed, trying whitespace-normalized fuzzy fallback"
            );

            return resolve_text_edit(source, old_text, context_line, new_text, true, filepath);
        };

        let abs_start = window_byte_start + abs_start;
        let abs_end = abs_start + old_text.len();

        Ok(ResolvedEditFree {
            start_byte: abs_start,
            end_byte: abs_end,
            replacement: new_text.as_bytes().to_vec(),
        })
    }
}

/// Splice `indented` code into `source` at the given `body_range`.
///
/// Handles two cases:
/// - **Brace-enclosed blocks** (Go/Rust/TS): keeps `{` and `}`, inserts body
///   between them with proper indentation for the closing brace.
/// - **Non-brace blocks** (Python): replaces only the byte range, trimming
///   trailing whitespace before the insertion point to avoid double indentation.
pub(crate) fn build_body_replacement(
    source: &[u8],
    body_range: &pathfinder_treesitter::surgeon::BodyRange,
    indented: &str,
) -> Result<String, ErrorData> {
    let is_brace_block = if body_range.end_byte > body_range.start_byte {
        source.get(body_range.start_byte) == Some(&b'{')
            && source.get(body_range.end_byte.saturating_sub(1)) == Some(&b'}')
    } else {
        false
    };

    let utf8_err =
        |e: std::str::Utf8Error| io_error_data(format!("source is not valid UTF-8: {e}"));

    if is_brace_block {
        let before = std::str::from_utf8(&source[..=body_range.start_byte]).map_err(utf8_err)?;
        let after = std::str::from_utf8(&source[body_range.end_byte.saturating_sub(1)..])
            .map_err(utf8_err)?;

        if indented.trim().is_empty() {
            Ok([before, after].concat())
        } else {
            let closing_indent = " ".repeat(body_range.indent_column);
            Ok([before, "\n", indented, "\n", &closing_indent, after].concat())
        }
    } else {
        // Non-brace block (e.g., Python): trim trailing whitespace from `before`.
        let mut end = body_range.start_byte;
        while end > 0 && (source[end - 1] == b' ' || source[end - 1] == b'\t') {
            end -= 1;
        }
        let before = std::str::from_utf8(&source[..end]).map_err(utf8_err)?;
        let after = std::str::from_utf8(&source[body_range.end_byte..]).map_err(utf8_err)?;
        Ok([before, indented, after].concat())
    }
}

/// Convert pre/post diagnostic lists into a `ValidationOutcome`.
///
/// Pure function: diffs the diagnostics, maps them to `DiagnosticError`,
/// and decides whether the edit should be blocked.
pub(crate) fn build_validation_outcome(
    pre_diags: &[pathfinder_lsp::types::LspDiagnostic],
    post_diags: &[pathfinder_lsp::types::LspDiagnostic],
    ignore_validation_failures: bool,
    file_path: &Path,
) -> ValidationOutcome {
    let diff = diff_diagnostics(pre_diags, post_diags);
    let has_new_errors = diff.has_new_errors();

    // C3: Audit logging for ignore_validation_failures flag usage
    if has_new_errors && ignore_validation_failures {
        tracing::warn!(
            file = %file_path.display(),
            error_count = diff.introduced.len(),
            "LSP validation introduced new errors but ignore_validation_failures=true, allowing write"
        );
    }

    let to_diag_error = |d: &pathfinder_lsp::types::LspDiagnostic| DiagnosticError {
        severity: d.severity as u8,
        code: d.code.clone().unwrap_or_default(),
        message: d.message.clone(),
        file: d.file.clone(),
    };

    let introduced: Vec<DiagnosticError> = diff.introduced.iter().map(to_diag_error).collect();
    let resolved: Vec<DiagnosticError> = diff.resolved.iter().map(to_diag_error).collect();

    let should_block = has_new_errors && !ignore_validation_failures;
    let status = if should_block { "failed" } else { "passed" };

    ValidationOutcome {
        validation: EditValidation {
            status: status.to_owned(),
            introduced_errors: introduced,
            resolved_errors: resolved,
        },
        skipped: false,
        skipped_reason: None,
        should_block,
    }
}

pub(crate) fn normalize_blank_lines(content: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(content.len());
    let mut i = 0;
    while i < content.len() {
        result.push(content[i]);
        if content[i] == b'\n' {
            let mut count = 1;
            while i + count < content.len() && content[i + count] == b'\n' {
                count += 1;
            }
            if count > 1 {
                result.push(b'\n');
            }
            i += count;
        } else {
            i += 1;
        }
    }
    result
}

pub(crate) fn is_whitespace_significant_file(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| matches!(ext, "py" | "yaml" | "yml" | "toml"))
}

pub(crate) fn strip_orphaned_doc_comment(source: &[u8], before_end: usize) -> usize {
    if before_end == 0 {
        return before_end;
    }
    let search_end = if source[before_end - 1] == b'\n' {
        before_end - 1
    } else {
        before_end
    };
    if search_end == 0 {
        return before_end;
    }
    let line_start = source[..search_end]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |pos| pos + 1);
    let line_bytes = &source[line_start..search_end];
    let Ok(line_str) = std::str::from_utf8(line_bytes) else {
        return before_end;
    };
    let stripped = line_str.trim_start_matches(|c: char| c == '}' || c.is_ascii_whitespace());
    if stripped.starts_with("///") || stripped.starts_with("//!") {
        if let Some(slash_idx) = line_str.find("//") {
            let mut del_start = slash_idx;
            while del_start > 0 {
                let prev_char = line_str[..del_start].chars().next_back().unwrap_or('\n');
                if prev_char.is_whitespace() && prev_char != '\n' {
                    del_start -= prev_char.len_utf8();
                } else {
                    break;
                }
            }
            return line_start + del_start;
        }
    }
    before_end
}

fn find_closest_match(window: &str, needle: &str) -> Option<String> {
    if needle.is_empty() || window.is_empty() {
        return None;
    }
    let needle_chars: Vec<char> = needle.chars().collect();
    let window_chars: Vec<char> = window.chars().collect();
    let needle_len = needle_chars.len();
    let window_len = window_chars.len();
    if needle_len == 0 || window_len == 0 || needle_len > window_len {
        return None;
    }

    // Precompute counts to avoid allocating inside the hot loop
    let mut needle_ascii_counts = [0usize; 256];
    let mut needle_other_counts = std::collections::HashMap::new();
    for &c in &needle_chars {
        if (c as u32) < 256 {
            needle_ascii_counts[c as usize] += 1;
        } else {
            *needle_other_counts.entry(c).or_insert(0) += 1;
        }
    }

    let mut best_score = 0.0;
    let mut best_slice = None;
    for start in 0..=(window_len - needle_len) {
        let slice = &window_chars[start..(start + needle_len)];

        let mut ascii_counts = needle_ascii_counts;
        // Cloning is essentially free when other_counts is empty (99% of source code)
        let mut other_counts = needle_other_counts.clone();

        let mut overlap = 0;
        for &c in slice {
            if (c as u32) < 256 {
                let count = &mut ascii_counts[c as usize];
                if *count > 0 {
                    overlap += 1;
                    *count -= 1;
                }
            } else if let Some(count) = other_counts.get_mut(&c) {
                if *count > 0 {
                    overlap += 1;
                    *count -= 1;
                }
            }
        }

        #[allow(clippy::cast_precision_loss)]
        // needle_len is a string length; f64 mantissa is sufficient for this heuristic score
        let score = f64::from(overlap) / needle_len as f64;
        if score > best_score {
            best_score = score;
            let byte_start = window.char_indices().nth(start).map_or(0, |(i, _)| i);
            let byte_end = window
                .char_indices()
                .nth(start + needle_len)
                .map_or(window.len(), |(i, _)| i);
            best_slice = Some(window[byte_start..byte_end].to_owned());
        }
    }

    if best_score >= 0.6 {
        best_slice
    } else {
        None
    }
}
