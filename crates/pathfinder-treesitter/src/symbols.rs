/// CSS style symbol extraction.
pub mod css;
/// JSX element symbol extraction.
pub mod jsx;
/// Vue Single-File Component symbol extraction.
pub mod vue;

pub use css::{extract_style_symbols, parse_css_selector_name};
pub use jsx::extract_jsx_children;
pub use vue::{extract_symbols_from_multizone, extract_template_symbols};

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
    let mut raw_symbols = Vec::with_capacity(64);
    let root = tree.root_node();
    let types = lang.node_types();

    // We start traversal at the root level without a parent path
    extract_symbols_recursive(root, source, types, lang, "", &mut raw_symbols);

    // Generic containment pass (TS-B3, TS-D2)
    let mut symbols = apply_containment_nesting(raw_symbols);

    // Language post_process hooks (TS-B3)
    if let Some(post_process) = lang.grammar_def().post_process {
        post_process(&mut symbols);
    }

    disambiguate_overloads_recursive("", &mut symbols);

    validate_symbol_invariants(&symbols);

    symbols
}

/// Generic containment pass (TS-B3, TS-D2).
///
/// Converts a list of symbols into a hierarchical tree based on byte range containment.
/// A symbol whose byte range is strictly inside another's becomes its child.
/// Handles Rust impls/traits/modules, Go methods/types, TS classes/interfaces/namespaces,
/// Java classes/interfaces/enums/records, and Python classes/nested functions uniformly.
#[must_use]
pub fn apply_containment_nesting(mut symbols: Vec<ExtractedSymbol>) -> Vec<ExtractedSymbol> {
    if symbols.is_empty() {
        return symbols;
    }

    // Sort symbols by byte_range.start ascending, then byte_range.end descending
    // (so outer/larger enclosing ranges come before inner/nested ranges).
    symbols.sort_by(|a, b| {
        a.byte_range
            .start
            .cmp(&b.byte_range.start)
            .then_with(|| b.byte_range.end.cmp(&a.byte_range.end))
    });

    let mut roots: Vec<ExtractedSymbol> = Vec::with_capacity(symbols.len());
    let mut stack: Vec<ExtractedSymbol> = Vec::new();

    for sym in symbols {
        // While the stack top does not strictly enclose sym, pop and attach to its parent (or root)
        while let Some(top) = stack.last() {
            let top_encloses = top.byte_range.start <= sym.byte_range.start
                && sym.byte_range.end <= top.byte_range.end
                && top.byte_range != sym.byte_range;
            if top_encloses {
                break;
            }
            let popped = stack.pop().unwrap_or_default();
            if let Some(parent) = stack.last_mut() {
                parent.children.push(popped);
            } else {
                roots.push(popped);
            }
        }
        stack.push(sym);
    }

    while let Some(popped) = stack.pop() {
        if let Some(parent) = stack.last_mut() {
            parent.children.push(popped);
        } else {
            roots.push(popped);
        }
    }

    // Recursively apply containment nesting on any existing children (unless JSX or Zone)
    for r in &mut roots {
        if !r.children.is_empty()
            && !r.children.iter().any(|c| {
                matches!(
                    c.kind,
                    SymbolKind::HtmlElement | SymbolKind::Component | SymbolKind::Zone
                )
            })
        {
            let ch = std::mem::take(&mut r.children);
            r.children = apply_containment_nesting(ch);
        }
    }

    roots
}

/// Compute the expanded attached range for a symbol including preceding decorators,
/// annotations, attributes, doc comments, and parent export prefix (TS-C1).
#[must_use]
pub fn compute_attached_range(
    node: Node<'_>,
    _source: &[u8],
    lang: SupportedLanguage,
) -> std::ops::Range<usize> {
    let base_range = node.byte_range();
    let mut start = base_range.start;
    let end = base_range.end;

    match lang {
        SupportedLanguage::Rust => {
            let mut prev = node.prev_sibling();
            while let Some(p) = prev {
                let kind = p.kind();
                if kind == "attribute_item"
                    || kind == "inner_attribute_item"
                    || kind == "line_comment"
                    || kind == "block_comment"
                {
                    start = p.byte_range().start;
                    prev = p.prev_sibling();
                } else if p.is_extra() {
                    prev = p.prev_sibling();
                } else {
                    break;
                }
            }
        }
        SupportedLanguage::TypeScript
        | SupportedLanguage::Tsx
        | SupportedLanguage::JavaScript
        | SupportedLanguage::Vue => {
            let check_node = if let Some(parent) = node.parent() {
                if parent.kind() == "export_statement" {
                    start = parent.byte_range().start.min(start);
                    parent
                } else {
                    node
                }
            } else {
                node
            };
            let mut prev = check_node.prev_sibling();
            while let Some(p) = prev {
                let kind = p.kind();
                if kind == "decorator" || kind == "comment" {
                    start = p.byte_range().start;
                    prev = p.prev_sibling();
                } else if p.is_extra() {
                    prev = p.prev_sibling();
                } else {
                    break;
                }
            }
        }
        SupportedLanguage::Python => {
            if let Some(parent) = node.parent() {
                if parent.kind() == "decorated_definition" {
                    start = parent.byte_range().start.min(start);
                }
            }
            let mut prev = node.prev_sibling();
            while let Some(p) = prev {
                if p.kind() == "comment" {
                    start = p.byte_range().start;
                    prev = p.prev_sibling();
                } else {
                    break;
                }
            }
        }
        SupportedLanguage::Java => {
            let mut prev = node.prev_sibling();
            while let Some(p) = prev {
                if p.kind() == "block_comment" || p.kind() == "line_comment" {
                    start = p.byte_range().start;
                    prev = p.prev_sibling();
                } else {
                    break;
                }
            }
        }
        SupportedLanguage::Go => {
            let mut prev = node.prev_sibling();
            while let Some(p) = prev {
                if p.kind() == "comment" {
                    start = p.byte_range().start;
                    prev = p.prev_sibling();
                } else {
                    break;
                }
            }
        }
    }

    start..end
}

