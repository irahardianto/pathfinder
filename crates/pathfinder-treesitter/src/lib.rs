//! The Surgeon — Tree-sitter engine for AST-aware operations in Pathfinder.
//!
//! This crate provides the [`Surgeon`] trait and its default implementation,
//! orchestrating tree-sitter parsers, queries, and AST caching for multiple
//! languages. It enables features like `read_symbol_scope` and semantic path
//! resolution.

pub mod cache;
pub mod error;
pub mod language;
pub mod mock;
pub mod parser;
pub mod repo_map;
pub mod surgeon;
pub mod symbols;
pub mod treesitter_surgeon;
pub mod vue_zones;

pub use error::SurgeonError;
pub use surgeon::{Surgeon, SurgeonCacheExt};
pub use treesitter_surgeon::TreeSitterSurgeon;
