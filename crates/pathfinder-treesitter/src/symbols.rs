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
    extract_symbols_recursive(root, source, types, lang, "", &mut symbols);

    if matches!(lang, SupportedLanguage::Rust) {
        merge_rust_impl_blocks(&mut symbols);
    }

    symbols
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
            return Some(refine_class_kind(node));
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
        let (unique_name, suffix) = make_unique_name(&mut self.name_counts, name);
        let path = self.build_path(&unique_name, &suffix);

        // Resolve the name node's column for LSP navigation positioning.
        // Falls back to 0 (start of line) when the name node cannot be found
        // (e.g., for anonymous constructs or grammars without a "name" field).
        let name_column = Self::resolve_name_node(child).map_or(0, |n| n.start_position().column);

        let access_level = detect_access_level(child, self.lang, self.source);
        let mut symbol = ExtractedSymbol {
            name: unique_name,
            semantic_path: path.clone(),
            kind: sk,
            byte_range: child.byte_range(),
            start_line: child.start_position().row,
            end_line: child.end_position().row,
            name_column,
            access_level,
            children: Vec::new(),
        };

        if matches!(
            sk,
            SymbolKind::Class | SymbolKind::Struct | SymbolKind::Interface | SymbolKind::Enum
        ) {
            self.extract_nested_symbols(child, &path, &mut symbol.children);
        }

        if matches!(sk, SymbolKind::Function)
            && matches!(
                self.lang,
                SupportedLanguage::Tsx | SupportedLanguage::JavaScript
            )
        {
            extract_jsx_children(child, self.source, &path, &mut symbol.children);
        }

        self.out.push(symbol);
    }

    /// Extract a module block as a named scope symbol with its children.
    ///
    /// The module's `name` field becomes the scope prefix for all nested symbols.
    /// Example: `mod tests { fn test_foo() {} }` becomes:
    ///   - `tests` (Module, with children)
    ///     - `test_foo` (Function, path = "`tests.test_foo`")
    fn extract_module_block(&mut self, child: Node<'a>) {
        // Get the module name node (Tree-sitter field: "name")
        let Some(name_node) = child.child_by_field_name("name") else {
            // Unnamed module — recurse with current parent path (safe fallback)
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

        let mut children = Vec::new();

        // Extract body contents as children scoped under `module_path`
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

        // Determine visibility via detect_access_level (Rust: pub/pub(crate)/pub(super)/bare).
        let name_column = name_node.start_position().column;
        let access_level = detect_access_level(child, self.lang, self.source);
        self.out.push(ExtractedSymbol {
            name: unique_name,
            semantic_path: module_path,
            kind: SymbolKind::Module,
            byte_range: child.byte_range(),
            start_line: child.start_position().row,
            end_line: child.end_position().row,
            name_column,
            access_level,
            children,
        });
    }

    fn build_path(&self, name: &str, suffix: &str) -> String {
        if self.parent_path.is_empty() {
            format!("{name}{suffix}")
        } else {
            format!("{}.{}{}", self.parent_path, name, suffix)
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
        name_counts: std::collections::HashMap::new(),
    };
    ctx.process_children();
}

/// Refine `class_kinds` match to precise kind.
///
/// - Go `type_spec`: checks `type` field for `interface_type`/`struct_type`.
/// - TS `enum_declaration` → `SymbolKind::Enum`.
/// - TS `type_alias_declaration` → `SymbolKind::Class` (type alias).
/// - Java `interface_declaration` / `annotation_type_declaration` → `SymbolKind::Interface`.
/// - Java `record_declaration` → `SymbolKind::Struct`.
/// - All others → `SymbolKind::Class`.
fn refine_class_kind(node: Node) -> SymbolKind {
    match node.kind() {
        // TypeScript `enum_declaration` / Rust `enum_item` / Java `enum_declaration`
        "enum_declaration" | "enum_item" => SymbolKind::Enum,
        // Rust struct_item / Java record_declaration → Struct
        "struct_item" | "record_declaration" => SymbolKind::Struct,
        // Rust trait_item / Java interface_declaration / Java @interface → Interface
        "trait_item" | "interface_declaration" | "annotation_type_declaration" => {
            SymbolKind::Interface
        }
        _ => {
            // Go type_spec: refine based on the `type` field
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
///
/// This replaces the former `has_visibility_modifier()` bool-returning function with a
/// 4-level enum that supports Java's visibility model without breaking other languages.
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
///
/// | AST text         | AccessLevel |
/// |-----------------|-------------|
/// | `pub(crate)`    | Package     |
/// | `pub(super)`    | Protected   |
/// | any other `pub` | Public      |
/// | (no modifier)   | Private     |
fn detect_rust_access_level(node: Node) -> crate::surgeon::AccessLevel {
    use crate::surgeon::AccessLevel;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "visibility_modifier" {
            // The visibility_modifier node's text determines fine-grained level.
            // We check the direct text children for `(crate)` or `(super)` tokens.
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
///
/// | Name pattern         | AccessLevel |
/// |---------------------|-------------|
/// | Starts with `_`     | Private     |
/// | Starts with uppercase | Public    |
/// | Starts with lowercase | Package   |
fn detect_go_access_level(node: Node, source: &[u8]) -> crate::surgeon::AccessLevel {
    use crate::surgeon::AccessLevel;
    // Extract the name from `name` field or `identifier` field
    let name_node = node
        .child_by_field_name("name")
        .or_else(|| node.child_by_field_name("identifier"));

    if let Some(nn) = name_node {
        if let Some(bytes) = source.get(nn.byte_range()) {
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
    }
    // Fallback: treat as package-level if we can't determine the name
    AccessLevel::Package
}

/// TypeScript/JavaScript/Vue visibility rules.
///
/// Preserves the existing parent-walk logic from `has_visibility_modifier`.
/// Also applies `_`-prefix check for private-by-convention symbols.
///
/// | Condition                                           | AccessLevel |
/// |----------------------------------------------------|-------------|
/// | Ancestor `export_statement` (before program/block) | Public      |
/// | Name starts with `_`                               | Private     |
/// | No export ancestor                                 | Package     |
fn detect_ts_access_level(node: Node, source: &[u8]) -> crate::surgeon::AccessLevel {
    use crate::surgeon::AccessLevel;
    // Walk up the parent chain looking for export_statement
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

    // Check name for `_` prefix convention
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
///
/// | Name pattern                      | AccessLevel |
/// |----------------------------------|-------------|
/// | Starts with `__`, ends with `__` | Public (dunder/magic method) |
/// | Starts with `__` (non-dunder)    | Private     |
/// | Starts with `_`                  | Protected   |
/// | No prefix                        | Public      |
fn detect_python_access_level(node: Node, source: &[u8]) -> crate::surgeon::AccessLevel {
    use crate::surgeon::AccessLevel;
    let name_node = node
        .child_by_field_name("name")
        .or_else(|| node.child_by_field_name("identifier"));

    if let Some(nn) = name_node {
        if let Some(bytes) = source.get(nn.byte_range()) {
            if let Ok(name) = std::str::from_utf8(bytes) {
                let name = name.trim();
                if name.starts_with("__") {
                    // Dunder methods (e.g. __init__, __str__) are public magic methods
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
///
/// | Modifier keyword  | `AccessLevel` |
/// |------------------|---------------|
/// | `public`         | Public        |
/// | `protected`      | Protected     |
/// | `private`        | Private       |
/// | (no modifier)    | Package       | — Java package-private (default)
///
/// Java modifiers are a child node named `modifiers`. The individual keywords
/// (`public`, `protected`, `private`) are **unnamed** nodes inside `modifiers`
/// (i.e. `is_named() == false`), so we must use `children()` not `named_children()`.
fn detect_java_access_level(node: Node) -> crate::surgeon::AccessLevel {
    use crate::surgeon::AccessLevel;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "modifiers" {
            let mut mod_cursor = child.walk();
            // NOTE: modifier keywords are UNNAMED nodes — use children(), not named_children()
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
    // No access modifier → package-private (Java default)
    AccessLevel::Package
}

/// Detect if a function node is a test function using language-specific rules.
///
/// Checks for:
/// - Rust: `#[test]`, `#[tokio::test]`, etc. in `attribute_item` children
/// - Python: `@pytest.mark` decorator or function named `test_*`
/// - Go: function name starting with `Test` in `*_test.go` file
/// - Java: `@Test` annotation in modifiers
/// - Plus naming conventions as fallback: `test_` prefix, `_test` suffix
fn is_test_function(
    node: Node<'_>,
    lang: SupportedLanguage,
    source: &[u8],
    func_name: Option<&str>,
) -> bool {
    match lang {
        SupportedLanguage::Rust => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                let kind = child.kind();
                if kind == "attribute_item" || kind == "inner_attribute_item" {
                    if let Some(attr_text) = source.get(child.byte_range()) {
                        let attr = String::from_utf8_lossy(attr_text);
                        if attr.contains("#[test]")
                            || attr.contains("#[tokio::test")
                            || attr.contains("#[rstest]")
                            || attr.contains("#[test_case")
                        {
                            return true;
                        }
                    }
                }
            }
            if let Some(name) = func_name {
                name.starts_with("test_") || name.ends_with("_test")
            } else {
                false
            }
        }
        SupportedLanguage::Python => {
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
        SupportedLanguage::Java => {
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
                name.starts_with("test")
            } else {
                false
            }
        }
    }
}

/// Generate unique name with suffix for duplicate symbols.
/// Returns `(unique_name, suffix)` where suffix is "#N" for N>1 or empty for first occurrence.
fn make_unique_name(
    name_counts: &mut std::collections::HashMap<String, usize>,
    name: String,
) -> (String, String) {
    // PERF: Avoid unconditional string allocation on cache hit.
    if !name_counts.contains_key(&name) {
        name_counts.insert(name.clone(), 0);
    }
    #[allow(clippy::expect_used)]
    let count = name_counts.get_mut(&name).expect("just inserted");
    *count += 1;
    let suffix = if *count > 1 {
        format!("#{count}")
    } else {
        String::default()
    };
    (name, suffix)
}

/// Strip angle-bracket generics and lifetimes from a type name.
fn strip_generics(type_name: &str) -> &str {
    match type_name.find('<') {
        Some(idx) => type_name[..idx].trim_end(),
        None => type_name.trim(),
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
    name_counts: &mut std::collections::HashMap<String, usize>,
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
    let type_name = strip_generics(type_name).to_string();

    let (unique_name, suffix) = make_unique_name(name_counts, type_name);
    let impl_path = if parent_path.is_empty() {
        format!("{unique_name}{suffix}")
    } else {
        format!("{parent_path}.{unique_name}{suffix}")
    };

    // Collect all child function_items from the impl body as Method symbols.
    let mut methods: Vec<ExtractedSymbol> = Vec::new();
    let mut method_name_counts: std::collections::HashMap<String, usize> =
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

            let (unique_method_name, method_suffix) =
                make_unique_name(&mut method_name_counts, method_name);
            let method_path = format!("{impl_path}.{unique_method_name}{method_suffix}");
            let method_name_column = name_node.start_position().column;
            methods.push(ExtractedSymbol {
                name: unique_method_name,
                semantic_path: method_path,
                kind: SymbolKind::Method,
                byte_range: item.byte_range(),
                start_line: item.start_position().row,
                end_line: item.end_position().row,
                name_column: method_name_column,
                access_level: crate::surgeon::AccessLevel::Public,
                children: Vec::new(),
            });
        }
    }

    if !methods.is_empty() {
        let impl_name_column = type_node.start_position().column;
        out.push(ExtractedSymbol {
            name: unique_name,
            semantic_path: impl_path,
            kind: SymbolKind::Impl,
            byte_range: node.byte_range(),
            start_line: node.start_position().row,
            end_line: node.end_position().row,
            name_column: impl_name_column,
            access_level: crate::surgeon::AccessLevel::Private,
            children: methods,
        });
    }
}

/// Resolve a `SymbolChain` against a list of extracted symbols.
///
/// ## Rust `impl` block fallback
///
/// For Rust files, `extract_impl_block` produces two distinct top-level symbols
/// for the same type name: a `Struct` / `Enum` node (no suffix) and an `Impl`
/// node (with a `#N` suffix, e.g. `MyStruct#2`).  When an agent writes a path
/// like `MyStruct.my_method`, the chain resolver must first navigate to `MyStruct`
/// (the struct symbol), then descend into `.my_method`.  But the struct symbol's
/// `children` list contains only fields / variants — the methods live under the
/// sibling `Impl` symbol.
///
/// To fix this without changing the extraction shape (the repo-map still renders
/// both the struct *and* the impl block), resolution applies an **impl-sibling
/// fallback**: when the next segment fails to resolve in the current symbol's
/// children, we gather every sibling `Impl` symbol with the same base name (i.e.
/// name matches ignoring the `#N` overload suffix) and search their children as
/// well.  The first match wins.
///
/// This is purely additive — it does not affect paths that already resolved
/// correctly, and it is only consulted when the normal traversal finds nothing.
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

/// Merge Rust Impl methods directly under their associated Struct/Enum/Interface symbols.
/// This prevents `SYMBOL_NOT_FOUND` when tools target methods using `MyStruct.method` instead
/// of distinguishing between multiple Impl blocks.
fn merge_rust_impl_blocks(symbols: &mut Vec<ExtractedSymbol>) {
    fn merge_recursive(syms: &mut Vec<ExtractedSymbol>) {
        let mut extracted_methods: std::collections::HashMap<String, Vec<ExtractedSymbol>> =
            std::collections::HashMap::new();
        let mut impl_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();

        // 1. Remove all Impl blocks and extract their children
        syms.retain_mut(|s| {
            if s.kind == SymbolKind::Impl {
                // PERF: Avoid unconditional string allocation on cache hit.
                if !extracted_methods.contains_key(&s.name) {
                    extracted_methods.insert(s.name.clone(), Vec::new());
                }
                #[allow(clippy::expect_used)]
                let entry = extracted_methods.get_mut(&s.name).expect("just inserted");
                for mut method in std::mem::take(&mut s.children) {
                    // Update method's semantic path to be under the struct instead of the Impl
                    // Impl blocks have `#` suffix, we want it under the Struct which doesn't
                    if let Some((parent_path, method_name)) = method.semantic_path.rsplit_once('.')
                    {
                        // strip #[0-9]+ from the end of the parent path
                        let clean_parent = match parent_path.rfind('#') {
                            Some(idx)
                                if parent_path[idx + 1..].chars().all(|c| c.is_ascii_digit()) =>
                            {
                                &parent_path[..idx]
                            }
                            _ => parent_path,
                        };
                        method.semantic_path = format!("{clean_parent}.{method_name}");
                    }
                    entry.push(method);
                }

                let clean_name = s.name.split('#').next().unwrap_or(&s.name);
                // PERF: Avoid unconditional string allocation on cache hit.
                if !impl_counts.contains_key(clean_name) {
                    impl_counts.insert(clean_name.to_string(), 0);
                }
                #[allow(clippy::expect_used)]
                let count = impl_counts.get_mut(clean_name).expect("just inserted");
                *count += 1;

                let suffix = if *count > 1 {
                    format!("#{count}")
                } else {
                    String::default()
                };

                s.name = format!("impl {clean_name}{suffix}");
                s.semantic_path.clone_from(&s.name);
            }
            true
        });

        // 2. Append methods to the matching structural type
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

        // 3. Re-insert remaining methods for types not in this scope (e.g. `impl ExternalType {}`)
        for (name, methods) in extracted_methods {
            if methods.is_empty() {
                continue;
            }
            syms.push(ExtractedSymbol {
                name: name.clone(),
                semantic_path: name.clone(),
                kind: SymbolKind::Impl,
                byte_range: 0..0,
                start_line: 0,
                end_line: 0,
                name_column: 0,
                access_level: crate::surgeon::AccessLevel::Private,
                children: methods,
            });
        }

        // 4. Recurse into children
        for s in syms.iter_mut() {
            if !s.children.is_empty() {
                merge_recursive(&mut s.children);
            }
        }
    }

    merge_recursive(symbols);
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

    // Dynamic threshold: allow more typos for longer symbol names
    let threshold = 5.max(target.len() / 4);

    let mut distances: Vec<(usize, &str)> = all_paths
        .into_iter()
        .filter_map(|path| {
            let dist = if path.contains(&target) || target.contains(path) {
                path.len().abs_diff(target.len())
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
            }

            // Always recurse into children because Rust impl methods might be
            // reparented under a struct whose line bounds do not contain them.
            search(&s.children, row, best);
        }
    }

    let mut best_match: Option<&ExtractedSymbol> = None;
    search(symbols, row, &mut best_match);
    best_match.map(|s| s.semantic_path.clone())
}

// ─── Vue multi-zone symbol extraction ─────────────────────────────────────────

/// Push a zone symbol (template or style) into the output if children are non-empty.
fn push_zone_symbol(
    output: &mut Vec<ExtractedSymbol>,
    zone_name: &str,
    children: Vec<ExtractedSymbol>,
    zone_range: Option<&crate::vue_zones::VueZoneRange>,
) {
    if children.is_empty() {
        return;
    }
    let byte_range = zone_range.map_or(0..0, |z| z.start_byte..z.end_byte);
    let start_line = zone_range.map_or(0, |z| z.start_point.row);
    let end_line = zone_range.map_or(0, |z| z.end_point.row);
    output.push(ExtractedSymbol {
        name: zone_name.to_string(),
        semantic_path: zone_name.to_string(),
        kind: crate::surgeon::SymbolKind::Zone,
        byte_range,
        start_line,
        end_line,
        name_column: 0,
        access_level: crate::surgeon::AccessLevel::Public,
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
pub fn extract_symbols_from_multizone(
    multi: &crate::vue_zones::MultiZoneTree,
) -> Vec<ExtractedSymbol> {
    let mut output: Vec<ExtractedSymbol> = Vec::new();

    // ── Script zone (top-level, backward-compatible) ──────────────────────────
    if let Some(ref tree) = multi.script_tree {
        let ts_syms = extract_symbols_from_tree(tree, &multi.source, SupportedLanguage::Vue);
        output.extend(ts_syms);
    }

    // ── Template zone ─────────────────────────────────────────────────────────
    if let Some(ref tree) = multi.template_tree {
        let children = extract_template_symbols(tree, &multi.source);
        push_zone_symbol(
            &mut output,
            "template",
            children,
            multi.zones.template.as_ref(),
        );
    }

    // ── Style zone ────────────────────────────────────────────────────────────
    if let Some(ref tree) = multi.style_tree {
        let children = extract_style_symbols(tree, &multi.source);
        push_zone_symbol(&mut output, "style", children, multi.zones.style.as_ref());
    }

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
    let mut symbols: Vec<ExtractedSymbol> = Vec::new();
    let mut tag_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    walk_html_elements_flat(
        tree.root_node(),
        source,
        "template",
        &mut symbols,
        &mut tag_counts,
    );

    symbols
}

/// Recursive HTML element walker that flattens elements into a single list.
fn walk_html_elements_flat(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    parent_path: &str,
    out: &mut Vec<ExtractedSymbol>,
    tag_counts: &mut std::collections::HashMap<String, usize>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let kind = child.kind();
        let tag_name_opt = resolve_tag_name(kind, child, source);

        if let Some(ref name) = tag_name_opt {
            let is_component = name.chars().next().is_some_and(char::is_uppercase);
            let sym_kind = if is_component {
                crate::surgeon::SymbolKind::Component
            } else {
                crate::surgeon::SymbolKind::HtmlElement
            };

            // PERF: Avoid unconditional string allocation on cache hit.
            if !tag_counts.contains_key(name) {
                tag_counts.insert(name.clone(), 0);
            }
            #[allow(clippy::expect_used)]
            let count = tag_counts.get_mut(name).expect("just inserted");
            *count += 1;
            let nth = *count;
            let sym_name = if nth == 1 {
                name.clone()
            } else {
                format!("{name}[{nth}]")
            };

            // Components are promoted to top-level paths (template.MyButton).
            // HTML elements retain hierarchical paths (template.div.span).
            let sym_path = if is_component {
                format!("template::{sym_name}")
            } else {
                format!("{parent_path}::{sym_name}")
            };

            out.push(ExtractedSymbol {
                name: sym_name,
                semantic_path: sym_path.clone(),
                kind: sym_kind,
                byte_range: child.byte_range(),
                start_line: child.start_position().row,
                end_line: child.end_position().row,
                name_column: child.start_position().column,
                access_level: crate::surgeon::AccessLevel::Public,
                children: Vec::new(), // Always flat
            });

            // Recurse into children
            walk_html_elements_flat(child, source, &sym_path, out, tag_counts);
        } else {
            walk_html_elements_flat(child, source, parent_path, out, tag_counts);
        }
    }
}

// ---------------------------------------------------------------------------
// E1-J: JSX/TSX symbol extraction
// ---------------------------------------------------------------------------

/// Resolve the tag name from a `jsx_element` or `jsx_self_closing_element` node.
///
/// For `jsx_element`, the structure is:
///   `jsx_element` → `jsx_opening_element` → `identifier` (tag name)
///
/// For `jsx_self_closing_element`, the structure is:
///   `jsx_self_closing_element` → `identifier` (`member_expression` for `Foo.Bar`)
fn resolve_jsx_tag_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    let kind = node.kind();

    if kind == "jsx_element" {
        // jsx_element → jsx_opening_element child → first identifier child
        let mut c = node.walk();
        let open_tag = node
            .named_children(&mut c)
            .find(|n| n.kind() == "jsx_opening_element")?;

        // The first named child of jsx_opening_element is the tag name
        // (identifier, member_expression, or jsx_namespace_name)
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
        // jsx_self_closing_element → identifier child directly
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
///
/// For `const Foo = () => <jsx/>`, the node structure is:
///   `lexical_declaration` → `variable_declarator` → `arrow_function`
///
/// Returns `None` if the node is already a function/arrow or has no inner function.
fn find_inner_function(node: Node<'_>) -> Option<Node<'_>> {
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
///
/// Walks the function AST node looking for JSX elements in:
/// - `return_statement` → `jsx_element` / `jsx_self_closing_element`
/// - Arrow function implicit returns (body is a JSX expression directly)
///
/// JSX children are flattened (top-level + one level of nesting) and
/// placed under `{function_path}.return.{TagName}` semantic paths.
fn extract_jsx_children(
    fn_node: Node<'_>,
    source: &[u8],
    fn_path: &str,
    out: &mut Vec<ExtractedSymbol>,
) {
    let return_path = format!("{fn_path}::return");
    let mut tag_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    // For `const Foo = () => <jsx/>` patterns, fn_node is a `lexical_declaration`;
    // we need to locate the inner `arrow_function` or `function_expression` node.
    let target = find_inner_function(fn_node).unwrap_or(fn_node);

    walk_jsx_return_sites(target, source, &return_path, out, &mut tag_counts);
}

/// Recursively descend into a function node to find JSX return sites.
///
/// Handles two patterns:
///   1. Explicit return: `return (<jsx/>)` — looks for `return_statement`
///   2. Arrow implicit return: `() => <jsx/>` — looks for `jsx_element` /
///      `jsx_self_closing_element` as direct body of `arrow_function`
fn walk_jsx_return_sites(
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
            // Explicit return: descend into the return statement to find JSX.
            "return_statement" => {
                collect_jsx_elements(child, source, return_path, out, tag_counts);
            }
            // Arrow function body that IS a JSX element (implicit return).
            "jsx_element" | "jsx_self_closing_element" if node.kind() == "arrow_function" => {
                emit_jsx_symbol(child, source, return_path, out, tag_counts);
                // Also collect nested JSX children (one level deep)
                collect_jsx_elements(child, source, return_path, out, tag_counts);
            }
            // Skip into nested scopes that might contain return statements,
            // but avoid descending into inner function declarations.
            "statement_block"
            | "parenthesized_expression"
            | "if_statement"
            | "switch_body"
            | "switch_case" => {
                walk_jsx_return_sites(child, source, return_path, out, tag_counts);
            }
            _ => {
                // For arrow_function nodes, continue searching for JSX in body
                if node.kind() == "arrow_function" {
                    walk_jsx_return_sites(child, source, return_path, out, tag_counts);
                }
            }
        }
    }
}

/// Collect all JSX elements recursively from under a given node.
fn collect_jsx_elements(
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
        // Recurse to find nested elements (works for both JSX and non-JSX nodes)
        collect_jsx_elements(child, source, parent_path, out, tag_counts);
    }
}

/// Emit a single JSX symbol from a `jsx_element` or `jsx_self_closing_element` node.
fn emit_jsx_symbol(
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

        // PERF: Avoid unconditional string allocation on cache hit.
        if !tag_counts.contains_key(name) {
            tag_counts.insert(name.clone(), 0);
        }
        #[allow(clippy::expect_used)]
        let count = tag_counts.get_mut(name).expect("just inserted");
        *count += 1;
        let nth = *count;
        let sym_name = if nth == 1 {
            name.clone()
        } else {
            format!("{name}[{nth}]")
        };

        let sym_path = format!("{parent_path}::{sym_name}");

        out.push(ExtractedSymbol {
            name: sym_name,
            semantic_path: sym_path,
            kind: sym_kind,
            byte_range: node.byte_range(),
            start_line: node.start_position().row,
            end_line: node.end_position().row,
            name_column: node.start_position().column,
            access_level: crate::surgeon::AccessLevel::Public,
            children: Vec::new(), // JSX children are flat, not nested
        });
    }
}

/// Resolves the actual tag name string from an HTML AST node.
fn resolve_tag_name(kind: &str, child: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    if kind == "element" {
        // start_tag child → tag_name grandchild
        let start_tag = child.child_by_field_name("start_tag").or_else(|| {
            let mut c = child.walk();
            let found = child
                .named_children(&mut c)
                .find(|n| n.kind() == "start_tag");
            // Materialize range before cursor drops
            found.map(|n| {
                // Re-lookup by byte range to avoid keeping the borrow alive
                child.child_by_field_name("start_tag").unwrap_or(n)
            })
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
        // self_closing_element → tag_name child
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

/// Extract CSS selector symbols from a CSS parse tree (`<style>` zone).
///
/// Emits:
/// - [`SymbolKind::CssSelector`] for class selectors (`.name`), id selectors (`#name`),
///   and bare element type selectors (`p`, `div`).
/// - [`SymbolKind::CssAtRule`] for `@media` and `@keyframes` rules.
///
/// Multiple `@media` rules are disambiguated with `[N]` suffixes.
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
fn walk_css_rules(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    parent_path: &str,
    out: &mut Vec<ExtractedSymbol>,
    at_counts: &mut std::collections::HashMap<String, usize>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            // Standard `selector { ... }` rule
            "rule_set" => {
                extract_css_rule_set(child, source, parent_path, out);
            }
            // @media, @keyframes, @supports …
            "media_statement" | "keyframes_statement" | "at_rule" => {
                let at_name = extract_at_rule_name(child, source);
                // PERF: Avoid unconditional string allocation on cache hit.
                if !at_counts.contains_key(&at_name) {
                    at_counts.insert(at_name.clone(), 0);
                }
                #[allow(clippy::expect_used)]
                let count = at_counts.get_mut(&at_name).expect("just inserted");
                *count += 1;
                let nth = *count;
                let sym_name = if nth == 1 {
                    format!("@{at_name}")
                } else {
                    format!("@{at_name}[{nth}]")
                };
                let sym_path = format!("{parent_path}::{sym_name}");
                out.push(ExtractedSymbol {
                    name: sym_name,
                    semantic_path: sym_path,
                    kind: crate::surgeon::SymbolKind::CssAtRule,
                    byte_range: child.byte_range(),
                    start_line: child.start_position().row,
                    end_line: child.end_position().row,
                    name_column: child.start_position().column,
                    access_level: crate::surgeon::AccessLevel::Public,
                    children: Vec::new(),
                });
            }
            _ => {
                // Recurse into stylesheet or other container nodes
                walk_css_rules(child, source, parent_path, out, at_counts);
            }
        }
    }
}

/// Extract the at-rule keyword (e.g. "media", "keyframes") from an at-rule node.
fn extract_at_rule_name(node: tree_sitter::Node<'_>, source: &[u8]) -> String {
    // tree-sitter-css at-rule-keyword / keyword node
    let mut c = node.walk();
    for child in node.named_children(&mut c) {
        if matches!(child.kind(), "at_keyword" | "keyword") {
            if let Some(bytes) = source.get(child.byte_range()) {
                if let Ok(s) = std::str::from_utf8(bytes) {
                    // Strip leading `@` if present
                    return s.trim_start_matches('@').trim().to_owned();
                }
            }
        }
    }
    // Fallback: derive from node kind
    match node.kind() {
        "media_statement" => "media".to_owned(),
        "keyframes_statement" => "keyframes".to_owned(),
        _ => "rule".to_owned(),
    }
}

/// Extract selector symbols from a single `rule_set` node.
fn extract_css_rule_set(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    parent_path: &str,
    out: &mut Vec<ExtractedSymbol>,
) {
    // The `selectors` child contains one or more selector nodes.
    let mut c = node.walk();
    let selectors_node = node
        .named_children(&mut c)
        .find(|n| n.kind() == "selectors");

    let Some(sel_node) = selectors_node else {
        return;
    };

    let mut sel_cursor = sel_node.walk();
    for selector in sel_node.named_children(&mut sel_cursor) {
        let name_opt = match selector.kind() {
            "class_selector" => {
                // class_selector → `.` + class_name
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
                // id_selector → `#` + id_name
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
            "tag_name" => {
                // Bare element type selector
                source
                    .get(selector.byte_range())
                    .and_then(|b| std::str::from_utf8(b).ok())
                    .map(|s| s.trim().to_owned())
            }
            _ => None,
        };

        if let Some(sel_name) = name_opt {
            if sel_name.is_empty() {
                continue;
            }
            let sym_path = format!("{parent_path}::{sel_name}");
            out.push(ExtractedSymbol {
                name: sel_name,
                semantic_path: sym_path,
                kind: crate::surgeon::SymbolKind::CssSelector,
                byte_range: node.byte_range(), // whole rule_set for read_symbol_scope
                start_line: node.start_position().row,
                end_line: node.end_position().row,
                name_column: selector.start_position().column,
                access_level: crate::surgeon::AccessLevel::Public,
                children: Vec::new(),
            });
        }
    }
}

// ─── Multi-zone tests ─────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod vue_multizone_tests {
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
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::language::SupportedLanguage;
    use crate::parser::AstParser;
    use pathfinder_common::types::SymbolChain;

    fn parse_and_extract(source: &str, lang: SupportedLanguage) -> Vec<ExtractedSymbol> {
        let source_bytes = source.as_bytes();
        let tree =
            AstParser::parse_source(std::path::Path::new("dummy.rs"), lang, source_bytes).unwrap();
        extract_symbols_from_tree(&tree, source_bytes, lang)
    }

    /// PATCH-002-T1: Basic mod block creates Module symbol with children
    #[test]
    fn test_extract_rust_mod_block_with_children() {
        let source = r"
fn outer() {}

mod helpers {
    fn inner_one() {}
    fn inner_two() {}
}
";
        let symbols = parse_and_extract(source, SupportedLanguage::Rust);
        let module = symbols
            .iter()
            .find(|s| s.name == "helpers")
            .expect("helpers module not found");
        assert_eq!(module.kind, SymbolKind::Module);
        assert_eq!(module.children.len(), 2);
        assert!(module.children.iter().any(|c| c.name == "inner_one"));
        assert!(module.children.iter().any(|c| c.name == "inner_two"));
        // Module path
        assert_eq!(module.semantic_path, "helpers");
        // Child paths include module prefix
        let child = module
            .children
            .iter()
            .find(|c| c.name == "inner_one")
            .unwrap();
        assert_eq!(child.semantic_path, "helpers.inner_one");
    }

    /// PATCH-002-T2: cfg(test) mod tests is extracted
    #[test]
    fn test_extract_rust_cfg_test_mod_block() {
        let source = r"
fn production_code() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic() { assert!(true); }

    #[test]
    fn test_advanced() { assert!(true); }
}
";
        let symbols = parse_and_extract(source, SupportedLanguage::Rust);
        let module = symbols
            .iter()
            .find(|s| s.name == "tests")
            .expect("tests module not found");
        assert_eq!(module.kind, SymbolKind::Module);
        assert_eq!(module.children.len(), 2);
        assert!(module.children.iter().any(|c| c.name == "test_basic"));
        assert!(module.children.iter().any(|c| c.name == "test_advanced"));
    }

    /// R4: `#[test]` attribute functions are classified as `SymbolKind::Test`
    #[test]
    fn test_rust_test_attribute_is_symbol_kind_test() {
        let source = r"
fn regular_function() {}

#[test]
fn test_with_attribute() { assert!(true); }

#[tokio::test]
async fn test_tokio_attribute() { assert!(true); }

fn test_naming_convention_only() {}
";
        let symbols = parse_and_extract(source, SupportedLanguage::Rust);

        let regular = symbols
            .iter()
            .find(|s| s.name == "regular_function")
            .unwrap();
        assert_eq!(
            regular.kind,
            SymbolKind::Function,
            "regular function is Function"
        );

        let test_attr = symbols
            .iter()
            .find(|s| s.name == "test_with_attribute")
            .unwrap();
        assert_eq!(test_attr.kind, SymbolKind::Test, "#[test] attribute → Test");

        let tokio_test = symbols
            .iter()
            .find(|s| s.name == "test_tokio_attribute")
            .unwrap();
        assert_eq!(tokio_test.kind, SymbolKind::Test, "#[tokio::test] → Test");

        let test_name_only = symbols
            .iter()
            .find(|s| s.name == "test_naming_convention_only")
            .unwrap();
        assert_eq!(
            test_name_only.kind,
            SymbolKind::Test,
            "test_ prefix → Test (consistent with Python/Go naming convention)"
        );
    }

    /// R4: Python pytest naming convention and decorators are detected
    #[test]
    fn test_pytest_functions_detected() {
        let source = r"
def regular_function():
    pass

def test_something():
    pass

@pytest.fixture
def my_fixture():
    pass
";
        let symbols = parse_and_extract(source, SupportedLanguage::Python);

        let regular = symbols
            .iter()
            .find(|s| s.name == "regular_function")
            .unwrap();
        assert_eq!(regular.kind, SymbolKind::Function);

        let test_by_name = symbols.iter().find(|s| s.name == "test_something").unwrap();
        assert_eq!(
            test_by_name.kind,
            SymbolKind::Test,
            "Python test_ prefix → Test"
        );

        let fixture = symbols.iter().find(|s| s.name == "my_fixture").unwrap();
        assert_eq!(fixture.kind, SymbolKind::Test, "@pytest.fixture → Test");
    }

    /// PATCH-002-T3: `resolve_symbol_chain` traverses through module
    #[test]
    fn test_resolve_symbol_chain_through_module() {
        let source = r"
mod tests {
    fn test_foo() {}
}
";
        let symbols = parse_and_extract(source, SupportedLanguage::Rust);
        let chain = SymbolChain::parse("tests.test_foo").unwrap();
        let resolved = resolve_symbol_chain(&symbols, &chain);
        assert!(resolved.is_some(), "tests.test_foo should resolve");
        assert_eq!(resolved.unwrap().name, "test_foo");
    }

    /// PATCH-002-T4: Nested mod (mod inside mod) works
    #[test]
    fn test_extract_rust_nested_mod_blocks() {
        let source = r"
mod outer {
    mod inner {
        fn deep() {}
    }
}
";
        let symbols = parse_and_extract(source, SupportedLanguage::Rust);
        let outer = symbols.iter().find(|s| s.name == "outer").unwrap();
        assert_eq!(outer.kind, SymbolKind::Module);
        let inner = outer.children.iter().find(|c| c.name == "inner").unwrap();
        assert_eq!(inner.kind, SymbolKind::Module);
        let deep = inner.children.iter().find(|c| c.name == "deep").unwrap();
        assert_eq!(deep.name, "deep");
        assert_eq!(deep.semantic_path, "outer.inner.deep");
    }

    /// PATCH-002-T5: Top-level functions are NOT affected (regression)
    #[test]
    fn test_extract_rust_top_level_unchanged_with_module_kinds() {
        let source = r"
fn top_level_a() {}
fn top_level_b() {}

mod helpers {
    fn helper() {}
}
";
        let symbols = parse_and_extract(source, SupportedLanguage::Rust);
        // Top-level functions still at root
        assert!(symbols
            .iter()
            .any(|s| s.name == "top_level_a" && s.kind == SymbolKind::Function));
        assert!(symbols
            .iter()
            .any(|s| s.name == "top_level_b" && s.kind == SymbolKind::Function));
        // Module present
        assert!(symbols
            .iter()
            .any(|s| s.name == "helpers" && s.kind == SymbolKind::Module));
        // helper is NOT at root level anymore
        assert!(!symbols
            .iter()
            .any(|s| s.name == "helper" && s.semantic_path == "helper"));
    }

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

        // Expect: one Class node holding the methods, and one Impl node kept for line tracking
        assert_eq!(syms.len(), 2);
        let struct_sym = &syms[0];
        assert_eq!(struct_sym.name, "MyStruct");
        assert_eq!(struct_sym.semantic_path, "MyStruct");
        assert_eq!(struct_sym.children.len(), 2);

        let impl_sym = &syms[1];
        assert_eq!(impl_sym.name, "impl MyStruct");
        assert_eq!(impl_sym.semantic_path, "impl MyStruct");
        assert_eq!(impl_sym.children.len(), 0);
        assert_eq!(struct_sym.children[0].name, "foo");
        assert_eq!(struct_sym.children[0].kind, SymbolKind::Method);
        assert_eq!(struct_sym.children[0].semantic_path, "MyStruct.foo");
        assert_eq!(struct_sym.children[1].name, "bar");
        assert_eq!(struct_sym.children[1].kind, SymbolKind::Method);
        assert_eq!(struct_sym.children[1].semantic_path, "MyStruct.bar");
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

    /// PATCH-001-T1: `name_column` points to identifier, not to `pub` or `fn` keyword.
    ///
    /// For `pub fn compute() { }`:
    /// - column 0 = `p` in `pub`
    /// - column 4 = `f` in `fn`
    /// - column 7 = `c` in `compute` ← `name_column` should point here
    #[test]
    fn test_extract_rust_name_column_points_to_identifier_not_keyword() {
        let source = b"pub fn compute() { }\n";
        let tree = AstParser::parse_source(
            std::path::Path::new("dummy.rs"),
            SupportedLanguage::Rust,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Rust);

        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "compute");
        // `compute` starts at column 7: `pub fn compute...`
        //            01234567
        assert_eq!(
            syms[0].name_column, 7,
            "name_column should point to 'c' in 'compute', not 'p' in 'pub'"
        );
    }

    /// Regression test for F-6a: `StructName.method` paths must resolve even
    /// when the method lives in a separate `impl StructName { }` block rather
    /// than being nested inside the struct definition itself.
    ///
    /// Previously, `resolve_symbol_chain` would find the `Struct` symbol
    /// (`MyStruct`, no suffix) and descend into its (empty) children list,
    /// returning `None`.  The `Impl` symbol (`MyStruct#2`) held the methods but
    /// was never consulted.  `resolve_symbol_chain_with_impl_fallback` fixes this.
    #[test]
    fn test_resolve_rust_impl_method_via_struct_path() {
        let source = b"struct MyStruct;\nimpl MyStruct {\n    fn foo(&self) {}\n    fn bar(&mut self) {}\n}\n";
        let tree = AstParser::parse_source(
            std::path::Path::new("dummy.rs"),
            SupportedLanguage::Rust,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Rust);

        // With impl merging, `MyStruct.foo` should resolve perfectly.
        let chain = SymbolChain::parse("MyStruct.foo").unwrap();
        let hit =
            resolve_symbol_chain(&syms, &chain).expect("impl merging must resolve MyStruct.foo");
        assert_eq!(hit.name, "foo");
        assert_eq!(hit.kind, SymbolKind::Method);
        assert_eq!(hit.semantic_path, "MyStruct.foo");
    }

    /// Confirm that the impl-fallback also resolves `bar` (the second method).
    #[test]
    fn test_resolve_rust_impl_second_method_via_struct_path() {
        let source = b"struct MyStruct;\nimpl MyStruct {\n    fn foo(&self) {}\n    fn bar(&mut self) {}\n}\n";
        let tree = AstParser::parse_source(
            std::path::Path::new("dummy.rs"),
            SupportedLanguage::Rust,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Rust);

        let chain = SymbolChain::parse("MyStruct.bar").unwrap();
        let hit =
            resolve_symbol_chain(&syms, &chain).expect("impl merging must resolve MyStruct.bar");
        assert_eq!(hit.name, "bar");
        assert_eq!(hit.kind, SymbolKind::Method);
    }

    /// Confirm that the fallback still does NOT resolve a non-existent method.
    #[test]
    fn test_resolve_rust_impl_nonexistent_method_returns_none() {
        let source = b"struct MyStruct;\nimpl MyStruct {\n    fn foo(&self) {}\n}\n";
        let tree = AstParser::parse_source(
            std::path::Path::new("dummy.rs"),
            SupportedLanguage::Rust,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Rust);

        let chain = SymbolChain::parse("MyStruct.nonexistent").unwrap();
        let hit = resolve_symbol_chain(&syms, &chain);
        assert!(hit.is_none(), "non-existent method must return None");
    }

    /// PathfinderError.hint was the exact failing path from the incident report.
    /// It uses an `enum` + separate `impl` pattern (same as struct + impl).
    #[test]
    fn test_resolve_enum_impl_method_via_enum_path() {
        let source = b"enum PathfinderError { Foo }\nimpl PathfinderError {\n    fn hint(&self) -> Option<String> { None }\n}\n";
        let tree = AstParser::parse_source(
            std::path::Path::new("dummy.rs"),
            SupportedLanguage::Rust,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Rust);

        let chain = SymbolChain::parse("PathfinderError.hint").unwrap();
        let hit = resolve_symbol_chain(&syms, &chain)
            .expect("impl merging must resolve PathfinderError.hint");
        assert_eq!(hit.name, "hint");
        assert_eq!(hit.kind, SymbolKind::Method);
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
            name_column: 0,
            access_level: crate::surgeon::AccessLevel::Public,
            children: vec![
                ExtractedSymbol {
                    name: "login".to_string(),
                    semantic_path: "AuthService.login".to_string(),
                    kind: SymbolKind::Method,
                    byte_range: 0..10,
                    start_line: 0,
                    end_line: 0,
                    name_column: 0,
                    access_level: crate::surgeon::AccessLevel::Public,
                    children: vec![],
                },
                ExtractedSymbol {
                    name: "login".to_string(),
                    semantic_path: "AuthService.login#2".to_string(),
                    kind: SymbolKind::Method,
                    byte_range: 10..20,
                    start_line: 1,
                    end_line: 1,
                    name_column: 0,
                    access_level: crate::surgeon::AccessLevel::Public,
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

    // ---------------------------------------------------------------
    // E1-J: JSX/TSX Symbol Extraction tests
    // ---------------------------------------------------------------

    #[test]
    fn test_extract_tsx_jsx_elements_in_return() {
        let source = br#"
    export function Greeting({ name }: { name: string }) {
      return (
        <div className="greeting">
          <h1>Hello {name}</h1>
          <Button onClick={() => alert('hi')}>Click</Button>
          <img src="test.png" />
        </div>
      );
    }
    "#;
        let tree = AstParser::parse_source(
            std::path::Path::new("component.tsx"),
            SupportedLanguage::Tsx,
            source,
        )
        .unwrap();

        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Tsx);

        // Should have one function: Greeting
        assert_eq!(syms.len(), 1);
        let greeting = &syms[0];
        assert_eq!(greeting.name, "Greeting");
        assert_eq!(greeting.kind, SymbolKind::Function);

        // Greeting should have JSX children under a "return" container
        assert!(
            !greeting.children.is_empty(),
            "Greeting should have JSX children, got none"
        );

        // Find the root JSX element (div)
        let div = greeting
            .children
            .iter()
            .find(|c| c.name == "div")
            .expect("should find <div> JSX element");
        assert_eq!(div.kind, SymbolKind::HtmlElement);
        assert_eq!(div.semantic_path, "Greeting::return::div");
    }

    #[test]
    fn test_extract_tsx_jsx_self_closing_element() {
        let source = br#"
    export function Avatar() {
      return <img src="test.png" />;
    }
    "#;
        let tree = AstParser::parse_source(
            std::path::Path::new("avatar.tsx"),
            SupportedLanguage::Tsx,
            source,
        )
        .unwrap();

        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Tsx);
        assert_eq!(syms.len(), 1);
        let avatar = &syms[0];
        assert_eq!(avatar.name, "Avatar");

        let img = avatar
            .children
            .iter()
            .find(|c| c.name == "img")
            .expect("should find <img /> self-closing JSX");
        assert_eq!(img.kind, SymbolKind::HtmlElement);
        assert_eq!(img.semantic_path, "Avatar::return::img");
    }

    #[test]
    fn test_extract_tsx_jsx_component_capitalized() {
        let source = br#"
    function App() {
      return (
        <div>
          <Header />
          <Button type="primary">Save</Button>
        </div>
      );
    }
    "#;
        let tree = AstParser::parse_source(
            std::path::Path::new("app.tsx"),
            SupportedLanguage::Tsx,
            source,
        )
        .unwrap();

        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Tsx);
        let app = &syms[0];

        // Components (capitalized) should be SymbolKind::Component
        let header = app
            .children
            .iter()
            .find(|c| c.name == "Header")
            .expect("should find <Header /> component");
        assert_eq!(header.kind, SymbolKind::Component);
        assert_eq!(header.semantic_path, "App::return::Header");

        let button = app
            .children
            .iter()
            .find(|c| c.name == "Button")
            .expect("should find <Button> component");
        assert_eq!(button.kind, SymbolKind::Component);
        assert_eq!(button.semantic_path, "App::return::Button");
    }

    #[test]
    fn test_extract_tsx_arrow_function_returning_jsx() {
        let source = br"const Arrow = () => <span>Hi</span>;";
        let tree = AstParser::parse_source(
            std::path::Path::new("arrow.tsx"),
            SupportedLanguage::Tsx,
            source,
        )
        .unwrap();

        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Tsx);
        assert_eq!(syms.len(), 1);
        let arrow = &syms[0];
        assert_eq!(arrow.name, "Arrow");
        assert_eq!(arrow.kind, SymbolKind::Function);

        let span = arrow
            .children
            .iter()
            .find(|c| c.name == "span")
            .expect("arrow function returning JSX should have span child");
        assert_eq!(span.kind, SymbolKind::HtmlElement);
        assert_eq!(span.semantic_path, "Arrow::return::span");
    }

    #[test]
    fn test_extract_tsx_jsx_duplicate_tags_get_nth_suffix() {
        let source = br"
    function List() {
      return (
        <ul>
          <li>First</li>
          <li>Second</li>
          <li>Third</li>
        </ul>
      );
    }
    ";
        let tree = AstParser::parse_source(
            std::path::Path::new("list.tsx"),
            SupportedLanguage::Tsx,
            source,
        )
        .unwrap();

        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Tsx);
        let list_fn = &syms[0];

        // Collect all "li" children
        let lis: Vec<&ExtractedSymbol> = list_fn
            .children
            .iter()
            .filter(|c| c.name.starts_with("li"))
            .collect();
        assert_eq!(lis.len(), 3, "should find 3 <li> elements");
        assert_eq!(lis[0].name, "li");
        assert_eq!(lis[0].semantic_path, "List::return::li");
        assert_eq!(lis[1].name, "li[2]");
        assert_eq!(lis[1].semantic_path, "List::return::li[2]");
        assert_eq!(lis[2].name, "li[3]");
        assert_eq!(lis[2].semantic_path, "List::return::li[3]");
    }

    #[test]
    fn test_extract_tsx_enclosing_symbol_inside_jsx() {
        // JSX elements should be findable via find_enclosing_symbol
        let source = br"
    function App() {
      return (
        <div>
          <Button>Click</Button>
        </div>
      );
    }
    ";
        let tree = AstParser::parse_source(
            std::path::Path::new("app.tsx"),
            SupportedLanguage::Tsx,
            source,
        )
        .unwrap();

        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Tsx);

        // Line 4 is inside <Button>Click</Button>
        let enclosing = find_enclosing_symbol(&syms, 4);
        assert!(
            enclosing.is_some(),
            "should find enclosing symbol for line inside JSX"
        );
        let path = enclosing.unwrap();
        // Should resolve to either Button itself or App (the function)
        assert!(
            path.contains("App"),
            "enclosing path should include the function name, got: {path}"
        );
    }

    #[test]
    fn test_extract_tsx_non_jsx_function_unchanged() {
        // Regular TS functions (no JSX) should behave identically to before
        let source = br"
    export function add(a: number, b: number): number {
      return a + b;
    }
    ";
        let tree = AstParser::parse_source(
            std::path::Path::new("utils.tsx"),
            SupportedLanguage::Tsx,
            source,
        )
        .unwrap();

        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Tsx);
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "add");
        assert_eq!(syms[0].kind, SymbolKind::Function);
        assert!(
            syms[0].children.is_empty(),
            "non-JSX function should have no JSX children"
        );
    }

    // ---------------------------------------------------------------
    // PATCH-005: Rust pub mod visibility + TypeScript missing node types
    // ---------------------------------------------------------------

    /// PATCH-005-C2: `pub mod` is detected as public (`is_public` = true)
    #[test]
    fn test_extract_rust_pub_mod_is_public() {
        let source = r"
pub mod types {
    pub fn foo() {}
}
";
        let symbols = parse_and_extract(source, SupportedLanguage::Rust);
        let module = symbols
            .iter()
            .find(|s| s.name == "types")
            .expect("types module not found");
        assert_eq!(module.kind, SymbolKind::Module);
        assert_eq!(
            module.access_level,
            crate::surgeon::AccessLevel::Public,
            "pub mod should have access_level = Public"
        );
    }

    /// PATCH-005-C2: Bare `mod` is private (`is_public` = false)
    #[test]
    fn test_extract_rust_private_mod_is_not_public() {
        let source = r"
mod internal {
    fn helper() {}
}
";
        let symbols = parse_and_extract(source, SupportedLanguage::Rust);
        let module = symbols
            .iter()
            .find(|s| s.name == "internal")
            .expect("internal module not found");
        assert_eq!(module.kind, SymbolKind::Module);
        assert_eq!(
            module.access_level,
            crate::surgeon::AccessLevel::Private,
            "bare mod should have access_level = Private"
        );
    }

    /// PATCH-005-C2: `pub(crate) mod` is detected as public
    #[test]
    fn test_extract_rust_pub_crate_mod_is_public() {
        let source = r"
pub(crate) mod types {
    fn foo() {}
}
";
        let symbols = parse_and_extract(source, SupportedLanguage::Rust);
        let module = symbols
            .iter()
            .find(|s| s.name == "types")
            .expect("types module not found");
        assert_eq!(module.kind, SymbolKind::Module);
        assert_eq!(
            module.access_level,
            crate::surgeon::AccessLevel::Package,
            "pub(crate) mod should have access_level = Package"
        );
    }

    /// PATCH-005-C4: TypeScript enum is extracted as `SymbolKind::Enum`
    #[test]
    fn test_extract_typescript_enum() {
        let source = b"enum Direction { Up, Down, Left, Right }";
        let tree = AstParser::parse_source(
            std::path::Path::new("dir.ts"),
            SupportedLanguage::TypeScript,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::TypeScript);
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "Direction");
        assert_eq!(syms[0].kind, SymbolKind::Enum);
    }

    /// PATCH-005-C4: TypeScript abstract class is extracted as `SymbolKind::Class`
    #[test]
    fn test_extract_typescript_abstract_class() {
        let source = b"abstract class Base { abstract doWork(): void; }";
        let tree = AstParser::parse_source(
            std::path::Path::new("base.ts"),
            SupportedLanguage::TypeScript,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::TypeScript);
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "Base");
        assert_eq!(syms[0].kind, SymbolKind::Class);
    }

    /// PATCH-005-C4: TypeScript type alias is extracted as `SymbolKind::Class`
    #[test]
    fn test_extract_typescript_type_alias() {
        let source = b"type Props = { name: string; age: number; }";
        let tree = AstParser::parse_source(
            std::path::Path::new("props.ts"),
            SupportedLanguage::TypeScript,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::TypeScript);
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "Props");
        assert_eq!(syms[0].kind, SymbolKind::Class);
    }

    /// PATCH-005: TypeScript namespace is extracted as `SymbolKind::Module`
    #[test]
    fn test_extract_typescript_namespace() {
        let source = "namespace Auth { export function login() {} }";
        let symbols = parse_and_extract(source, SupportedLanguage::TypeScript);

        assert_eq!(symbols.len(), 1);
        let ns = &symbols[0];
        assert_eq!(ns.name, "Auth");
        assert_eq!(ns.kind, SymbolKind::Module);
        assert_eq!(ns.children.len(), 1);

        let login = &ns.children[0];
        assert_eq!(login.name, "login");
        assert_eq!(login.kind, SymbolKind::Function);
        assert_eq!(login.semantic_path, "Auth.login");
    }

    /// PATCH-005: TypeScript export namespace is extracted as `SymbolKind::Module`
    #[test]
    fn test_extract_typescript_export_namespace() {
        let source = "export namespace Auth { export function login() {} }";
        let symbols = parse_and_extract(source, SupportedLanguage::TypeScript);

        assert_eq!(symbols.len(), 1);
        let ns = &symbols[0];
        assert_eq!(ns.name, "Auth");
        assert_eq!(ns.kind, SymbolKind::Module);
        assert_eq!(ns.children.len(), 1);
        assert!(matches!(
            ns.access_level,
            crate::surgeon::AccessLevel::Public
        ));

        let login = &ns.children[0];
        assert_eq!(login.name, "login");
        assert_eq!(login.kind, SymbolKind::Function);
        assert_eq!(login.semantic_path, "Auth.login");
    }
    #[test]
    fn test_extract_typescript_export_declare_namespace() {
        let source = "export declare namespace Auth { export function login() {} }";
        let symbols = parse_and_extract(source, SupportedLanguage::TypeScript);

        assert_eq!(symbols.len(), 1);
        let ns = &symbols[0];
        assert_eq!(ns.name, "Auth");
        assert_eq!(ns.kind, SymbolKind::Module);
        assert_eq!(ns.children.len(), 1);
        assert!(matches!(
            ns.access_level,
            crate::surgeon::AccessLevel::Public
        ));
    }

    /// PATCH-005-C4: TSX enum is also extracted (verify cross-extension support)
    #[test]
    fn test_extract_tsx_enum() {
        let source = b"export enum Status { Active, Inactive }";
        let tree = AstParser::parse_source(
            std::path::Path::new("status.tsx"),
            SupportedLanguage::Tsx,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Tsx);
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "Status");
        assert_eq!(syms[0].kind, SymbolKind::Enum);
    }

    /// PATCH-009: Verify Python function `name_column` points to function name
    ///
    /// For the line `def compute(x: int) -> int:`:
    /// - Column 0: 'd' in 'def'
    /// - Column 4: 'c' in 'compute'
    /// - `name_column` should be 4 (pointing to 'c' in 'compute', not 'd' in 'def')
    #[test]
    fn test_python_name_column_points_to_function_name() {
        let source = r"

def compute(x: int) -> int:
    return x * 2
";
        let source_bytes = source.as_bytes();
        let tree = AstParser::parse_source(
            std::path::Path::new("compute.py"),
            SupportedLanguage::Python,
            source_bytes,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source_bytes, SupportedLanguage::Python);

        assert_eq!(syms.len(), 1, "should extract one function");
        assert_eq!(syms[0].name, "compute", "function name should be compute");
        assert_eq!(
            syms[0].name_column, 4,
            "name_column should point to 'c' in 'compute' (column 4), not 'd' in 'def' (column 0)"
        );
    }

    // ---------------------------------------------------------------
    // AC-0.9: detect_access_level() — per-language detection rules
    // ---------------------------------------------------------------

    /// AC-0.9 Rust: `pub fn` → `AccessLevel::Public`
    #[test]
    fn test_detect_rust_pub_fn() {
        let source = b"pub fn greet() {}";
        let tree = AstParser::parse_source(
            std::path::Path::new("lib.rs"),
            SupportedLanguage::Rust,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Rust);
        assert_eq!(syms.len(), 1);
        assert_eq!(
            syms[0].access_level,
            crate::surgeon::AccessLevel::Public,
            "pub fn should be Public"
        );
    }

    /// AC-0.9 Rust: `pub(crate) mod` → `AccessLevel::Package`
    #[test]
    fn test_detect_rust_pub_crate_mod() {
        let source = b"pub(crate) mod utils {}";
        let tree = AstParser::parse_source(
            std::path::Path::new("lib.rs"),
            SupportedLanguage::Rust,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Rust);
        assert_eq!(syms.len(), 1);
        assert_eq!(
            syms[0].access_level,
            crate::surgeon::AccessLevel::Package,
            "pub(crate) mod should be Package"
        );
    }

    /// AC-0.9 Rust: `pub(super) fn` → `AccessLevel::Protected`
    #[test]
    fn test_detect_rust_pub_super_fn() {
        let source = b"pub(super) fn helper() {}";
        let tree = AstParser::parse_source(
            std::path::Path::new("lib.rs"),
            SupportedLanguage::Rust,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Rust);
        assert_eq!(syms.len(), 1);
        assert_eq!(
            syms[0].access_level,
            crate::surgeon::AccessLevel::Protected,
            "pub(super) fn should be Protected"
        );
    }

    /// AC-0.9 Rust: bare `fn` (no visibility modifier) → `AccessLevel::Private`
    #[test]
    fn test_detect_rust_private_fn() {
        let source = b"fn internal() {}";
        let tree = AstParser::parse_source(
            std::path::Path::new("lib.rs"),
            SupportedLanguage::Rust,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Rust);
        assert_eq!(syms.len(), 1);
        assert_eq!(
            syms[0].access_level,
            crate::surgeon::AccessLevel::Private,
            "bare fn should be Private"
        );
    }

    /// AC-0.9 Go: uppercase-initial name → `AccessLevel::Public`
    #[test]
    fn test_detect_go_uppercase_function() {
        let source = b"package main\nfunc Export() {}";
        let tree = AstParser::parse_source(
            std::path::Path::new("main.go"),
            SupportedLanguage::Go,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Go);
        let sym = syms.iter().find(|s| s.name == "Export").unwrap();
        assert_eq!(
            sym.access_level,
            crate::surgeon::AccessLevel::Public,
            "Go uppercase fn should be Public"
        );
    }

    /// AC-0.9 Go: lowercase-initial name → `AccessLevel::Package`
    #[test]
    fn test_detect_go_lowercase_function() {
        let source = b"package main\nfunc internal() {}";
        let tree = AstParser::parse_source(
            std::path::Path::new("main.go"),
            SupportedLanguage::Go,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Go);
        let sym = syms.iter().find(|s| s.name == "internal").unwrap();
        assert_eq!(
            sym.access_level,
            crate::surgeon::AccessLevel::Package,
            "Go lowercase fn should be Package"
        );
    }

    /// AC-0.9 Go: `_`-prefixed name → `AccessLevel::Private`
    #[test]
    fn test_detect_go_underscore_function() {
        let source = b"package main\nfunc _hidden() {}";
        let tree = AstParser::parse_source(
            std::path::Path::new("main.go"),
            SupportedLanguage::Go,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Go);
        let sym = syms.iter().find(|s| s.name == "_hidden").unwrap();
        assert_eq!(
            sym.access_level,
            crate::surgeon::AccessLevel::Private,
            "Go _-prefix fn should be Private"
        );
    }

    /// AC-0.9 TypeScript: exported function → `AccessLevel::Public`
    #[test]
    fn test_detect_ts_exported_function() {
        let source = b"export function greet() {}";
        let tree = AstParser::parse_source(
            std::path::Path::new("lib.ts"),
            SupportedLanguage::TypeScript,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::TypeScript);
        let sym = syms.iter().find(|s| s.name == "greet").unwrap();
        assert_eq!(
            sym.access_level,
            crate::surgeon::AccessLevel::Public,
            "exported TS function should be Public"
        );
    }

    /// AC-0.9 TypeScript: non-exported function → `AccessLevel::Package`
    #[test]
    fn test_detect_ts_non_exported_function() {
        let source = b"function helper() {}";
        let tree = AstParser::parse_source(
            std::path::Path::new("lib.ts"),
            SupportedLanguage::TypeScript,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::TypeScript);
        let sym = syms.iter().find(|s| s.name == "helper").unwrap();
        assert_eq!(
            sym.access_level,
            crate::surgeon::AccessLevel::Package,
            "non-exported TS function should be Package"
        );
    }

    /// AC-0.9 TypeScript: `_`-prefixed non-exported function → `AccessLevel::Private`
    #[test]
    fn test_detect_ts_underscore_function() {
        let source = b"function _internal() {}";
        let tree = AstParser::parse_source(
            std::path::Path::new("lib.ts"),
            SupportedLanguage::TypeScript,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::TypeScript);
        let sym = syms.iter().find(|s| s.name == "_internal").unwrap();
        assert_eq!(
            sym.access_level,
            crate::surgeon::AccessLevel::Private,
            "TS _-prefix function should be Private"
        );
    }

    /// AC-0.9 Python: bare name → `AccessLevel::Public`
    #[test]
    fn test_detect_python_public_function() {
        let source = b"def compute(): pass";
        let tree = AstParser::parse_source(
            std::path::Path::new("mod.py"),
            SupportedLanguage::Python,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Python);
        let sym = syms.iter().find(|s| s.name == "compute").unwrap();
        assert_eq!(
            sym.access_level,
            crate::surgeon::AccessLevel::Public,
            "Python bare fn should be Public"
        );
    }

    /// AC-0.9 Python: single-underscore name → `AccessLevel::Protected`
    #[test]
    fn test_detect_python_single_underscore() {
        let source = b"def _helper(): pass";
        let tree = AstParser::parse_source(
            std::path::Path::new("mod.py"),
            SupportedLanguage::Python,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Python);
        let sym = syms.iter().find(|s| s.name == "_helper").unwrap();
        assert_eq!(
            sym.access_level,
            crate::surgeon::AccessLevel::Protected,
            "Python single-underscore fn should be Protected"
        );
    }

    /// AC-0.9 Python: double-underscore non-dunder name → `AccessLevel::Private`
    #[test]
    fn test_detect_python_double_underscore() {
        let source = b"def __secret(): pass";
        let tree = AstParser::parse_source(
            std::path::Path::new("mod.py"),
            SupportedLanguage::Python,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Python);
        let sym = syms.iter().find(|s| s.name == "__secret").unwrap();
        assert_eq!(
            sym.access_level,
            crate::surgeon::AccessLevel::Private,
            "Python __ non-dunder fn should be Private"
        );
    }

    /// AC-0.9 Python: dunder method (`__init__`) → `AccessLevel::Public` (not Private)
    #[test]
    fn test_detect_python_dunder_method() {
        let source = b"def __init__(self): pass";
        let tree = AstParser::parse_source(
            std::path::Path::new("mod.py"),
            SupportedLanguage::Python,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Python);
        let sym = syms.iter().find(|s| s.name == "__init__").unwrap();
        assert_eq!(
            sym.access_level,
            crate::surgeon::AccessLevel::Public,
            "__init__ dunder should be Public, not Private"
        );
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Phase 1 Java Tests
    // ═══════════════════════════════════════════════════════════════════════════

    /// AC-1.3 / AC-1.4: Basic Java class — extracts class with correct kind and
    /// extracts constructor + methods as children. Fields must NOT be extracted
    /// (`constant_kinds` is empty for Java, see §2.1).
    #[test]
    fn test_java_basic_class_symbols() {
        let source = b"package com.example;\n\
public class BasicClass {\n\
    private String name;\n\
    protected int count;\n\
\n\
    public BasicClass(String name) {\n\
        this.name = name;\n\
    }\n\
\n\
    public String getName() { return name; }\n\
    private void helper() {}\n\
    void packageMethod() {}\n\
}\n";
        let tree = AstParser::parse_source(
            std::path::Path::new("BasicClass.java"),
            SupportedLanguage::Java,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Java);

        // Top-level class
        let class = syms.iter().find(|s| s.name == "BasicClass").unwrap();
        assert_eq!(class.kind, crate::surgeon::SymbolKind::Class);
        assert_eq!(class.access_level, crate::surgeon::AccessLevel::Public);

        // Constructor is a child (AC-1.3)
        let ctor = class
            .children
            .iter()
            .find(|s| s.name == "BasicClass")
            .unwrap();
        assert_eq!(ctor.kind, crate::surgeon::SymbolKind::Function);
        assert_eq!(ctor.access_level, crate::surgeon::AccessLevel::Public);

        // Public method (AC-1.3)
        let get_name = class.children.iter().find(|s| s.name == "getName").unwrap();
        assert_eq!(get_name.kind, crate::surgeon::SymbolKind::Function);
        assert_eq!(get_name.access_level, crate::surgeon::AccessLevel::Public);

        // Private method (AC-1.5)
        let helper = class.children.iter().find(|s| s.name == "helper").unwrap();
        assert_eq!(helper.access_level, crate::surgeon::AccessLevel::Private);

        // Package-private method (AC-1.5)
        let pkg_method = class
            .children
            .iter()
            .find(|s| s.name == "packageMethod")
            .unwrap();
        assert_eq!(
            pkg_method.access_level,
            crate::surgeon::AccessLevel::Package
        );

        // Fields must NOT be extracted (constant_kinds empty, see §2.1)
        assert!(
            class
                .children
                .iter()
                .all(|s| s.name != "name" && s.name != "count"),
            "Java fields should not be extracted as symbols"
        );
    }

    /// AC-1.4: Java interface → `SymbolKind::Interface`
    #[test]
    fn test_java_interface_kind() {
        let source = b"public interface Sortable {\n\
    void sort();\n\
    default void printSorted() { sort(); }\n\
}\n";
        let tree = AstParser::parse_source(
            std::path::Path::new("Sortable.java"),
            SupportedLanguage::Java,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Java);

        let iface = syms.iter().find(|s| s.name == "Sortable").unwrap();
        assert_eq!(iface.kind, crate::surgeon::SymbolKind::Interface);
        assert_eq!(iface.access_level, crate::surgeon::AccessLevel::Public);
    }

    /// AC-1.4: Java enum → `SymbolKind::Enum`
    #[test]
    fn test_java_enum_kind() {
        let source = b"public enum Status {\n\
    ACTIVE, INACTIVE;\n\
    public boolean isActive() { return this == ACTIVE; }\n\
}\n";
        let tree = AstParser::parse_source(
            std::path::Path::new("Status.java"),
            SupportedLanguage::Java,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Java);

        let e = syms.iter().find(|s| s.name == "Status").unwrap();
        assert_eq!(e.kind, crate::surgeon::SymbolKind::Enum);
        assert_eq!(e.access_level, crate::surgeon::AccessLevel::Public);

        // Enum method is extracted as a child
        let is_active = e.children.iter().find(|s| s.name == "isActive").unwrap();
        assert_eq!(is_active.kind, crate::surgeon::SymbolKind::Function);
        assert_eq!(is_active.access_level, crate::surgeon::AccessLevel::Public);
    }

    /// AC-1.4: Java record → `SymbolKind::Struct` (Java 16+)
    #[test]
    fn test_java_record_kind() {
        let source = b"public record Point(int x, int y) {\n\
    public double distance() {\n\
        return Math.sqrt(x * x + y * y);\n\
    }\n\
}\n";
        let tree = AstParser::parse_source(
            std::path::Path::new("Point.java"),
            SupportedLanguage::Java,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Java);

        let record = syms.iter().find(|s| s.name == "Point").unwrap();
        assert_eq!(record.kind, crate::surgeon::SymbolKind::Struct);
        assert_eq!(record.access_level, crate::surgeon::AccessLevel::Public);

        // Record method is extracted as a child
        let distance = record
            .children
            .iter()
            .find(|s| s.name == "distance")
            .unwrap();
        assert_eq!(distance.kind, crate::surgeon::SymbolKind::Function);
    }

    /// AC-1.4: Java annotation type → `SymbolKind::Interface`
    #[test]
    fn test_java_annotation_type_kind() {
        let source = b"public @interface MyAnnotation {\n\
    String value();\n\
    int priority() default 0;\n\
}\n";
        let tree = AstParser::parse_source(
            std::path::Path::new("MyAnnotation.java"),
            SupportedLanguage::Java,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Java);

        let annotation = syms.iter().find(|s| s.name == "MyAnnotation").unwrap();
        assert_eq!(annotation.kind, crate::surgeon::SymbolKind::Interface);
        assert_eq!(annotation.access_level, crate::surgeon::AccessLevel::Public);
    }

    /// AC-1.5: All four Java access levels
    #[test]
    fn test_java_access_levels_all_four() {
        let source = b"class Visibility {\n\
    public void pub_method() {}\n\
    protected void prot_method() {}\n\
    private void priv_method() {}\n\
    void pkg_method() {}\n\
}\n";
        let tree = AstParser::parse_source(
            std::path::Path::new("Visibility.java"),
            SupportedLanguage::Java,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Java);

        let cls = syms.iter().find(|s| s.name == "Visibility").unwrap();

        let pub_m = cls
            .children
            .iter()
            .find(|s| s.name == "pub_method")
            .unwrap();
        assert_eq!(pub_m.access_level, crate::surgeon::AccessLevel::Public);

        let prot_m = cls
            .children
            .iter()
            .find(|s| s.name == "prot_method")
            .unwrap();
        assert_eq!(prot_m.access_level, crate::surgeon::AccessLevel::Protected);

        let priv_m = cls
            .children
            .iter()
            .find(|s| s.name == "priv_method")
            .unwrap();
        assert_eq!(priv_m.access_level, crate::surgeon::AccessLevel::Private);

        let pkg_m = cls
            .children
            .iter()
            .find(|s| s.name == "pkg_method")
            .unwrap();
        assert_eq!(pkg_m.access_level, crate::surgeon::AccessLevel::Package);
    }

    /// AC-1.6: Nested/inner classes produce hierarchical symbol trees
    #[test]
    fn test_java_inner_classes_hierarchical() {
        let source = b"public class Outer {\n\
    public class Inner { void innerMethod() {} }\n\
    public static class StaticNested { void nestedMethod() {} }\n\
}\n";
        let tree = AstParser::parse_source(
            std::path::Path::new("Outer.java"),
            SupportedLanguage::Java,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Java);

        let outer = syms.iter().find(|s| s.name == "Outer").unwrap();
        assert_eq!(outer.kind, crate::surgeon::SymbolKind::Class);

        // Inner class is a child of Outer
        let inner = outer.children.iter().find(|s| s.name == "Inner").unwrap();
        assert_eq!(inner.kind, crate::surgeon::SymbolKind::Class);
        assert_eq!(inner.access_level, crate::surgeon::AccessLevel::Public);

        // Inner class method is a child of Inner
        let inner_method = inner
            .children
            .iter()
            .find(|s| s.name == "innerMethod")
            .unwrap();
        assert_eq!(inner_method.kind, crate::surgeon::SymbolKind::Function);

        // Static nested class is also a child of Outer
        let nested = outer
            .children
            .iter()
            .find(|s| s.name == "StaticNested")
            .unwrap();
        assert_eq!(nested.kind, crate::surgeon::SymbolKind::Class);
        let nested_method = nested
            .children
            .iter()
            .find(|s| s.name == "nestedMethod")
            .unwrap();
        assert_eq!(nested_method.kind, crate::surgeon::SymbolKind::Function);
    }

    /// AC-1.7: Anonymous classes are silently skipped (no panic, no garbage symbols).
    ///
    /// The anonymous class itself is not extracted as a named symbol (no `anonymous_class_body`
    /// symbol appears). Methods inside the anonymous class may bubble up as a known side effect
    /// of the recursive extractor, but no crash or empty-name symbol is produced.
    #[test]
    fn test_java_anonymous_class_skipped() {
        // Helper functions must come before statements in test functions
        fn no_empty_names(syms: &[crate::surgeon::ExtractedSymbol]) -> bool {
            syms.iter()
                .all(|s| !s.name.is_empty() && no_empty_names(&s.children))
        }
        fn no_anon_body(syms: &[crate::surgeon::ExtractedSymbol]) -> bool {
            syms.iter()
                .all(|s| s.kind != crate::surgeon::SymbolKind::Class || !s.name.is_empty())
                && syms.iter().all(|s| no_anon_body(&s.children))
        }

        let source = b"public class Outer {\n\
    public class Inner { void innerMethod() {} }\n\
    public static class StaticNested { void nestedMethod() {} }\n\
    Runnable r = new Runnable() { public void run() {} };\n\
}\n";
        let tree = AstParser::parse_source(
            std::path::Path::new("InnerClasses.java"),
            SupportedLanguage::Java,
            source,
        )
        .unwrap();
        // Must not panic
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Java);

        // The Outer class should be present
        assert!(
            syms.iter().any(|s| s.name == "Outer"),
            "Outer class must be extracted"
        );

        // No symbol with empty name should appear anywhere in the tree (AC-1.7: no garbage)
        assert!(no_empty_names(&syms), "No empty-name symbols should exist");

        // The anonymous class body itself must NOT appear as a named container symbol.
        // (Its methods may leak as a known side effect of recursive extraction — acceptable.)
        assert!(
            no_anon_body(&syms),
            "anonymous_class_body must not appear as an extracted Class symbol"
        );
    }

    /// AC-1.3: Generic class extracts correctly (generics don't break name resolution)
    #[test]
    fn test_java_generic_class() {
        let source = b"public class Container<T extends Comparable<T>> {\n\
    private T value;\n\
    public <R> R transform(java.util.function.Function<T, R> fn) {\n\
        return fn.apply(value);\n\
    }\n\
}\n";
        let tree = AstParser::parse_source(
            std::path::Path::new("Container.java"),
            SupportedLanguage::Java,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Java);

        let cls = syms.iter().find(|s| s.name == "Container").unwrap();
        assert_eq!(cls.kind, crate::surgeon::SymbolKind::Class);

        let transform = cls.children.iter().find(|s| s.name == "transform").unwrap();
        assert_eq!(transform.kind, crate::surgeon::SymbolKind::Function);
    }

    /// AC-1.3: module-info.java edge case — no symbols extracted, no panic
    #[test]
    fn test_java_module_info_no_symbols() {
        let source = b"module com.example.app {\n\
    requires java.base;\n\
    exports com.example.api;\n\
}\n";
        let tree = AstParser::parse_source(
            std::path::Path::new("module-info.java"),
            SupportedLanguage::Java,
            source,
        )
        .unwrap();
        // Must not panic
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Java);
        // module declarations are not mapped in module_kinds for Java
        assert!(
            syms.is_empty(),
            "module-info.java should produce zero symbols, got: {syms:?}"
        );
    }

    /// AC-1.3: Sealed class (Java 17+) extracts correctly
    #[test]
    fn test_java_sealed_class() {
        let source = b"public sealed class Shape permits Circle, Rectangle {\n\
    public record Circle(double radius) implements Shape {}\n\
    public record Rectangle(double w, double h) implements Shape {}\n\
}\n";
        let tree = AstParser::parse_source(
            std::path::Path::new("Shape.java"),
            SupportedLanguage::Java,
            source,
        )
        .unwrap();
        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Java);

        let shape = syms.iter().find(|s| s.name == "Shape").unwrap();
        assert_eq!(shape.kind, crate::surgeon::SymbolKind::Class);

        // Inner records are Struct kind
        let circle = shape.children.iter().find(|s| s.name == "Circle").unwrap();
        assert_eq!(circle.kind, crate::surgeon::SymbolKind::Struct);
        let rect = shape
            .children
            .iter()
            .find(|s| s.name == "Rectangle")
            .unwrap();
        assert_eq!(rect.kind, crate::surgeon::SymbolKind::Struct);
    }

    /// BUG-REGRESSION: Impl blocks with lifetimes/generics must merge correctly.
    #[test]
    fn test_impl_block_with_lifetime_generics_merges_correctly() {
        let source = b"struct Context<'a> { data: &'a str }\n\
impl<'a> Context<'a> {\n\
    fn new(data: &'a str) -> Self { Context { data } }\n\
    fn get_data(&self) -> &str { self.data }\n\
}\n";

        let tree = AstParser::parse_source(
            std::path::Path::new("test.rs"),
            SupportedLanguage::Rust,
            source,
        )
        .unwrap();

        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Rust);

        let struct_sym = syms
            .iter()
            .find(|s| s.name == "Context")
            .expect("struct Context should exist with clean name (no '<'a>')");
        assert_eq!(struct_sym.kind, SymbolKind::Struct);

        let has_new = struct_sym.children.iter().any(|s| s.name == "new");
        let has_get_data = struct_sym.children.iter().any(|s| s.name == "get_data");
        assert!(has_new, "method 'new' should be merged under Context");
        assert!(
            has_get_data,
            "method 'get_data' should be merged under Context"
        );

        let new_method = struct_sym
            .children
            .iter()
            .find(|s| s.name == "new")
            .unwrap();
        assert_eq!(
            new_method.semantic_path, "Context.new",
            "semantic path should be 'Context.new', NOT 'Context<'a>.new'"
        );

        let chain = SymbolChain::parse("Context.new").unwrap();
        let resolved = resolve_symbol_chain(&syms, &chain);
        assert!(
            resolved.is_some(),
            "resolve_symbol_chain should find Context.new"
        );

        let no_lifetime_struct = syms.iter().find(|s| s.name.contains('<'));
        assert!(
            no_lifetime_struct.is_none(),
            "No symbol should have '<' or '>' in its name. Found: {:?}",
            no_lifetime_struct.map(|s| &s.name)
        );
    }

    #[test]
    fn test_impl_block_with_multiple_generics() {
        let source = b"struct Pair<K, V> { key: K, value: V }\n\
impl<K, V> Pair<K, V> {\n\
    fn key(&self) -> &K { &self.key }\n\
}\n\
impl Pair<i32, String> {\n\
    fn format(&self) -> String { format!(\"{}: {}\", self.key, self.value) }\n\
}\n";

        let tree = AstParser::parse_source(
            std::path::Path::new("test.rs"),
            SupportedLanguage::Rust,
            source,
        )
        .unwrap();

        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Rust);

        let struct_sym = syms
            .iter()
            .find(|s| s.name == "Pair")
            .expect("struct Pair should exist");

        let method_names: Vec<_> = struct_sym
            .children
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert!(
            method_names.contains(&"key"),
            "key() from generic impl should be merged"
        );
        assert!(
            method_names.contains(&"format"),
            "format() from concrete impl should be merged"
        );
    }

    #[test]
    fn test_impl_block_with_path_qualified_type() {
        let source = b"struct Wrapper<T>(T);\n\
impl<T> std::fmt::Display for Wrapper<T>\n\
where\n\
    T: std::fmt::Display,\n\
{\n\
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {\n\
        write!(f, \"Wrapper({})\", self.0)\n\
    }\n\
}\n";

        let tree = AstParser::parse_source(
            std::path::Path::new("test.rs"),
            SupportedLanguage::Rust,
            source,
        )
        .unwrap();

        let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Rust);

        let wrapper = syms
            .iter()
            .find(|s| s.name == "Wrapper")
            .expect("struct Wrapper should exist");
        assert_eq!(wrapper.kind, SymbolKind::Struct);

        let no_bad_names = syms
            .iter()
            .all(|s| !s.name.contains('<') && !s.name.contains('>'));
        assert!(no_bad_names, "No symbol should have '<' or '>' in name");
    }
}