/// Post-process hook for Rust symbol extraction (TS-B3).
pub fn post_process_rust(symbols: &mut Vec<ExtractedSymbol>) {
    merge_rust_impl_blocks(symbols);
}

/// Post-process hook for Go symbol extraction (TS-A6, TS-B3).
pub fn post_process_go(symbols: &mut Vec<ExtractedSymbol>) {
    // 1. Identify declared top-level types (Struct, Interface, Class)
    let declared_types: std::collections::HashSet<String> = symbols
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Struct | SymbolKind::Interface | SymbolKind::Class
            )
        })
        .map(|s| s.name.clone())
        .collect();

    // 2. Identify methods with receiver types
    let mut receiver_methods: std::collections::HashMap<String, Vec<ExtractedSymbol>> =
        std::collections::HashMap::new();

    symbols.retain_mut(|s| {
        if s.kind == SymbolKind::Method {
            if let Some((recv, method_name)) = s.semantic_path.split_once('.') {
                let clean_recv = recv.split('#').next().unwrap_or(recv);
                if declared_types.contains(clean_recv) {
                    s.name = method_name
                        .split('#')
                        .next()
                        .unwrap_or(method_name)
                        .to_string();
                    receiver_methods
                        .entry(clean_recv.to_string())
                        .or_default()
                        .push(s.clone());
                    return false;
                }
            }
        }
        true
    });

    // 3. Attach receiver methods to matching types
    for s in symbols.iter_mut() {
        if matches!(
            s.kind,
            SymbolKind::Struct | SymbolKind::Interface | SymbolKind::Class
        ) {
            if let Some(methods) = receiver_methods.remove(&s.name) {
                s.children.extend(methods);
            }
        }
    }
}

/// Post-process hook for Java symbol extraction (TS-B3).
#[allow(clippy::ptr_arg)]
pub fn post_process_java(symbols: &mut Vec<ExtractedSymbol>) {
    for s in symbols.iter_mut() {
        if !s.children.is_empty() {
            post_process_java(&mut s.children);
        }
    }
}

/// Post-process hook for TypeScript symbol extraction (TS-B3).
#[allow(clippy::ptr_arg)]
pub fn post_process_typescript(symbols: &mut Vec<ExtractedSymbol>) {
    for s in symbols.iter_mut() {
        if !s.children.is_empty() {
            post_process_typescript(&mut s.children);
        }
    }
}

/// Post-process hook for JavaScript symbol extraction (TS-B3).
#[allow(clippy::ptr_arg)]
pub fn post_process_javascript(symbols: &mut Vec<ExtractedSymbol>) {
    for s in symbols.iter_mut() {
        if !s.children.is_empty() {
            post_process_javascript(&mut s.children);
        }
    }
}

/// Post-process hook for Python symbol extraction (TS-B3).
#[allow(clippy::ptr_arg)]
pub fn post_process_python(symbols: &mut Vec<ExtractedSymbol>) {
    for s in symbols.iter_mut() {
        if !s.children.is_empty() {
            post_process_python(&mut s.children);
        }
    }
}

/// Context for symbol extraction, bundling shared parameters.
struct SymbolExtractionContext<'a> {
    node: Node<'a>,
    source: &'a [u8],
    types: &'a crate::language::LanguageNodeTypes,
    lang: SupportedLanguage,
    parent_path: &'a str,
    out: &'a mut Vec<ExtractedSymbol>,
    name_counts: std::collections::HashMap<String, usize>,
}

impl<'a> SymbolExtractionContext<'a> {
    fn process_children(&mut self) {
        let mut cursor = self.node.walk();
        for child in self.node.named_children(&mut cursor) {
            self.process_child(child);
        }
    }

    fn process_child(&mut self, child: Node<'a>) {
        let kind = child.kind();

        if self.types.impl_kinds.contains(&kind) {
            extract_impl_block(
                child,
                self.source,
                self.types,
                self.parent_path,
                self.out,
                &mut self.name_counts,
            );
            return;
        }

        if self.types.module_kinds.contains(&kind) {
            self.extract_module_block(child);
            return;
        }

        // Go const_declaration and var_declaration handling (TS-A6)
        if self.lang == SupportedLanguage::Go
            && (kind == "const_declaration" || kind == "var_declaration")
        {
            extract_go_const_var_specs(
                child,
                self.source,
                self.lang,
                self.parent_path,
                self.out,
                &mut self.name_counts,
            );
            return;
        }

        // Python module/class-level simple assignment (`name = value`), e.g.
        // package-level constants. Deliberately narrow: only a plain
        // `identifier` left-hand side is handled (not tuple/list unpacking,
        // not augmented/annotated assignment) — extending this is a
        // separate, larger change.
        if self.lang == SupportedLanguage::Python && kind == "expression_statement" {
            if let Some(sym) = extract_python_simple_assignment(
                child,
                self.source,
                self.lang,
                self.parent_path,
                &mut self.name_counts,
            ) {
                self.out.push(sym);
                return;
            }
        }

        let sym_kind = self.determine_symbol_kind(child, kind);
        if let Some(sk) = sym_kind {
            if let Some(name_node) = Self::resolve_name_node(child) {
                if let Some(name) = self.extract_name(name_node) {
                    self.extract_symbol(child, name, sk);
                    return;
                }
            }
        }

        // Recurse for unrecognized symbols or failed extraction
        extract_symbols_recursive(
            child,
            self.source,
            self.types,
            self.lang,
            self.parent_path,
            self.out,
        );
    }

