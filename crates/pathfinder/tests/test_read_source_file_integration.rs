#![allow(clippy::uninlined_format_args)]
use pathfinder_treesitter::surgeon::Surgeon;
use pathfinder_treesitter::TreeSitterSurgeon;
use std::path::PathBuf;

#[tokio::test]
async fn run_it() {
    let surgeon = TreeSitterSurgeon::new(10);
    let workspace = PathBuf::from("/home/irahardianto/works/projects/pathfinder");
    let file = PathBuf::from("crates/pathfinder-common/src/types.rs");

    match surgeon.read_source_file(&workspace, &file).await {
        Ok((_, _, lang, syms)) => {
            println!("Success! Lang: {}", lang);
            println!("Symbols: {}", syms.len());
        }
        Err(e) => {
            println!("Error: {:?}", e);
            panic!("It errored!");
        }
    }
}
