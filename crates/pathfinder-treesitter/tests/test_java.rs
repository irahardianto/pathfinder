//! Integration tests for Phase 1: Tree-sitter Java Support.
//!
//! Each test loads a real Java fixture file and asserts on the symbol tree
//! produced by `extract_symbols_from_tree`.  Fixture files live in
//! `tests/fixtures/`.
//!
//! Acceptance criteria references follow the Phase 1 spec (§2 of the
//! Java-support requirements).

use pathfinder_treesitter::{
    language::SupportedLanguage,
    parser::AstParser,
    surgeon::{AccessLevel, SymbolKind},
    symbols::extract_symbols_from_tree,
};
use std::path::Path;

// ─── helpers ────────────────────────────────────────────────────────────────

fn parse_fixture(filename: &str, source: &[u8]) -> Vec<pathfinder_treesitter::surgeon::ExtractedSymbol> {
    let tree = AstParser::parse_source(Path::new(filename), SupportedLanguage::Java, source)
        .unwrap_or_else(|e| panic!("failed to parse {filename}: {e:?}"));
    extract_symbols_from_tree(&tree, source, SupportedLanguage::Java)
}

fn no_empty_names(syms: &[pathfinder_treesitter::surgeon::ExtractedSymbol]) -> bool {
    syms.iter()
        .all(|s| !s.name.is_empty() && no_empty_names(&s.children))
}

// ─── BasicClass.java ─────────────────────────────────────────────────────────

/// AC-1.3 / AC-1.4 / AC-1.5: Basic class with constructor, methods, and fields.
/// Fields must NOT be extracted. All four access levels verified.
#[test]
fn test_fixture_basic_class() {
    let source = include_bytes!("fixtures/BasicClass.java");
    let syms = parse_fixture("BasicClass.java", source);

    let class = syms
        .iter()
        .find(|s| s.name == "BasicClass")
        .expect("BasicClass must be extracted");

    assert_eq!(class.kind, SymbolKind::Class, "BasicClass should be Class");
    assert_eq!(class.access_level, AccessLevel::Public, "BasicClass should be Public");

    // Constructor
    assert!(
        class.children.iter().any(|s| s.name == "BasicClass" && s.kind == SymbolKind::Function),
        "Constructor BasicClass() must be present"
    );

    // Public method
    assert!(
        class.children.iter().any(|s| s.name == "getName" && s.access_level == AccessLevel::Public),
        "getName() must be Public"
    );

    // Private method
    assert!(
        class.children.iter().any(|s| s.name == "helper" && s.access_level == AccessLevel::Private),
        "helper() must be Private"
    );

    // Package-private method
    assert!(
        class.children.iter().any(|s| s.name == "packageMethod" && s.access_level == AccessLevel::Package),
        "packageMethod() must be Package"
    );

    // Fields must NOT be extracted (AC-1.3)
    assert!(
        class.children.iter().all(|s| s.name != "name" && s.name != "count"),
        "Java fields must not be extracted as symbols"
    );
    assert!(no_empty_names(&syms), "no empty-name symbols");
}

// ─── InterfaceWithDefaults.java ───────────────────────────────────────────────

/// AC-1.4: Interface with abstract + default methods.
#[test]
fn test_fixture_interface_with_defaults() {
    let source = include_bytes!("fixtures/InterfaceWithDefaults.java");
    let syms = parse_fixture("InterfaceWithDefaults.java", source);

    let iface = syms
        .iter()
        .find(|s| s.name == "Sortable")
        .expect("Sortable interface must be extracted");

    assert_eq!(iface.kind, SymbolKind::Interface, "Sortable should be Interface");
    assert_eq!(iface.access_level, AccessLevel::Public, "Sortable should be Public");
    assert!(no_empty_names(&syms), "no empty-name symbols");
}

// ─── RecordExample.java ───────────────────────────────────────────────────────

/// AC-1.4: Record (Java 16+) → SymbolKind::Struct.
#[test]
fn test_fixture_record_example() {
    let source = include_bytes!("fixtures/RecordExample.java");
    let syms = parse_fixture("RecordExample.java", source);

    let record = syms
        .iter()
        .find(|s| s.name == "Point")
        .expect("Point record must be extracted");

    assert_eq!(record.kind, SymbolKind::Struct, "Point should be Struct (record)");
    assert_eq!(record.access_level, AccessLevel::Public, "Point should be Public");

    let distance = record
        .children
        .iter()
        .find(|s| s.name == "distance")
        .expect("distance() must be a child of Point");
    assert_eq!(distance.kind, SymbolKind::Function);
    assert!(no_empty_names(&syms), "no empty-name symbols");
}