    fn determine_symbol_kind(&self, node: Node<'a>, kind: &str) -> Option<SymbolKind> {
        if self.types.function_kinds.contains(&kind) {
            let func_name = Self::resolve_name_node(node)
                .and_then(|n| self.source.get(n.byte_range()))
                .and_then(|b| std::str::from_utf8(b).ok())
                .map(str::trim);
            if is_test_function(node, self.lang, self.source, func_name) {
                return Some(SymbolKind::Test);
            }
            return Some(SymbolKind::Function);
        }

        if self.types.class_kinds.contains(&kind) {
            return Some(refine_class_kind(node, self.source));
        }

        if self.types.method_kinds.contains(&kind) {
            let func_name = Self::resolve_name_node(node)
                .and_then(|n| self.source.get(n.byte_range()))
                .and_then(|b| std::str::from_utf8(b).ok())
                .map(str::trim);
            if is_test_function(node, self.lang, self.source, func_name) {
                return Some(SymbolKind::Test);
            }
            return Some(SymbolKind::Method);
        }

        if self.types.constant_kinds.contains(&kind) {
            return Some(refine_constant_kind(node, kind));
        }

        None
    }

    fn resolve_name_node(child: Node<'a>) -> Option<Node<'a>> {
        child
            .child_by_field_name("name")
            .or_else(|| child.child_by_field_name("identifier"))
            .or_else(|| {
                child
                    .child_by_field_name("definition")
                    .and_then(|d| d.child_by_field_name("name"))
            })
            .or_else(|| find_variable_declarator_name(child))
    }

    fn extract_name(&self, name_node: Node<'a>) -> Option<String> {
        let name_bytes = self.source.get(name_node.byte_range())?;
        std::str::from_utf8(name_bytes)
            .ok()
            .map(str::trim)
            .map(String::from)
    }

    fn extract_symbol(&mut self, child: Node<'a>, name: String, sk: SymbolKind) {
        // Go method receiver handling (TS-A6)
        let go_receiver =
            if self.lang == SupportedLanguage::Go && child.kind() == "method_declaration" {
                extract_go_receiver_type_name(child, self.source)
            } else {
                None
            };

        let (unique_name, suffix) = make_unique_name(&mut self.name_counts, name);
        let path = if let Some(ref recv) = go_receiver {
            if self.parent_path.is_empty() {
                format!("{recv}.{unique_name}{suffix}")
            } else {
                format!("{}.{recv}.{unique_name}{suffix}", self.parent_path)
            }
        } else {
            self.build_path(&unique_name, &suffix)
        };

        let (name_point, name_column) = if let Some(name_node) = Self::resolve_name_node(child) {
            let pt = name_node.start_position();
            debug_assert!(
                pt.row >= child.start_position().row,
                "name_node row ({}) is before child start row ({}) for {}",
                pt.row,
                child.start_position().row,
                unique_name
            );
            (pt, pt.column)
        } else {
            let pt = child.start_position();
            (pt, pt.column)
        };

        let access_level = detect_access_level(child, self.lang, self.source);
        let byte_range = child.byte_range();
        let attached_range = compute_attached_range(child, self.source, self.lang);
        let mut symbol = ExtractedSymbol {
            name: unique_name,
            semantic_path: path.clone(),
            kind: sk,
            byte_range,
            attached_range,
            start_line: child.start_position().row,
            end_line: child.end_position().row,
            name_column,
            name_point,
            access_level,
            children: Vec::default(),
        };

        if matches!(
            sk,
            SymbolKind::Class | SymbolKind::Struct | SymbolKind::Interface | SymbolKind::Enum
        ) {
            self.extract_nested_symbols(child, &path, &mut symbol.children);
        }

        if matches!(sk, SymbolKind::Function | SymbolKind::Method)
            && matches!(
                self.lang,
                SupportedLanguage::Python
                    | SupportedLanguage::JavaScript
                    | SupportedLanguage::TypeScript
                    | SupportedLanguage::Tsx
                    | SupportedLanguage::Vue
            )
        {
            self.extract_nested_symbols(child, &path, &mut symbol.children);
        }

        if matches!(sk, SymbolKind::Function)
            && matches!(
                self.lang,
                SupportedLanguage::Tsx | SupportedLanguage::JavaScript
            )
        {
            crate::symbols::jsx::extract_jsx_children(
                child,
                self.source,
                &path,
                &mut symbol.children,
            );
        }

        self.out.push(symbol);
    }

    fn extract_module_block(&mut self, child: Node<'a>) {
        let Some(name_node) = child.child_by_field_name("name") else {
            extract_symbols_recursive(
                child,
                self.source,
                self.types,
                self.lang,
                self.parent_path,
                self.out,
            );
            return;
        };

        let Some(name) = self.extract_name(name_node) else {
            return;
        };

        let (unique_name, suffix) = make_unique_name(&mut self.name_counts, name);
        let module_path = self.build_path(&unique_name, &suffix);

        let mut children = Vec::default();

        if let Some(body) = child.child_by_field_name("body") {
            extract_symbols_recursive(
                body,
                self.source,
                self.types,
                self.lang,
                &module_path,
                &mut children,
            );
        }

        let name_point = name_node.start_position();
        debug_assert!(
            name_point.row >= child.start_position().row,
            "name_node row ({}) is before module start row ({}) for {}",
            name_point.row,
            child.start_position().row,
            unique_name
        );
        let name_column = name_point.column;
        let access_level = detect_access_level(child, self.lang, self.source);
        let byte_range = child.byte_range();
        let attached_range = compute_attached_range(child, self.source, self.lang);
        self.out.push(ExtractedSymbol {
            name: unique_name,
            semantic_path: module_path,
            kind: SymbolKind::Module,
            byte_range,
            attached_range,
            start_line: child.start_position().row,
            end_line: child.end_position().row,
            name_column,
            name_point,
            access_level,
            children,
        });
    }

    fn build_path(&self, name: &str, suffix: &str) -> String {
        if self.parent_path.is_empty() {
            let mut s = String::with_capacity(name.len() + suffix.len());
            s.push_str(name);
            s.push_str(suffix);
            s
        } else {
            let cap = self.parent_path.len() + 1 + name.len() + suffix.len();
            let mut s = String::with_capacity(cap);
            s.push_str(self.parent_path);
            s.push('.');
            s.push_str(name);
            s.push_str(suffix);
            s
        }
    }

    fn extract_nested_symbols(
        &self,
        child: Node<'a>,
        path: &str,
        children_out: &mut Vec<ExtractedSymbol>,
    ) {
        let body_node = child
            .child_by_field_name("body")
            .or_else(|| child.child_by_field_name("type"));

        if let Some(body) = body_node {
            extract_symbols_recursive(body, self.source, self.types, self.lang, path, children_out);
        } else {
            extract_symbols_recursive(
                child,
                self.source,
                self.types,
                self.lang,
                path,
                children_out,
            );
        }
    }
}

/// Extract Go receiver type name (e.g. `*Server` or `Server` → `Server`) (TS-A6).
fn extract_go_receiver_type_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    let receiver = if let Some(r) = node.child_by_field_name("receiver") {
        r
    } else {
        let mut c = node.walk();
        let mut found = None;
        for n in node.children(&mut c) {
            if n.kind() == "parameter_list" {
                found = Some(n);
                break;
            }
        }
        found?
    };

