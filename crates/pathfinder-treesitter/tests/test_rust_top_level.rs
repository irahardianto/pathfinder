#![allow(clippy::unwrap_used)]
use pathfinder_treesitter::language::SupportedLanguage;
use pathfinder_treesitter::parser::AstParser;
use pathfinder_treesitter::symbols::extract_symbols_from_tree;
use std::path::Path;

#[test]
fn test_enclosing_symbol_rust_top_level() {
    let source = "fn test() {}\n";
    let tree = AstParser::parse_source(
        Path::new("test.rs"),
        SupportedLanguage::Rust,
        source.as_bytes(),
    )
    .unwrap();
    let symbols = extract_symbols_from_tree(&tree, source.as_bytes(), SupportedLanguage::Rust);
    assert_eq!(symbols.len(), 1);
}
