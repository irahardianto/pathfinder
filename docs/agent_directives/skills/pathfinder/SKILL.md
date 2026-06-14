---
name: pathfinder
description: "Session bootstrap + workflows for Pathfinder semantic navigation tools. Covers: discovery protocol, tool chaining patterns (explore, impact, audit, debug), search optimization, LSP degraded mode, and error recovery."
---

# Pathfinder Skill

## Session Bootstrap

Before any code exploration or analysis, confirm Pathfinder is available:

```
mcp({ server: "pathfinder" })
```

- **Tools listed** → Pathfinder is live. Use semantic tools for all source code operations.
- **Error or empty** → Pathfinder unavailable. Use built-in tools. Stop here.

Check once per session. Re-check only if a tool call fails with a connection error.

## Semantic Paths

All Pathfinder tools that take a `semantic_path` require `file_path::symbol_chain`:

- ✅ `src/auth.ts::AuthService.login`
- ✅ `crates/pathfinder/src/server.rs::PathfinderServer.new`
- ❌ `AuthService.login` (interpreted as a file named "AuthService.login")
- ❌ `login` (interpreted as a file named "login")

Bare file paths (no `::`) are valid only for whole-file operations like `read(filepath="...")`.

## Workflows

### Explore (Understand a Codebase)

```
1. explore(depth=3, detail="symbols")
   → Full project skeleton with semantic paths
   → Low coverage_percent? Increase max_tokens
   → Use detail="structure" for high-level folder overview first
   → Use detail="files" for dirs + all filenames
   → Use changed_since="3h" to scope to recent changes
   → Use include_extensions=["ts","tsx"] for language focus

2. search(query="<entry point>", path_glob="src/**/*")
   → Find main handlers, routes, or CLI commands
   → Copy enclosing_semantic_path for next step
   → Check files_searched/files_in_scope/coverage_percent for completeness

3. inspect(semantic_path="<entry point>", include_dependencies=true)
   → Source + signatures of everything it calls
   → First call may take 5-30s during LSP warmup

4. trace(semantic_path="<key function>", scope="callers", max_depth=3)
   → Who calls it (incoming) + what it calls (outgoing)
```

**Done when:** You can explain the architecture and trace a request through the system.

### Impact Assessment (Before Any Change)

```
1. inspect(semantic_path="<target>", include_dependencies=true)
   → Current code + dependency signatures

2. trace(semantic_path="<target>", scope="callers", max_depth=3)
   → All callers = blast radius
   → For large API changes: use max_depth=4-5
   → When NOT degraded: empty arrays = LSP confirmed zero

3. For each caller:
   inspect(semantic_path="<caller>")
   → Verify assumptions about the target's interface

4. locate(semantic_path="<suspicious dependency>")
   → Jump to external dependency source
```

**Rule:** ALWAYS run `trace(scope="callers")` before recommending a refactor.

### Audit (Review Code Quality)

```
1. explore(depth=5, detail="symbols")

2. For each module:
   a. read(filepath="<file>", detail_level="source_only")
      → For targeted line reading (lowest tokens)
   b. read(filepath="<file>", detail_level="compact")
      → For code + flat symbol list
   c. inspect(semantic_path="<complex function>", include_dependencies=true)
   d. search(query="<danger pattern>", mode="regex")
      Go: panic|log\.Fatal
      Rust: unwrap\(\)|expect\(|panic!
      Python: except:|pass\s+# noqa
      TypeScript: as any|@ts-ignore
   e. search(query="TODO|FIXME|HACK", mode="regex")

3. For critical findings:
   trace(semantic_path="<problem>", scope="callers")
   → Blast radius before recommending changes
```

### Debug (Trace a Problem)

```
1. inspect(semantic_path="<failing function>", include_dependencies=true)
   → Source + all dependencies

2. locate(semantic_path="<suspicious call>")
   → Jump to the callee's definition

3. trace(semantic_path="<failing function>", scope="callers", max_depth=1)
   → Find callers to understand inputs

4. search(query="<error message>")
   → Locate error origin
```

## Search Optimization

`search` parameters for token efficiency:

