#![allow(clippy::uninlined_format_args)]
use pathfinder_treesitter::surgeon::Surgeon;
use pathfinder_treesitter::TreeSitterSurgeon;
use std::path::PathBuf;

#[tokio::test]
async fn run_it() -> Result<(), Box<dyn std::error::Error>> {
    let surgeon = TreeSitterSurgeon::new(10);
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .ok_or("no parent")?
        .parent()
        .ok_or("no workspace")?
        .to_path_buf();
    let file = PathBuf::from("crates/pathfinder-common/src/types.rs");

    match surgeon.read_source_file(&workspace, &file).await {
        Ok((_, _, lang, syms)) => {
            println!("Success! Lang: {}", lang);
            println!("Symbols: {}", syms.len());
        }
        Err(e) => {
            println!("Error: {:?}", e);
            return Err(e.into());
        }
    }
    Ok(())
}
