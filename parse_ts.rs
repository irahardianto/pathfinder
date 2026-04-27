use tree_sitter::Parser;

fn main() {
    let mut parser = Parser::new();
    parser.set_language(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()).unwrap();

    let source = "export declare namespace Auth { function login(): void; }";
    let tree = parser.parse(source, None).unwrap();
    println!("{}", tree.root_node().to_sexp());
}
