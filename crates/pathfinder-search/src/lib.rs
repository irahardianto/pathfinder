//! Pathfinder Search — The Scout Engine.
//!
//! Provides Ripgrep-powered text search for the `search_codebase` MCP tool.
//!
//! # Architecture
//!
//! - [`Scout`] — testability trait (I/O boundary)
//! - [`RipgrepScout`] — production implementation using the `grep-*` crate family
//! - [`MockScout`] — test double for unit testing consumers
//! - [`types`] — `SearchParams`, `SearchMatch`, `SearchResult`

/// Mock module for testing purposes
pub mod mock;
/// The `ripgrep` module provides functionality for fast text searching using the ripgrep engine.
pub mod ripgrep;
/// Public module for searcher functionality.
pub mod searcher;
/// Module containing type definitions
pub mod types;

pub use mock::MockScout;
pub use ripgrep::RipgrepScout;
pub use searcher::Scout;
pub use types::{SearchMatch, SearchParams, SearchResult};
