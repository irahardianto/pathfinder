## Pathfinder Tool Routing

> **Applicability:** When Pathfinder MCP tools are available (tools prefixed with `mcp_pathfinder_`), follow these routing rules. If Pathfinder tools are NOT available, use built-in tools as normal.

### Core Principle

Pathfinder tools operate at the **semantic level** (symbols, functions, classes) while built-in tools operate at the **text level** (lines, strings, files). Prefer semantic tools for source code operations.

### Tool Preference Table

When performing an action in the left column, use the **Prefer** tool instead of the **Avoid** tool.

| Action | Prefer (Pathfinder) | Avoid (Built-in) | Why |
|---|---|---|---|
| **Explore project structure** | `get_repo_map` | `list_dir` + `view_file_outline` | One call returns full skeleton with semantic paths + version hashes for immediate editing |
| **Search for code patterns** | `search_codebase` | `grep_search` | Returns `enclosing_semantic_path` + `version_hash` per match, enabling Discovery→Edit chaining |
| **Read a function/class** | `read_symbol_scope` | `view_file` | Extracts exactly one symbol — no context waste, returns `version_hash` for OCC |
| **Understand dependencies before editing** | `read_with_deep_context` | Multiple `view_file` calls | Returns target code + signatures of all called functions in one call |
| **Jump to a definition** | `get_definition` | `grep_search` (approximation) | LSP-powered, follows imports and re-exports across files |
| **Assess refactoring impact** | `analyze_impact` | No equivalent | Maps all incoming callers + outgoing callees with BFS traversal |
| **Edit a function body** | `replace_body` | `replace_file_content` | Semantic addressing + auto-indentation + LSP validation before disk write |
| **Edit an entire declaration** | `replace_full` | `replace_file_content` | Semantic addressing, includes signature/decorators/doc comments |
| **Add code before/after a symbol** | `insert_before` / `insert_after` | `replace_file_content` | Semantic anchor point + auto-spacing |
| **Delete a function/class** | `delete_symbol` | `replace_file_content` (empty) | Handles decorators, doc comments, whitespace cleanup |
| **Pre-check a risky edit** | `validate_only` | No equivalent | Dry-run with LSP diagnostics, zero disk side-effects |
| **Create a new file** | `create_file` | `write_to_file` | Returns `version_hash` for subsequent OCC-protected edits |
| **Edit a config file** | `write_file` | `replace_file_content` | OCC protection; supports search-and-replace mode |
| **Read a config file** | `read_file` | `view_file` | Either is fine — roughly equivalent |

### Keep Using Built-in Tools For

These tasks have **no Pathfinder equivalent** — always use built-in tools:

- **Listing directory contents** → `list_dir`, `find_by_name`
- **Running commands** (tests, linters, builds) → `run_command`
- **Viewing binary files** (images, videos) → `view_file`
- **Making multiple non-contiguous edits in one file** → `multi_replace_file_content` (then validate with Pathfinder's `validate_only` if desired)
- **Quick one-line fixes where you already know the exact line** → `replace_file_content` is acceptable for trivial edits

### The Pathfinder-First Workflow

When starting any code task, follow this discovery chain:

```
1. get_repo_map          → Understand project skeleton, get semantic paths
2. search_codebase       → Find specific patterns, get version_hashes
3. read_symbol_scope     → Read the target function precisely
   OR read_with_deep_context → Read target + all its dependencies
4. analyze_impact        → (Before refactoring) understand blast radius
5. replace_body / replace_full / insert_before / insert_after / delete_symbol
                         → Make the edit with semantic addressing
6. run_command           → Run tests/linters (built-in, no Pathfinder equivalent)
```

### Error Recovery

When a Pathfinder tool returns `SYMBOL_NOT_FOUND`:
1. Check the `did_you_mean` field in the error response — it contains Levenshtein-distance suggestions
2. Use the suggested semantic path to retry
3. If `did_you_mean` is empty, use `get_repo_map` to discover the correct path

When a Pathfinder edit tool returns validation failures (`introduced_errors`):
1. Read the `introduced_errors` array — each entry has `message`, `file`, `severity`
2. Fix the issue in your code and retry the edit
3. Use `validate_only` to dry-run the fix before committing

### OCC (Optimistic Concurrency Control)

All Pathfinder edit tools require a `base_version` (SHA-256 hash). Get it from:
- `read_symbol_scope` → `version_hash` field
- `read_with_deep_context` → `version_hash` field
- `search_codebase` → `version_hash` field (per match)
- `get_repo_map` → `version_hashes` map (per file)
- Previous edit response → `new_version_hash` field

If you get a `VERSION_MISMATCH` error, re-read the file to get the latest `version_hash`, then retry.

### Related Principles
- Code Completion Mandate @code-completion-mandate.md
- Rugged Software Constitution @rugged-software-constitution.md
