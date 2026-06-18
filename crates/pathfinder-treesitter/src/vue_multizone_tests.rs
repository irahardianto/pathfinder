use super::*;
use crate::vue_zones::parse_vue_multizone;

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

#[test]
fn test_extract_multizone_script_symbols_at_top_level() {
    let multi = parse_vue_multizone(BASIC_SFC).unwrap();
    let syms = extract_symbols_from_multizone(&multi);

    // Script symbols should be at top level (backward compat — no zone prefix)
    let func = syms.iter().find(|s| s.name == "doThing");
    assert!(
        func.is_some(),
        "doThing function should be a top-level symbol"
    );
    assert_eq!(func.unwrap().semantic_path, "doThing");
}

#[test]
fn test_extract_multizone_template_zone_container() {
    let multi = parse_vue_multizone(BASIC_SFC).unwrap();
    let syms = extract_symbols_from_multizone(&multi);

    let template_sym = syms.iter().find(|s| s.name == "template");
    assert!(
        template_sym.is_some(),
        "template zone container should exist"
    );
    assert_eq!(template_sym.unwrap().kind, crate::surgeon::SymbolKind::Zone);
}

#[test]
fn test_extract_multizone_template_component_child() {
    let multi = parse_vue_multizone(BASIC_SFC).unwrap();
    let syms = extract_symbols_from_multizone(&multi);

    let template_sym = syms.iter().find(|s| s.name == "template").unwrap();
    let my_button = template_sym.children.iter().find(|c| c.name == "MyButton");
    assert!(
        my_button.is_some(),
        "MyButton component should be extracted"
    );
    assert_eq!(
        my_button.unwrap().kind,
        crate::surgeon::SymbolKind::Component
    );
    assert_eq!(my_button.unwrap().semantic_path, "template::MyButton");
}

#[test]
fn test_extract_multizone_template_html_element() {
    let multi = parse_vue_multizone(BASIC_SFC).unwrap();
    let syms = extract_symbols_from_multizone(&multi);

    let template_sym = syms.iter().find(|s| s.name == "template").unwrap();
    let div = template_sym.children.iter().find(|c| c.name == "div");
    assert!(div.is_some(), "div element should be extracted");
    assert_eq!(div.unwrap().kind, crate::surgeon::SymbolKind::HtmlElement);
}

#[test]
fn test_extract_multizone_style_zone_container() {
    let multi = parse_vue_multizone(BASIC_SFC).unwrap();
    let syms = extract_symbols_from_multizone(&multi);

    let style_sym = syms.iter().find(|s| s.name == "style");
    assert!(style_sym.is_some(), "style zone container should exist");
    assert_eq!(style_sym.unwrap().kind, crate::surgeon::SymbolKind::Zone);
}

#[test]
fn test_extract_multizone_style_class_selector() {
    let multi = parse_vue_multizone(BASIC_SFC).unwrap();
    let syms = extract_symbols_from_multizone(&multi);

    let style_sym = syms.iter().find(|s| s.name == "style").unwrap();
    let class_sel = style_sym.children.iter().find(|c| c.name == ".app");
    assert!(class_sel.is_some(), ".app CSS class should be extracted");
    assert_eq!(
        class_sel.unwrap().kind,
        crate::surgeon::SymbolKind::CssSelector
    );
    assert_eq!(class_sel.unwrap().semantic_path, "style::.app");
}

#[test]
fn test_extract_multizone_style_id_selector() {
    let multi = parse_vue_multizone(BASIC_SFC).unwrap();
    let syms = extract_symbols_from_multizone(&multi);

    let style_sym = syms.iter().find(|s| s.name == "style").unwrap();
    let id_sel = style_sym.children.iter().find(|c| c.name == "#main");
    assert!(id_sel.is_some(), "#main CSS id should be extracted");
    assert_eq!(id_sel.unwrap().semantic_path, "style::#main");
}

#[test]
fn test_extract_multizone_style_at_rule() {
    let multi = parse_vue_multizone(BASIC_SFC).unwrap();
    let syms = extract_symbols_from_multizone(&multi);

    let style_sym = syms.iter().find(|s| s.name == "style").unwrap();
    let media = style_sym.children.iter().find(|c| c.name == "@media");
    assert!(media.is_some(), "@media rule should be extracted");
    assert_eq!(media.unwrap().kind, crate::surgeon::SymbolKind::CssAtRule);
}

#[test]
fn test_extract_multizone_template_only_sfc() {
    let sfc = b"<template><div>Hello</div></template>\n";
    let multi = parse_vue_multizone(sfc).unwrap();
    let syms = extract_symbols_from_multizone(&multi);

    // No script symbols
    assert!(
        !syms
            .iter()
            .any(|s| s.kind == crate::surgeon::SymbolKind::Function),
        "no function symbols in template-only SFC"
    );
    // Template zone container should be present
    assert!(syms.iter().any(|s| s.name == "template"));
}

#[test]
fn test_find_enclosing_symbol_in_template_zone() {
    let multi = parse_vue_multizone(BASIC_SFC).unwrap();
    let syms = extract_symbols_from_multizone(&multi);

    // Find the template zone
    let template_sym = syms.iter().find(|s| s.name == "template").unwrap();
    // The template zone spans certain lines; the enclosing symbol for a line
    // inside it should return the template zone (or a child).
    let result = find_enclosing_symbol(&syms, template_sym.start_line + 1);
    assert!(
        result.is_some(),
        "should find an enclosing symbol inside template zone"
    );
    assert!(
        result.unwrap().starts_with("template"),
        "enclosing symbol path should start with 'template'"
    );
}

#[test]
fn test_vue_template_depth_cap() {
    let sfc = br#"<template>
  <div> <!-- depth 0 -->
    <span> <!-- depth 1 -->
      <p> <!-- depth 2 -->
        <a> <!-- depth 3 -->
          <span></span> <!-- depth 4 -->
        </a>
        <MyComponent></MyComponent> <!-- component at depth 3, promoted -->
      </p>
    </span>
  </div>
</template>
<script setup lang="ts"></script>"#;
    let multi = parse_vue_multizone(sfc).unwrap();
    let syms = extract_symbols_from_multizone(&multi);
    let template_sym = syms.iter().find(|s| s.name == "template").unwrap();

    // Check div at depth 0
    let div = template_sym.children.iter().find(|c| c.name == "div");
    assert!(div.is_some(), "div at depth 0 should exist");
    assert_eq!(div.unwrap().semantic_path, "template::div");

    // Check span at depth 1
    let span = template_sym.children.iter().find(|c| c.name == "span");
    assert!(span.is_some(), "span at depth 1 should exist");
    assert_eq!(span.unwrap().semantic_path, "template::div::span");

    // Check p at depth 2
    let p = template_sym.children.iter().find(|c| c.name == "p");
    assert!(p.is_some(), "p at depth 2 should exist");
    assert_eq!(p.unwrap().semantic_path, "template::div::span::p");

    // Check a at depth 3 (native html, depth >= 3) -> should not exist
    let a = template_sym.children.iter().find(|c| c.name == "a");
    assert!(a.is_none(), "a at depth 3 should NOT exist");

    // Check MyComponent at depth 3 (component, always promoted) -> should exist
    let comp = template_sym
        .children
        .iter()
        .find(|c| c.name == "MyComponent");
    assert!(comp.is_some(), "MyComponent should exist even at depth 3");
    assert_eq!(comp.unwrap().semantic_path, "template::MyComponent");
}
