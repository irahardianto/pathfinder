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
