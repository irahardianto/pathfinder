#![allow(clippy::unwrap_used)]
use pathfinder_treesitter::language::SupportedLanguage;
use pathfinder_treesitter::parser::AstParser;
use pathfinder_treesitter::symbols::{extract_symbols_from_tree, find_enclosing_symbol};
use std::path::Path;

#[test]
fn test_extract_rust_impl_symbols() {
    let source = "impl Test { fn test() {} }\n";
    let tree = AstParser::parse_source(
        Path::new("test.rs"),
        SupportedLanguage::Rust,
        source.as_bytes(),
    )
    .unwrap();
    let symbols = extract_symbols_from_tree(&tree, source.as_bytes(), SupportedLanguage::Rust);
    assert_eq!(symbols.len(), 2); // The impl and the fn
}

#[test]
fn test_enclosing_symbol_rust_impl() {
    let source =
        b"struct MyStruct;\n\nimpl MyStruct {\n    fn method() {\n        // here\n    }\n}\n";
    let tree =
        AstParser::parse_source(Path::new("test.rs"), SupportedLanguage::Rust, source).unwrap();
    let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Rust);

    let found = find_enclosing_symbol(&syms, 4); // "        // here\n" (0-indexed row 4)
    assert_eq!(found.as_deref(), Some("MyStruct.method"));
}
