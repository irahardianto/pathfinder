# Research Log: Epic 1 — Core Foundation

## Technologies Researched

### 1. rmcp (Official Rust MCP SDK) — v1.1.0
- **Source:** https://docs.rs/rmcp/latest/rmcp/ + https://github.com/modelcontextprotocol/rust-sdk
- **Key patterns:**
  - `ServerHandler` trait — implement to define server behavior
  - `#[tool]` attribute macro — auto-generates tool schema from function signatures
  - `ServiceExt` trait — `.serve(transport)` pattern to start server
  - `transport::io::stdio()` — stdio transport for `(stdin(), stdout())`
  - Feature flags: `server`, `macros`, `transport-io`
  - Dependencies: `tokio`, `serde`, `schemars` (JSON Schema generation)

### 2. Cargo Workspace Layout
- **Source:** `.agent/rules/project-structure-rust-cargo.md`
- **Structure:** 6 crates under `crates/` directory:
  - `pathfinder` — main binary (MCP transport, tool dispatch)
  - `pathfinder-treesitter` — Tree-sitter engine
  - `pathfinder-lsp` — LSP client
  - `pathfinder-search` — Ripgrep integration
  - `pathfinder-edit` — Shadow Editor
  - `pathfinder-common` — Shared types and sandbox

### 3. Error Handling
- **Source:** `.agent/rules/rust-idioms-and-patterns.md`
- Library crates: `thiserror` for typed error enums
- Application crate: `anyhow` for ergonomic chaining
- Never mix — library crates should not depend on `anyhow`

### 4. Async Runtime
- **Source:** `.agent/rules/rust-idioms-and-patterns.md`
- Tokio as the async runtime
- `tokio::fs` instead of `std::fs` inside async functions
- `tokio::task::spawn_blocking` for CPU-heavy work

### 5. File Watching
- **Source:** PRD Section 4.4
- `notify` crate for cross-platform file system events
- Synchronous cache eviction model (no shared mutable state)
- Hash-compare on events — no `expected_writes` HashMap

### 6. Configuration
- **Source:** PRD Section 10
- Zero-config default with auto-detection
- Optional `pathfinder.config.json` for non-standard setups
- `serde` + `serde_json` for config deserialization

## Key Gotchas
- rmcp uses `schemars` v2 for JSON Schema generation — tool parameters must derive `schemars::JsonSchema`
- Sandbox enforcement must be checked before ANY file operation
- All file paths must be resolved relative to workspace root
- `.pathfinderignore` uses `.gitignore` syntax — need `ignore` crate or similar

## Architecture Decision
Using `rmcp` (official Rust MCP SDK) over `rust-mcp-sdk` because:
- Official Anthropic-maintained SDK
- Better `#[tool]` macro support
- More active maintenance (64 releases)
- Aligned with MCP spec 2025-11-25
