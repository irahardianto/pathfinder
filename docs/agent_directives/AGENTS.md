## Pathfinder Tool Routing

Semantic navigation tools. Workflows and deep details: `docs/agent_directives/skills/pathfinder/SKILL.md`.

### Pre-Flight

```
mcp({ server: "pathfinder" })  // Tools listed → available. Error → use built-in.
```

Check once per session.

### Tool Table

| Task | Tool | Notes |
|---|---|---|
| Project skeleton | `get_repo_map` | Returns semantic paths — copy-paste into other tools |
| Search code | `search_codebase` | AST-filtered, returns `enclosing_semantic_path`. Check `coverage_percent`. |
| Read one symbol | `read_symbol_scope` | Exact function/class extraction |
| Read full file + AST | `read_source_file` | Source files only; use `read_file` for config. `detail_level="source_only"` for minimal tokens. |
| Symbol + dependencies | `read_with_deep_context` | LSP-powered callee signatures |
| Jump to definition | `get_definition` | LSP with ripgrep fallback |
| Find callers and callees | `find_callers_callees` | Callers + callees via LSP call hierarchy. Default max_depth=3. |
| Find all references | `find_all_references` | All usages including non-call references (field access, imports, type annotations) |
| Resolve symbol by name | `find_symbol` | Bare name → file::symbol paths. Filter by `kind` ("class", "function", "struct"). |
| Batch read files | `read_files` | Multiple files in one call. AST for source files, raw for config. Max 10 files. |
| Symbol overview | `symbol_overview` | Source + callers + callees + references in one call |
| LSP status | `lsp_health` | Check when navigation returns `degraded: true` |
| Read config file | `read_file` | For YAML, TOML, JSON, .env, Dockerfile |
| Location → semantic path | `get_semantic_path` | File:line → semantic path. For stack traces, grep results, error messages. |

### Addressing

Semantic paths MUST include file path + `::` + symbol. Example: `src/auth.ts::AuthService.login`

### Degraded Mode

`get_definition`, `find_callers_callees`, `read_with_deep_context`, `find_all_references`, `symbol_overview` use LSP. When `degraded: true`:
- Text output starts with: `⚠️ DEGRADED ({reason}) — {tool-specific guidance}`
- Results are best-effort — never treat empty as confirmed-zero
- Check `degraded_reason` and `lsp_readiness`

### Budget Controls

| Parameter | Tool | Default | Purpose |
|---|---|---|---|
| `project_only` | `find_callers_callees`, `read_with_deep_context` | `true` | Filter out stdlib/vendor noise |
| `max_references` | `find_callers_callees` | `50` | Cap total BFS references |
| `max_depth` | `find_callers_callees` | `3` | BFS traversal depth (clamped 1–5). Use 4-5 for large-scale API changes. |
| `max_dependencies` | `read_with_deep_context` | `50` | Cap outgoing dependency entries |
| `max_tokens` | `get_repo_map` | auto | Auto-scales for monorepos |

When `references_truncated` or `dependencies_truncated` is true, increase the corresponding limit.

### Fallback

If Pathfinder unavailable → use built-in tools (`Read`, `Grep`, `Glob`). Do not block.
