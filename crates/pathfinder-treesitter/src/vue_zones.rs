//! Multi-zone Vue SFC parsing via tree-sitter's `set_included_ranges`.
//!
//! Parses a Vue Single-File Component into three semantic zones (`<template>`,
//! `<script>`, `<style>`), each with its own grammar. Tree-sitter's native
//! `set_included_ranges` API is used so that all AST nodes carry **correct
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

// ─── Zone range types ─────────────────────────────────────────────────────────

/// Byte range and tree-sitter [`Point`] coordinates for a single SFC zone.
///
/// Both positions are relative to the **original SFC source** (not an extracted
/// slice). Passing these directly to `Parser::set_included_ranges` makes
/// tree-sitter emit nodes with correct global byte offsets and line numbers.
#[derive(Debug, Clone)]
pub struct VueZoneRange {
    /// Byte offset of the first byte of zone content (character after opening tag `>`).
    pub start_byte: usize,
    /// Byte offset of the first byte of the closing tag (`</tag>`).
    pub end_byte: usize,
    /// Row / column of `start_byte` in the original SFC (0-indexed row, byte column).
    pub start_point: Point,
    /// Row / column of `end_byte` in the original SFC.
    pub end_point: Point,
}

/// The three optional SFC zones located within the raw SFC source.
#[derive(Debug, Clone, Default)]
pub struct VueZones {
    /// The `<script>` or `<script setup>` block content range.
    pub script: Option<VueZoneRange>,
    /// The `<template>` block content range.
    pub template: Option<VueZoneRange>,
    /// The `<style>` block content range.
    pub style: Option<VueZoneRange>,
}

// ─── Multi-zone parse result ──────────────────────────────────────────────────

/// Parsed AST trees for all three zones of a Vue SFC.
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

/// Convert a byte offset in `source` to a tree-sitter [`Point`] (row, column).
///
/// # Column semantics
///
/// `column` is **byte-based**, not character-based. This is intentional and
/// correct: tree-sitter's `Point` uses byte offsets for column positions, not
/// Unicode character counts. Changing this to character-based would break
/// [`set_included_ranges`] interop with the tree-sitter C API.
fn byte_to_point(source: &[u8], byte: usize) -> Point {
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

/// Locate the content byte range for a single top-level `<tagname ...>...</tagname>`.
///
/// Only the first outermost occurrence is returned (Vue SFCs have at most one
/// `<template>`, `<script>`, and `<style>` at the top level).
fn find_zone(source: &[u8], tag: &str) -> Option<VueZoneRange> {
    let text = std::str::from_utf8(source).ok()?;
    let bytes = source;

    let open_prefix = format!("<{tag}");
    let close_tag = format!("</{tag}>");

    let mut search_from = 0usize;
    loop {
        // Find the next `<tagname` occurrence
        let rel = text[search_from..].find(open_prefix.as_str())?;
        let open_pos = search_from + rel;

        // Guard: must be `<tag` not `<tag-something` — next byte must be `>`,
        // whitespace, or end of input.
        let after_name = open_pos + open_prefix.len();
        let next_byte = bytes.get(after_name).copied().unwrap_or(b'>');
        if !matches!(next_byte, b'>' | b' ' | b'\t' | b'\n' | b'\r') {
            search_from = open_pos + 1;
            continue;
        }

        // Guard: must not be a closing tag `</tagname>`.
        if bytes.get(open_pos + 1).copied() == Some(b'/') {
            search_from = open_pos + 1;
            continue;
        }

        // Find the `>` that closes the opening tag (handles attributes).
        let gt_rel = text[open_pos..].find('>')?;
        let content_start = open_pos + gt_rel + 1; // byte immediately after `>`

        // Find the matching `</tagname>`.
        let close_rel = text[content_start..].find(close_tag.as_str())?;
        let content_end = content_start + close_rel; // first byte of `</tagname>`

        return Some(VueZoneRange {
            start_byte: content_start,
            end_byte: content_end,
            start_point: byte_to_point(source, content_start),
            end_point: byte_to_point(source, content_end),
        });
    }
}

// ─── Public API ───────────────────────────────────────────────────────────────

/// Scan the raw SFC bytes for the `<template>`, `<script>`, and `<style>` zones.
///
/// Returns the byte ranges of the *content* inside each top-level tag. Only the
/// first occurrence of each tag is recognised (Vue SFCs have at most one of each).
///
/// Missing zones are represented as `None`. The function never fails — malformed
/// or template-only SFCs simply yield fewer zones.
#[must_use]
pub fn scan_vue_zones(source: &[u8]) -> VueZones {
    VueZones {
        script: find_zone(source, "script"),
        template: find_zone(source, "template"),
        style: find_zone(source, "style"),
    }
}

/// Parse a Vue SFC into multiple zone trees using `set_included_ranges`.
///
/// Each zone is parsed with its own tree-sitter grammar. All AST nodes carry
/// **global byte offsets** relative to the original SFC — no offset arithmetic
/// is required by callers.
///
/// # Degraded mode
///
/// If the HTML or CSS grammar is unavailable at compile time, the corresponding
/// zone tree will be `None` and `MultiZoneTree::degraded` will be `true`. The
/// script zone is always attempted (TypeScript is a Tier-1 grammar).
///
/// # Errors
///
/// Returns `SurgeonError::ParseError` only if the TypeScript grammar cannot be
/// loaded (a compile-time dependency failure). HTML/CSS grammar failures are
/// non-fatal.
pub fn parse_vue_multizone(source: &[u8]) -> Result<MultiZoneTree, SurgeonError> {
    let zones = scan_vue_zones(source);
    let mut degraded = false;

    // ── Script zone (TypeScript grammar) ─────────────────────────────────────
    let script_tree = parse_zone_with_grammar(
        source,
        zones.script.as_ref(),
        &tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "<vue-script>",
        true, // fatal if this fails
        &mut degraded,
    )?;

    // ── Template zone (HTML grammar) ─────────────────────────────────────────
    let template_tree = parse_zone_with_grammar(
        source,
        zones.template.as_ref(),
        &tree_sitter_html::LANGUAGE.into(),
        "<vue-template>",
        false, // non-fatal
        &mut degraded,
    )?;

    // ── Style zone (CSS grammar) ──────────────────────────────────────────────
    let style_tree = parse_zone_with_grammar(
        source,
        zones.style.as_ref(),
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

/// Internal helper: set up a parser for one zone and run it.
///
/// If `fatal` is `false` any error sets `degraded = true` and returns `Ok(None)`.
/// If `fatal` is `true` any error is propagated as `Err(SurgeonError::ParseError)`.
fn parse_zone_with_grammar(
    source: &[u8],
    zone: Option<&VueZoneRange>,
    grammar: &tree_sitter::Language,
    zone_label: &str,
    fatal: bool,
    degraded: &mut bool,
) -> Result<Option<Tree>, SurgeonError> {
    let Some(z) = zone else {
        return Ok(None);
    };

    let ts_range = Range {
        start_byte: z.start_byte,
        end_byte: z.end_byte,
        start_point: z.start_point,
        end_point: z.end_point,
    };

    let mut parser = Parser::new();

    let set_lang_result = parser.set_language(grammar);
    let set_ranges_result = set_lang_result
        .ok()
        .and_then(|()| parser.set_included_ranges(&[ts_range]).ok());

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

    let tree = parser.parse(source, None);
    if tree.is_none() && fatal {
        return Err(SurgeonError::ParseError {
            path: std::path::PathBuf::from(zone_label),
            reason: "tree-sitter parse returned None".into(),
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