| Parameter | Default | Effect |
|---|---|---|
| `mode` | `text` | `text` for literal search; `regex` for patterns; `symbol` for name resolution |
| `path_glob` | `**/*` | Scope search (e.g. `src/**/*.ts`) |
| `exclude_glob` | `""` | Skip files before reading (e.g. `**/*.test.*`) |
| `known_files` | `[]` | Files already in context — matches return metadata only, no content |
| `max_results` | `50` | Cap results. Applies to all modes including symbol. |
| `context_lines` | `2` | Context above/below matches (text/regex modes only) |

**Coverage metadata:**
- `files_searched` — actual files that were searched
- `files_in_scope` — files matching path_glob
- `coverage_percent` — % of in-scope files searched. <100% means some files skipped.

**Search counts:**
- `total_matches` — post-filter count (equals `matches.len()`). This is the ground truth.
- `raw_match_count` — ripgrep pre-filter count (before code-only filter drops comments/strings).
- `filtered_count` — `raw_match_count - total_matches` (how many rows were removed by the filter).
- `truncated` — result set was capped at `max_results`. Increase `max_results` or narrow your query.

**Token-saving pattern:**
```
search(query="deprecated_api",
       known_files=["src/fileA.ts", "src/fileB.ts"],
       exclude_glob="**/*.test.*")
```

## Token Budget Controls

Use these parameters to prevent context-window overflow in large repos:

### `trace` budget parameters

| Parameter | Default | Effect |
|---|---|---|
| `max_references` | `50` | Hard cap on total references returned. In `overview` scope, controls both callers/callees and references caps. |
| `max_depth` | `3` | BFS traversal depth (clamped 1–5). Use 3 for standard refactoring, 4-5 for large-scale API changes. `scope="callers"` only. |

When `references_truncated: true` in the response, the budget was hit — either increase `max_references` or decrease `max_depth`.

### `inspect` budget parameters

| Parameter | Default | Effect |
|---|---|---|
| `max_dependencies` | `50` | Hard cap on outgoing dependency entries (with `include_dependencies=true`) |

When `dependencies_truncated: true` in the response, increase `max_dependencies` to see more.

### `explore` budget parameters

| Parameter | Default | Effect |
|---|---|---|
| `max_tokens` | auto | Auto-scales for repos > 20 files: `clamp(file_count × 800, 16000, 48000)` |
| `depth` | `3` | Directory traversal depth. Use 1-2 for large repos, 5+ for small repos. |

Check `max_tokens_used` in the response to see the effective budget applied.

## LSP Degraded Mode

**Tools that use LSP:** `locate`, `trace`, `inspect(include_dependencies=true)`.

**Visual indicator:** When degraded, text output starts with:
```
⚠️ DEGRADED ({reason}) — {tool-specific guidance}
```

Every response includes `degraded` (bool), `degraded_reason`, and `lsp_readiness`:

| `degraded_reason` | Meaning | Action |
|---|---|---|
| `null` | LSP confirmed | Trust fully |
| `no_lsp` | No language server available | Accept limited results |
| `lsp_warmup_empty_unverified` | LSP indexing; empty = unverified | Re-run in 10-30s |
| `lsp_warmup_grep_fallback` | LSP returned null; fell back to grep | Verify with read |
| `lsp_timeout_grep_fallback` | LSP timed out; fell back to grep | Re-run or use tree-sitter tools |
| `lsp_error_grep_fallback` | LSP error; fell back to grep | Check health |
| `no_lsp_grep_fallback` | No LSP; fell back to grep | Install language server |
| `grep_fallback_file_scoped` | File-scoped grep | Good confidence |
| `grep_fallback_impl_scoped` | Impl-block grep | Good for methods |
| `grep_fallback_global` | Global grep | Least precise — verify |
| `unsupported_language_filter_bypassed` | Language unsupported; filter bypassed | Results may include noise |
| `unsupported_language` | Language not supported | Use read for raw content |
| `git_error` | Git operation failed | explore changed_since fell back |

**LSP Readiness values:**
- `"ready"` — LSP is fully operational
- `"warming_up"` — LSP still indexing
- `"unavailable"` — No LSP available

**Critical rule:** When `degraded: true`, **never treat empty results as confirmed-zero.** Re-run after LSP finishes indexing, or check `health`.

**Null vs Empty Array distinction:**
- `incoming: null` (or `outgoing: null`, `references: null`) — unknown, degraded
- `incoming: []` (or `outgoing: []`, `references: []`) — LSP confirmed: truly zero

## Error Recovery

