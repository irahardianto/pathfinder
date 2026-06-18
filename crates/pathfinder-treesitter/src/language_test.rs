use super::*;

#[test]
fn test_detect_language() {
    assert_eq!(
        SupportedLanguage::detect(Path::new("main.go")),
        Some(SupportedLanguage::Go)
    );
    assert_eq!(
        SupportedLanguage::detect(Path::new("app.ts")),
        Some(SupportedLanguage::TypeScript)
    );
    assert_eq!(
        SupportedLanguage::detect(Path::new("app.tsx")),
        Some(SupportedLanguage::Tsx)
    );
    assert_eq!(
        SupportedLanguage::detect(Path::new("script.js")),
        Some(SupportedLanguage::JavaScript)
    );
    assert_eq!(
        SupportedLanguage::detect(Path::new("script.py")),
        Some(SupportedLanguage::Python)
    );
    assert_eq!(
        SupportedLanguage::detect(Path::new("lib.rs")),
        Some(SupportedLanguage::Rust)
    );

    assert_eq!(
        SupportedLanguage::detect(Path::new("App.vue")),
        Some(SupportedLanguage::Vue)
    );

    assert_eq!(SupportedLanguage::detect(Path::new("text.txt")), None);
    assert_eq!(SupportedLanguage::detect(Path::new("Makefile")), None);
    assert_eq!(SupportedLanguage::detect(Path::new(".gitignore")), None);

    // AC-1.1 / AC-1.2: Java detection
    assert_eq!(
        SupportedLanguage::detect(Path::new("Main.java")),
        Some(SupportedLanguage::Java)
    );
    assert_eq!(
        SupportedLanguage::detect(Path::new("src/com/example/Service.java")),
        Some(SupportedLanguage::Java)
    );
}

#[test]
fn test_grammar_loads_successfully() {
    // Just verify these don't panic or return invalid grammars
    let _go = SupportedLanguage::Go.grammar();
    let _ts = SupportedLanguage::TypeScript.grammar();
    let _py = SupportedLanguage::Python.grammar();
    let _vue = SupportedLanguage::Vue.grammar();
}

/// AC-1.1: Java grammar loads without panic.
#[test]
fn test_grammar_java_loads_successfully() {
    let _java = SupportedLanguage::Java.grammar();
}

/// AC-1.1: Java `as_str` returns "java".
#[test]
fn test_java_as_str() {
    assert_eq!(SupportedLanguage::Java.as_str(), "java");
}

/// AC-1.1: Java `node_types` returns correct function and class kinds.
#[test]
fn test_java_node_types() {
    let nt = SupportedLanguage::Java.node_types();
    assert!(nt.function_kinds.contains(&"method_declaration"));
    assert!(nt.function_kinds.contains(&"constructor_declaration"));
    assert!(nt.class_kinds.contains(&"class_declaration"));
    assert!(nt.class_kinds.contains(&"interface_declaration"));
    assert!(nt.class_kinds.contains(&"enum_declaration"));
    assert!(nt.class_kinds.contains(&"record_declaration"));
    assert!(nt.class_kinds.contains(&"annotation_type_declaration"));
    assert!(
        nt.constant_kinds.is_empty(),
        "Java constant_kinds must be empty (see §2.1)"
    );
    assert!(nt.impl_kinds.is_empty(), "Java impl_kinds must be empty");
    assert!(
        nt.module_kinds.is_empty(),
        "Java module_kinds must be empty"
    );
}

#[test]
fn test_extract_vue_script_basic() {
    let sfc = b"<template><div>Hello</div></template>\n<script>\nexport default {}\n</script>\n";
    let result = extract_vue_script(sfc);
    // 2 newlines before script block (one after </template>, one after <script>)
    assert!(!result.is_empty());
    let text = std::str::from_utf8(&result).unwrap();
    assert!(text.contains("export default {}"));
    // Should start with padded spaces matching bytes in <template> section
    assert!(text.starts_with(' '));
}

#[test]
fn test_extract_vue_script_setup() {
    let sfc = b"<template><p>Hello</p></template>\n<script setup lang=\"ts\">\nconst count = ref(0)\n</script>\n";
    let result = extract_vue_script(sfc);
    let text = std::str::from_utf8(&result).unwrap();
    assert!(text.contains("const count = ref(0)"));
}

#[test]
fn test_extract_vue_script_no_script_block() {
    let sfc = b"<template><p>Template only</p></template>\n";
    let result = extract_vue_script(sfc);
    // No script block -> returns empty (parser creates valid empty AST)
    assert!(result.is_empty() || std::str::from_utf8(&result).unwrap().trim().is_empty());
}
