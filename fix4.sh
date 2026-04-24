#!/bin/bash
sed -i 's/-> Result<(), Box<dyn std::error::Error>>//g' crates/pathfinder-treesitter/tests/test_impl.rs
sed -i 's/-> Result<(), Box<dyn std::error::Error>>//g' crates/pathfinder-treesitter/tests/test_rust_top_level.rs
sed -i 's/?;/unwrap();/g' crates/pathfinder-treesitter/tests/test_impl.rs
sed -i 's/?;/unwrap();/g' crates/pathfinder-treesitter/tests/test_rust_top_level.rs

perl -i -ne 'print unless /Ok\(\(\)\)/' crates/pathfinder-treesitter/tests/test_impl.rs
perl -i -ne 'print unless /Ok\(/' crates/pathfinder-treesitter/tests/test_rust_top_level.rs

