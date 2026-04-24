#!/bin/bash
sed -i 's/unwrap()/expect("parse failed")/g' crates/pathfinder-treesitter/tests/test_impl.rs
sed -i 's/unwrap()/expect("parse failed")/g' crates/pathfinder-treesitter/tests/test_rust_top_level.rs
sed -i 's/c.len_utf16() as u32/c.len_utf16().try_into().unwrap_or(0)/g' crates/pathfinder-lsp/src/client/mod.rs
sed -i 's/as_u64()? as u32/as_u64()?.try_into().unwrap_or(0)/g' crates/pathfinder-lsp/src/client/mod.rs
