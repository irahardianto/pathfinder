## Pathfinder Tool Routing

Semantic navigation tools. Workflows and deep details: `docs/agent_directives/skills/pathfinder/SKILL.md`.

### Pre-Flight

Call `health()` once at session start. If it returns results, Pathfinder is available. If `health()` fails or is not listed in available tools, fall back to built-in tools. Check once per session.

### Tool Table

| Task | Tool | Notes |
|---|---|---|
| Project skeleton | `explore` | Three detail levels: `structure` (dirs only), `files` (dirs + filenames), `symbols` (default — full AST). Configurable `depth` (default 3) and `max_tokens`. |
| Search code | `search` | Three modes: `text` (default), `regex`, `symbol`. `symbol` mode: Resolves bare symbol names to semantic paths. Returns `did_you_mean` suggestions when no exact match found. Use `kind` to filter by symbol type (canonical: function/class/struct/interface/enum/type/constant/module/impl; aliases: method/fn→function, trait→interface, const/static/let→constant, mod/namespace→module). `type` is the broadest type-level filter (class+struct+interface+trait+enum). Invalid kind values return INVALID_PARAMS. Use `filter_mode` to control code vs comment filtering (`code_only` default, `all`, `comments_only`). Returns `enclosing_semantic_path`. Check `coverage_percent`. |
| Read file(s) | `read` | Single file (`filepath`) or batch (`paths`, max 10). Auto-detects source vs config. Source files get AST parsing with `detail_level`. Supports `start_line`/`end_line`. |
| Read one symbol | `inspect` | Extract symbol source by semantic path. Default: source only (fast). With `include_dependencies=true`: also fetches callee signatures (LSP-powered). |
| Batch read symbols | `inspect` | Use `semantic_paths=["...", ...]` (max 10). Returns `BatchInspectResult` with per-entry status. Prefer for 3+ symbols over multiple single calls. |
| Jump to definition | `locate` | Provide `semantic_path` for definition lookup. LSP with ripgrep fallback. |
| Batch jump to definitions | `locate` | Use `locations=[{semantic_path: "..."}, {file: "...", line: N}]` (max 10). Returns `BatchLocateResult`; each entry includes `input` echo for correlation. |
| Location → semantic path | `locate` | Provide `file` + `line` for semantic path resolution. For stack traces, grep results, error messages. |
| Find callers and callees | `trace` | `scope="callers"` (default). Callers + callees via LSP call hierarchy. `max_depth` (default 3, clamped 1–5). |
| Find all references | `trace` | `scope="references"`. All usages including non-call references (field access, imports, type annotations). |
| Symbol overview | `trace` | `scope="overview"`. Source + callers + callees + references in one call. |
| LSP status | `health` | Check when navigation returns `degraded: true`. Supports `action="restart"` for stuck LSPs. Pass `force_probe=true` to force a live probe immediately (bypasses cache). |

### Addressing

Semantic paths MUST include file path + `::` + symbol. Example: `src/auth.ts::AuthService.login`

### Gotchas

- `filter_mode="comments_only"` (alias: `non_code`) matches comments AND string literals (non-code content).
- `kind="class"` matches classes, structs, and interfaces, but NOT enums. `kind="struct"` matches ONLY structs.

### Degraded Mode

`locate` (definition mode), `trace`, `inspect(include_dependencies=true)` use LSP. When `degraded: true`:
- Text output starts with: `⚠️ DEGRADED ({reason}) — {tool-specific guidance}`
- Results are best-effort — never treat empty as confirmed-zero
- Check `degraded_reason` and `lsp_readiness`

**Critical — null vs empty array are NOT equivalent in `trace` results:**
```
null  = UNKNOWN (degraded — callers/callees/references may exist but LSP couldn't confirm)
[]    = CONFIRMED ZERO (LSP verified — safe to conclude no callers/callees/references)
```
Mistaking `null` for "zero results" leads to dangerous refactoring decisions.

### Budget Controls

| Parameter | Tool | Default | Purpose |
|---|---|---|---|
| `max_references` | `trace` | `50` | Cap total references. In `overview` scope, controls both callers/callees and references. |
| `max_depth` | `trace` | `3` | BFS traversal depth (clamped 1–5). Use 4-5 for large-scale API changes. `scope="callers"` only. |
| `max_dependencies` | `inspect` | `50` | Cap outgoing dependency entries (with `include_dependencies=true`). |
| `max_tokens` | `explore` | `16000` | Total token budget for skeleton output. Auto-scales based on repo size. When response includes `suggested_max_tokens`, use that value for a retry to achieve full coverage. |
| `max_results` | `search` | `50` | Cap search matches. Applies to all modes including `symbol`. |

When `references_truncated` or `dependencies_truncated` is true, increase the corresponding limit.

### Fallback

If Pathfinder unavailable → use built-in tools (`Read`, `Grep`, `Glob`). Do not block.
