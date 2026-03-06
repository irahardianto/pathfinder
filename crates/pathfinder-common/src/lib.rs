//! Pathfinder Common — shared types, errors, and infrastructure.
//!
//! This crate provides the foundational building blocks used by all
//! Pathfinder crates:
//!
//! - **Error taxonomy** — standardized MCP error codes (PRD §5)
//! - **Domain types** — `SemanticPath`, `VersionHash`, `WorkspaceRoot`
//! - **Configuration** — zero-config defaults + optional JSON override
//! - **Sandbox** — three-tier file access control
//! - **File watcher** — external change detection for cache eviction

pub mod config;
pub mod error;
pub mod file_watcher;
pub mod indent;
pub mod normalize;
pub mod sandbox;
pub mod types;
