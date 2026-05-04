//! Pathfinder LSP client — the Lawyer engine.
//!
//! This crate provides the [`Lawyer`] trait (the testability boundary for
//! all LSP operations) and supporting types.
//!
//! # Module Layout
//!
//! - [`error`]  — error types for LSP operations
//! - [`types`]  — result types returned by the Lawyer trait
//! - [`lawyer`] — the `Lawyer` trait itself
//! - [`client`] — `LspClient` production implementation
//! - [`mock`]   — `MockLawyer` test double
//! - [`no_op`]  — `NoOpLawyer` for graceful degradation when no LSP is configured

/// Client module.
pub mod client;
/// Module for error handling and definitions.
pub mod error;
/// Module for legal functionalities.
/// The `lawyer` module providing legal-related functionality.
pub mod lawyer;
/// Mock implementation for testing.
pub mod mock;
/// The `no_op` module provides no-operation stub implementations.
pub mod no_op;

/// LT-2: Language Plugin trait — per-language behaviour abstraction.
pub mod plugin;

/// Module containing type definitions for the language server protocol.
pub mod types;

pub use client::LspClient;
pub use error::LspError;
pub use lawyer::Lawyer;
pub use mock::MockLawyer;
pub use no_op::NoOpLawyer;
pub use types::{DefinitionLocation, LspDiagnostic, LspDiagnosticSeverity};

pub use plugin::{
    all_plugins, plugin_for_extension, plugin_for_language, GoPlugin, LanguagePlugin, LspCandidate,
    PythonPlugin, RustPlugin, TypeScriptPlugin,
};
