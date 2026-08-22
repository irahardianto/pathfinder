use crate::language::SupportedLanguage;
use crate::surgeon::{AccessLevel, ExtractedSymbol, SymbolKind};
use crate::symbols::{
    disambiguate_overloads_recursive, extract_symbols_from_tree, validate_symbol_invariants,
};
use crate::vue_zones::{MultiZoneTree, VueZoneRange};

/// Push a zone symbol (template or style) into the output if children are non-empty.
pub fn push_zone_symbol(
    output: &mut Vec<ExtractedSymbol>,
    zone_name: &str,
    children: Vec<ExtractedSymbol>,
    zone_range: Option<&VueZoneRange>,
) {
    if children.is_empty() {
        return;
    }
    let byte_range = zone_range.map_or(0..0, |z| z.start_byte..z.end_byte);
    let start_line = zone_range.map_or(0, |z| z.start_point.row);
    let end_line = zone_range.map_or(0, |z| z.end_point.row);
    let name_point = zone_range.map_or(
        tree_sitter::Point {
            row: start_line,
            column: 0,
        },
        |z| z.start_point,
    );
    output.push(ExtractedSymbol {
        name: zone_name.to_string(),
        semantic_path: zone_name.to_string(),
        kind: SymbolKind::Zone,
        byte_range: byte_range.clone(),
        attached_range: byte_range,
        start_line,
        end_line,
        name_column: 0,
        name_point,
        access_level: AccessLevel::Public,
        children,
    });
}

/// Extract symbols from all zones of a parsed Vue SFC.
///
/// Returns a flat list that includes:
/// - Script zone symbols at the **top level** (backward-compatible, no zone prefix needed).
/// - A `Zone` symbol named `"template"` with HTML component/element children.
/// - A `Zone` symbol named `"style"` with CSS selector children.
///
/// Agents targeting script symbols use `file.vue::FunctionName` (existing).
/// Agents targeting template / style symbols use `file.vue::template.ComponentName`
/// or `file.vue::style..className` via dot-separated chain segments.
#[must_use]
pub fn extract_symbols_from_multizone(multi: &MultiZoneTree) -> Vec<ExtractedSymbol> {
    let mut output: Vec<ExtractedSymbol> = Vec::default();

    // ── Script zone (top-level, backward-compatible) ──────────────────────────
    if let Some(ref tree) = multi.script_tree {
        let ts_syms = extract_symbols_from_tree(tree, &multi.source, SupportedLanguage::Vue);
        output.extend(ts_syms);
    }

    // ── Template zone ─────────────────────────────────────────────────────────
    if let Some(ref tree) = multi.template_tree {
        let mut children = extract_template_symbols(tree, &multi.source);
        disambiguate_overloads_recursive("template", &mut children);
        push_zone_symbol(
            &mut output,
            "template",
            children,
            multi.zones.template.as_ref(),
        );
    }

    // ── Style zone ────────────────────────────────────────────────────────────
    if let Some(ref tree) = multi.style_tree {
        let mut children = crate::symbols::css::extract_style_symbols(tree, &multi.source);
        disambiguate_overloads_recursive("style", &mut children);
        push_zone_symbol(&mut output, "style", children, multi.zones.style.as_ref());
    }

    validate_symbol_invariants(&output);

    output
}

/// Extract component/element symbols from an HTML parse tree (`<template>` zone).
///
/// Promotion rule: **Vue Components** (tags starting with an uppercase letter, e.g.
/// `<MyButton>`) are always emitted as **direct children** of the template zone,
/// regardless of how deeply they are nested in the DOM. This makes them addressable
/// as `template.MyButton` in semantic paths without requiring agents to know the DOM
/// nesting depth.
///
/// Native HTML elements (`<div>`, `<p>`, …) are collected at the root level of the
/// template only. Nested HTML elements are not promoted.
#[must_use]
pub fn extract_template_symbols(tree: &tree_sitter::Tree, source: &[u8]) -> Vec<ExtractedSymbol> {
    let mut symbols: Vec<ExtractedSymbol> = Vec::default();
    let mut tag_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::default();

    walk_html_elements_flat(
        tree.root_node(),
        source,
        "template",
        0,
        &mut symbols,
        &mut tag_counts,
    );

    symbols
}

