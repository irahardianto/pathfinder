---
trigger: always_on
---

## Pathfinder Tool Routing

> **Applicability:** When Pathfinder MCP tools are available (tools prefixed with `mcp_pathfinder_`), follow these routing rules. If Pathfinder tools are NOT available, use built-in tools as normal.

### Core Principle

Pathfinder tools operate at the **semantic level** (symbols, functions, classes) while built-in tools operate at the **text level** (lines, strings, files). **Always prefer semantic tools for source code operations.**

### Addressing Rules (Semantic Paths)

A **Semantic Path** is the addressing scheme for all Pathfinder tools. 
**IMPORTANT:** Unless specifically targeting an entire file (like passing a bare file path to `insert_before` for the top of a file), semantic paths MUST ALWAYS include the file path and `::`.
- **Correct:** `src/main.rs::MyClass.my_function`
- **Incorrect:** `MyClass.my_function` (This will fail as it's parsed as a missing file named `MyClass.my_function`)

### Tool Preference (Quick Reference)

| Action | Prefer (Pathfinder) | Instead of (Built-in) |
|---|---|---|
| Explore project structure | `get_repo_map` | `list_dir` + `view_file` |
| Search for code patterns | `search_codebase` | `grep_search` |
| Read a function or class | `read_symbol_scope` | `view_file` |
| Read entire source file + AST hierarchy | `read_source_file` | `view_file` + `view_file_outline` |
| Read function + its dependencies | `read_with_deep_context` | Multiple `view_file` calls |
| Jump to a definition | `get_definition` | `grep_search` (approximation) |
| Assess refactoring impact | `analyze_impact` | No equivalent |
| Edit a function body | `replace_body` | `replace_file_content` |
| Edit an entire declaration | `replace_full` | `replace_file_content` |
| Batch-edit multiple symbols in one file | `replace_batch` | `multi_replace_file_content` |
| Edit Vue `<template>`/`<style>` zones | `replace_batch` (text targeting, Option B) | `multi_replace_file_content` |
| Add code before/after a symbol | `insert_before` / `insert_after` | `replace_file_content` |
| Delete a function or class | `delete_symbol` | `replace_file_content` |
| Pre-check a risky edit | `validate_only` | No equivalent |
| Create a new file | `create_file` | `write_to_file` |
| Edit config files (YAML, Dockerfile, .env) | `write_file` | `replace_file_content` |

### Keep Using Built-in Tools For

- **Listing directories** → `list_dir`, `find_by_name`
- **Running commands** (tests, linters, builds) → `run_command`
- **Viewing binary files** → `view_file`
- **Config/docs files** (YAML, TOML, JSON, Markdown, Dockerfile) → `read_file` (not `read_source_file` — AST tools return `UNSUPPORTED_LANGUAGE` for non-source files)

### Multi-Symbol Edits in One File

When editing **multiple non-contiguous symbols** in a single file, you have three approaches:

| Approach | Tool | Pros | Cons |
|---|---|---|---|
| **`replace_batch`** (preferred) | Single `replace_batch` call with an array of edits | Atomic single-call, semantic targeting, LSP validation, single OCC guard | One validation pass for all edits combined |
| **Sequential Pathfinder** (fallback) | Multiple `replace_body`/`replace_full` calls, chaining `new_version_hash` between edits | Per-edit LSP validation, semantic targeting | More tool calls, must chain version hashes |
| **Batch built-in** (last resort) | Single `multi_replace_file_content` call | One tool call for all edits | Fragile string matching, no LSP validation, no OCC |

**Default to `replace_batch`** — it applies all edits atomically with a single OCC guard. Fall back to sequential chaining when edits depend on each other's results, or to `multi_replace_file_content` when Pathfinder tools are unavailable.

### Graceful Fallback

If Pathfinder MCP tools are not available (server offline, tools not surfaced), fall back to built-in tools transparently:

- `view_file` / `view_code_item` for reading
- `replace_file_content` / `multi_replace_file_content` for editing
- `grep_search` for searching

Do not block on Pathfinder being unavailable — complete the work with built-in tools and note the degradation to the user if asked.

### Detailed Usage

For concrete workflow chains, error recovery patterns, OCC mechanics, and advanced multi-file editing, activate the **Pathfinder Workflow Skill** (`pathfinder-workflow`).
