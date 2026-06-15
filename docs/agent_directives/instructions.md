# Pathfinder MCP Server — Agent Instructions

## Semantic Paths

All symbol-level tools (`inspect`, `locate`, `trace`) require semantic paths in `file_path::symbol` format:

- ✅ `src/auth.ts::AuthService.login`
- ✅ `crates/pathfinder/src/server.rs::PathfinderServer.new`
- ❌ `AuthService.login` (missing file path — will be treated as a file name)

Bare file paths (no `::`) are valid only for `read(filepath="...")` and `explore(path="...")`.

## Pre-Flight Check

Run once per session to confirm Pathfinder is available:

```
mcp({ server: "pathfinder" })
```

If tools are listed, Pathfinder is live. If error, fall back to built-in tools (Read, Grep, Glob).

## Tool Selection Quick Guide

| I want to... | Tool |
|---|---|
| Understand project structure | `explore` |
| Find text/patterns in code | `search` (mode: text/regex) |
| Find a symbol by name | `search` (mode: symbol) |
| Read a file | `read` |
| Read a symbol's source code | `inspect` |
| Jump to a definition | `locate` (with semantic_path) |
| Convert file:line to symbol | `locate` (with file + line) |
| See callers/callees | `trace` (scope: callers) |
| See all references | `trace` (scope: references) |
| Get full symbol overview | `trace` (scope: overview) |
| Check LSP status | `health` |

## Degraded Mode

When LSP is unavailable, tools fall back to grep/Tree-sitter heuristics. Check the `degraded` field in responses. When `degraded: true`, never treat empty results as confirmed-zero — results are best-effort.

## Detailed Workflows

For step-by-step workflows, error recovery patterns, and advanced usage, see the Pathfinder skill file: `docs/agent_directives/skills/pathfinder/SKILL.md`
