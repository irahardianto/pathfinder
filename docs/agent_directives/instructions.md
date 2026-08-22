# Pathfinder MCP Server — Agent Instructions

## Semantic Paths

All symbol-level tools (`locate`, `trace`, `inspect`) require semantic paths in `file_path::symbol` format:

- ✅ `src/auth.ts::AuthService.login`
- ✅ `crates/pathfinder/src/server.rs::PathfinderServer.new`
- ❌ `AuthService.login` (missing file path)

Symbol segments are separated by `.` (e.g. `ClassName.method_name`).

## Response Format & Contracts

Pathfinder v6 tools return responses through two synchronized channels:
- **Text content** — the formatted human-readable output (source slices, call hierarchies, reference locations). Always read this.
- **Structured content** — typed metadata (`status`, `confidence`, counts, flags, pagination). Use for programmatic decisions.

### Tool Status & Readiness (Contract 1)
- `Ok`: Normal operational success.
- `NotReady`: LSP indexing/warmup in progress. Check `retry_after_secs` and retry with `retry_attempt = attempt + 1`.
- `Degraded`: Operating with heuristic fallbacks (tree-sitter/ripgrep).
- `Unavailable`: Language server unavailable.

### Confidence & Evidence Tri-State (Contract 2 & 3)
- `Verified`: Compiler / LSP confirmed truth.
- `Heuristic`: Best-effort tree-sitter or ripgrep fallback (marked `[HEURISTIC]`).
- **Tri-State Rule**:
  - `null` = **UNKNOWN** (not verified — callers/callees/references may exist, do NOT treat as zero).
  - `[]` = **CONFIRMED ZERO** (LSP verified that exactly zero references/callers exist).

## Tool Selection Quick Guide

| I want to... | Tool | Parameters |
|---|---|---|
| Jump to a definition | `locate` | `semantic_path="path/to/file.ext::SymbolName"` |
| Convert file:line to semantic path | `locate` | `file="path/to/file.ext"`, `line=42` |
| Batch jump to definitions | `locate` | `locations=[{semantic_path: "..."}, {file: "...", line: 42}]` |
| Read a symbol's source code | `inspect` | `semantic_path="path/to/file.ext::SymbolName"` |
| Inspect symbol + dependencies | `inspect` | `semantic_path="..."`, `include_dependencies=true` |
| Batch inspect symbols | `inspect` | `semantic_paths=["...", "..."]` |
| Trace callers & callees | `trace` | `semantic_path="..."`, `scope="callers"` |
| Trace all references (usages/imports) | `trace` | `semantic_path="..."`, `scope="references"` |
| Full symbol overview | `trace` | `semantic_path="..."`, `scope="overview"` |

## Fallback

If Pathfinder tools are unavailable or return `ToolStatus::Unavailable`, fall back gracefully to built-in host tools (Read, Grep, Glob).
