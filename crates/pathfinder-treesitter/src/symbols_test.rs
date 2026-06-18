use super::*;
use crate::language::SupportedLanguage;
use crate::parser::AstParser;
use pathfinder_common::types::SymbolChain;

#[test]
fn test_is_rust_test_helper() {
    let source = r"
            #[test]
            fn my_test() {}

            fn normal_fn() {}
        ";
    let source_bytes = source.as_bytes();
    let tree = AstParser::parse_source(
        std::path::Path::new("dummy.rs"),
        SupportedLanguage::Rust,
        source_bytes,
    )
    .unwrap();
    let root = tree.root_node();
    let mut fns = Vec::new();
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        if child.kind() == "function_item" {
            fns.push(child);
        }
    }
    assert_eq!(fns.len(), 2);
    assert!(is_rust_test(fns[0], source_bytes, Some("my_test")));
    assert!(!is_rust_test(fns[1], source_bytes, Some("normal_fn")));
}

#[test]
fn test_is_python_test_helper() {
    let source = r"
@pytest.mark.asyncio
def test_foo():
    pass

def normal_fn():
    pass
        ";
    let source_bytes = source.as_bytes();
    let tree = AstParser::parse_source(
        std::path::Path::new("dummy.py"),
        SupportedLanguage::Python,
        source_bytes,
    )
    .unwrap();
    let root = tree.root_node();
    let mut fns = Vec::new();
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        if child.kind() == "function_definition" || child.kind() == "decorated_definition" {
            fns.push(child);
        }
    }
    assert_eq!(fns.len(), 2);
    assert!(is_python_test(fns[0], source_bytes, Some("test_foo")));
    assert!(!is_python_test(fns[1], source_bytes, Some("normal_fn")));
}

#[test]
fn test_is_java_test_helper() {
    let source = r"
            class A {
                @Test
                void testMethod() {}

                void normalMethod() {}

                void testability() {}
            }
        ";
    let source_bytes = source.as_bytes();
    let tree = AstParser::parse_source(
        std::path::Path::new("dummy.java"),
        SupportedLanguage::Java,
        source_bytes,
    )
    .unwrap();
    let root = tree.root_node();
    let class_decl = root.named_child(0).unwrap();
    let class_body = class_decl.child_by_field_name("body").unwrap();
    let mut fns = Vec::new();
    let mut cursor = class_body.walk();
    for child in class_body.named_children(&mut cursor) {
        if child.kind() == "method_declaration" {
            fns.push(child);
        }
    }
    assert_eq!(fns.len(), 3);
    assert!(is_java_test(fns[0], source_bytes, Some("testMethod")));
    assert!(!is_java_test(fns[1], source_bytes, Some("normalMethod")));
    assert!(!is_java_test(fns[2], source_bytes, Some("testability")));
}

