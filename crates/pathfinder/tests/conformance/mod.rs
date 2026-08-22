//! Conformance suite module for Pathfinder.
//!
//! Exposes the table-driven conformance harness, assertion implementations,
//! fixture descriptors, and matrix evaluation for language support tiers.

#![allow(unused_imports)]

pub mod assertions;
pub mod fixtures;
pub mod runner;
pub mod types;

pub use assertions::*;
pub use fixtures::LanguageFixture;
pub use runner::run_conformance_suite;
pub use types::{AssertionId, AssertionStatus, ConformanceMatrix, LanguageId, LanguageTier};
