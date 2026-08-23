//! Multi-zone and injected grammar parsing via tree-sitter's `set_included_ranges`.
//!
//! Parses a multi-zone file (such as a Vue Single-File Component) into semantic zones
//! (`<template>`, `<script>`, `<style>`), each with its own grammar. Tree-sitter's
//! native `set_included_ranges` API is used so that all AST nodes carry **correct
//! global byte offsets** — no custom offset arithmetic is required.
//!
//! # Zone Model
//!
//! | Zone       | Grammar       | Symbol kind produced                 |
//! |------------|---------------|--------------------------------------|
//! | `script`   | TypeScript    | Function, Class, Constant, etc.      |
//! | `template` | HTML          | Component (capitalised), HtmlElement |
//! | `style`    | CSS           | CssSelector, CssAtRule               |

use crate::error::SurgeonError;
use std::sync::Arc;
use tree_sitter::{Parser, Point, Range, Tree};

// ─── Injection range types ───────────────────────────────────────────────────

/// Byte range and tree-sitter [`Point`] coordinates for a single injected region or SFC zone.
///
/// Both positions are relative to the **original source** (not an extracted slice).
/// Passing these directly to `Parser::set_included_ranges` makes tree-sitter emit
/// nodes with correct global byte offsets and line numbers.
#[derive(Debug, Clone)]
pub struct InjectionRange {
    /// Byte offset of the first byte of zone content (character after opening tag `>`).
    pub start_byte: usize,
    /// Byte offset of the first byte of the closing tag (`</tag>`).
    pub end_byte: usize,
    /// Row / column of `start_byte` in the original source (0-indexed row, byte column).
    pub start_point: Point,
    /// Row / column of `end_byte` in the original source.
    pub end_point: Point,
}

/// Backwards-compatibility alias for Vue SFC zone range.
pub type VueZoneRange = InjectionRange;

/// The three optional SFC zones located within the raw SFC source.
#[derive(Debug, Clone, Default)]
pub struct VueZones {
    /// The primary `<script>` or `<script setup>` block content range.
    pub script: Option<InjectionRange>,
    /// All `<script>` and `<script setup>` block content ranges.
    pub scripts: Vec<InjectionRange>,
    /// The `<template>` block content range.
    pub template: Option<InjectionRange>,
    /// The primary `<style>` block content range.
    pub style: Option<InjectionRange>,
    /// All `<style>` block content ranges.
    pub styles: Vec<InjectionRange>,
}

// ─── Multi-zone parse result ──────────────────────────────────────────────────

