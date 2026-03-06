use crate::language::SupportedLanguage;
use crate::surgeon::{ExtractedSymbol, SymbolKind};
use pathfinder_common::types::SymbolChain;
use strsim::levenshtein;
use tree_sitter::{Node, Tree};

/// Extract all supported symbols from a parsed AST tree using `TreeCursor` traversal.
#[must_use]
pub fn extract_symbols_from_tree(
    tree: &Tree,
    source: &[u8],
    lang: SupportedLanguage,
) -> Vec<ExtractedSymbol> {
    let mut symbols = Vec::new();
    let root = tree.root_node();
    let types = lang.node_types();

    // We start traversal at the root level without a parent path
    extract_symbols_recursive(root, source, types, "", &mut symbols);
    symbols
}

fn extract_symbols_recursive(
    node: Node,
    source: &[u8],
    types: &crate::language::LanguageNodeTypes,
    parent_path: &str,
    out: &mut Vec<ExtractedSymbol>,
) {
    let mut cursor = node.walk();

    // Check all children
    for child in node.named_children(&mut cursor) {
        let kind = child.kind();

        let sym_kind = if types.function_kinds.contains(&kind) {
            Some(SymbolKind::Function)
        } else if types.class_kinds.contains(&kind) {
            Some(SymbolKind::Class)
        } else if types.method_kinds.contains(&kind) {
            Some(SymbolKind::Method)
        } else if types.constant_kinds.contains(&kind) {
            Some(SymbolKind::Constant)
        } else {
            None
        };

        // Handle impl blocks (Rust-style): extract the implementing type name and
        // list all associated functions as Method children under that type.
        if types.impl_kinds.contains(&kind) {
            extract_impl_block(child, source, types, parent_path, out);
            continue;
        }

        if let Some(sk) = sym_kind {
            // Try to extract the name
            if let Some(name_node) = child
                .child_by_field_name("name")
                .or_else(|| child.child_by_field_name("identifier"))
            {
                if let Ok(name) = std::str::from_utf8(&source[name_node.byte_range()]) {
                    let name = name.trim().to_string();
                    let path = if parent_path.is_empty() {
                        name.clone()
                    } else {
                        format!("{parent_path}.{name}")
                    };

                    let mut symbol = ExtractedSymbol {
                        name,
                        semantic_path: path.clone(),
                        kind: sk,
                        byte_range: child.byte_range(),
                        start_line: child.start_position().row,
                        end_line: child.end_position().row,
                        children: Vec::new(),
                    };

                    // For classes/structs, we want to extract nested methods
                    if sk == SymbolKind::Class {
                        // Normally, languages have a `body` block inside the class
                        if let Some(body) = child.child_by_field_name("body") {
                            extract_symbols_recursive(
                                body,
                                source,
                                types,
                                &path,
                                &mut symbol.children,
                            );
                        } else {
                            // Fallback if no specific body field, just traverse children
                            extract_symbols_recursive(
                                child,
                                source,
                                types,
                                &path,
                                &mut symbol.children,
                            );
                        }
                    }

                    out.push(symbol);
                    continue;
                }
            }
        }

        // If not a recognized symbol, or we failed to extract a name, still recurse down
        // to find nested symbols (like functions inside export blocks)
        extract_symbols_recursive(child, source, types, parent_path, out);
    }
}