// ─── SealedHierarchy.java ─────────────────────────────────────────────────────

/// AC-1.3: Sealed class hierarchy (Java 17+).
#[test]
fn test_fixture_sealed_hierarchy() {
    let source = include_bytes!("fixtures/SealedHierarchy.java");
    let syms = parse_fixture("SealedHierarchy.java", source);

    let shape = syms
        .iter()
        .find(|s| s.name == "Shape")
        .expect("Shape sealed class must be extracted");

    assert_eq!(shape.kind, SymbolKind::Class, "Shape should be Class");

    // Inner records are Struct
    let circle = shape.children.iter().find(|s| s.name == "Circle").expect("Circle");
    assert_eq!(circle.kind, SymbolKind::Struct, "Circle record should be Struct");

    let rect = shape.children.iter().find(|s| s.name == "Rectangle").expect("Rectangle");
    assert_eq!(rect.kind, SymbolKind::Struct, "Rectangle record should be Struct");
    assert!(no_empty_names(&syms), "no empty-name symbols");
}

// ─── EnumWithMethods.java ─────────────────────────────────────────────────────

/// AC-1.4: Enum → SymbolKind::Enum; methods extracted as children.
#[test]
fn test_fixture_enum_with_methods() {
    let source = include_bytes!("fixtures/EnumWithMethods.java");
    let syms = parse_fixture("EnumWithMethods.java", source);

    let e = syms
        .iter()
        .find(|s| s.name == "Status")
        .expect("Status enum must be extracted");

    assert_eq!(e.kind, SymbolKind::Enum, "Status should be Enum");
    assert_eq!(e.access_level, AccessLevel::Public, "Status should be Public");

    let is_active = e
        .children
        .iter()
        .find(|s| s.name == "isActive")
        .expect("isActive() must be a child of Status");
    assert_eq!(is_active.kind, SymbolKind::Function);
    assert_eq!(is_active.access_level, AccessLevel::Public);
    assert!(no_empty_names(&syms), "no empty-name symbols");
}

// ─── AnnotationType.java ─────────────────────────────────────────────────────

/// AC-1.4: Annotation type (`@interface`) → SymbolKind::Interface.
#[test]
fn test_fixture_annotation_type() {
    let source = include_bytes!("fixtures/AnnotationType.java");
    let syms = parse_fixture("AnnotationType.java", source);

    let annotation = syms
        .iter()
        .find(|s| s.name == "MyAnnotation")
        .expect("MyAnnotation must be extracted");

    assert_eq!(annotation.kind, SymbolKind::Interface, "annotation type should be Interface");
    assert_eq!(annotation.access_level, AccessLevel::Public, "MyAnnotation should be Public");
    assert!(no_empty_names(&syms), "no empty-name symbols");
}

// ─── InnerClasses.java ────────────────────────────────────────────────────────

/// AC-1.6: Nested/inner classes → hierarchical tree. AC-1.7: anonymous class skipped.
#[test]
fn test_fixture_inner_classes() {
    let source = include_bytes!("fixtures/InnerClasses.java");
    let syms = parse_fixture("InnerClasses.java", source);

    let outer = syms
        .iter()
        .find(|s| s.name == "Outer")
        .expect("Outer class must be extracted");

    assert_eq!(outer.kind, SymbolKind::Class);

    let inner = outer.children.iter().find(|s| s.name == "Inner").expect("Inner");
    assert_eq!(inner.kind, SymbolKind::Class);
    assert_eq!(inner.access_level, AccessLevel::Public);

    // Inner method is a child of Inner (AC-1.6)
    assert!(
        inner.children.iter().any(|s| s.name == "innerMethod"),
        "innerMethod() must be a child of Inner"
    );

    let nested = outer.children.iter().find(|s| s.name == "StaticNested").expect("StaticNested");
    assert_eq!(nested.kind, SymbolKind::Class);

    // No empty-name symbols (AC-1.7: anonymous class guard)
    assert!(no_empty_names(&syms), "no empty-name symbols (anonymous class must not produce garbage)");
}

// ─── GenericClass.java ────────────────────────────────────────────────────────

