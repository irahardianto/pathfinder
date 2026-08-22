use crate::surgeon::{AccessLevel, ExtractedSymbol, SymbolKind};
use tree_sitter::Node;

/// Extract CSS selector symbols from a CSS parse tree (`<style>` zone).
#[must_use]
pub fn extract_style_symbols(tree: &tree_sitter::Tree, source: &[u8]) -> Vec<ExtractedSymbol> {
    let mut symbols: Vec<ExtractedSymbol> = Vec::new();
    let mut at_rule_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    walk_css_rules(
        tree.root_node(),
        source,
        "style",
        &mut symbols,
        &mut at_rule_counts,
    );
    symbols
}

/// Recursive CSS rule walker for style symbol extraction.
pub(crate) fn walk_css_rules(
    node: Node<'_>,
    source: &[u8],
    parent_path: &str,
    out: &mut Vec<ExtractedSymbol>,
    at_counts: &mut std::collections::HashMap<String, usize>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "rule_set" => {
                extract_css_rule_set(child, source, parent_path, out);
            }
            "media_statement" | "keyframes_statement" | "at_rule" => {
                let at_name = extract_at_rule_name(child, source);
                let count = at_counts.entry(at_name.clone()).or_insert(0);
                *count += 1;
                let nth = *count;
                let sym_name = if nth == 1 {
                    format!("@{at_name}")
                } else {
                    format!("@{at_name}[{nth}]")
                };
                let sym_path = format!("{parent_path}.{sym_name}");
                let name_point = child.start_position();
                let name_column = name_point.column;
                let byte_range = child.byte_range();
                out.push(ExtractedSymbol {
                    name: sym_name,
                    semantic_path: sym_path,
                    kind: SymbolKind::CssAtRule,
                    byte_range: byte_range.clone(),
                    attached_range: byte_range,
                    start_line: child.start_position().row,
                    end_line: child.end_position().row,
                    name_column,
                    name_point,
                    access_level: AccessLevel::Public,
                    children: Vec::new(),
                });
            }
            _ => {
                walk_css_rules(child, source, parent_path, out, at_counts);
            }
        }
    }
}

/// Extract the at-rule keyword (e.g. "media", "keyframes") from an at-rule node.
#[must_use]
pub fn extract_at_rule_name(node: Node<'_>, source: &[u8]) -> String {
    let mut c = node.walk();
    for child in node.named_children(&mut c) {
        if matches!(child.kind(), "at_keyword" | "keyword") {
            if let Some(bytes) = source.get(child.byte_range()) {
                if let Ok(s) = std::str::from_utf8(bytes) {
                    return s.trim_start_matches('@').trim().to_owned();
                }
            }
        }
    }
    match node.kind() {
        "media_statement" => "media".to_owned(),
        "keyframes_statement" => "keyframes".to_owned(),
        _ => "rule".to_owned(),
    }
}

/// Parse the name of a CSS selector node (class `.foo`, ID `#bar`, or tag `p`).
#[must_use]
pub fn parse_css_selector_name(selector: Node<'_>, source: &[u8]) -> Option<String> {
    match selector.kind() {
        "class_selector" => {
            let mut cc = selector.walk();
            let found = selector
                .named_children(&mut cc)
                .find(|n| n.kind() == "class_name");
            let range = found.map(|n| n.byte_range());
            range
                .and_then(|r| source.get(r))
                .and_then(|b| std::str::from_utf8(b).ok())
                .map(|s| format!(".{}", s.trim()))
        }
        "id_selector" => {
            let mut cc = selector.walk();
            let found = selector
                .named_children(&mut cc)
                .find(|n| n.kind() == "id_name");
            let range = found.map(|n| n.byte_range());
            range
                .and_then(|r| source.get(r))
                .and_then(|b| std::str::from_utf8(b).ok())
                .map(|s| format!("#{}", s.trim()))
        }
        "tag_name" => source
            .get(selector.byte_range())
            .and_then(|b| std::str::from_utf8(b).ok())
            .map(|s| s.trim().to_owned()),
        _ => None,
    }
}

/// Extract selector symbols from a single `rule_set` node.
pub fn extract_css_rule_set(
    node: Node<'_>,
    source: &[u8],
    parent_path: &str,
    out: &mut Vec<ExtractedSymbol>,
) {
    let mut c = node.walk();
    let selectors_node = node
        .named_children(&mut c)
        .find(|n| n.kind() == "selectors");

    let Some(sel_node) = selectors_node else {
        return;
    };

    let mut sel_cursor = sel_node.walk();
    for selector in sel_node.named_children(&mut sel_cursor) {
        let name_opt = parse_css_selector_name(selector, source);

        if let Some(sel_name) = name_opt {
            if sel_name.is_empty() {
                continue;
            }
            let sym_path = format!("{parent_path}.{sel_name}");
            let name_point = selector.start_position();
            let name_column = name_point.column;
            let byte_range = node.byte_range();
            out.push(ExtractedSymbol {
                name: sel_name,
                semantic_path: sym_path,
                kind: SymbolKind::CssSelector,
                byte_range: byte_range.clone(),
                attached_range: byte_range,
                start_line: node.start_position().row,
                end_line: node.end_position().row,
                name_column,
                name_point,
                access_level: AccessLevel::Public,
                children: Vec::new(),
            });
        }
    }
}
