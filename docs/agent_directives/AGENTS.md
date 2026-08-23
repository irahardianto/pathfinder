## Pathfinder Tool Routing

Semantic navigation tools (`locate`, `trace`, `inspect`). Workflows and deep details: `docs/agent_directives/skills/pathfinder/SKILL.md`.

### Tool Table

| Task | Tool | Parameters & Notes |
|---|---|---|
| Jump to definition | `locate` | `semantic_path="src/auth.ts::AuthService.login"`. LSP with ripgrep fallback. |
| Batch jump to definitions | `locate` | `locations=[{semantic_path: "..."}, {file: "...", line: N}]` (max 10). |
| Location → semantic path | `locate` | `file="src/auth.ts"`, `line=42`. For stack traces, grep results, error messages. |
| Read symbol source | `inspect` | `semantic_path="..."`. Extract exact symbol block by semantic path. |
| Inspect symbol + dependencies | `inspect` | `semantic_path="..."`, `include_dependencies=true`. Fetches callee signatures (LSP-powered). |
| Batch inspect symbols | `inspect` | `semantic_paths=["...", ...]`. (max 10). |
| Find callers and callees | `trace` | `semantic_path="..."`, `scope="callers"` (default). Callers + callees via LSP call hierarchy. `max_depth` (default 3, clamped 1–5). |
| Find all references | `trace` | `semantic_path="..."`, `scope="references"`. All usages including non-call references (field access, imports, type annotations). |
| Symbol overview | `trace` | `semantic_path="..."`, `scope="overview"`. Source + callers + callees + references in one call. |

### Addressing

Semantic paths MUST include file path + `::` + symbol. Example: `src/auth.ts::AuthService.login`
Symbol segments are separated by `.` (e.g. `ClassName.method_name`).

### Response Contracts & Readiness (Contracts 1–3)

Pathfinder v6 tools return dual-channel responses with explicit status:
- `ToolStatus::Ok`: Verified compilation/LSP results.
- `ToolStatus::NotReady`: LSP warmup/indexing in progress. Check `retry_after_secs` and retry with `retry_attempt = attempt + 1`.
- `ToolStatus::Degraded`: Fallback mode (tree-sitter / ripgrep).
- `ToolStatus::Unavailable`: Language server unavailable.

**Critical — Tri-State Certainty:**
```
null  = UNKNOWN (degraded / unverified — callers/callees/references may exist, do NOT treat as zero)
[]    = CONFIRMED ZERO (LSP verified — safe to conclude no callers/callees/references exist)
```
Mistaking `null` for "zero results" leads to dangerous refactoring decisions.

### Budget Controls

| Parameter | Tool | Default | Purpose |
|---|---|---|---|
| `max_references` | `trace` | `50` | Cap total references. In `overview` scope, controls both callers/callees and references. |
| `max_depth` | `trace` | `3` | BFS traversal depth (clamped 1–5). Use 4-5 for large-scale API changes. `scope="callers"` only. |
| `max_dependencies` | `inspect` | `50` | Cap outgoing dependency entries (with `include_dependencies=true`). |

### Fallback

If Pathfinder tools are unavailable or return `ToolStatus::Unavailable` → use built-in tools (`Read`, `Grep`, `Glob`). Do not block.
