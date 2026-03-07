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

pub mod client;
pub mod error;
pub mod lawyer;
pub mod mock;
pub mod no_op;
pub mod types;

pub use client::LspClient;
pub use error::LspError;
pub use lawyer::Lawyer;
pub use mock::MockLawyer;
pub use no_op::NoOpLawyer;
pub use types::{DefinitionLocation, LspDiagnostic, LspDiagnosticSeverity};

