use tree_sitter::Parser;
fn main() {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_go::LANGUAGE.into()).unwrap();
    let src = "type Storage interface { Create() error }";
    let tree = parser.parse(src, None).unwrap();
    println!("{}", tree.root_node().to_sexp());
}