#[test]
fn test_parse_css_selector_name_helper() {
    let source = r"
            .my-class {}
            #my-id {}
            div {}
        ";
    let source_bytes = source.as_bytes();
    // CSS uses HTML parser for Vue zone integration or similar, let's use supported language HTML/CSS, but SupportedLanguage is Vue / Go / Rust / Java / Python / TypeScript / Tsx / JavaScript.
    // Wait, SupportedLanguage has Go, Rust, Java, Python, TypeScript, Tsx, JavaScript, Vue.
    // Wait, what SupportedLanguage does CSS use? Let's check SupportedLanguage definition.
    // Oh, let's check SupportedLanguage enum. It has TypeScript, Tsx, JavaScript, Vue, Rust, Go, Python, Java.
    // Let's use SupportedLanguage::Vue which has CSS zones or check how we can parse CSS.
    // Let's see: how is CSS parsed in symbols.rs?
    // extract_style_symbols takes a tree which is tree_sitter::Tree.
    // We can parse it as SupportedLanguage::Vue? No, Vue zones are parsed with tree-sitter-css.
    // Wait! In parser.rs, does AstParser support CSS directly?
    // Let's check parser.rs or SupportedLanguage.
    // Actually, CSS is parsed as part of Vue zones or custom.
    // Let's check if SupportedLanguage has a HTML or CSS parser.
    // In language.rs, let's grep for SupportedLanguage variants.
    // Let's check how SupportedLanguage is defined or just use Vue language, or let's use the HTML/CSS tree-sitter parser directly.
    // Since tree-sitter-css is in Cargo.toml (tree-sitter-css = \"0.25\"), let's see how it's initialized.
    // Let's check parser.rs or just call tree_sitter::Parser directly with tree_sitter_css::LANGUAGE.
    // Yes, that is extremely safe and doesn't rely on SupportedLanguage mapping!
    // Let's write the CSS parsing code:
    // let mut parser = tree_sitter::Parser::new().unwrap();
    // parser.set_language(&tree_sitter_css::LANGUAGE.into()).unwrap();
    // let tree = parser.parse(source, None).unwrap();
    // Wait, let's check if tree_sitter_css is imported. In Cargo.toml, it was `tree-sitter-css = "0.25"`.
    // Let's check if we can import `tree_sitter_css`. Yes, we can just use `tree_sitter_css::LANGUAGE` or `tree_sitter_css::language()`.
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_css::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    let root = tree.root_node();
    let mut selectors = Vec::new();
    let mut cursor = root.walk();
    for rule_set in root.named_children(&mut cursor) {
        if rule_set.kind() == "rule_set" {
            let mut c = rule_set.walk();
            let selectors_node = rule_set
                .named_children(&mut c)
                .find(|n| n.kind() == "selectors")
                .unwrap();
            let mut sel_cursor = selectors_node.walk();
            for sel in selectors_node.named_children(&mut sel_cursor) {
                selectors.push(sel);
            }
        }
    }
    assert_eq!(selectors.len(), 3);
    assert_eq!(
        parse_css_selector_name(selectors[0], source_bytes).unwrap(),
        ".my-class"
    );
    assert_eq!(
        parse_css_selector_name(selectors[1], source_bytes).unwrap(),
        "#my-id"
    );
    assert_eq!(
        parse_css_selector_name(selectors[2], source_bytes).unwrap(),
        "div"
    );
}

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
    let source =
        b"struct MyStruct;\nimpl MyStruct {\n    fn foo(&self) {}\n    fn bar(&mut self) {}\n}\n";
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
    let source =
        b"struct MyStruct;\nimpl MyStruct {\n    fn foo(&self) {}\n    fn bar(&mut self) {}\n}\n";
    let tree = AstParser::parse_source(
        std::path::Path::new("dummy.rs"),
        SupportedLanguage::Rust,
        source,
    )
    .unwrap();
    let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Rust);

    // With impl merging, `MyStruct.foo` should resolve perfectly.
    let chain = SymbolChain::parse("MyStruct.foo").unwrap();
    let hit = resolve_symbol_chain(&syms, &chain).expect("impl merging must resolve MyStruct.foo");
    assert_eq!(hit.name, "foo");
    assert_eq!(hit.kind, SymbolKind::Method);
    assert_eq!(hit.semantic_path, "MyStruct.foo");
}

/// Confirm that the impl-fallback also resolves `bar` (the second method).
#[test]
fn test_resolve_rust_impl_second_method_via_struct_path() {
    let source =
        b"struct MyStruct;\nimpl MyStruct {\n    fn foo(&self) {}\n    fn bar(&mut self) {}\n}\n";
    let tree = AstParser::parse_source(
        std::path::Path::new("dummy.rs"),
        SupportedLanguage::Rust,
        source,
    )
    .unwrap();
    let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Rust);

    let chain = SymbolChain::parse("MyStruct.bar").unwrap();
    let hit = resolve_symbol_chain(&syms, &chain).expect("impl merging must resolve MyStruct.bar");
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

/// Nested Python function: `def outer(): def inner()` → `outer` has child `inner`
/// with `semantic_path` `outer.inner`.
#[test]
fn test_python_nested_function_captured_as_child() {
    let source = "def outer():\n    def inner():\n        pass\n    return inner\n";
    let symbols = parse_and_extract(source, SupportedLanguage::Python);

    let outer = symbols
        .iter()
        .find(|s| s.name == "outer")
        .expect("outer function must be extracted");
    assert_eq!(outer.kind, SymbolKind::Function);

    let inner = outer
        .children
        .iter()
        .find(|c| c.name == "inner")
        .expect("inner function must be a child of outer");
    assert_eq!(inner.kind, SymbolKind::Function);
    assert_eq!(
        inner.semantic_path, "outer.inner",
        "inner function path must be outer.inner"
    );
}

/// Nested JS function: arrow function inside outer function is captured.
#[test]
fn test_js_nested_function_captured_as_child() {
    let source = "function outer() {\n  function inner() { return 1; }\n  return inner;\n}\n";
    let symbols = parse_and_extract(source, SupportedLanguage::JavaScript);

    let outer = symbols
        .iter()
        .find(|s| s.name == "outer")
        .expect("outer function must be extracted");

    let inner = outer
        .children
        .iter()
        .find(|c| c.name == "inner")
        .expect("inner function must be a child of outer");
    assert_eq!(inner.kind, SymbolKind::Function);
    assert_eq!(inner.semantic_path, "outer.inner");
}