/// Parsed AST trees for all zones of a multi-grammar file (e.g. Vue SFC).
#[derive(Debug, Clone)]
pub struct MultiZoneTree {
    /// TypeScript AST for the `<script>` block (if present).
    pub script_tree: Option<Tree>,
    /// HTML AST for the `<template>` block (if present).
    pub template_tree: Option<Tree>,
    /// CSS AST for the `<style>` block (if present).
    pub style_tree: Option<Tree>,
    /// Byte ranges of the three zones (used for line-to-zone lookup).
    pub zones: VueZones,
    /// Original, unmodified SFC source bytes.
    ///
    /// Kept intact for version hashing and change detection.
    pub source: Arc<[u8]>,
    /// `true` when HTML or CSS grammar was unavailable and the respective zone
    /// was skipped. Template / style symbols are absent; script symbols are unaffected.
    pub degraded: bool,
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

const PARSE_TIMEOUT_MICROS: u64 = 500_000;

/// Convert a byte offset in `source` to a tree-sitter [`Point`] (row, column).
///
/// # Column semantics
///
/// `column` is **byte-based**, not character-based. This is intentional and
/// correct: tree-sitter's `Point` uses byte offsets for column positions, not
/// Unicode character counts. Changing this to character-based would break
/// `set_included_ranges` interop with the tree-sitter C API.
#[must_use]
pub fn byte_to_point(source: &[u8], byte: usize) -> Point {
    let safe = byte.min(source.len());
    let prefix = &source[..safe];
    #[allow(clippy::naive_bytecount)]
    let row = prefix.iter().filter(|&&b| b == b'\n').count();
    let col = prefix
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(safe, |nl| safe - nl - 1);
    Point { row, column: col }
}

/// Locate the template content byte range, correctly tracking nested `<template>` tags.
#[must_use]
pub fn find_template_zone(source: &[u8]) -> Option<InjectionRange> {
    let text = std::str::from_utf8(source).ok()?;
    let bytes = source;

    let mut search_from = 0usize;
    loop {
        let rel = text[search_from..].find("<template")?;
        let open_pos = search_from + rel;

        // Guard: must be `<template` not `<template-something`
        let after_name = open_pos + "<template".len();
        let next_byte = bytes.get(after_name).copied().unwrap_or(b'>');
        if !matches!(next_byte, b'>' | b' ' | b'\t' | b'\n' | b'\r' | b'/') {
            search_from = open_pos + 1;
            continue;
        }

        // Guard: must not be closing tag `</template>`
        if bytes.get(open_pos + 1).copied() == Some(b'/') {
            search_from = open_pos + 1;
            continue;
        }

        // Find '>' that closes the opening tag
        let gt_rel = text[open_pos..].find('>')?;
        let content_start = open_pos + gt_rel + 1;

        // Check if self-closing `<template ... />`
        let tag_slice = &text[open_pos..=(open_pos + gt_rel)];
        if tag_slice.trim_end_matches('>').trim_end().ends_with('/') {
            search_from = content_start;
            continue;
        }

        // Track nested `<template>` tags
        let mut depth = 1usize;
        let mut idx = content_start;

        while idx < bytes.len() {
            if bytes[idx] == b'<' {
                // Skip HTML comments `<!-- ... -->`
                if text[idx..].starts_with("<!--") {
                    if let Some(comment_end) = text[idx..].find("-->") {
                        idx += comment_end + 3;
                        continue;
                    }
                }

                // Check for closing `</template>`
                if text[idx..].starts_with("</template") {
                    let after = idx + "</template".len();
                    let nb = bytes.get(after).copied().unwrap_or(b'>');
                    if matches!(nb, b'>' | b' ' | b'\t' | b'\n' | b'\r') {
                        if let Some(gt) = text[idx..].find('>') {
                            depth -= 1;
                            if depth == 0 {
                                let content_end = idx;
                                return Some(InjectionRange {
                                    start_byte: content_start,
                                    end_byte: content_end,
                                    start_point: byte_to_point(source, content_start),
                                    end_point: byte_to_point(source, content_end),
                                });
                            }
                            idx += gt + 1;
                            continue;
                        }
                    }
                }

                // Check for nested opening `<template`
                if text[idx..].starts_with("<template") {
                    let after = idx + "<template".len();
                    let nb = bytes.get(after).copied().unwrap_or(b'>');
                    if matches!(nb, b'>' | b' ' | b'\t' | b'\n' | b'\r' | b'/') {
                        if let Some(gt) = text[idx..].find('>') {
                            let nested_tag_slice = &text[idx..=(idx + gt)];
                            if !nested_tag_slice
                                .trim_end_matches('>')
                                .trim_end()
                                .ends_with('/')
                            {
                                depth += 1;
                            }
                            idx += gt + 1;
                            continue;
                        }
                    }
                }
            }
            idx += 1;
        }

        search_from = open_pos + 1;
    }
}

fn find_close_tag(slice: &str, tag: &str) -> Option<usize> {
    let bytes = slice.as_bytes();
    let close_target = format!("</{tag}>");
    let close_bytes = close_target.as_bytes();
    let mut i = 0;
    let len = bytes.len();

    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut in_backtick = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;

    while i < len {
        let b = bytes[i];

        // Handle escape characters inside strings
        if (in_single_quote || in_double_quote || in_backtick) && b == b'\\' {
            i += 2;
            continue;
        }

        if in_line_comment {
            if b == b'\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }

        if in_block_comment {
            if b == b'*' && i + 1 < len && bytes[i + 1] == b'/' {
                in_block_comment = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }

        if in_single_quote {
            if b == b'\'' {
                in_single_quote = false;
            }
            i += 1;
            continue;
        }

        if in_double_quote {
            if b == b'"' {
                in_double_quote = false;
            }
            i += 1;
            continue;
        }

        if in_backtick {
            if b == b'`' {
                in_backtick = false;
            }
            i += 1;
            continue;
        }

        // Check for comment starts
        if b == b'/' && i + 1 < len {
            if bytes[i + 1] == b'/' {
                in_line_comment = true;
                i += 2;
                continue;
            }
            if bytes[i + 1] == b'*' {
                in_block_comment = true;
                i += 2;
                continue;
            }
        }

        // Check for string starts
        if b == b'\'' {
            in_single_quote = true;
            i += 1;
            continue;
        }
        if b == b'"' {
            in_double_quote = true;
            i += 1;
            continue;
        }
        if b == b'`' {
            in_backtick = true;
            i += 1;
            continue;
        }

        // Check for closing tag. `slice.get()` (not indexing) is required here:
        // `i` always lands on a `<` byte, which is always a valid UTF-8 boundary,
        // but `i + close_bytes.len()` is an arbitrary byte offset that can split a
        // multi-byte character (e.g. a `<` comparison/generic followed within
        // 7-8 bytes by non-ASCII text). Direct `&str` indexing panics on a
        // non-boundary bound; `get()` returns `None` instead.
        if b == b'<' {
            if let Some(candidate) = slice.get(i..i + close_bytes.len()) {
                if candidate.eq_ignore_ascii_case(&close_target) {
                    return Some(i);
                }
            }
        }

        i += 1;
    }

    None
}

/// Locate all content byte ranges for top-level `<tagname ...>...</tagname>` occurrences.
#[must_use]
pub fn find_all_zones(source: &[u8], tag: &str) -> Vec<InjectionRange> {
    let Ok(text) = std::str::from_utf8(source) else {
        return Vec::new();
    };
    let bytes = source;

    let open_prefix = format!("<{tag}");
    let close_tag = format!("</{tag}>");

    let mut ranges = Vec::new();
    let mut search_from = 0usize;

    while search_from < text.len() {
        let Some(rel) = text[search_from..].find(open_prefix.as_str()) else {
            break;
        };
        let open_pos = search_from + rel;

        // Guard: must be `<tag` not `<tag-something`
        let after_name = open_pos + open_prefix.len();
        let next_byte = bytes.get(after_name).copied().unwrap_or(b'>');
        if !matches!(next_byte, b'>' | b' ' | b'\t' | b'\n' | b'\r' | b'/') {
            search_from = open_pos + 1;
            continue;
        }

        // Guard: must not be a closing tag `</tagname>`
        if bytes.get(open_pos + 1).copied() == Some(b'/') {
            search_from = open_pos + 1;
            continue;
        }

        // Find `>` closing the opening tag
        let Some(gt_rel) = text[open_pos..].find('>') else {
            break;
        };
        let content_start = open_pos + gt_rel + 1;

        // Check if self-closing `<tag ... />`
        let tag_slice = &text[open_pos..=(open_pos + gt_rel)];
        if tag_slice.trim_end_matches('>').trim_end().ends_with('/') {
            search_from = content_start;
            continue;
        }

        // Find matching `</tagname>` using quote- and comment-aware scan
        let Some(close_rel) = find_close_tag(&text[content_start..], tag) else {
            search_from = content_start;
            continue;
        };
        let content_end = content_start + close_rel;

        ranges.push(InjectionRange {
            start_byte: content_start,
            end_byte: content_end,
            start_point: byte_to_point(source, content_start),
            end_point: byte_to_point(source, content_end),
        });

        search_from = content_end + close_tag.len();
    }

    ranges
}

// ─── Public API ───────────────────────────────────────────────────────────────

/// Scan the raw SFC bytes for all `<template>`, `<script>`, and `<style>` zones.
///
/// Supports `<script>` + `<script setup>` and multiple `<style>` blocks.
#[must_use]
pub fn scan_vue_zones(source: &[u8]) -> VueZones {
    let scripts = find_all_zones(source, "script");
    let template = find_template_zone(source);
    let styles = find_all_zones(source, "style");

    VueZones {
        script: scripts.first().cloned(),
        scripts,
        template,
        style: styles.first().cloned(),
        styles,
    }
}

/// Parse injected ranges with a given grammar.
///
/// # Errors
/// Returns `SurgeonError::ParseError` if parsing fatal ranges fails.
pub fn parse_injected_ranges(
    source: &[u8],
    ranges: &[InjectionRange],
    grammar: &tree_sitter::Language,
    zone_label: &str,
    fatal: bool,
    degraded: &mut bool,
) -> Result<Option<Tree>, SurgeonError> {
    parse_zones_with_grammar(source, ranges, grammar, zone_label, fatal, degraded)
}

/// Parse a Vue SFC into multiple zone trees using `set_included_ranges`.
///
/// Each zone is parsed with its own tree-sitter grammar. All AST nodes carry
/// **global byte offsets** relative to the original SFC — no offset arithmetic
/// is required by callers.
///
/// # Errors
///
/// Returns [`SurgeonError::ParseError`] if parsing the `<script>` zone fails.
pub fn parse_vue_multizone(source: &[u8]) -> Result<MultiZoneTree, SurgeonError> {
    let zones = scan_vue_zones(source);
    let mut degraded = false;

    // ── Script zone (TypeScript grammar) ─────────────────────────────────────
    let script_tree = parse_zones_with_grammar(
        source,
        &zones.scripts,
        &tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "<vue-script>",
        true, // fatal if this fails
        &mut degraded,
    )?;

    // ── Template zone (HTML grammar) ─────────────────────────────────────────
    let template_ranges = zones
        .template
        .as_ref()
        .map_or_else(Vec::new, |t| vec![t.clone()]);
    let template_tree = parse_zones_with_grammar(
        source,
        &template_ranges,
        &tree_sitter_html::LANGUAGE.into(),
        "<vue-template>",
        false, // non-fatal
        &mut degraded,
    )?;

    // ── Style zone (CSS grammar) ──────────────────────────────────────────────
    let style_tree = parse_zones_with_grammar(
        source,
        &zones.styles,
        &tree_sitter_css::LANGUAGE.into(),
        "<vue-style>",
        false, // non-fatal
        &mut degraded,
    )?;

    Ok(MultiZoneTree {
        script_tree,
        template_tree,
        style_tree,
        zones,
        source: Arc::from(source),
        degraded,
    })
}

/// Internal helper: set up a parser for one or more ranges of a zone and run it with a timeout.
fn parse_zones_with_grammar(
    source: &[u8],
    zones: &[InjectionRange],
    grammar: &tree_sitter::Language,
    zone_label: &str,
    fatal: bool,
    degraded: &mut bool,
) -> Result<Option<Tree>, SurgeonError> {
    if zones.is_empty() {
        return Ok(None);
    }

    let ts_ranges: Vec<Range> = zones
        .iter()
        .map(|z| Range {
            start_byte: z.start_byte,
            end_byte: z.end_byte,
            start_point: z.start_point,
            end_point: z.end_point,
        })
        .collect();

    let mut parser = Parser::new();

    let set_lang_result = parser.set_language(grammar);
    let set_ranges_result = set_lang_result
        .ok()
        .and_then(|()| parser.set_included_ranges(&ts_ranges).ok());

    if set_ranges_result.is_none() {
        if fatal {
            return Err(SurgeonError::ParseError {
                path: std::path::PathBuf::from(zone_label),
                reason: "failed to configure tree-sitter grammar or ranges".into(),
            });
        }
        *degraded = true;
        return Ok(None);
    }

    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_micros(PARSE_TIMEOUT_MICROS);
    let source_len = source.len();

    let mut progress_cb = |_state: &tree_sitter::ParseState| {
        if start.elapsed() > timeout {
            std::ops::ControlFlow::Break(())
        } else {
            std::ops::ControlFlow::Continue(())
        }
    };

    let options = tree_sitter::ParseOptions::new().progress_callback(&mut progress_cb);

    let tree = parser.parse_with_options(
        &mut |i, _| {
            if i < source_len {
                &source[i..]
            } else {
                &[]
            }
        },
        None,
        Some(options),
    );

    if tree.is_none() && fatal {
        let timed_out = start.elapsed() > timeout;
        return Err(SurgeonError::ParseError {
            path: std::path::PathBuf::from(zone_label),
            reason: if timed_out {
                "Vue zone parse timed out (exceeded 500ms limit)".into()
            } else {
                "tree-sitter parse returned None".into()
            },
        });
    }
    if tree.is_none() {
        *degraded = true;
    }
    Ok(tree)
}

#[cfg(test)]
#[path = "vue_zones_test.rs"]
mod tests;
