//! Mock LSP server for testing the pathfinder-lsp client.
//!
//! This crate provides a mock implementation of the Language Server Protocol
//! that can be used to test the LSP client without requiring real language servers.

pub mod config;
pub mod handlers;
pub mod protocol;
