#!/bin/bash
sed -i 's/fn test_enclosing_symbol_rust_impl() {/fn test_enclosing_symbol_rust_impl() -> Result<(), Box<dyn std::error::Error>> {/g' crates/pathfinder-treesitter/tests/test_impl.rs
sed -i 's/.expect("parse failed");/?;/g' crates/pathfinder-treesitter/tests/test_impl.rs
echo "Ok(()) }" >> crates/pathfinder-treesitter/tests/test_impl.rs
sed -i 's/}$//' crates/pathfinder-treesitter/tests/test_impl.rs # remove old closing brace, wait, I'll just use perl

perl -i -pe 's/fn test_enclosing_symbol_rust_impl\(\) \{/fn test_enclosing_symbol_rust_impl\(\) -> Result<\(\), Box<dyn std::error::Error>> \{/' crates/pathfinder-treesitter/tests/test_impl.rs
perl -i -pe 's/\.expect\("parse failed"\)/?/' crates/pathfinder-treesitter/tests/test_impl.rs
perl -i -pe 's/^\}$/    Ok\(\)\n\}/' crates/pathfinder-treesitter/tests/test_impl.rs

perl -i -pe 's/fn test_rust_top_level_declarations\(\) \{/fn test_rust_top_level_declarations\(\) -> Result<\(\), Box<dyn std::error::Error>> \{/' crates/pathfinder-treesitter/tests/test_rust_top_level.rs
perl -i -pe 's/\.expect\("parse failed"\)/?/' crates/pathfinder-treesitter/tests/test_rust_top_level.rs
perl -i -pe 's/^\}$/    Ok\(\)\n\}/' crates/pathfinder-treesitter/tests/test_rust_top_level.rs
