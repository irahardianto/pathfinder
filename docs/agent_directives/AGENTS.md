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
| Search code | `search_codebase` | AST-filtered, returns `enclosing_semantic_path` |
| Read one symbol | `read_symbol_scope` | Exact function/class extraction |
| Read full file + AST | `read_source_file` | Source files only; use `read_file` for config |
| Symbol + dependencies | `read_with_deep_context` | LSP-powered callee signatures |
| Jump to definition | `get_definition` | LSP with ripgrep fallback |
| Blast radius | `analyze_impact` | Callers + callees via LSP call hierarchy |
| LSP status | `lsp_health` | Check when navigation returns `degraded: true` |
| Read config file | `read_file` | For YAML, TOML, JSON, .env, Dockerfile |

### Addressing

Semantic paths MUST include file path + `::` + symbol. Example: `src/auth.ts::AuthService.login`

### Degraded Mode

`get_definition`, `analyze_impact`, `read_with_deep_context` use LSP. When `degraded: true`, results are best-effort — never treat empty as confirmed-zero. Check `degraded_reason` (enum: `no_lsp`, `lsp_warmup_grep_fallback`, `lsp_timeout_grep_fallback`, etc.). See skill doc for the full table.

### Budget Controls

| Parameter | Tool | Default | Purpose |
|---|---|---|---|
| `project_only` | `analyze_impact`, `read_with_deep_context` | `true` | Filter out stdlib/vendor noise |
| `max_references` | `analyze_impact` | `50` | Cap total BFS references |
| `max_dependencies` | `read_with_deep_context` | `50` | Cap outgoing dependency entries |
| `max_tokens` | `get_repo_map` | auto | Auto-scales for monorepos |

When `references_truncated` or `dependencies_truncated` is true, increase the corresponding limit.

### Fallback

If Pathfinder unavailable → use built-in tools (`Read`, `Grep`, `Glob`). Do not block.
