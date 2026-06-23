# Pathfinder MCP Server — Agent Instructions

## Semantic Paths

All symbol-level tools (`inspect`, `locate`, `trace`) require semantic paths in `file_path::symbol` format:

- ✅ `src/auth.ts::AuthService.login`
- ✅ `crates/pathfinder/src/server.rs::PathfinderServer.new`
- ❌ `AuthService.login` (missing file path — will be treated as a file name)

Bare file paths (no `::`) are valid only for `read(filepath="...")` and `explore(path="...")`.

## Response Format (Critical)

Pathfinder tools return responses through two channels:
- **Text content** — the primary output (skeleton, source code, formatted results). Always read this.
- **Structured content** — metadata only (counts, flags, status). Use for programmatic decisions.

`explore` skeleton is in the **text content** only. `files_scanned: 0` in structured_content for
`detail="structure"` is expected — structure mode reads directory names, not source files.

`search` puts everything (including results) in the text channel as JSON. No structured_content.

See the skill file for the full per-tool response guide.

## Pre-Flight Check

Call `health()` once at session start. If it returns results, Pathfinder is available. If `health()` fails or is not listed in available tools, fall back to built-in tools (Read, Grep, Glob).

## Critical Gotchas

- `filter_mode="comments_only"` matches both comments AND string literals (non-code content).
- `kind="class"` is a broad filter matching classes, structs, and interfaces, but NOT enums; `kind="struct"` matches ONLY structs.

## Tool Selection Quick Guide

| I want to... | Tool |
|---|---|
| Understand project structure | `explore` |
| Find text/patterns in code | `search` (mode: text/regex) |
| Find a symbol by name | `search` (mode: symbol) — returns `did_you_mean` suggestions when no exact match |
| Read a file | `read` |
| Read a symbol's source code | `inspect` |
| Jump to a definition | `locate` (with semantic_path) |
| Convert file:line to symbol | `locate` (with file + line) |
| See callers/callees | `trace` (scope: callers) |
| See all references | `trace` (scope: references) |
| Get full symbol overview | `trace` (scope: overview) |
| Check LSP status | `health` |

## Degraded Mode

When LSP is unavailable, tools fall back to grep/Tree-sitter heuristics. Check the `degraded` field.
When `degraded: true`, never treat empty results as confirmed-zero — results are best-effort.

Critical: `null` and `[]` in trace results are NOT the same:
- `null` = unknown (degraded — callers may exist)
- `[]` = LSP confirmed zero callers

## Detailed Workflows

For step-by-step workflows, error recovery patterns, response format details, and advanced usage,
see the Pathfinder skill file: `docs/agent_directives/skills/pathfinder/SKILL.md`
