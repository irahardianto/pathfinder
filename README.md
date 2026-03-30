<div align="center">
  <h1 align="center">🧭 Pathfinder</h1>

  <p align="center">
    The Headless IDE — an MCP server that gives AI coding agents<br />
    AST-aware code intelligence, safe edits, and LSP validation.
    <br />
    <br />
    <a href="#getting-started">Getting Started</a>
    ·
    <a href="#tools">View Tools</a>
    ·
    <a href="https://github.com/irahardianto/pathfinder/issues">Request Feature</a>
    <br />
    <br />
  </p>
</div>

<!-- ABOUT THE PROJECT -->
## About Pathfinder

**Pathfinder** is an [MCP (Model Context Protocol)](https://modelcontextprotocol.io/) server written in Rust that gives AI coding agents the same capabilities a human developer gets from an IDE — but without a GUI.

Instead of treating source code as flat text, Pathfinder understands your code structurally through **Tree-sitter AST parsing** and semantically through **Language Server Protocol (LSP)** integration. This means AI agents can navigate, search, edit, and validate code at the *symbol* level — functions, classes, methods — rather than fragile line-by-line string matching.

### Why Pathfinder?

Traditional AI coding workflows suffer from:

- **Fragile text edits** — line-based search-and-replace breaks when code shifts.
- **No compile-time feedback** — agents write code blindly, with no way to know if edits introduce errors until tests run.
- **Limited code understanding** — flat text search misses semantic structure (e.g., a search hit in a comment vs. actual code).

Pathfinder solves these problems by providing:

- 🌳 **AST-Aware Operations** — navigate and edit at the symbol level using semantic paths (e.g., `src/auth.ts::AuthService.login`).
- ✅ **LSP Validation** — every edit is validated against a real language server before being written to disk, catching type errors and compilation failures *before* they happen.
- 🔒 **Optimistic Concurrency Control (OCC)** — SHA-256 version hashes prevent conflicting writes and stale-data overwrites.
- 🔍 **Semantic Search** — filter search results by AST context (code-only, comments-only, or all) powered by ripgrep + Tree-sitter.
- 🛡️ **Sandbox Security** — a 3-tier file access model prevents path traversal attacks and unauthorized file access.
- 📊 **Built-in Observability** — per-engine telemetry (`ripgrep_ms`, `tree_sitter_parse_ms`, `lsp_ms`) and optional `--lsp-trace` for raw JSON-RPC debugging.

### Key Features

- 🛠️ **16 MCP Tools** — covering code navigation, semantic editing, file operations, search, and impact analysis.
- 🌐 **6 Languages** — native Tree-sitter support for Go, TypeScript, TSX, JavaScript, Python, and Rust.
- 🏗️ **5 Rust Crates** — modular workspace architecture for clean separation of concerns.
- ⚡ **Zero Configuration** — auto-detects languages and LSP servers in your workspace.
- 🧪 **Shadow Editor** — a validation pipeline that catches introduced errors by diffing LSP diagnostics before and after each edit.

<!-- GETTING STARTED -->
## Getting Started

### Prerequisites

- **Rust toolchain** (1.75+ recommended) — [Install via rustup](https://rustup.rs/)
- **An MCP-compatible AI client** — such as [Antigravity](https://antigravity.dev/), Claude Desktop, Cursor, or any tool supporting MCP stdio transport.
- **(Optional) Language servers** — for LSP validation support (e.g., `gopls` for Go, `typescript-language-server` for TS/JS, `rust-analyzer` for Rust, `pyright` for Python).

### Installation

> **Note:** Pre-built binaries are planned for future releases. For now, you need to build from source.

**Build from source:**

```sh
# Clone the repository
git clone https://github.com/irahardianto/pathfinder.git
cd pathfinder

# Build in release mode
cargo build --release

# The binary will be at target/release/pathfinder
```

**Verify the installation:**

```sh
./target/release/pathfinder --help
```

### Configuration

#### MCP Client Configuration

Add Pathfinder to your MCP client's server configuration. The exact format depends on your client.

**Example (JSON config for most MCP clients):**

```json
{
  "mcpServers": {
    "pathfinder": {
      "command": "/path/to/pathfinder",
      "args": ["/path/to/your/workspace"]
    }
  }
}
```

**With LSP trace enabled (for debugging):**

```json
{
  "mcpServers": {
    "pathfinder": {
      "command": "/path/to/pathfinder",
      "args": ["--lsp-trace", "/path/to/your/workspace"]
    }
  }
}
```

#### CLI Usage

```
pathfinder [OPTIONS] <WORKSPACE_PATH>

Arguments:
  <WORKSPACE_PATH>  Path to the workspace root directory

Options:
      --lsp-trace  Enable raw LSP JSON-RPC tracing to stderr (DEBUG level)
  -h, --help       Print help
  -V, --version    Print version
```

Pathfinder communicates over **stdio** using the MCP protocol. Logs are emitted as structured JSON to **stderr** (since stdout is reserved for MCP transport).

<!-- TOOLS -->
## Tools

Pathfinder exposes 16 tools organized into three categories. Every tool operates within the workspace sandbox and returns structured JSON responses.

### 🔍 Search & Navigation

| Tool | Description |
|---|---|
| `search_codebase` | Search for text patterns (literal or regex) with AST-aware filtering (`code_only`, `comments_only`, `all`). Returns matching lines with context and enclosing semantic paths. |
| `get_repo_map` | Generate a structural skeleton of the project — an indented tree of classes, functions, and type signatures with semantic path annotations. Token-budgeted for LLM context windows. |
| `read_symbol_scope` | Extract the exact source code of a single symbol (function, class, method) by its semantic path. Returns code, line range, and version hash. |
| `read_with_deep_context` | Read a symbol's source code **plus** the signatures of all functions it calls. Ideal for understanding dependencies before editing. |
| `get_definition` | Jump to where a symbol is defined. Provide a semantic path to a reference and get the definition's file, line, and a code preview. |
| `analyze_impact` | Find all callers of a symbol (incoming) and all symbols it calls (outgoing). Essential for understanding the blast radius before refactoring. |

### ✏️ AST-Aware Editing

All edit tools use the **Shadow Editor** validation pipeline — edits are validated against the LSP before being written to disk. Every edit requires a `base_version` (SHA-256 hash) for optimistic concurrency control.

| Tool | Description |
|---|---|
| `replace_body` | Replace the internal logic of a block-scoped construct (function, method, class body), keeping the signature intact. |
| `replace_full` | Replace an entire declaration including its signature, body, decorators, and doc comments. |
| `insert_before` | Insert new code before a target symbol. Use a bare file path (without `::`) to insert at the top of a file. |
| `insert_after` | Insert new code after a target symbol. Use a bare file path (without `::`) to append to the bottom of a file. |
| `delete_symbol` | Delete a symbol and all its associated decorators, attributes, and doc comments. |
| `validate_only` | Dry-run an edit without writing to disk. Pre-check risky changes and get the same validation results as a real edit. |

### 📁 File Operations

| Tool | Description |
|---|---|
| `read_file` | Read raw file content with pagination (`start_line`, `max_lines`). Best for configuration files (YAML, TOML, Dockerfile). For source code, prefer `read_symbol_scope`. |
| `write_file` | Write to configuration files. Supports full replacement or surgical search-and-replace via a `replacements` array. **Not for source code** — use the AST-aware edit tools instead. |
| `create_file` | Create a new file with initial content. Parent directories are created automatically. |
| `delete_file` | Delete a file. Requires `base_version` (OCC) to prevent deleting a file modified after you last read it. |

<!-- ARCHITECTURE -->
## Architecture

Pathfinder is structured as a Rust workspace with 5 crates, each with a clear responsibility:

```
pathfinder/
├── crates/
│   ├── pathfinder/              # MCP server, CLI, tool routing
│   │   └── src/
│   │       ├── main.rs          # CLI entry point (clap)
│   │       └── server/
│   │           ├── server.rs    # MCP tool router
│   │           ├── types.rs     # Parameter & response types
│   │           ├── helpers.rs   # Shared utilities
│   │           └── tools/       # One module per tool category
│   │               ├── search.rs
│   │               ├── edit.rs
│   │               ├── navigation.rs
│   │               ├── file_ops.rs
│   │               ├── repo_map.rs
│   │               ├── symbols.rs
│   │               └── diagnostics.rs
│   │
│   ├── pathfinder-common/       # Shared types, errors, config, sandbox
│   ├── pathfinder-treesitter/   # The Surgeon — AST parsing & symbol extraction
│   ├── pathfinder-search/       # The Scout — ripgrep-powered code search
│   └── pathfinder-lsp/          # The Lawyer — LSP client & lifecycle management
│
├── docs/
│   ├── requirements/            # PRD and specifications
│   ├── research_logs/           # Design decisions and research
│   └── audits/                  # Code audit findings
│
├── Cargo.toml                   # Workspace manifest
├── LICENSE                      # MIT License
└── README.md
```

### The Three Engines

Pathfinder internally delegates work to three specialized engines, each abstracted behind a trait for testability:

| Engine | Crate | Trait | Responsibility |
|---|---|---|---|
| **The Surgeon** | `pathfinder-treesitter` | `Surgeon` | AST parsing, symbol extraction, semantic path resolution, repo map generation |
| **The Scout** | `pathfinder-search` | `Scout` | Ripgrep-powered full-text search with Tree-sitter enrichment for AST-aware filtering |
| **The Lawyer** | `pathfinder-lsp` | `Lawyer` | LSP process lifecycle, edit validation (Shadow Editor), go-to-definition |

Each engine can be mocked independently for unit testing, and the server gracefully degrades when an engine is unavailable (e.g., falls back to Tree-sitter heuristics when no LSP is running).

### Core Concepts

#### Semantic Paths

Pathfinder identifies code symbols using semantic paths — a human-readable notation that mirrors how developers think about code structure:

```
src/auth.ts::AuthService.login          # Method
src/utils/math.go::CalculateDiscount    # Function
lib/models.py::User                     # Class
```

Format: `<relative_file_path>::<Symbol>[.<Method>]`

#### Optimistic Concurrency Control (OCC)

Every file read returns a `version_hash` (SHA-256 digest of the file content). Edit and delete operations require this hash as `base_version` — if the file has changed since you last read it, the operation is rejected. This prevents conflicting writes in multi-agent environments.

#### The Shadow Editor

For AST-aware edits, Pathfinder runs a "validation sandwich":

1. `didOpen` — notify LSP of original content
2. `pull_diagnostics` — capture baseline errors
3. `didChange` — notify LSP of proposed edit
4. `pull_diagnostics` — capture post-edit errors
5. Revert — restore LSP state
6. **Diff** — compare pre/post errors using a multiset algorithm that's resilient to line shifts

If new errors are **introduced**, the edit fails by default (overridable with `ignore_validation_failures`).

### Supported Languages

| Language | Extension(s) | Tree-sitter | LSP (Auto-detected) |
|---|---|---|---|
| Go | `.go` | ✅ | `gopls` |
| TypeScript | `.ts` | ✅ | `typescript-language-server` |
| TSX | `.tsx` | ✅ | `typescript-language-server` |
| JavaScript | `.js`, `.jsx` | ✅ | `typescript-language-server` |
| Python | `.py` | ✅ | `pyright` |
| Rust | `.rs` | ✅ | `rust-analyzer` |

> **Note:** Tree-sitter support works out of the box (no external tools needed). LSP support requires the respective language server to be installed and available on your `PATH`.

<!-- OBSERVABILITY -->
## Observability

Pathfinder emits structured JSON logs to stderr with per-engine timing breakdowns:

```json
{
  "timestamp": "2026-03-31T05:30:00Z",
  "level": "INFO",
  "message": "search_codebase completed",
  "ripgrep_ms": 12,
  "tree_sitter_parse_ms": 45,
  "total_matches": 23,
  "duration_ms": 62
}
```

Enable `--lsp-trace` for full JSON-RPC request/response logging at DEBUG level — useful for diagnosing LSP communication issues.

<!-- SECURITY -->
## Security

Pathfinder implements a **3-tier sandbox model**:

| Tier | Type | What It Blocks |
|---|---|---|
| **Tier 1** | Hardcoded Deny *(cannot be overridden)* | `.git/objects/`, `.git/HEAD`, `*.pem`, `*.key`, `*.pfx` — security-critical paths |
| **Tier 2** | Default Deny *(overridable via config)* | `.env`, `node_modules/`, `vendor/`, `dist/`, `build/`, `__pycache__/` |
| **Tier 3** | User-Defined | Patterns in `.pathfinderignore` (gitignore syntax) |

- All file paths are canonicalized and validated before any I/O operation.
- Path traversal attacks (e.g., `../../etc/passwd`) are rejected at Tier 1.
- The `WorkspaceRoot` type enforces that only valid, existing directories are accepted as workspace roots.
- Tier 2 patterns can be selectively overridden via `SandboxConfig.allow_override`; additional deny patterns can be added via `SandboxConfig.additional_deny`.

<!-- ROADMAP -->
## Roadmap

- [x] Core MCP server with stdio transport
- [x] Tree-sitter-powered AST parsing (Go, TypeScript, TSX, JavaScript, Python, Rust)
- [x] Ripgrep search with AST-aware filtering
- [x] Full suite of AST-aware edit tools with OCC
- [x] LSP integration with Shadow Editor validation
- [x] LSP lifecycle management (auto-start, crash recovery, idle termination)
- [x] 3-tier sandbox security model
- [x] Per-engine observability and telemetry
- [ ] Pre-built binaries for easy installation
- [ ] Additional language support (Java, C/C++, C#, etc.)
- [ ] Configuration file for custom LSP commands and settings

<!-- CONTRIBUTING -->
## Contributing

Contributions are welcome! Pathfinder follows strict engineering practices:

1. Fork the repository
2. Create your feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'feat: add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

### Development

```sh
# Run tests
cargo test --workspace

# Run with clippy (pedantic + deny unwrap)
cargo clippy --workspace --all-targets

# Format
cargo fmt --all -- --check
```

> The workspace enforces `clippy::pedantic`, `deny(unwrap_used)`, and `deny(unsafe_code)`.

<!-- LICENSE -->
## License

Distributed under the MIT License. See the [LICENSE](LICENSE) file for details.

---

<p align="center">
  Built with 🦀 in Rust
</p>
