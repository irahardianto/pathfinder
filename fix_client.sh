#!/bin/bash
sed -i 's/        end_line: u32,/        end_line: u32,\n        _original_content: \&str,/g' crates/pathfinder-lsp/src/client/mod.rs

cat << 'INNER_EOF' >> crates/pathfinder-lsp/src/client/mod.rs

    async fn did_change_watched_files(&self, _changes: Vec<crate::FileEvent>) -> Result<(), LspError> {
        Ok(())
    }
INNER_EOF
