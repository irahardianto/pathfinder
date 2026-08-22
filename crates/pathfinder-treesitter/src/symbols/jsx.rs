use crate::surgeon::{AccessLevel, ExtractedSymbol, SymbolKind};
use tree_sitter::Node;

/// Resolve the tag name from a `jsx_element` or `jsx_self_closing_element` node.
#[must_use]
pub fn resolve_jsx_tag_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    let kind = node.kind();

    if kind == "jsx_element" {
        let mut c = node.walk();
        let open_tag = node
            .named_children(&mut c)
            .find(|n| n.kind() == "jsx_opening_element")?;

        let mut oc = open_tag.walk();
        let name_node = open_tag.named_children(&mut oc).find(|n| {
            matches!(
                n.kind(),
                "identifier" | "member_expression" | "jsx_namespace_name"
            )
        })?;
        let name_bytes = source.get(name_node.byte_range())?;
        std::str::from_utf8(name_bytes)
            .ok()
            .map(|s| s.trim().to_owned())
    } else if kind == "jsx_self_closing_element" {
        let mut c = node.walk();
        let name_node = node.named_children(&mut c).find(|n| {
            matches!(
                n.kind(),
                "identifier" | "member_expression" | "jsx_namespace_name"
            )
        })?;
        let name_bytes = source.get(name_node.byte_range())?;
        std::str::from_utf8(name_bytes)
            .ok()
            .map(|s| s.trim().to_owned())
    } else {
        None
    }
}

/// Walk into a `lexical_declaration`/`variable_declaration` to find the inner
/// `arrow_function` or `function_expression` node.
#[must_use]
pub fn find_inner_function(node: Node<'_>) -> Option<Node<'_>> {
    let kind = node.kind();
    if kind == "arrow_function" || kind == "function_declaration" || kind == "function_expression" {
        return Some(node);
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if let Some(found) = find_inner_function(child) {
            return Some(found);
        }
    }
    None
}

/// Extract JSX elements from a function's body as child symbols.
pub fn extract_jsx_children(
    fn_node: Node<'_>,
    source: &[u8],
    fn_path: &str,
    out: &mut Vec<ExtractedSymbol>,
) {
    let mut tag_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    // For `const Foo = () => <jsx/>` patterns, fn_node is a `lexical_declaration`;
    // we need to locate the inner `arrow_function` or `function_expression` node.
    let target = find_inner_function(fn_node).unwrap_or(fn_node);

    walk_jsx_return_sites(target, source, fn_path, out, &mut tag_counts);
}

/// Recursively descend into a function node to find JSX return sites.
pub(crate) fn walk_jsx_return_sites(
    node: Node<'_>,
    source: &[u8],
    return_path: &str,
    out: &mut Vec<ExtractedSymbol>,
    tag_counts: &mut std::collections::HashMap<String, usize>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let kind = child.kind();

        match kind {
            "return_statement" => {
                collect_jsx_elements(child, source, return_path, out, tag_counts);
            }
            "jsx_element" | "jsx_self_closing_element"
                if node.kind() == "arrow_function" || node.kind() == "parenthesized_expression" =>
            {
                emit_jsx_symbol(child, source, return_path, out, tag_counts);
                collect_jsx_elements(child, source, return_path, out, tag_counts);
            }
            "statement_block"
            | "parenthesized_expression"
            | "if_statement"
            | "switch_body"
            | "switch_case" => {
                walk_jsx_return_sites(child, source, return_path, out, tag_counts);
            }
            _ => {
                if node.kind() == "arrow_function" || node.kind() == "parenthesized_expression" {
                    walk_jsx_return_sites(child, source, return_path, out, tag_counts);
                }
            }
        }
    }
}

/// Collect all JSX elements recursively from under a given node.
pub(crate) fn collect_jsx_elements(
    node: Node<'_>,
    source: &[u8],
    parent_path: &str,
    out: &mut Vec<ExtractedSymbol>,
    tag_counts: &mut std::collections::HashMap<String, usize>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let kind = child.kind();
        if kind == "jsx_element" || kind == "jsx_self_closing_element" {
            emit_jsx_symbol(child, source, parent_path, out, tag_counts);
        }
        collect_jsx_elements(child, source, parent_path, out, tag_counts);
    }
}

/// Emit a single JSX symbol from a `jsx_element` or `jsx_self_closing_element` node.
pub(crate) fn emit_jsx_symbol(
    node: Node<'_>,
    source: &[u8],
    parent_path: &str,
    out: &mut Vec<ExtractedSymbol>,
    tag_counts: &mut std::collections::HashMap<String, usize>,
) {
    if let Some(ref name) = resolve_jsx_tag_name(node, source) {
        let is_component = name.chars().next().is_some_and(char::is_uppercase);
        let sym_kind = if is_component {
            SymbolKind::Component
        } else {
            SymbolKind::HtmlElement
        };

        let count = tag_counts.entry(name.clone()).or_insert(0);
        *count += 1;
        let nth = *count;
        let sym_name = if nth == 1 {
            name.clone()
        } else {
            format!("{name}[{nth}]")
        };

        let sym_path = format!("{parent_path}.{sym_name}");
        let name_point = node.start_position();
        let name_column = name_point.column;
        let byte_range = node.byte_range();

        out.push(ExtractedSymbol {
            name: sym_name,
            semantic_path: sym_path,
            kind: sym_kind,
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
