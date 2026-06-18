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
    let mut symbols = Vec::with_capacity(64);
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

        // Extract nested function definitions inside function bodies.
        // This captures inner functions in Python (`def outer(): def inner():`)
        // and JavaScript/TypeScript closures. Rust/Go/Java don't have common
        // inner-function patterns at the symbol level, so we skip them to
        // avoid recursing into every function body unnecessarily.
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
fn is_rust_test(node: Node<'_>, source: &[u8], func_name: Option<&str>) -> bool {
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

fn is_python_test(node: Node<'_>, source: &[u8], func_name: Option<&str>) -> bool {
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

fn is_java_test(node: Node<'_>, source: &[u8], func_name: Option<&str>) -> bool {
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
/// Returns `(unique_name, suffix)` where suffix is "#N" for N>1 or empty for first occurrence.
fn make_unique_name(
    name_counts: &mut std::collections::HashMap<String, usize>,
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
        let mut s = String::with_capacity(unique_name.len() + suffix.len());
        s.push_str(&unique_name);
        s.push_str(&suffix);
        s
    } else {
        let cap = parent_path.len() + 1 + unique_name.len() + suffix.len();
        let mut s = String::with_capacity(cap);
        s.push_str(parent_path);
        s.push('.');
        s.push_str(&unique_name);
        s.push_str(&suffix);
        s
    };

    // Collect all child function_items from the impl body as Method symbols.
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

fn merge_rust_impl_blocks_recursive(syms: &mut Vec<ExtractedSymbol>) {
    let mut extracted_methods: std::collections::HashMap<String, Vec<ExtractedSymbol>> =
        std::collections::HashMap::new();
    let mut impl_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    // 1. Remove all Impl blocks and extract their children
    syms.retain_mut(|s| {
        if s.kind == SymbolKind::Impl {
            let entry = extracted_methods.entry(s.name.clone()).or_default();
            for mut method in std::mem::take(&mut s.children) {
                // Update method's semantic path to be under the struct instead of the Impl
                // Impl blocks have `#` suffix, we want it under the Struct which doesn't
                if let Some((parent_path, method_name)) = method.semantic_path.rsplit_once('.') {
                    // strip #[0-9]+ from the end of the parent path
                    let clean_parent = match parent_path.rfind('#') {
                        Some(idx) if parent_path[idx + 1..].chars().all(|c| c.is_ascii_digit()) => {
                            &parent_path[..idx]
                        }
                        _ => parent_path,
                    };
                    method.semantic_path = format!("{clean_parent}.{method_name}");
                }
                entry.push(method);
            }

            let clean_name = s.name.split('#').next().unwrap_or(&s.name);
            let count = impl_counts.entry(clean_name.to_string()).or_insert(0);
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
            merge_rust_impl_blocks_recursive(&mut s.children);
        }
    }
}

/// Merge Rust Impl methods directly under their associated Struct/Enum/Interface symbols.
/// This prevents `SYMBOL_NOT_FOUND` when tools target methods using `MyStruct.method` instead
/// of distinguishing between multiple Impl blocks.
fn merge_rust_impl_blocks(symbols: &mut Vec<ExtractedSymbol>) {
    merge_rust_impl_blocks_recursive(symbols);
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

    // Phase 1: short-circuit on exact match — avoids O(n) Levenshtein scan
    if let Some(exact) = all_paths.iter().find(|p| **p == target) {
        return vec![(*exact).to_string()];
    }

    // Phase 2: fuzzy match
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
        0,
        &mut symbols,
        &mut tag_counts,
    );

    symbols
}

fn process_html_child_element(
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
        crate::surgeon::SymbolKind::Component
    } else {
        crate::surgeon::SymbolKind::HtmlElement
    };

    let count = tag_counts.entry(name.to_owned()).or_insert(0);
    *count += 1;
    let nth = *count;
    let sym_name = if nth == 1 {
        name.to_owned()
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

    let should_emit = is_component || depth < 3;

    if should_emit {
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
        walk_html_elements_flat(child, source, &sym_path, depth + 1, out, tag_counts);
    } else {
        // Recurse into children
        walk_html_elements_flat(child, source, parent_path, depth + 1, out, tag_counts);
    }
}

/// Recursive HTML element walker that flattens elements into a single list.
fn walk_html_elements_flat(
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

        let count = tag_counts.entry(name.clone()).or_insert(0);
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
                let count = at_counts.entry(at_name.clone()).or_insert(0);
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

fn parse_css_selector_name(selector: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    match selector.kind() {
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
        let name_opt = parse_css_selector_name(selector, source);

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

#[cfg(test)]
#[path = "vue_multizone_tests.rs"]
mod vue_multizone_tests;

#[cfg(test)]
#[path = "symbols_test.rs"]
mod tests;