    let mut cursor = receiver.walk();
    for child in receiver.named_children(&mut cursor) {
        if child.kind() == "parameter_declaration" {
            let mut target_node = child.child_by_field_name("type");
            if target_node.is_none() {
                let mut pcursor = child.walk();
                for n in child.named_children(&mut pcursor) {
                    if n.kind() == "pointer_type"
                        || n.kind() == "type_identifier"
                        || n.kind() == "qualified_type"
                    {
                        target_node = Some(n);
                        break;
                    }
                }
            }
            if let Some(type_node) = target_node {
                if let Some(bytes) = source.get(type_node.byte_range()) {
                    if let Ok(s) = std::str::from_utf8(bytes) {
                        let trimmed = s.trim().trim_start_matches('*').trim();
                        if !trimmed.is_empty() {
                            return Some(trimmed.to_string());
                        }
                    }
                }
            }
            if let Some(bytes) = source.get(child.byte_range()) {
                if let Ok(s) = std::str::from_utf8(bytes) {
                    if let Some(last_word) = s.split_whitespace().last() {
                        let trimmed = last_word.trim_start_matches('*').trim();
                        if !trimmed.is_empty() {
                            return Some(trimmed.to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

/// Extract Go package-level constants and variables from `const_declaration` / `var_declaration` (TS-A6).
fn extract_go_const_var_specs<'a>(
    node: Node<'a>,
    source: &'a [u8],
    lang: SupportedLanguage,
    parent_path: &'a str,
    out: &'a mut Vec<ExtractedSymbol>,
    name_counts: &mut std::collections::HashMap<String, usize>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "const_spec" || child.kind() == "var_spec" {
            extract_single_go_spec(child, node, source, lang, parent_path, out, name_counts);
        } else if child.kind() == "const_spec_list" || child.kind() == "var_spec_list" {
            let mut list_cursor = child.walk();
            for spec in child.named_children(&mut list_cursor) {
                if spec.kind() == "const_spec" || spec.kind() == "var_spec" {
                    extract_single_go_spec(spec, node, source, lang, parent_path, out, name_counts);
                }
            }
        }
    }
}

fn extract_single_go_spec<'a>(
    spec: Node<'a>,
    parent_decl: Node<'a>,
    source: &'a [u8],
    lang: SupportedLanguage,
    parent_path: &'a str,
    out: &'a mut Vec<ExtractedSymbol>,
    name_counts: &mut std::collections::HashMap<String, usize>,
) {
    let mut spec_cursor = spec.walk();
    for name_node in spec.named_children(&mut spec_cursor) {
        if name_node.kind() == "identifier" {
            if let Some(name_bytes) = source.get(name_node.byte_range()) {
                if let Ok(name) = std::str::from_utf8(name_bytes) {
                    let name = name.trim().to_string();
                    if name.is_empty() {
                        continue;
                    }
                    let (unique_name, suffix) = make_unique_name(name_counts, name);
                    let path = if parent_path.is_empty() {
                        format!("{unique_name}{suffix}")
                    } else {
                        format!("{parent_path}.{unique_name}{suffix}")
                    };
                    let name_point = name_node.start_position();
                    let name_column = name_point.column;
                    let access_level = detect_go_access_level(name_node, source);
                    let byte_range = spec.byte_range();
                    let attached_range = compute_attached_range(parent_decl, source, lang);
                    out.push(ExtractedSymbol {
                        name: unique_name,
                        semantic_path: path,
                        kind: SymbolKind::Constant,
                        byte_range,
                        attached_range,
                        start_line: spec.start_position().row,
                        end_line: spec.end_position().row,
                        name_column,
                        name_point,
                        access_level,
                        children: Vec::default(),
                    });
                }
            }
        }
    }
}

/// Extract a Python module/class-level simple assignment (`name = value`) as
/// a `Constant` symbol. Returns `None` (and extracts nothing) for anything
/// other than a single plain-`identifier`-target assignment — tuple/list
/// unpacking (`a, b = ...`), augmented assignment (`x += 1`), annotated
/// assignment without special-casing, and non-assignment expression
/// statements (docstrings, bare calls) are all deliberately left unextracted
/// rather than guessed at.
fn extract_python_simple_assignment<'a>(
    expr_stmt: Node<'a>,
    source: &'a [u8],
    lang: SupportedLanguage,
    parent_path: &'a str,
    name_counts: &mut std::collections::HashMap<String, usize>,
) -> Option<ExtractedSymbol> {
    let mut cursor = expr_stmt.walk();
    let mut named_children = expr_stmt.named_children(&mut cursor);
    let assignment = named_children.next()?;
    if named_children.next().is_some() || assignment.kind() != "assignment" {
        return None;
    }

    let name_node = assignment.child_by_field_name("left")?;
    if name_node.kind() != "identifier" {
        return None;
    }
    let name_bytes = source.get(name_node.byte_range())?;
    let name = std::str::from_utf8(name_bytes).ok()?.trim().to_string();
    if name.is_empty() {
        return None;
    }

    // Python's leading-underscore convention (PEP 8), applied directly since
    // `detect_python_access_level` expects a def/class-shaped node with a
    // `name` field, not a bare identifier. Mirrors that function's nesting
    // exactly: dunder names (`__x__`) are Public, `__x` is Private, and a
    // lone leading `_x` is Protected.
    let access_level = if name.starts_with("__") {
        if name.ends_with("__") {
            crate::surgeon::AccessLevel::Public
        } else {
            crate::surgeon::AccessLevel::Private
        }
    } else if name.starts_with('_') {
        crate::surgeon::AccessLevel::Protected
    } else {
        crate::surgeon::AccessLevel::Public
    };

    let (unique_name, suffix) = make_unique_name(name_counts, name);
    let path = if parent_path.is_empty() {
        format!("{unique_name}{suffix}")
    } else {
        format!("{parent_path}.{unique_name}{suffix}")
    };
    let name_point = name_node.start_position();
    let attached_range = compute_attached_range(expr_stmt, source, lang);
    Some(ExtractedSymbol {
        name: unique_name,
        semantic_path: path,
        kind: SymbolKind::Constant,
        byte_range: expr_stmt.byte_range(),
        attached_range,
        start_line: expr_stmt.start_position().row,
        end_line: expr_stmt.end_position().row,
        name_column: name_point.column,
        name_point,
        access_level,
        children: Vec::default(),
    })
}

/// Core recursive extraction function (delegates to `SymbolExtractionContext`).
fn extract_symbols_recursive(
    node: Node,
    source: &[u8],
    types: &crate::language::LanguageNodeTypes,
    lang: SupportedLanguage,
    parent_path: &str,
    out: &mut Vec<ExtractedSymbol>,
) {
    let mut ctx = SymbolExtractionContext {
        node,
        source,
        types,
        lang,
        parent_path,
        out,
        name_counts: std::collections::HashMap::with_capacity(4),
    };
    ctx.process_children();
}

/// Refine `class_kinds` match to precise kind.
fn refine_class_kind(node: Node, source: &[u8]) -> SymbolKind {
    match node.kind() {
        "enum_declaration" | "enum_item" => SymbolKind::Enum,
        "struct_item" | "record_declaration" | "union_item" => SymbolKind::Struct,
        "trait_item" | "interface_declaration" | "annotation_type_declaration" => {
            SymbolKind::Interface
        }
        "class_definition" => {
            if let Some(superclasses) = node.child_by_field_name("superclasses") {
                if let Some(bytes) = source.get(superclasses.byte_range()) {
                    let text = String::from_utf8_lossy(bytes);
                    if text.contains("ABC") || text.contains("Protocol") {
                        return SymbolKind::Interface;
                    }
                }
            }
            SymbolKind::Class
        }
        _ => {
            node.child_by_field_name("type")
                .map_or(SymbolKind::Class, |type_node| match type_node.kind() {
                    "interface_type" => SymbolKind::Interface,
                    "struct_type" => SymbolKind::Struct,
                    _ => SymbolKind::Class,
                })
        }
    }
}

/// Refine constant kind to detect arrow functions in JS/TS.
fn refine_constant_kind(node: Node, kind: &str) -> SymbolKind {
    if (kind == "lexical_declaration" || kind == "variable_declaration")
        && has_arrow_function_value(node)
    {
        SymbolKind::Function
    } else {
        SymbolKind::Constant
    }
}

/// Check if a variable declaration contains an arrow function or function expression.
fn has_arrow_function_value(node: Node) -> bool {
    let mut cursor = node.walk();
    for decl in node.named_children(&mut cursor) {
        if decl.kind() == "variable_declarator" {
            if let Some(val) = decl.child_by_field_name("value") {
                if val.kind() == "arrow_function" || val.kind() == "function_expression" {
                    return true;
                }
            }
        }
    }
    false
}

/// Find the name node within a `variable_declarator` child.
fn find_variable_declarator_name(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    for n in node.named_children(&mut cursor) {
        if n.kind() == "variable_declarator" {
            if let Some(name) = n.child_by_field_name("name") {
                return Some(name);
            }
        }
    }
    None
}

/// Detect the access level of a tree-sitter node based on language-specific rules.
fn detect_access_level(
    node: Node,
    lang: SupportedLanguage,
    source: &[u8],
) -> crate::surgeon::AccessLevel {
    match lang {
        SupportedLanguage::Rust => detect_rust_access_level(node),
        SupportedLanguage::Go => detect_go_access_level(node, source),
        SupportedLanguage::TypeScript
        | SupportedLanguage::Tsx
        | SupportedLanguage::JavaScript
        | SupportedLanguage::Vue => detect_ts_access_level(node, source),
        SupportedLanguage::Python => detect_python_access_level(node, source),
        SupportedLanguage::Java => detect_java_access_level(node),
    }
}

/// Rust visibility rules based on `visibility_modifier` AST child.
#[must_use]
pub fn detect_rust_access_level(node: Node) -> crate::surgeon::AccessLevel {
    use crate::surgeon::AccessLevel;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "visibility_modifier" {
            let mut inner = child.walk();
            let mut has_paren_content = false;
            let mut is_crate = false;
            let mut is_super = false;
            for tok in child.named_children(&mut inner) {
                has_paren_content = true;
                if tok.kind() == "crate" {
                    is_crate = true;
                } else if tok.kind() == "super" {
                    is_super = true;
                }
            }
            if has_paren_content && is_crate {
                return AccessLevel::Package;
            }
            if has_paren_content && is_super {
                return AccessLevel::Protected;
            }
            return AccessLevel::Public;
        }
    }
    AccessLevel::Private
}

/// Go visibility rules based on name convention.
#[must_use]
pub fn detect_go_access_level(node: Node, source: &[u8]) -> crate::surgeon::AccessLevel {
    use crate::surgeon::AccessLevel;
    let name_node = node
        .child_by_field_name("name")
        .or_else(|| node.child_by_field_name("identifier"))
        .unwrap_or(node);

    if let Some(bytes) = source.get(name_node.byte_range()) {
        if let Ok(name) = std::str::from_utf8(bytes) {
            let name = name.trim();
            if name.starts_with('_') {
                return AccessLevel::Private;
            }
            if name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                return AccessLevel::Public;
            }
            return AccessLevel::Package;
        }
    }
    AccessLevel::Package
}

/// TypeScript/JavaScript/Vue visibility rules.
#[must_use]
pub fn detect_ts_access_level(node: Node, source: &[u8]) -> crate::surgeon::AccessLevel {
    use crate::surgeon::AccessLevel;
    let mut current = node.parent();
    while let Some(p) = current {
        if p.kind() == "export_statement" {
            return AccessLevel::Public;
        }
        if p.kind() == "program" || p.kind() == "statement_block" {
            break;
        }
        current = p.parent();
    }

    let name_node = node
        .child_by_field_name("name")
        .or_else(|| node.child_by_field_name("identifier"));

    if let Some(nn) = name_node {
        if let Some(bytes) = source.get(nn.byte_range()) {
            if let Ok(name) = std::str::from_utf8(bytes) {
                if name.trim().starts_with('_') {
                    return AccessLevel::Private;
                }
            }
        }
    }

    AccessLevel::Package
}

/// Python visibility rules based on name-prefix convention.
#[must_use]
pub fn detect_python_access_level(node: Node, source: &[u8]) -> crate::surgeon::AccessLevel {
    use crate::surgeon::AccessLevel;
    let name_node = node
        .child_by_field_name("name")
        .or_else(|| node.child_by_field_name("identifier"));

    if let Some(nn) = name_node {
        if let Some(bytes) = source.get(nn.byte_range()) {
            if let Ok(name) = std::str::from_utf8(bytes) {
                let name = name.trim();
                if name.starts_with("__") {
                    if name.ends_with("__") {
                        return AccessLevel::Public;
                    }
                    return AccessLevel::Private;
                }
                if name.starts_with('_') {
                    return AccessLevel::Protected;
                }
            }
        }
    }
    AccessLevel::Public
}

/// Java visibility rules based on `modifiers` AST child containing explicit keywords.
#[must_use]
pub fn detect_java_access_level(node: Node) -> crate::surgeon::AccessLevel {
    use crate::surgeon::AccessLevel;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "modifiers" {
            let mut mod_cursor = child.walk();
            for modifier in child.children(&mut mod_cursor) {
                match modifier.kind() {
                    "public" => return AccessLevel::Public,
                    "protected" => return AccessLevel::Protected,
                    "private" => return AccessLevel::Private,
                    _ => {}
                }
            }
        }
    }
    AccessLevel::Package
}

/// Detect if a Rust function is a test function (TS-A8).
///
/// Handles preceding sibling `attribute_item` / `inner_attribute_item` correctly,
/// supporting `#[test]`, `#[tokio::test]`, `#[rstest]`, `#[test_case]`, etc.
#[must_use]
pub fn is_rust_test(node: Node<'_>, source: &[u8], func_name: Option<&str>) -> bool {
    let mut prev = node.prev_named_sibling();
    while let Some(sibling) = prev {
        if sibling.kind() == "attribute_item" || sibling.kind() == "inner_attribute_item" {
            if let Some(attr_text) = source.get(sibling.byte_range()) {
                let attr = String::from_utf8_lossy(attr_text);
                if attr.contains("#[test]")
                    || attr.contains("#[tokio::test")
                    || attr.contains("#[rstest]")
                    || attr.contains("#[test_case")
                    || attr.contains("#[wasm_bindgen_test")
                    || attr.contains("#[async_std::test")
                {
                    return true;
                }
            }
            prev = sibling.prev_named_sibling();
        } else {
            break;
        }
    }
    if let Some(name) = func_name {
        name.starts_with("test_") || name.ends_with("_test")
    } else {
        false
    }
}

/// Detect if a Python function or method is a test function.
#[must_use]
pub fn is_python_test(node: Node<'_>, source: &[u8], func_name: Option<&str>) -> bool {
    let mut check_node = node;
    loop {
        if let Some(node_bytes) = source.get(check_node.byte_range()) {
            let node_text = String::from_utf8_lossy(node_bytes);
            if node_text.contains("@pytest")
                || node_text.contains("unittest")
                || node_text.contains("@given")
            {
                return true;
            }
        }
        if check_node.kind() == "decorated_definition" {
            break;
        }
        match check_node.parent() {
            Some(parent) if parent.kind() == "decorated_definition" => {
                check_node = parent;
            }
            _ => break,
        }
    }
    if let Some(name) = func_name {
        name.starts_with("test_")
    } else {
        false
    }
}

/// Detect if a Java method is a test method.
#[must_use]
pub fn is_java_test(node: Node<'_>, source: &[u8], func_name: Option<&str>) -> bool {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "modifiers" {
            if let Some(mod_text) = source.get(child.byte_range()) {
                let modifiers = String::from_utf8_lossy(mod_text);
                if modifiers.contains("@Test")
                    || modifiers.contains("@org.junit.Test")
                    || modifiers.contains("@ParameterizedTest")
                {
                    return true;
                }
            }
        }
    }
    if let Some(name) = func_name {
        name == "test"
            || name.starts_with("test_")
            || (name.starts_with("test") && name.chars().nth(4).is_some_and(char::is_uppercase))
    } else {
        false
    }
}

