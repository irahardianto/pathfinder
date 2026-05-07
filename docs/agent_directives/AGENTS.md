## Pathfinder Tool Routing

Semantic navigation tools. Workflows and deep details: `.agents/skills/pathfinder/SKILL.md`.

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

`get_definition`, `analyze_impact`, `read_with_deep_context` use LSP. When `degraded: true`, results are best-effort — never treat empty as confirmed-zero. See skill doc for details.

### Fallback

If Pathfinder unavailable → use built-in tools (`Read`, `Grep`, `Glob`). Do not block.
