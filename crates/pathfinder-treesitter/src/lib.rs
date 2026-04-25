//! The Surgeon — Tree-sitter engine for AST-aware operations in Pathfinder.
//!
//! This crate provides the [`Surgeon`] trait and its default implementation,
//! orchestrating tree-sitter parsers, queries, and AST caching for multiple
//! languages. It enables features like `read_symbol_scope` and semantic path
//! resolution.

/// Module for cache functionality.
pub mod cache;
/// Module containing error types and utilities.
/// Module for error types and utilities.
pub mod error;
/// Language detection and support for various programming languages.
pub mod language;
/// Module containing mock implementations for testing.
pub mod mock;
/// The parser module providing parsing capabilities.
pub mod parser;
/// Module for repository mapping.
pub mod repo_map;
/// Provides functionality to manipulate and transform Tree-sitter syntax trees.
pub mod surgeon;
/// Public module for symbol definitions.
pub mod symbols;
/// Provides utilities for surgically manipulating Tree-sitter parse trees.
/// Public module providing Tree-sitter-based surgery utilities.
pub mod treesitter_surgeon;
/// Vue multi-zone parsing utilities.
pub mod vue_zones;

pub use error::SurgeonError;
pub use surgeon::{Surgeon, SurgeonCacheExt};
pub use treesitter_surgeon::TreeSitterSurgeon;