/// Detect if a function node is a test function using language-specific rules.
#[must_use]
pub fn is_test_function(
    node: Node<'_>,
    lang: SupportedLanguage,
    source: &[u8],
    func_name: Option<&str>,
) -> bool {
    match lang {
        SupportedLanguage::Rust => is_rust_test(node, source, func_name),
        SupportedLanguage::Python => is_python_test(node, source, func_name),
        SupportedLanguage::TypeScript
        | SupportedLanguage::Tsx
        | SupportedLanguage::JavaScript
        | SupportedLanguage::Vue => {
            if let Some(name) = func_name {
                name.starts_with("test_")
                    || name.starts_with("it_")
                    || name.ends_with("_test")
                    || name == "test"
                    || name == "it"
                    || name == "describe"
            } else {
                false
            }
        }
        SupportedLanguage::Go => {
            if let Some(name) = func_name {
                name.starts_with("Test")
            } else {
                false
            }
        }
        SupportedLanguage::Java => is_java_test(node, source, func_name),
    }
}

/// Generate unique name with suffix for duplicate symbols.
pub fn make_unique_name<S: std::hash::BuildHasher>(
    name_counts: &mut std::collections::HashMap<String, usize, S>,
    name: String,
) -> (String, String) {
    let count = name_counts.entry(name.clone()).or_insert(0);
    *count += 1;
    let suffix = if *count > 1 {
        format!("#{count}")
    } else {
        String::default()
    };
    (name, suffix)
}

