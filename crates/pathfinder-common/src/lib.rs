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

/// The `config` module containing configuration structures and functions.
pub mod config;
/// Module for error types and related functionality.
pub mod error;
/// Provides functionality to watch files for changes.
pub mod file_watcher;
/// A module for interacting with Git repositories.
pub mod git;
/// Module containing utilities for indenting text.
pub mod indent;
/// Module for normalization utilities.
pub mod normalize;
/// Sandbox module.
pub mod sandbox;
/// Domain types used across Pathfinder crates.
pub mod types;