/// AC-1.3: Generic type parameters don't break name resolution.
#[test]
fn test_fixture_generic_class() {
    let source = include_bytes!("fixtures/GenericClass.java");
    let syms = parse_fixture("GenericClass.java", source);

    let cls = syms
        .iter()
        .find(|s| s.name == "Container")
        .expect("Container must be extracted");

    assert_eq!(cls.kind, SymbolKind::Class);

    let transform = cls.children.iter().find(|s| s.name == "transform").expect("transform");
    assert_eq!(transform.kind, SymbolKind::Function);
    assert!(no_empty_names(&syms), "no empty-name symbols");
}

// ─── LambdaExpressions.java ───────────────────────────────────────────────────

/// Edge case: lambda expressions in field initializers must not crash the extractor.
/// (Fields are not extracted, so lambdas inside them are silently ignored.)
#[test]
fn test_fixture_lambda_expressions() {
    let source = include_bytes!("fixtures/LambdaExpressions.java");
    let syms = parse_fixture("LambdaExpressions.java", source);

    // The class itself is extracted
    assert!(
        syms.iter().any(|s| s.name == "LambdaExample"),
        "LambdaExample class must be extracted"
    );
    // No empty-name symbols from anonymous lambdas
    assert!(no_empty_names(&syms), "no empty-name symbols from lambda expressions");
}

// ─── ModuleInfo.java ──────────────────────────────────────────────────────────

/// AC-1.3: `module-info.java` edge case — no symbols extracted, no panic.
#[test]
fn test_fixture_module_info() {
    let source = include_bytes!("fixtures/ModuleInfo.java");
    let syms = parse_fixture("module-info.java", source);

    assert!(
        syms.is_empty(),
        "module-info.java should produce zero symbols, got: {syms:?}"
    );
}

// ─── PackageInfo.java ─────────────────────────────────────────────────────────

/// Edge case: `package-info.java` — contains only package declaration and
/// package-level annotations. No class declarations should result in no symbols.
///
/// Unlike `module-info.java` which has a `module` declaration (not in class_kinds),
/// `package-info.java` has no type declarations at all. The recursive extractor
/// should iterate the AST without panicking and return empty symbol list.
#[test]
fn test_fixture_package_info() {
    let source = include_bytes!("fixtures/PackageInfo.java");
    let syms = parse_fixture("package-info.java", source);

    assert!(
        syms.is_empty(),
        "package-info.java should produce zero symbols (no class declarations), got: {syms:?}"
    );
}

// ─── MultipleTopLevel.java ─────────────────────────────────────────────────────

/// Edge case: Multiple top-level classes in one Java file.
///
/// Java allows multiple package-private top-level types in a single file
/// (although only one public class matching the filename is allowed).
///
/// The extractor iterates `named_children()` of the root node. Each
/// `class_declaration`, `enum_declaration`, `interface_declaration` at
/// the root level should be extracted independently.
#[test]
fn test_fixture_multiple_top_level() {
    let source = include_bytes!("fixtures/MultipleTopLevel.java");
    let syms = parse_fixture("MultipleTopLevel.java", source);

    // Assert 4 top-level symbols: FirstClass, SecondInterface, ThirdEnum, FourthClass
    let class_count = syms
        .iter()
        .filter(|s| s.kind == SymbolKind::Class)
        .count();
    let iface_count = syms
        .iter()
        .filter(|s| s.kind == SymbolKind::Interface)
        .count();
    let enum_count = syms
        .iter()
        .filter(|s| s.kind == SymbolKind::Enum)
        .count();

    assert_eq!(
        class_count, 2,
        "should find 2 top-level classes (FirstClass, FourthClass), got: {}",
        syms.iter().filter(|s| s.kind == SymbolKind::Class).map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")
    );
    assert_eq!(iface_count, 1, "should find 1 top-level interface (SecondInterface)");
    assert_eq!(enum_count, 1, "should find 1 top-level enum (ThirdEnum)");
    assert_eq!(syms.len(), 4, "should have 4 top-level symbols total");

    // All should be package-private (no public modifier on any)
    for sym in &syms {
        assert_eq!(
            sym.access_level, AccessLevel::Package,
            "{} should be Package access (no modifier)",
            sym.name
        );
    }

    // Verify FirstClass
    let first = syms.iter().find(|s| s.name == "FirstClass").expect("FirstClass");
    assert!(
        first.children.iter().any(|s| s.name == "firstMethod"),
        "FirstClass should contain firstMethod()"
    );

    // Verify ThirdEnum with its method
    let third = syms.iter().find(|s| s.name == "ThirdEnum").expect("ThirdEnum");
    assert_eq!(third.kind, SymbolKind::Enum);
    assert!(
        third.children.iter().any(|s| s.name == "thirdMethod"),
        "ThirdEnum should contain thirdMethod()"
    );

    // Verify FourthClass has a nested inner class
    let fourth = syms.iter().find(|s| s.name == "FourthClass").expect("FourthClass");
    let nested_inside = fourth
        .children
        .iter()
        .find(|s| s.name == "NestedInside");
    assert!(nested_inside.is_some(), "FourthClass should contain NestedInside inner class");
    assert!(
        nested_inside.unwrap().children.iter().any(|s| s.name == "nestedMethod"),
        "NestedInside should contain nestedMethod()"
    );

    assert!(no_empty_names(&syms), "no empty-name symbols");
}