fn strip_path_prefix(type_name: &str) -> &str {
    match type_name.rfind("::") {
        Some(idx) => &type_name[idx + 2..],
        None => type_name,
    }
}

fn strip_generics(type_name: &str) -> &str {
    match type_name.find('<') {
        Some(idx) => type_name[..idx].trim_end(),
        None => type_name.trim(),
    }
}

/// Extract methods from a Rust `impl_item` node.
fn extract_impl_block(
    node: Node,
    source: &[u8],
    types: &crate::language::LanguageNodeTypes,
    parent_path: &str,
    out: &mut Vec<ExtractedSymbol>,
    name_counts: &mut std::collections::HashMap<String, usize>,
) {
    let Some(type_node) = node.child_by_field_name("type") else {
        return;
    };
    let Some(type_name_bytes) = source.get(type_node.byte_range()) else {
        return;
    };
    let Ok(type_name) = std::str::from_utf8(type_name_bytes) else {
        return;
    };
    let type_name = strip_path_prefix(strip_generics(type_name)).to_string();

    let (unique_name, suffix) = make_unique_name(name_counts, type_name);
    let impl_path = if parent_path.is_empty() {
        format!("{unique_name}{suffix}")
    } else {
        format!("{parent_path}.{unique_name}{suffix}")
    };

    let mut methods: Vec<ExtractedSymbol> = Vec::with_capacity(8);
    let mut method_name_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::with_capacity(8);
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

            let (unique_method_name, method_suffix) =
                make_unique_name(&mut method_name_counts, method_name);
            let method_path = format!("{impl_path}.{unique_method_name}{method_suffix}");
            let method_name_point = name_node.start_position();
            debug_assert!(
                method_name_point.row >= item.start_position().row,
                "name_node row ({}) is before method start row ({}) for {}",
                method_name_point.row,
                item.start_position().row,
                unique_method_name
            );
            let method_name_column = method_name_point.column;
            // TS-A8: Call detect_rust_access_level on impl methods
            let access_level = detect_rust_access_level(item);
            let byte_range = item.byte_range();
            let attached_range = compute_attached_range(item, source, SupportedLanguage::Rust);
            let method_kind = if is_rust_test(item, source, Some(&unique_method_name)) {
                SymbolKind::Test
            } else {
                SymbolKind::Method
            };
            methods.push(ExtractedSymbol {
                name: unique_method_name,
                semantic_path: method_path,
                kind: method_kind,
                byte_range: byte_range.clone(),
                attached_range,
                start_line: item.start_position().row,
                end_line: item.end_position().row,
                name_column: method_name_column,
                name_point: method_name_point,
                access_level,
                children: Vec::default(),
            });
        }
    }

    if !methods.is_empty() {
        let impl_name_point = type_node.start_position();
        debug_assert!(
            impl_name_point.row >= node.start_position().row,
            "type_node row ({}) is before impl start row ({}) for {}",
            impl_name_point.row,
            node.start_position().row,
            unique_name
        );
        let impl_name_column = impl_name_point.column;
        let byte_range = node.byte_range();
        let attached_range = compute_attached_range(node, source, SupportedLanguage::Rust);
        out.push(ExtractedSymbol {
            name: unique_name,
            semantic_path: impl_path,
            kind: SymbolKind::Impl,
            byte_range: byte_range.clone(),
            attached_range,
            start_line: node.start_position().row,
            end_line: node.end_position().row,
            name_column: impl_name_column,
            name_point: impl_name_point,
            access_level: crate::surgeon::AccessLevel::Private,
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

    let full_path = chain.to_string();
    for s in symbols {
        if s.semantic_path == full_path {
            return Some(s);
        }
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

fn merge_rust_impl_blocks_recursive(syms: &mut Vec<ExtractedSymbol>) {
    let declared_types: std::collections::HashSet<String> = syms
        .iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Interface | SymbolKind::Class
            )
        })
        .map(|s| s.name.clone())
        .collect();

    let mut extracted_methods: std::collections::HashMap<String, Vec<ExtractedSymbol>> =
        std::collections::HashMap::default();

    syms.retain_mut(|s| {
        if s.kind == SymbolKind::Impl {
            let clean_name = s.name.split('#').next().unwrap_or(&s.name).to_string();
            if declared_types.contains(&clean_name) {
                let entry = extracted_methods.entry(clean_name).or_default();
                for method in std::mem::take(&mut s.children) {
                    entry.push(method);
                }
                return false;
            }
            s.name = clean_name;
            return true;
        }
        true
    });

    for s in syms.iter_mut() {
        if matches!(
            s.kind,
            SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Interface | SymbolKind::Class
        ) {
            if let Some(methods) = extracted_methods.remove(&s.name) {
                s.children.extend(methods);
            }
        }
    }

    for s in syms.iter_mut() {
        if !s.children.is_empty() {
            merge_rust_impl_blocks_recursive(&mut s.children);
        }
    }
}

/// Assigns deterministic `#N` overload suffixes based on `(byte_range.start, byte_range.end)` ordering.
pub fn disambiguate_overloads_recursive(parent_path: &str, syms: &mut [ExtractedSymbol]) {
    syms.sort_by_key(|s| (s.byte_range.start, s.byte_range.end));

    let mut name_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::with_capacity(syms.len());

    for s in syms.iter_mut() {
        if s.kind == SymbolKind::Zone {
            s.semantic_path = s.name.clone();
            disambiguate_overloads_recursive(&s.semantic_path, &mut s.children);
            continue;
        }

        if matches!(s.kind, SymbolKind::Component | SymbolKind::HtmlElement) {
            // JSX symbols already have their semantic_path assigned with [N] notation
            continue;
        }

        let count = name_counts.entry(s.name.clone()).or_insert(0);
        *count += 1;
        let suffix = if *count > 1 {
            format!("#{count}")
        } else {
            String::new()
        };

        s.semantic_path = if parent_path.is_empty() {
            if let Some(dot_idx) = s.semantic_path.rfind('.') {
                let prefix = &s.semantic_path[..dot_idx];
                format!("{prefix}.{}{suffix}", s.name)
            } else {
                format!("{}{suffix}", s.name)
            }
        } else {
            format!("{parent_path}.{}{suffix}", s.name)
        };

        disambiguate_overloads_recursive(&s.semantic_path, &mut s.children);
    }
}

/// Validate structural invariants on extracted symbols.
pub fn validate_symbol_invariants(syms: &[ExtractedSymbol]) {
    for s in syms {
        debug_assert!(
            !s.semantic_path.contains("::"),
            "Symbol semantic_path '{}' contains '::' (TS-A4 invariant violation)",
            s.semantic_path
        );
        debug_assert!(
            !s.byte_range.is_empty() || s.kind == SymbolKind::Zone,
            "Symbol '{}' has empty byte range (TS-A3 phantom symbol violation)",
            s.semantic_path
        );
        validate_symbol_invariants(&s.children);
    }
}

/// Merges methods from `impl Foo` blocks into their corresponding `struct Foo` / `enum Foo` / `trait Foo` declarations.
pub fn merge_rust_impl_blocks(symbols: &mut Vec<ExtractedSymbol>) {
    merge_rust_impl_blocks_recursive(symbols);
}

/// Find the innermost symbol enclosing a given 0-indexed row.
#[must_use]
pub fn find_enclosing_symbol(symbols: &[ExtractedSymbol], row: usize) -> Option<String> {
    find_enclosing_symbol_ref(symbols, row).map(|s| s.semantic_path.clone())
}

/// Find the innermost symbol enclosing the given 0-indexed row,
/// returning a reference to the full [`ExtractedSymbol`] (including `start_line`).
#[must_use]
pub fn find_enclosing_symbol_ref(
    symbols: &[ExtractedSymbol],
    row: usize,
) -> Option<&ExtractedSymbol> {
    fn search<'a>(syms: &'a [ExtractedSymbol], row: usize, best: &mut Option<&'a ExtractedSymbol>) {
        for s in syms {
            if s.start_line <= row && row <= s.end_line {
                if let Some(current_best) = best {
                    let current_lines = current_best.end_line - current_best.start_line;
                    let target_lines = s.end_line - s.start_line;
                    if target_lines <= current_lines {
                        *best = Some(s);
                    }
                } else {
                    *best = Some(s);
                }
            }

            search(&s.children, row, best);
        }
    }

    let mut best_match: Option<&ExtractedSymbol> = None;
    search(symbols, row, &mut best_match);
    best_match
}

