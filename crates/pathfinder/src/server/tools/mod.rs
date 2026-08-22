//! Tool handler implementations for the Pathfinder MCP server.
//!
//! Each submodule contains the business logic for a group of related tools.
//! The `impl PathfinderServer` in `server.rs` delegates to these functions,
//! keeping the macro-decorated handler stubs thin and the logic testable.

pub(super) mod inspect;
pub(super) mod navigation;
pub(super) mod semantic_path;