// ─── Large Generated File (Performance Regression Guard) ─────────────────────

/// Performance regression guard: very large Java file with many methods.
///
/// Requirements edge-case checklist: "Very large Java file (10000+ lines
/// — perf test, should complete in <1s".
///
/// This test programmatically generates a Java class with:
/// - 200 private fields
/// - 200 getter methods (public)
/// - 200 setter methods (public)
///
/// Total: 600+ symbols to extract. If extraction accidentally becomes O(n²)
/// (e.g., repeated linear searches), this test will take noticeably longer
/// or fail by timeout.
///
/// Purpose:
/// 1. Validate no panic on large inputs
/// 2. Validate correct symbol count (regression guard)
/// 3. Implicit performance check (if O(n²), it won't complete quickly)
#[test]
fn test_large_generated_class() {
    let method_count: usize = 200;

    // Build class header
    let mut java_source = String::from(
        "package com.example.perf;\n\n\
         /// Performance test class — auto-generated with many methods.\n\
         public class PerfTest {\n"
    );

    // Add fields
    for i in 0..method_count {
        java_source.push_str(&format!("    private int field{};\n", i));
    }
    java_source.push_str("\n");

    // Add getters
    for i in 0..method_count {
        java_source.push_str(&format!(
            "    public int getField{}() {{ return field{}; }}\n",
            i, i
        ));
    }
    java_source.push_str("\n");

    // Add setters
    for i in 0..method_count {
        java_source.push_str(&format!(
            "    public void setField{}(int value) {{ field{} = value; }}\n",
            i, i
        ));
    }

    // Close class
    java_source.push_str("}\n");

    // Parse and extract
    let syms = parse_fixture("PerfTest.java", java_source.as_bytes());

    // Should have exactly 1 top-level symbol: the class
    assert_eq!(syms.len(), 1);
    let class = &syms[0];
    assert_eq!(class.name, "PerfTest");
    assert_eq!(class.kind, SymbolKind::Class);
    assert_eq!(class.access_level, AccessLevel::Public);

    // Verify child count: getters + setters = 2 * method_count
    // (Fields are NOT extracted because constant_kinds is empty for Java)
    let method_children: Vec<_> = class
        .children
        .iter()
        .filter(|s| s.kind == SymbolKind::Function)
        .collect();

    assert_eq!(
        method_children.len(),
        2 * method_count,
        "Expected {} methods ({} getters + {} setters), got {}. \n\
         Children count: {}.\n\
         First 10: {}",
        2 * method_count,
        method_count,
        method_count,
        method_children.len(),
        class.children.len(),
        class
            .children
            .iter()
            .take(10)
            .map(|s| format!("{}:{:?}", s.name, s.kind))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // All methods should be Public
    for m in &method_children {
        assert_eq!(
            m.access_level, AccessLevel::Public,
            "Method {} should be Public (has public keyword)",
            m.name
        );
    }

    // Verify a few specific getters/setters exist
    assert!(
        class.children.iter().any(|s| s.name == "getField0"),
        "Should have getField0"
    );
    assert!(
        class.children.iter().any(|s| s.name == "setField0"),
        "Should have setField0"
    );
    let last_idx = method_count - 1;
    assert!(
        class
            .children
            .iter()
            .any(|s| s.name == format!("getField{}", last_idx)),
        "Should have getField{}",
        last_idx
    );
    assert!(
        class
            .children
            .iter()
            .any(|s| s.name == format!("setField{}", last_idx)),
        "Should have setField{}",
        last_idx
    );

    assert!(no_empty_names(&syms), "no empty-name symbols");
}
