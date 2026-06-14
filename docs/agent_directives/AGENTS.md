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
| Project skeleton | `explore` | Three detail levels: `structure` (dirs only), `files` (dirs + filenames), `symbols` (default — full AST). Configurable `depth` (default 3) and `max_tokens`. |
| Search code | `search` | Three modes: `text` (default), `regex`, `symbol`. AST-filtered (code-only). Returns `enclosing_semantic_path`. Check `coverage_percent`. |
| Read file(s) | `read` | Single file (`filepath`) or batch (`paths`, max 10). Auto-detects source vs config. Source files get AST parsing with `detail_level`. Supports `start_line`/`end_line`. |
| Read one symbol | `inspect` | Extract symbol source by semantic path. Default: source only (fast). With `include_dependencies=true`: also fetches callee signatures (LSP-powered). |
| Jump to definition | `locate` | Provide `semantic_path` for definition lookup. LSP with ripgrep fallback. |
| Location → semantic path | `locate` | Provide `file` + `line` for semantic path resolution. For stack traces, grep results, error messages. |
| Find callers and callees | `trace` | `scope="callers"` (default). Callers + callees via LSP call hierarchy. `max_depth` (default 3, clamped 1–5). |
| Find all references | `trace` | `scope="references"`. All usages including non-call references (field access, imports, type annotations). |
| Symbol overview | `trace` | `scope="overview"`. Source + callers + callees + references in one call. |
| LSP status | `health` | Check when navigation returns `degraded: true`. Supports `action="restart"` for stuck LSPs. |

### Addressing

Semantic paths MUST include file path + `::` + symbol. Example: `src/auth.ts::AuthService.login`

### Degraded Mode

`locate`, `trace`, `inspect(include_dependencies=true)` use LSP. When `degraded: true`:
- Text output starts with: `⚠️ DEGRADED ({reason}) — {tool-specific guidance}`
- Results are best-effort — never treat empty as confirmed-zero
- Check `degraded_reason` and `lsp_readiness`

### Budget Controls

| Parameter | Tool | Default | Purpose |
|---|---|---|---|
| `max_references` | `trace` | `50` | Cap total references. In `overview` scope, controls both callers/callees and references. |
| `max_depth` | `trace` | `3` | BFS traversal depth (clamped 1–5). Use 4-5 for large-scale API changes. `scope="callers"` only. |
| `max_tokens` | `explore` | auto | Auto-scales for monorepos |
| `max_results` | `search` | `50` | Cap search matches. Applies to all modes including `symbol`. |

When `references_truncated` or `dependencies_truncated` is true, increase the corresponding limit.

### Fallback

If Pathfinder unavailable → use built-in tools (`Read`, `Grep`, `Glob`). Do not block.
