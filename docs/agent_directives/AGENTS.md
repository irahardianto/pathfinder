## Pathfinder Tool Routing

Canonical location for Pathfinder tool routing, addressing rules, and fallback details.
`APPEND_SYSTEM.md` §1 points here for bootstrap. Full tool chains and workflows: `.agents/skills/pathfinder-workflow/SKILL.md`.

### Pre-Flight Check

Before using Pathfinder tools, confirm they're available:
```
mcp({ server: "pathfinder" })  // If tools listed → available. If error → use built-in.
```

Do this once per session, not per task.

### Core Principle
Pathfinder operates at the **semantic level** (symbols, functions, classes). Built-in tools operate at **text level**. **Always prefer semantic tools for source code.**

### Tool Preference

| Action | Prefer (Pathfinder) | Instead of (Built-in) | Notes |
|---|---|---|---|
| Explore project structure | `get_repo_map` | directory listing | One call returns skeleton + version hashes |
| Search for code patterns | `search_codebase` | grep | Returns semantic paths + version hashes |
| Read a function or class | `read_symbol_scope` | read file | Exact symbol extraction, no context waste |
| Read function + dependencies | `read_with_deep_context` | Multiple reads | Source + callee signatures in one call. LSP-powered |
| Jump to a definition | `get_definition` | grep (approximation) | LSP-powered, follows imports/re-exports. Has grep fallback when degraded |
| Assess refactoring impact | `analyze_impact` | No equivalent | Maps callers + callees with BFS. LSP-powered |
| Edit a function body | `replace_body` | edit file | Semantic addressing + auto-indent + LSP validation |
| Edit entire declaration | `replace_full` | edit file | Includes signature/decorators/doc comments |
| Batch-edit multiple symbols | `replace_batch` | multiple edits | Atomic single-call with single OCC guard |
| Add code before/after symbol | `insert_before` / `insert_after` | edit file | Semantic anchor point + auto-spacing |
| Delete a function or class | `delete_symbol` | edit file | Handles decorators, doc comments, whitespace |
| Pre-check a risky edit | `validate_only` | no equivalent | Dry-run with LSP diagnostics. Returns `status: "passed"`, `"failed"`, `"uncertain"`, or `"skipped"`. `"uncertain"` means LSP returned empty diagnostics (could be warmup — not confirmed clean). `"skipped"` means no LSP available. Never trust `"uncertain"` or `"skipped"` as confirmation |
| Create a new file | `create_file` | write file | Returns version_hash for subsequent edits |
| Edit config files | `write_file` | edit file | OCC-protected, supports search-and-replace |

### LSP-Dependent Tools and Degraded Mode

Three Pathfinder tools depend on LSP (Language Server Protocol) for precise results: `get_definition`, `analyze_impact`, and `read_with_deep_context`. When the LSP is unavailable or still indexing, these tools degrade gracefully:

- **`degraded: false`** — LSP confirmed the result. Trust it fully. Exception: `read_with_deep_context` may return `degraded: false` with 0 dependencies when the LSP is still warming up — if the result seems wrong (a function that clearly calls other functions shows 0 deps), re-run after a few seconds.
- **`degraded: true`** — Result is a best-effort approximation. Check `degraded_reason` for specifics:
  - `no_lsp` — No language server for this language. Install it or accept limited results.
  - `lsp_warmup_*` — LSP is still indexing. Empty results are UNVERIFIED (there may be callers/definitions the LSP hasn't found yet). Re-run after indexing completes.
  - `grep_fallback_*` — `get_definition` or `analyze_impact` fell back to ripgrep search. Verify with `read_source_file`.
  - `lsp_error` — LSP returned an error. Results are from Tree-sitter/grep only.

**Key rule:** When `degraded: true`, do NOT treat empty results as confirmed-zero. Re-run the tool after a few seconds if the LSP is warming up.

### Addressing Rules
Semantic paths MUST include file path and `::`. Example: `src/main.rs::MyClass.my_function`

### Graceful Fallback
If Pathfinder unavailable, fall back to built-in tools transparently. Do not block.
