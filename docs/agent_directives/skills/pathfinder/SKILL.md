---
name: pathfinder
description: "Workflows and protocols for Pathfinder v6 semantic navigation tools (locate, trace, inspect). Covers: semantic addressing, dual-channel response model, tri-state evidence, budget controls, and error recovery."
---

# Pathfinder v6 Skill

## Tool Surface

Pathfinder v6 exposes exactly 3 precision navigation tools:

| Tool | Purpose | Key Parameters |
|---|---|---|
| `locate` | Jump to definition, batch locate definitions, or resolve file:line to semantic path | `semantic_path`, `locations`, `file`, `line` |
| `trace` | Callers/callees hierarchy, references lookup, and symbol overview | `semantic_path`, `scope` (`callers` | `references` | `overview`), `max_depth`, `max_references` |
| `inspect` | Extract symbol source, batch read symbols, and analyze outgoing dependencies | `semantic_path`, `semantic_paths`, `include_dependencies`, `max_dependencies` |

## Semantic Paths

All symbol-level tools require semantic paths in `file_path::symbol_chain` format:
- `src/auth.ts::AuthService.login`
- `crates/pathfinder/src/server.rs::PathfinderServer.new`

Symbol segments are separated by `.` (e.g. `ClassName.method_name`).

## Response Model & Contracts

Every tool returns dual-channel output:
- **Text channel**: Formatted human-readable output (source blocks, call hierarchies, reference locations).
- **Structured channel**: Typed JSON metadata with `status`, `confidence`, `evidence`, and diagnostic indicators.

### Tool Status & Readiness (Contract 1)
- `ToolStatus::Ok`: Normal operational success with verified compilation/LSP results.
- `ToolStatus::NotReady`: LSP indexing/warmup in progress. Check `retry_after_secs` and retry with `retry_attempt = attempt + 1`.
- `ToolStatus::Degraded`: Fallback mode (tree-sitter / ripgrep).
- `ToolStatus::Unavailable`: Language server unavailable.

### Tri-State Certainty & Evidence (Contracts 2 & 3)
- `Verified`: Compiler / LSP confirmed truth.
- `Heuristic`: Best-effort tree-sitter or ripgrep fallback (marked `[HEURISTIC]`).
- **Tri-State Rule**:
  - `null` = **UNKNOWN** (degraded / unverified — callers/callees/references may exist, do NOT treat as zero).
  - `[]` = **CONFIRMED ZERO** (LSP verified that exactly zero references/callers exist).

## Quick Reference

| I want to... | Tool & Parameters |
|---|---|
| Jump to a definition | `locate(semantic_path="src/auth.ts::AuthService.login")` |
| Batch jump to definitions | `locate(locations=[{semantic_path: "..."}, {file: "...", line: 42}])` |
| Convert file:line to semantic path | `locate(file="src/auth.ts", line=42)` |
| Read symbol source code | `inspect(semantic_path="src/auth.ts::AuthService.login")` |
| Inspect symbol + dependencies | `inspect(semantic_path="...", include_dependencies=true)` |
| Batch read symbols | `inspect(semantic_paths=["...", "..."])` (max 10) |
| Trace callers & callees | `trace(semantic_path="...", scope="callers")` |
| Trace all references | `trace(semantic_path="...", scope="references")` |
| Full symbol overview | `trace(semantic_path="...", scope="overview")` |

## Token Budget Controls

| Parameter | Tool | Default | Purpose |
|---|---|---|---|
| `max_references` | `trace` | `50` | Cap total references in `references` and `overview` scopes |
| `max_depth` | `trace` | `3` | BFS traversal depth (clamped 1–5). `scope="callers"` only |
| `max_dependencies` | `inspect` | `50` | Cap outgoing dependency entries (with `include_dependencies=true`) |

## Fallback

If Pathfinder tools are unavailable or return `ToolStatus::Unavailable`, fall back gracefully to built-in host tools (`Read`, `Grep`, `Glob`). Do not block.