/// Computes string similarity to offer did-you-mean suggestions.
#[must_use]
pub fn did_you_mean(
    symbols: &[ExtractedSymbol],
    chain: &SymbolChain,
    max_suggestions: usize,
) -> Vec<String> {
    fn collect_paths<'a>(syms: &'a [ExtractedSymbol], out: &mut Vec<&'a str>) {
        for s in syms {
            out.push(&s.semantic_path);
            collect_paths(&s.children, out);
        }
    }

    if chain.segments.is_empty() {
        return Vec::default();
    }

    let target = chain.to_string();

    let mut all_paths: Vec<&str> = Vec::default();
    collect_paths(symbols, &mut all_paths);

    if let Some(exact) = all_paths.iter().find(|p| **p == target) {
        return vec![(*exact).to_string()];
    }

    let target_len = target.len();
    let threshold = 5.max(target_len / 4);
    let max_len_delta = threshold;

    let mut distances: Vec<(usize, &str)> = all_paths
        .into_iter()
        .filter_map(|path| {
            let len_delta = path.len().abs_diff(target_len);
            if len_delta > max_len_delta && !path.contains(&target) && !target.contains(path) {
                return None;
            }

            let dist = if path.contains(&target) || target.contains(path) {
                len_delta
            } else {
                levenshtein(&target, path)
            };

            if dist <= threshold || path.contains(&target) {
                Some((dist, path))
            } else {
                None
            }
        })
        .collect();

    distances.sort_by_key(|(dist, _)| *dist);

    distances
        .into_iter()
        .take(max_suggestions)
        .map(|(_, path)| path.to_string())
        .collect()
}

/// Computes file-qualified did-you-mean suggestions.
#[must_use]
pub fn did_you_mean_file_qualified(
    file_path: &std::path::Path,
    symbols: &[ExtractedSymbol],
    chain: &SymbolChain,
    max_suggestions: usize,
) -> Vec<String> {
    let prefix = file_path.to_string_lossy();
    did_you_mean(symbols, chain, max_suggestions)
        .into_iter()
        .map(|sym_path| format!("{prefix}::{sym_path}"))
        .collect()
}

#[cfg(test)]
#[path = "vue_multizone_tests.rs"]
mod vue_multizone_tests;

#[cfg(test)]
#[path = "symbols_test.rs"]
mod tests;
