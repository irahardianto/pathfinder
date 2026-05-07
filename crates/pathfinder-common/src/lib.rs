//! Pathfinder Common — shared types, errors, and infrastructure.
//!
//! This crate provides the foundational building blocks used by all
//! Pathfinder crates:
//!
//! - **Error taxonomy** — standardized MCP error codes (PRD §5)
//! - **Domain types** — `SemanticPath`, `VersionHash`, `WorkspaceRoot`
//! - **Configuration** — zero-config defaults + optional JSON override
//! - **Sandbox** — three-tier file access control
//! - **Git integration** — changed-file detection for repo map filtering

/// The `config` module containing configuration structures and functions.
pub mod config;
/// Module for error types and related functionality.
pub mod error;
/// A module for interacting with Git repositories.
pub mod git;
/// Sandbox module.
pub mod sandbox;
/// Domain types used across Pathfinder crates.
pub mod types;