/// Process an HTML child element node, emitting a Component or `HtmlElement` symbol.
pub(crate) fn process_html_child_element(
    child: tree_sitter::Node<'_>,
    name: &str,
    source: &[u8],
    parent_path: &str,
    depth: usize,
    out: &mut Vec<ExtractedSymbol>,
    tag_counts: &mut std::collections::HashMap<String, usize>,
) {
    let is_component = name.chars().next().is_some_and(char::is_uppercase);
    let sym_kind = if is_component {
        SymbolKind::Component
    } else {
        SymbolKind::HtmlElement
    };

    let count = tag_counts.entry(name.to_owned()).or_insert(0);
    *count += 1;
    let nth = *count;
    let sym_name = if nth == 1 {
        name.to_owned()
    } else {
        format!("{name}[{nth}]")
    };

    // All template elements and components are direct children of template zone
    let sym_path = format!("template.{sym_name}");

    let should_emit = is_component || depth < 3;

    if should_emit {
        let name_point = child.start_position();
        let name_column = name_point.column;
        let byte_range = child.byte_range();
        out.push(ExtractedSymbol {
            name: sym_name,
            semantic_path: sym_path.clone(),
            kind: sym_kind,
            byte_range: byte_range.clone(),
            attached_range: byte_range,
            start_line: child.start_position().row,
            end_line: child.end_position().row,
            name_column,
            name_point,
            access_level: AccessLevel::Public,
            children: Vec::new(), // Always flat
        });

        // Recurse into children
        walk_html_elements_flat(child, source, &sym_path, depth + 1, out, tag_counts);
    } else {
        // Recurse into children
        walk_html_elements_flat(child, source, parent_path, depth + 1, out, tag_counts);
    }
}

/// Recursive HTML element walker that flattens elements into a single list.
pub(crate) fn walk_html_elements_flat(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    parent_path: &str,
    depth: usize,
    out: &mut Vec<ExtractedSymbol>,
    tag_counts: &mut std::collections::HashMap<String, usize>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let kind = child.kind();
        let tag_name_opt = resolve_tag_name(kind, child, source);

        if let Some(ref name) = tag_name_opt {
            process_html_child_element(child, name, source, parent_path, depth, out, tag_counts);
        } else {
            walk_html_elements_flat(child, source, parent_path, depth, out, tag_counts);
        }
    }
}

/// Resolves the actual tag name string from an HTML AST node.
pub fn resolve_tag_name(kind: &str, child: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    if kind == "element" {
        let start_tag = child.child_by_field_name("start_tag").or_else(|| {
            let mut c = child.walk();
            let found = child
                .named_children(&mut c)
                .find(|n| n.kind() == "start_tag");
            found.map(|n| child.child_by_field_name("start_tag").unwrap_or(n))
        });
        let tag_name_range = start_tag.and_then(|tag| {
            let mut c = tag.walk();
            let found = tag.named_children(&mut c).find(|n| n.kind() == "tag_name");
            found.map(|n| n.byte_range())
        });
        tag_name_range
            .and_then(|r| source.get(r))
            .and_then(|b| std::str::from_utf8(b).ok())
            .map(str::trim)
            .map(str::to_owned)
    } else if kind == "self_closing_element" {
        let mut c = child.walk();
        let found = child
            .named_children(&mut c)
            .find(|n| n.kind() == "tag_name");
        let range = found.map(|n| n.byte_range());
        range
            .and_then(|r| source.get(r))
            .and_then(|b| std::str::from_utf8(b).ok())
            .map(str::trim)
            .map(str::to_owned)
    } else {
        None
    }
}
