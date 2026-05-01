# Research Log: Epic 4 — The Lawyer (LSP Client)

> **Update (2026-05-01):** Rust LSP remediation is complete and the architecture
> is now language-agnostic. The column-1 fix, empty probe, and didOpen lifecycle
> apply to all languages. New languages need: `detect.rs` entry + tree-sitter
> grammar (Python has both). Diagnostics strategy (pull vs push) is the remaining
> gap for full cross-language parity. See
> `docs/requirements/patches/20260501/ARCHITECTURE.md` for the full architecture reference.

## Date: 2026-03-07

## Context

Epic 4 implements the LSP integration layer. It enables 3 remaining stub tools
(`get_definition`, `read_with_deep_context`, `analyze_impact`) plus wires LSP
validation into the Shadow Editor (stories 5.9–5.11).

**Milestone 1 scope (this session):** `Lawyer` trait + `MockLawyer` + degraded-mode
`get_definition` (Tree-sitter heuristic fallback when no LSP).

## Technologies Researched

### 1. lsp-types crate

- **Source:** docs.rs/lsp-types, web search
- Key types in use:
  - `GotoDefinitionParams` / `GotoDefinitionResponse` — request+response for `textDocument/definition`
  - `Diagnostic` — error/warning with range, severity, code, source
  - `DiagnosticServerCapabilities` / `DiagnosticOptions` — capability flags for pull diagnostics
  - `DocumentDiagnosticReport` — response to `textDocument/diagnostic`
  - `Position { line: u32, character: u32 }` — 0-indexed line+column
  - `Location { uri: Url, range: Range }` — file URL + byte range
  - `ServerCapabilities` — parsed on initialize response

### 2. JSON-RPC over stdio (Custom LSP Client)

Per PRD §2, the LSP client is custom ~500-800 LOC. The PRD specifies JSON-RPC
over stdio to child LSP processes. Architecture:

```
PathfinderServer
  └── Lawyer (trait)
        └── LspClient (production impl — Milestone 2)
              ├── tokio::process::Command (spawn LSP child)
              ├── BufWriter<ChildStdin>  (outgoing messages)
              ├── BufReader<ChildStdout> (incoming messages)
              └── tokio::sync::oneshot  (request → response correlation)
```

**LSP wire format:**
```
Content-Length: <N>\r\n
\r\n
{"jsonrpc":"2.0","id":1,"method":"textDocument/definition","params":{...}}
```

**Key pattern:** Message correlation via `id` field. Request sends oneshot sender,
response dispatcher matches `id` and fires the oneshot receiver.

### 3. Existing Crate Patterns (Scout/Surgeon consistency)

From reviewing `searcher.rs` and `surgeon.rs`:
- Trait defined in its own file (testability boundary doc comment)
- `#[async_trait::async_trait]` for async methods
- `Arc<dyn Trait>` in `PathfinderServer`
- `Mock*` struct uses `Arc<Mutex<Option<Result<_, _>>>>` for thread-safe test config
- Errors: `thiserror` enum in same crate as trait; maps to `PathfinderError` via `From`

### 4. Degraded Mode Strategy (Milestone 1)

PRD §8 specifies graceful degradation when LSP unavailable:

| Tool                     | Degraded Behavior                               |
| ------------------------ | ----------------------------------------------- |
| `get_definition`         | `NOT_SUPPORTED` (LSP required; no TS heuristic) |
| `read_with_deep_context` | Tree-sitter scope only (no context appendix)    |
| `analyze_impact`         | Tree-sitter outgoing only                       |

For Milestone 1, all 3 tools return structured "no LSP" responses with
`"degraded": true` — better than stub panics. The `NoOpLawyer` implements this.

### 5. PathfinderServer Constructor Pattern

From `server.rs`:
- `new(workspace_root, config)` — creates production engines
- `with_engines(workspace_root, config, sandbox, scout, surgeon)` — test injection

The `lawyer` field slot follows the same constructor injection pattern.

## Architecture Decisions

### Decision: NoOpLawyer for degraded mode

Instead of an `Option<Arc<dyn Lawyer>>`, use a `NoOpLawyer` that always returns
`Err(LspError::NoLspAvailable)`. This keeps the `lawyer` field non-optional and
avoids Option checks at every call site. The `NoOpLawyer` is the production default
until a real LSP client is configured.

**Rationale:** Matches the pattern where `MockSurgeon` returns configured errors.
Keeps tool handlers simple — they just map `LspError::NoLspAvailable` to the
appropriate degraded response.

### Decision: Separate `pathfinder-lsp` crate

Mirrors the `pathfinder-treesitter` boundary. The `pathfinder-lsp` crate owns:
- `Lawyer` trait
- `LspError` enum
- `MockLawyer` test double
- (Milestone 2) `LspClient` production implementation

The `pathfinder` binary crate depends on `pathfinder-lsp` but never on LSP
implementation details — only on the trait.

## Sources

- PRD v4.6 §4–§6 (LSP lifecycle, capability detection, graceful degradation §8)
- `pathfinder-search/src/searcher.rs` — Scout trait pattern
- `pathfinder-treesitter/src/surgeon.rs` — Surgeon trait pattern
- `pathfinder/src/server.rs` — constructor pattern, `Arc<dyn Trait>` usage
- Web search: lsp-types, Rust LSP client 2025, JSON-RPC stdio
- Relying on training data for JSON-RPC framing details and lsp-types API
