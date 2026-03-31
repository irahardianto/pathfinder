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

#[allow(clippy::too_many_lines)]
fn extract_symbols_recursive(
    node: Node,
    source: &[u8],
    types: &crate::language::LanguageNodeTypes,
    parent_path: &str,
    out: &mut Vec<ExtractedSymbol>,
) {
    let mut cursor = node.walk();
    let mut name_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    // Check all children
    for child in node.named_children(&mut cursor) {
        let kind = child.kind();

        let sym_kind = if types.function_kinds.contains(&kind) {
            Some(SymbolKind::Function)
        } else if types.class_kinds.contains(&kind) {
            // For Go `type_spec`, inspect the type body to assign a precise kind:
            // `interface_type` → Interface, `struct_type` → Struct, otherwise Class.
            let refined =
                child
                    .child_by_field_name("type")
                    .map_or(SymbolKind::Class, |type_node| match type_node.kind() {
                        "interface_type" => SymbolKind::Interface,
                        "struct_type" => SymbolKind::Struct,
                        _ => SymbolKind::Class,
                    });
            Some(refined)
        } else if types.method_kinds.contains(&kind) {
            Some(SymbolKind::Method)
        } else if types.constant_kinds.contains(&kind) {
            let mut kind_refine = SymbolKind::Constant;
            if kind == "lexical_declaration" || kind == "variable_declaration" {
                let mut d_cursor = child.walk();
                for decl in child.named_children(&mut d_cursor) {
                    if decl.kind() == "variable_declarator" {
                        if let Some(val) = decl.child_by_field_name("value") {
                            if val.kind() == "arrow_function" || val.kind() == "function_expression"
                            {
                                kind_refine = SymbolKind::Function;
                                break;
                            }
                        }
                    }
                }
            }
            Some(kind_refine)
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
                .or_else(|| {
                    let mut wc = child.walk();
                    for n in child.named_children(&mut wc) {
                        if n.kind() == "variable_declarator" {
                            if let Some(name) = n.child_by_field_name("name") {
                                return Some(name);
                            }
                        }
                    }
                    None
                })
            {
                if let Some(name_bytes) = source.get(name_node.byte_range()) {
                    if let Ok(name) = std::str::from_utf8(name_bytes) {
                        let name = name.trim().to_string();

                        let count = name_counts.entry(name.clone()).or_insert(0);
                        *count += 1;

                        let suffix = if *count > 1 {
                            format!("#{count}")
                        } else {
                            String::new()
                        };

                        let path = if parent_path.is_empty() {
                            format!("{name}{suffix}")
                        } else {
                            format!("{parent_path}.{name}{suffix}")
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

                        // For classes/structs/interfaces, recurse to extract nested methods.
                        // Try `body` first (TypeScript/Python classes), then `type` (Go type_spec),
                        // then fall back to traversing all children.
                        if matches!(
                            sk,
                            SymbolKind::Class | SymbolKind::Struct | SymbolKind::Interface
                        ) {
                            let body_node = child
                                .child_by_field_name("body")
                                .or_else(|| child.child_by_field_name("type"));
                            if let Some(body) = body_node {
                                extract_symbols_recursive(
                                    body,
                                    source,
                                    types,
                                    &path,
                                    &mut symbol.children,
                                );
                            } else {
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
    let Some(type_name_bytes) = source.get(type_node.byte_range()) else {
        return;
    };
    let Ok(type_name) = std::str::from_utf8(type_name_bytes) else {
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
    let mut name_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    if let Some(body) = node.child_by_field_name("body") {
        let mut body_cursor = body.walk();
        for item in body.named_children(&mut body_cursor) {
            if !types.function_kinds.contains(&item.kind()) {
                continue;
            }
            let Some(name_node) = item.child_by_field_name("name") else {
                continue;
            };
            let Some(method_name_bytes) = source.get(name_node.byte_range()) else {
                continue;
            };
            let Ok(method_name) = std::str::from_utf8(method_name_bytes) else {
                continue;
            };
            let method_name = method_name.trim().to_string();

            let count = name_counts.entry(method_name.clone()).or_insert(0);
            *count += 1;
            let suffix = if *count > 1 {
                format!("#{count}")
            } else {
                String::new()
            };

            let method_path = format!("{impl_path}.{method_name}{suffix}");
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

    let mut current_symbols = symbols;
    let mut result = None;

    for segment in &chain.segments {
        let target_idx = segment.overload_index.unwrap_or(1).saturating_sub(1) as usize;
        let mut match_count = 0;
        let mut found = None;

        for s in current_symbols {
            if s.name == segment.name {
                if match_count == target_idx {
                    found = Some(s);
                    break;
                }
                match_count += 1;
            }
        }

        let match_symbol = found?;
        result = Some(match_symbol);
        current_symbols = &match_symbol.children;
    }

    result
}

/// Computes string similarity to offer did-you-mean suggestions.
///
/// Borrows semantic paths directly from the symbol tree to avoid allocating
/// intermediate `String` values. Only the final `max_suggestions` results are
/// converted to owned `String`s.
#[must_use]
pub fn did_you_mean(
    symbols: &[ExtractedSymbol],
    chain: &SymbolChain,
    max_suggestions: usize,
) -> Vec<String> {
    // Collect &str references — no cloning here.
    fn collect_paths<'a>(syms: &'a [ExtractedSymbol], out: &mut Vec<&'a str>) {
        for s in syms {
            out.push(&s.semantic_path);
            collect_paths(&s.children, out);
        }
    }

    if chain.segments.is_empty() {
        return Vec::new();
    }

    let target = chain.to_string();

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
        let tree = AstParser::parse_source(
            std::path::Path::new("dummy.go"),
            SupportedLanguage::Go,
            source,
        )
        .unwrap();

        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Go);
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "Login");
        assert_eq!(syms[0].kind, SymbolKind::Function);
    }

    #[test]
    fn test_extract_go_interface() {
        let source =
            b"package main\n\ntype Storage interface {\n\tCreate() error\n\tGetByID(id string) (*User, error)\n}\n";
        let tree = AstParser::parse_source(
            std::path::Path::new("dummy.go"),
            SupportedLanguage::Go,
            source,
        )
        .unwrap();

        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Go);
        assert_eq!(
            syms.len(),
            1,
            "expected exactly one symbol (Storage interface)"
        );
        assert_eq!(syms[0].name, "Storage");
        assert_eq!(syms[0].kind, SymbolKind::Interface);
        assert_eq!(syms[0].semantic_path, "Storage");
        assert_eq!(syms[0].children.len(), 2, "methods must be extracted");
        assert_eq!(syms[0].children[0].name, "Create");
        assert_eq!(syms[0].children[1].name, "GetByID");
    }

    #[test]
    fn test_extract_go_struct() {
        let source = b"package main\n\ntype Lesson struct {\n\tID string\n\tTitle string\n}\n";
        let tree = AstParser::parse_source(
            std::path::Path::new("dummy.go"),
            SupportedLanguage::Go,
            source,
        )
        .unwrap();

        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Go);
        assert_eq!(syms.len(), 1, "expected exactly one symbol (Lesson struct)");
        assert_eq!(syms[0].name, "Lesson");
        assert_eq!(syms[0].kind, SymbolKind::Struct);
        assert_eq!(syms[0].semantic_path, "Lesson");
    }

    #[test]
    fn test_extract_go_type_alias() {
        let source = b"package main\n\ntype ID = string\n";
        let tree = AstParser::parse_source(
            std::path::Path::new("dummy.go"),
            SupportedLanguage::Go,
            source,
        )
        .unwrap();

        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Go);
        assert_eq!(syms.len(), 1, "expected exactly one symbol (ID type alias)");
        assert_eq!(syms[0].name, "ID");
        // Type aliases have no interface_type or struct_type body -> SymbolKind::Class
        assert_eq!(syms[0].kind, SymbolKind::Class);
    }

    #[test]
    fn test_extract_go_mixed_file() {
        let source = b"package main\n\ntype Storage interface {\n\tCreate() error\n}\n\ntype Lesson struct {\n\tID string\n}\n\nfunc NewStorage() Storage { return nil }\n";
        let tree = AstParser::parse_source(
            std::path::Path::new("dummy.go"),
            SupportedLanguage::Go,
            source,
        )
        .unwrap();

        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Go);
        assert_eq!(
            syms.len(),
            3,
            "expected Storage interface, Lesson struct, NewStorage func"
        );

        let iface = syms.iter().find(|s| s.name == "Storage").unwrap();
        assert_eq!(iface.kind, SymbolKind::Interface);

        let strct = syms.iter().find(|s| s.name == "Lesson").unwrap();
        assert_eq!(strct.kind, SymbolKind::Struct);

        let func = syms.iter().find(|s| s.name == "NewStorage").unwrap();
        assert_eq!(func.kind, SymbolKind::Function);
    }

    #[test]
    fn test_extract_ts_class_with_methods() {
        let source = b"class AuthService {\n  login() {}\n  logout() {}\n}";
        let tree = AstParser::parse_source(
            std::path::Path::new("dummy.ts"),
            SupportedLanguage::TypeScript,
            source,
        )
        .unwrap();

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
    fn test_extract_ts_exported_arrow_function() {
        let source = b"export const completeLesson = async () => {};\nconst someConst = 42;";
        let tree = AstParser::parse_source(
            std::path::Path::new("dummy.ts"),
            SupportedLanguage::TypeScript,
            source,
        )
        .unwrap();

        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::TypeScript);
        assert_eq!(syms.len(), 2);
        assert_eq!(syms[0].name, "completeLesson");
        assert_eq!(syms[0].kind, SymbolKind::Function);
        assert_eq!(syms[1].name, "someConst");
        assert_eq!(syms[1].kind, SymbolKind::Constant);
    }

    #[test]
    fn test_did_you_mean() {
        let source = b"class AuthService {\n  login() {}\n}";
        let tree = AstParser::parse_source(
            std::path::Path::new("dummy.ts"),
            SupportedLanguage::TypeScript,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::TypeScript);

        let chain = SymbolChain::parse("AuthService.logni").unwrap();
        let suggestions = did_you_mean(&syms, &chain, 3);
        assert_eq!(suggestions, vec!["AuthService.login"]);
    }

    #[test]
    fn test_find_enclosing_symbol() {
        let source = b"func A() {\n  // line 1 \n}\nfunc B() {}\n";
        let tree = AstParser::parse_source(
            std::path::Path::new("dummy.go"),
            SupportedLanguage::Go,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Go);

        let path = find_enclosing_symbol(&syms, 1).unwrap();
        assert_eq!(path, "A");
    }

    #[test]
    fn test_extract_rust_impl_methods() {
        let source = b"struct MyStruct;\nimpl MyStruct {\n    fn foo(&self) {}\n    fn bar(&mut self) {}\n}\n";
        let tree = AstParser::parse_source(
            std::path::Path::new("dummy.rs"),
            SupportedLanguage::Rust,
            source,
        )
        .unwrap();
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
        let tree = AstParser::parse_source(
            std::path::Path::new("dummy.rs"),
            SupportedLanguage::Rust,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Rust);

        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "compute");
        assert_eq!(syms[0].kind, SymbolKind::Function);
    }

    #[test]
    fn test_extract_overloads() {
        let source = b"class AuthService {\n  login() {}\n  login(user) {}\n}";
        let tree = AstParser::parse_source(
            std::path::Path::new("dummy.ts"),
            SupportedLanguage::TypeScript,
            source,
        )
        .unwrap();

        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::TypeScript);
        assert_eq!(syms.len(), 1);
        let class = &syms[0];
        assert_eq!(class.name, "AuthService");
        assert_eq!(class.children.len(), 2);

        assert_eq!(class.children[0].name, "login");
        assert_eq!(class.children[0].semantic_path, "AuthService.login");

        assert_eq!(class.children[1].name, "login");
        assert_eq!(class.children[1].semantic_path, "AuthService.login#2");
    }

    #[test]
    fn test_resolve_overloads() {
        let class = ExtractedSymbol {
            name: "AuthService".to_string(),
            semantic_path: "AuthService".to_string(),
            kind: SymbolKind::Class,
            byte_range: 0..20,
            start_line: 0,
            end_line: 1,
            children: vec![
                ExtractedSymbol {
                    name: "login".to_string(),
                    semantic_path: "AuthService.login".to_string(),
                    kind: SymbolKind::Method,
                    byte_range: 0..10,
                    start_line: 0,
                    end_line: 0,
                    children: vec![],
                },
                ExtractedSymbol {
                    name: "login".to_string(),
                    semantic_path: "AuthService.login#2".to_string(),
                    kind: SymbolKind::Method,
                    byte_range: 10..20,
                    start_line: 1,
                    end_line: 1,
                    children: vec![],
                },
            ],
        };

        let symbols = vec![class];

        // test #1
        let chain1 = SymbolChain::parse("AuthService.login").unwrap();
        let res1 = resolve_symbol_chain(&symbols, &chain1).unwrap();
        assert_eq!(res1.semantic_path, "AuthService.login");

        // test #2
        let chain2 = SymbolChain::parse("AuthService.login#2").unwrap();
        let res2 = resolve_symbol_chain(&symbols, &chain2).unwrap();
        assert_eq!(res2.semantic_path, "AuthService.login#2");

        // test out of bounds
        let chain3 = SymbolChain::parse("AuthService.login#3").unwrap();
        let res3 = resolve_symbol_chain(&symbols, &chain3);
        assert!(res3.is_none());
    }
}
