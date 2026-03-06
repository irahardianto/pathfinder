# Epic 6: File-Level Operations Research

## Context
Implementing file-level operations (`create_file`, `delete_file`, `read_file`, `write_file`) for the Pathfinder MCP server as specified in PRD v4.6 Section 3.5.

## Key Findings & Constraints

1. **Sandbox Integration:** All write/create/delete operations must check `self.sandbox.check_read` or `check_write`.
2. **Version Hash (OCC):**
   - Read operations return `VersionHash`.
   - Write/Delete operations require `base_version` to match current file hash before mutating.
3. **Atomic Operations:**
   - `create_file` uses `std::fs::OpenOptions::new().write(true).create_new(true)` to prevent TOCTOU races.
   - `write_file` uses `std::fs::write` (in-place) rather than `rename()` to preserve inodes for HMR and watchers.
4. **Validation/Degraded Mode:**
   - Since Epic 4 (LSP) is not yet implemented, `create_file` and `delete_file` will operate in "degraded mode" (Write only, Delete only) without LSP validation.
5. **No Tree-sitter / AST context:**
   - These tools operate purely on raw text, without `search_codebase`'s semantic path features.
6. **Error Taxonomy:**
   - Mapped errors must use `PathfinderError` from `pathfinder-common::error`.

## Action Items
Proceed with implementation of the 4 MCP tools in `server.rs` and verify them with isolated tests using temporary workspaces.
