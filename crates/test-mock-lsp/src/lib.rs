#![allow(
    clippy::pedantic,
    clippy::missing_errors_doc,
    clippy::cast_possible_truncation,
    clippy::doc_markdown,
    clippy::needless_pass_by_value,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::field_reassign_with_default
)]

//! Mock LSP server for testing the pathfinder-lsp client.
//!
//! This crate provides a scriptable, feature-rich mock implementation of the
//! Language Server Protocol with:
//! - Scriptable response timelines (`--script`)
//! - Position-aware canned index for fixture repos (`--index`)
//! - Server-initiated message emitter (dynamic registrations, progress notifications, config requests)
//! - Handlers for definition, references, implementation, and call hierarchy
//! - Malformed framing injection (`--malform`) for protocol defensibility tests
//! - Position encoding negotiation (`--position-encoding`)

pub mod canned_index;
pub mod config;
pub mod emitter;
pub mod handlers;
pub mod protocol;
pub mod script;

pub use canned_index::{CannedIndex, PositionEntry, PositionKey};
pub use config::MockConfig;
pub use emitter::MessageEmitter;
pub use protocol::{JsonRpcError, JsonRpcMessage, JsonRpcResponse, MalformMode};
pub use script::{ScriptAction, ScriptEngine, ScriptEvaluation, ScriptRule};