**SYMBOL_NOT_FOUND with `did_you_mean` suggestions:**
```
Error: SYMBOL_NOT_FOUND for "src/auth.ts::AuthServce.login"
       data.details.did_you_mean: ["AuthService.login", "AuthService.logout"]
→ Use the corrected path from did_you_mean
→ If suggestions list is empty: use search(query="login") to find the right file
```

**SYMBOL_NOT_FOUND — no suggestions (wrong file):**
```
Error: SYMBOL_NOT_FOUND, did_you_mean: []
→ The symbol probably lives in a different file
→ search(query="<symbol_name>") → find file → retry with correct path
```

**LSP Timeout / Degraded Navigation:**
```
degraded=true, degraded_reason="lsp_warmup_empty_unverified"
→ Check health for indexing status
→ Re-run after indexing completes
→ If no_lsp: use search + inspect as fallback
```

## Common Mistakes

### Wrong File in Semantic Path
If `locate(semantic_path="logic.go::CompleteLesson")` returns SYMBOL_NOT_FOUND,
the symbol might be defined in a different file. The semantic path requires the
**actual file where the symbol is defined**, not just any file in the module.

**Solution:**
1. Use `search(query="CompleteLesson")` to find which file defines the symbol
2. Use `read(filepath="logic.go", detail_level="symbols")` to see all symbols in a specific file
3. Then use the correct file path in the semantic path

### Tool Selection: source files vs config files

| File Type | Tool | Why |
|---|---|---|
| .rs, .go, .ts, .tsx, .js, .jsx, .py, .vue, .java | `read(filepath="...")` | Auto-detected as source → AST parsing, symbol extraction |
| .yaml, .yml, .toml, .json, .env, .md, Dockerfile | `read(filepath="...")` | Auto-detected as config → raw content |
| Multiple files | `read(paths=["..."])` | Batch read, max 10 files per call |

### read detail_level options

| Option | Output | Use Case |
|---|---|---|
| `source_only` | Source code only | Lowest token cost, targeted reading |
| `compact` (default) | Source + flat symbol list | General purpose |
| `symbols` | Symbol tree only, no source | Discover available symbols |
| `full` | Source + nested symbol tree | Deep understanding |

### Converting Grep/Stack Trace Results to Semantic Paths

When you have a file + line from grep, error output, or a stack trace:
```
1. locate(file="src/auth.ts", line=42)
   → Returns semantic path of the symbol at that line
   → null if line is outside any named symbol

2. Use the returned semantic_path with any other Pathfinder tool:
   inspect(semantic_path="<returned path>")
   trace(semantic_path="<returned path>", scope="callers")
```

Supported languages: .rs, .ts, .tsx, .go, .py, .vue, .js, .jsx, .java.
For unsupported languages, use `read(filepath="...", detail_level="symbols")` instead.

## Quick Reference

| I want to... | Tool chain |
|---|---|
| Understand a new project | `explore` → `inspect(include_dependencies=true)` |
| Read one function precisely | `inspect(semantic_path="...")` |
| Read a full source file | `read(filepath="...")` (use `detail_level="source_only"` for minimal tokens) |
| Batch read multiple files | `read(paths=["..."])` (max 10 files per call) |
| Find a function by name/pattern | `search(query="...")` → `inspect` |
| Resolve a symbol name to its file | `search(mode="symbol", query="...")` |
| Get full symbol overview | `trace(scope="overview")` (source + callers + callees + refs) |
| See all callers of a function | `trace(scope="callers")` |
| See all callees of a function | `inspect(include_dependencies=true)` or `trace(scope="callers")` |
| Find ALL references (including non-call) | `trace(scope="references")` |
| Jump to a definition | `locate(semantic_path="...")` |
| Find tech debt | `search(query="TODO\|FIXME", mode="regex")` |
| Check LSP status | `health` (pass `language="rust"` for specific lang, `action="restart"` to force-restart) |
| Read a config file | `read(filepath="...")` |
| Convert file:line to semantic path | `locate(file="...", line=42)` (for stack traces, grep results, error messages) |

## Fallback (Pathfinder Unavailable)

If tools are not available, fall back transparently:

| Pathfinder | Built-in |
|---|---|
| `inspect` | `Read` with line ranges |
| `read` | `Read` |
| `search` | `Grep` |
| `explore` | `Glob` or `ls` |
| `trace` / `locate` | `Grep` (approximate) |

Do not block on Pathfinder. Complete the work with built-in tools.