/// Extract methods from a Rust `impl_item` node.
///
/// Reads the implementing type from the `type` field, then iterates the `body`
/// `declaration_list` to extract associated `function_item` nodes as `Method`
/// children. The resulting symbol is appended directly to `out` so that the
/// repo-map renderer can display it under its type name.
fn extract_impl_block(
    node: Node,
    source: &[u8],
    types: &crate::language::LanguageNodeTypes,
    parent_path: &str,
    out: &mut Vec<ExtractedSymbol>,
) {
    // The `type` field holds the type being implemented (e.g., `MyStruct`).
    let Some(type_node) = node.child_by_field_name("type") else {
        return;
    };
    let Ok(type_name) = std::str::from_utf8(&source[type_node.byte_range()]) else {
        return;
    };
    let type_name = type_name.trim().to_string();
    let impl_path = if parent_path.is_empty() {
        type_name.clone()
    } else {
        format!("{parent_path}.{type_name}")
    };

    // Collect all child function_items from the impl body as Method symbols.
    let mut methods: Vec<ExtractedSymbol> = Vec::new();
    if let Some(body) = node.child_by_field_name("body") {
        let mut body_cursor = body.walk();
        for item in body.named_children(&mut body_cursor) {
            if !types.function_kinds.contains(&item.kind()) {
                continue;
            }
            let Some(name_node) = item.child_by_field_name("name") else {
                continue;
            };
            let Ok(method_name) = std::str::from_utf8(&source[name_node.byte_range()]) else {
                continue;
            };
            let method_name = method_name.trim().to_string();
            let method_path = format!("{impl_path}.{method_name}");
            methods.push(ExtractedSymbol {
                name: method_name,
                semantic_path: method_path,
                kind: SymbolKind::Method,
                byte_range: item.byte_range(),
                start_line: item.start_position().row,
                end_line: item.end_position().row,
                children: Vec::new(),
            });
        }
    }

    if !methods.is_empty() {
        out.push(ExtractedSymbol {
            name: type_name,
            semantic_path: impl_path,
            kind: SymbolKind::Impl,
            byte_range: node.byte_range(),
            start_line: node.start_position().row,
            end_line: node.end_position().row,
            children: methods,
        });
    }
}

/// Resolve a `SymbolChain` against a list of extracted symbols.
#[must_use]
pub fn resolve_symbol_chain<'a>(
    symbols: &'a [ExtractedSymbol],
    chain: &SymbolChain,
) -> Option<&'a ExtractedSymbol> {
    if chain.segments.is_empty() {
        return None;
    }

    let first_target = &chain.segments[0];

    // Find the matching root symbol
    // TODO: handle overloads properly. For now we just take the first match.
    let root_match = symbols.iter().find(|s| s.name == first_target.name)?;

    if chain.segments.len() == 1 {
        return Some(root_match);
    }

    // Recursively resolve the rest of the chain
    let mut current = root_match;
    for segment in &chain.segments[1..] {
        current = current.children.iter().find(|s| s.name == segment.name)?;
    }

    Some(current)
}

/// Computes string similarity to offer did-you-mean suggestions.
///
/// Borrows semantic paths directly from the symbol tree to avoid allocating
/// intermediate `String` values. Only the final `max_suggestions` results are
/// converted to owned `String`s.
#[allow(clippy::items_after_statements)]
#[must_use]
pub fn did_you_mean(
    symbols: &[ExtractedSymbol],
    chain: &SymbolChain,
    max_suggestions: usize,
) -> Vec<String> {
    if chain.segments.is_empty() {
        return Vec::new();
    }

    let target = chain.to_string();

    // Collect &str references — no cloning here.
    fn collect_paths<'a>(syms: &'a [ExtractedSymbol], out: &mut Vec<&'a str>) {
        for s in syms {
            out.push(&s.semantic_path);
            collect_paths(&s.children, out);
        }
    }

    let mut all_paths: Vec<&str> = Vec::new();
    collect_paths(symbols, &mut all_paths);

    // Compute Levenshtein distance for each candidate path.
    let mut distances: Vec<(usize, &str)> = all_paths
        .into_iter()
        .map(|path| (levenshtein(&target, path), path))
        // Only keep sensible distances
        .filter(|(dist, _)| *dist <= 5)
        .collect();

    // Sort by smallest distance
    distances.sort_by_key(|(dist, _)| *dist);

    // Allocate only for the final results — at most max_suggestions strings.
    distances
        .into_iter()
        .take(max_suggestions)
        .map(|(_, path)| path.to_string())
        .collect()
}

