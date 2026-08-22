//! Impl block whose type is declared elsewhere (`types.rs`).

use crate::types::ExternalConfig;

/// Trait for formatting summaries.
pub trait Summarize {
    fn summarize(&self) -> String;
}

impl Summarize for ExternalConfig {
    fn summarize(&self) -> String {
        format!("timeout={}, retries={}", self.timeout_seconds, self.max_retries)
    }
}
