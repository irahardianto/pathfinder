use crate::language::SupportedLanguage;
use crate::parser::AstParser;
use crate::symbols::{extract_symbols_from_tree, find_enclosing_symbol};
use std::path::Path;

#[test]
fn test_enclosing_symbol_rust_impl() {
    let source = b"struct MyStruct;\n\nimpl MyStruct {\n    fn method() {\n        // here\n    }\n}\n";
    let tree = AstParser::parse_source(Path::new("test.rs"), SupportedLanguage::Rust, source).unwrap();
    let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Rust);
    
    let found = find_enclosing_symbol(&syms, 4); // "        // here\n"
    assert_eq!(found.as_deref(), Some("MyStruct.method"));
}
