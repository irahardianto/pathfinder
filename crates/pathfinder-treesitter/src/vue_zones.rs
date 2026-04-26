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
    /// Kept intact for version hashing, OCC, and symbol source extraction.
    pub source: Arc<[u8]>,
    /// `true` when HTML or CSS grammar was unavailable and the respective zone
    /// was skipped. Template / style symbols are absent; script symbols are unaffected.
    pub degraded: bool,
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

/// Compute the `tree_sitter::Point` (row, column) for a byte offset.
///
/// Row is 0-indexed; column is the byte distance from the last `\n`.
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

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A representative three-zone Vue SFC used across multiple tests.
    const BASIC_SFC: &[u8] = br#"<template>
  <div class="app">
    <MyButton @click="doThing">Click me</MyButton>
    <router-view />
  </div>
</template>
<script setup lang="ts">
import { ref } from 'vue'
const count = ref(0)
function doThing() { count.value++ }
</script>
<style scoped>
.app { color: red; }
#main { font-size: 16px; }
@media (max-width: 768px) { .app { display: none; } }
</style>"#;

    // ── Zone scanner tests ────────────────────────────────────────────────────

    #[test]
    fn test_scan_vue_zones_all_three_present() {
        let zones = scan_vue_zones(BASIC_SFC);
        assert!(zones.template.is_some(), "template zone should be found");
        assert!(zones.script.is_some(), "script zone should be found");
        assert!(zones.style.is_some(), "style zone should be found");
    }

    #[test]
    fn test_scan_vue_zones_content_bytes_correct() {
        let zones = scan_vue_zones(BASIC_SFC);
        let sfc_str = std::str::from_utf8(BASIC_SFC).unwrap();

        let script = zones.script.unwrap();
        let script_content = &sfc_str[script.start_byte..script.end_byte];
        assert!(
            script_content.contains("const count = ref(0)"),
            "script content must include TS code"
        );

        let template = zones.template.unwrap();
        let tmpl_content = &sfc_str[template.start_byte..template.end_byte];
        assert!(
            tmpl_content.contains("MyButton"),
            "template content must include component tag"
        );

        let style = zones.style.unwrap();
        let style_content = &sfc_str[style.start_byte..style.end_byte];
        assert!(
            style_content.contains(".app"),
            "style content must include CSS class"
        );
    }

    #[test]
    fn test_scan_vue_zones_template_only() {
        let sfc = b"<template><div>Hello</div></template>\n";
        let zones = scan_vue_zones(sfc);
        assert!(zones.template.is_some());
        assert!(zones.script.is_none());
        assert!(zones.style.is_none());
    }

    #[test]
    fn test_scan_vue_zones_does_not_match_partial_tag() {
        // `<script-runner>` must not match `<script>`
        let sfc = b"<template><script-runner /></template>\n";
        let zones = scan_vue_zones(sfc);
        assert!(
            zones.script.is_none(),
            "script-runner must not match <script>"
        );
        assert!(zones.template.is_some());
    }

    #[test]
    fn test_scan_vue_zones_byte_to_point_newline_accuracy() {
        // "<template>" is 10 bytes. The first byte of content is `\n` at index 10.
        // It resides at row 0, column 10 (since the \n hasn't been passed yet).
        let sfc = b"<template>\n<div/>\n</template>\n";
        let zones = scan_vue_zones(sfc);
        let tmpl = zones.template.unwrap();
        assert_eq!(tmpl.start_byte, 10, "content starts after '<template>'");
        assert_eq!(tmpl.start_point.row, 0, "should be on row 0");
        assert_eq!(tmpl.start_point.column, 10, "should be at column 10");
    }

    #[test]
    fn test_scan_vue_zones_empty_source() {
        let zones = scan_vue_zones(b"");
        assert!(zones.script.is_none());
        assert!(zones.template.is_none());
        assert!(zones.style.is_none());
    }

    // ── Multi-grammar parse tests ─────────────────────────────────────────────

    #[test]
    fn test_parse_vue_multizone_produces_all_trees() {
        let result = parse_vue_multizone(BASIC_SFC).unwrap();
        assert!(result.script_tree.is_some(), "script tree should parse");
        assert!(result.template_tree.is_some(), "template tree should parse");
        assert!(result.style_tree.is_some(), "style tree should parse");
        assert!(!result.degraded, "should not be degraded");
    }

    #[test]
    fn test_parse_vue_multizone_script_root_is_program() {
        let result = parse_vue_multizone(BASIC_SFC).unwrap();
        let tree = result.script_tree.unwrap();
        assert_eq!(
            tree.root_node().kind(),
            "program",
            "TypeScript root node should be 'program'"
        );
    }

    #[test]
    fn test_parse_vue_multizone_no_script_block() {
        let sfc = b"<template><div>Hello</div></template>\n";
        let result = parse_vue_multizone(sfc).unwrap();
        assert!(result.script_tree.is_none());
        assert!(result.template_tree.is_some());
        assert!(result.style_tree.is_none());
    }

    #[test]
    fn test_parse_vue_multizone_source_preserved() {
        let result = parse_vue_multizone(BASIC_SFC).unwrap();
        assert_eq!(
            result.source,
            Arc::from(BASIC_SFC),
            "source bytes should be preserved unchanged"
        );
    }

    #[test]
    fn test_parse_vue_multizone_script_node_has_correct_global_offset() {
        // The script tree nodes should have byte offsets inside the original SFC,
        // not offsets relative to an extracted script slice.
        let result = parse_vue_multizone(BASIC_SFC).unwrap();
        let tree = result.script_tree.unwrap();
        let zones = result.zones;

        // The script zone starts after `<script setup lang="ts">\n`
        // Root node's start_byte should be >= zones.script.start_byte
        let script_start = zones.script.unwrap().start_byte;
        let root_start = tree.root_node().start_byte();
        assert!(
            root_start >= script_start,
            "script root start_byte ({root_start}) should be >= zone start ({script_start})"
        );
    }

    // ── Helper: byte_to_point ─────────────────────────────────────────────────

    #[test]
    fn test_byte_to_point_start_of_file() {
        let p = byte_to_point(b"hello", 0);
        assert_eq!(p.row, 0);
        assert_eq!(p.column, 0);
    }

    #[test]
    fn test_byte_to_point_second_line() {
        // "hello\n" is 6 bytes; byte 6 is start of second line
        let p = byte_to_point(b"hello\nworld", 6);
        assert_eq!(p.row, 1);
        assert_eq!(p.column, 0);
    }

    #[test]
    fn test_byte_to_point_mid_line() {
        // "abc\nde" — byte 5 is 'e', row 1, col 1
        let p = byte_to_point(b"abc\nde", 5);
        assert_eq!(p.row, 1);
        assert_eq!(p.column, 1);
    }
}