/// Find the innermost symbol enclosing a given 0-indexed row.
#[allow(clippy::items_after_statements)]
#[must_use]
pub fn find_enclosing_symbol(symbols: &[ExtractedSymbol], row: usize) -> Option<String> {
    fn search<'a>(syms: &'a [ExtractedSymbol], row: usize, best: &mut Option<&'a ExtractedSymbol>) {
        for s in syms {
            if s.start_line <= row && row <= s.end_line {
                // If this is tighter than the current best match, replace it
                if let Some(current_best) = best {
                    let current_lines = current_best.end_line - current_best.start_line;
                    let target_lines = s.end_line - s.start_line;
                    if target_lines <= current_lines {
                        // Favor deeper children with same line bounds
                        *best = Some(s);
                    }
                } else {
                    *best = Some(s);
                }

                // Recurse into children
                search(&s.children, row, best);
            }
        }
    }

    let mut best_match: Option<&ExtractedSymbol> = None;
    search(symbols, row, &mut best_match);
    best_match.map(|s| s.semantic_path.clone())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::language::SupportedLanguage;
    use crate::parser::AstParser;

    #[test]
    fn test_extract_go_function() {
        let source = b"package main\n\nfunc Login() {}\n";
        let tree = AstParser::parse_source(SupportedLanguage::Go, source).unwrap();

        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Go);
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "Login");
        assert_eq!(syms[0].kind, SymbolKind::Function);
    }

    #[test]
    fn test_extract_ts_class_with_methods() {
        let source = b"class AuthService {\n  login() {}\n  logout() {}\n}";
        let tree = AstParser::parse_source(SupportedLanguage::TypeScript, source).unwrap();

        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::TypeScript);
        assert_eq!(syms.len(), 1);
        let class = &syms[0];
        assert_eq!(class.name, "AuthService");
        assert_eq!(class.kind, SymbolKind::Class);
        assert_eq!(class.children.len(), 2);
        assert_eq!(class.children[0].name, "login");
        assert_eq!(class.children[1].name, "logout");
        assert_eq!(class.children[0].semantic_path, "AuthService.login");
    }

    #[test]
    fn test_did_you_mean() {
        let source = b"class AuthService {\n  login() {}\n}";
        let tree = AstParser::parse_source(SupportedLanguage::TypeScript, source).unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::TypeScript);

        let chain = SymbolChain::parse("AuthService.logni").unwrap();
        let suggestions = did_you_mean(&syms, &chain, 3);
        assert_eq!(suggestions, vec!["AuthService.login"]);
    }

    #[test]
    fn test_find_enclosing_symbol() {
        let source = b"func A() {\n  // line 1 \n}\nfunc B() {}\n";
        let tree = AstParser::parse_source(SupportedLanguage::Go, source).unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Go);

        let path = find_enclosing_symbol(&syms, 1).unwrap();
        assert_eq!(path, "A");
    }

    #[test]
    fn test_extract_rust_impl_methods() {
        let source = b"struct MyStruct;\nimpl MyStruct {\n    fn foo(&self) {}\n    fn bar(&mut self) {}\n}\n";
        let tree = AstParser::parse_source(SupportedLanguage::Rust, source).unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Rust);

        // Expect: one Struct + one Impl (with 2 Method children)
        let impl_sym = syms.iter().find(|s| s.kind == SymbolKind::Impl).unwrap();
        assert_eq!(impl_sym.name, "MyStruct");
        assert_eq!(impl_sym.semantic_path, "MyStruct");
        assert_eq!(impl_sym.children.len(), 2);
        assert_eq!(impl_sym.children[0].name, "foo");
        assert_eq!(impl_sym.children[0].kind, SymbolKind::Method);
        assert_eq!(impl_sym.children[0].semantic_path, "MyStruct.foo");
        assert_eq!(impl_sym.children[1].name, "bar");
        assert_eq!(impl_sym.children[1].kind, SymbolKind::Method);
        assert_eq!(impl_sym.children[1].semantic_path, "MyStruct.bar");
    }

    #[test]
    fn test_extract_rust_free_functions_unchanged() {
        // Free functions at the crate root should still be extracted as Function
        let source = b"fn compute(x: u32) -> u32 { x * 2 }\n";
        let tree = AstParser::parse_source(SupportedLanguage::Rust, source).unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Rust);

        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "compute");
        assert_eq!(syms[0].kind, SymbolKind::Function);
    }
}
