---
name: pathfinder
description: "Session bootstrap + workflows for Pathfinder semantic navigation tools. Covers: discovery protocol, tool chaining patterns (explore, trace, audit, debug), search optimization, LSP degraded mode, and error recovery."
---

# Pathfinder Skill

## Pre-Flight Check

Call `health()` once at session start.
If it returns results, Pathfinder is available.
If `health()` fails or is not listed in available tools, fall back to built-in tools (grep, file read).

Check once per session. Re-check only if a tool call fails with a connection error.

## Quick Reference

| I want to... | Tool chain |
|---|---|
| Understand a new project | `explore` → `inspect(include_dependencies=true)` |
| Read one function precisely | `inspect(semantic_path="...")` |
| Batch read multiple symbols | `inspect(semantic_paths=["...", "..."])` (max 10 per call) |
| Read a full source file | `read(filepath="...")` (use `detail_level="source_only"` for minimal tokens) |
| Batch read multiple files | `read(paths=["..."])` (max 10 files per call) |
| Find a function by name/pattern | `search(query="...")` → `inspect` |
| Resolve a symbol name to its file | `search(mode="symbol", query="...")` — returns `did_you_mean` suggestions on no exact match |
| Get full symbol overview | `trace(scope="overview")` (source + callers + callees + refs) |
| See all callers of a function | `trace(scope="callers")` |
| See all callees of a function | `inspect(include_dependencies=true)` (preferred — source + callee signatures) or `trace(scope="callers")` (returns both callers AND callees) |
| Find ALL references (including non-call) | `trace(scope="references")` |
| Jump to a definition | `locate(semantic_path="...")` |
| Batch jump to definitions | `locate(locations=[{semantic_path: "..."}, {file: "...", line: N}])` (max 10) |
| Find tech debt | `search(query="TODO\|FIXME", mode="regex")` |
| Check LSP status | `health` (pass `language="rust"` for specific lang, `action="restart"`, `force_probe=true` to bypass cache) |
| Read a config file | `read(filepath="...")` |
| Convert file:line to semantic path | `locate(file="...", line=42)` (for stack traces, grep results, error messages) |

## Critical: Null vs [] Distinction

> [!IMPORTANT]
> **Null vs Empty Array — critical distinction for safe refactoring:**
> - `null` = UNKNOWN (degraded — callers/callees/references may exist but LSP couldn't verify)
> - `[]` = CONFIRMED ZERO (LSP verified — safe to conclude no callers/callees/references exist)
>
> Never treat `null` as "no callers" or "no references" — it means the answer is unknown. Only `[]` (empty array) from a non-degraded response is a confirmed zero.
>
> **Use the machine-readable verified flags** (`incoming_verified`, `outgoing_verified` on `find_callers_callees` results) to disambiguate without parsing hint prose:
>
> | `incoming_verified` | `incoming` | Meaning | Action |
> |---|---|---|---|
> | `true` | `[]` | LSP confirmed zero callers | Safe to refactor — but consider dynamic dispatch |
> | `true` | `[vec]` | LSP-confirmed callers (verified) | Trust the list |
> | `false` | `null` | Callers UNKNOWN (LSP down, no heuristic match) | **Do NOT** treat as zero — `search` to verify |
> | `false` | `[vec]` | Heuristic grep candidates (LSP unavailable) | Treat as candidate set, may include false positives |
> | absent | — | Field not applicable for this scope | Skip |
>
> Same matrix applies to `outgoing_verified` / `outgoing` for callees. The `hint` field provides the same warning in prose form for agents that read text rather than structured fields.

## Token Budget Controls

Use these parameters to prevent context-window overflow in large repos:

### `trace` budget parameters

| Parameter | Default | Effect |
|---|---|---|
| `max_references` | `50` | Hard cap on total references returned. In `overview` scope, controls both callers/callees and references caps. |
| `max_depth` | `3` | BFS traversal depth (clamped 1–5). Use 3 for standard refactoring, 4-5 for large-scale API changes. `scope="callers"` only. |
| `offset` | `0` | Pagination offset. Applies to `scope="references"` only; ignored for `callers` and `overview`. |

When `references_truncated: true` in the response, the budget was hit — either increase `max_references` or decrease `max_depth`.

### `inspect` budget parameters

| Parameter | Default | Effect |
|---|---|---|
| `max_dependencies` | `50` | Hard cap on outgoing dependency entries (with `include_dependencies=true`) |
| `include_imports` | `false` | Include file-level import statements. Useful for Java, C#, Kotlin where imports clarify types in scope. Only used with `include_dependencies=true`. |

When `dependencies_truncated: true` in the response, increase `max_dependencies` to see more.

### `explore` budget parameters

| Parameter | Default | Effect |
|---|---|---|
| `max_tokens` | `16000` | Applies to `detail="symbols"` only. Auto-scales for repos over 20 files: `clamp(file_count × 800, 16000, 48000)`. Ignored for structure/files modes. |
| `depth` | `3` | Directory traversal depth. Use 1-2 for large repos, 5+ for small repos. |
| `visibility` | `public` | `public` shows only exported/public symbols; `all` includes private/internal symbols. |
| `max_tokens_per_file` | `2000` | Per-file token cap in symbol output. Limits how much of any single file's symbols are emitted. |
| `include_extensions` | `[]` | Only include files with these extensions (e.g. `["rs", "toml"]`). Empty = all files. |
| `exclude_extensions` | `[]` | Exclude files with these extensions (e.g. `["lock", "min.js"]`). Empty = no exclusions. |

**Per-detail token caps (hard limits, cannot be overridden):**
- `detail="structure"`: max 4,000 tokens (dirs + manifests only)
- `detail="files"`: max 8,000 tokens (dirs + all filenames)
- `detail="symbols"`: uses provided `max_tokens` (default 16,000, auto-scales up to 48,000)

Check `max_tokens_used` in the response to see the effective budget applied.

## Semantic Paths

All Pathfinder tools that take a `semantic_path` require `file_path::symbol_chain`:

- ✅ `src/auth.ts::AuthService.login`
- ✅ `crates/pathfinder/src/server.rs::PathfinderServer.new`
- ❌ `AuthService.login` (interpreted as a file named "AuthService.login")
- ❌ `login` (interpreted as a file named "login")

Bare file paths (no `::`) are valid only for whole-file operations like `read(filepath="...")`.

## Common Mistakes

### Path Not Found? Use "Did You Mean"

If `trace()` or `inspect()` returns `SYMBOL_NOT_FOUND`, check the `hint` field.
It often contains a "Did you mean: X?" suggestion with the correct path.

**`search(mode="symbol")` also has did-you-mean:** When a symbol search finds no exact matches,
the response includes `did_you_mean: ["suggestions..."]` and a `hint` field guiding you to check them.

Common cause: Rust impl blocks may use qualified names (e.g., `super::Type.method`) that differ from what search/locate returns.

Recovery workflow:
1. Try the semantic path from search/locate
2. On `SYMBOL_NOT_FOUND`, use the "did you mean" suggestion
3. If `search(mode="symbol")` returns empty symbols, check its `did_you_mean` field for alternative paths

### `kind=struct` Doesn't Match Enums

`kind=struct` matches ONLY structs. `kind=enum` matches ONLY enums.
For broad type-level search, use `kind=type` (matches class + struct + interface + trait + enum) or `kind=class` (matches class + struct + interface, but NOT enums).

### `filter_mode=comments_only` Also Matches String Literals

Despite the name, `comments_only` matches both comments AND string literals. This is by design — both are "non-code" content.
If you need ONLY comments (no strings), there is currently no filter for that.

### `read()` Has Two Parameters for File Input

- `filepath`: single file path (string)
- `paths`: array of file paths (max 10, batch mode)

Use one or the other, not both.

### Misreading `explore` Response

The `explore` skeleton is in the **text content**, not in `structured_content`. The structured_content contains only metadata.

For `detail="structure"`, metadata shows `files_scanned: 0` — **this is correct**. Structure mode reads directory names and manifest files only, not source files. The actual directory tree IS in the text output.

For all detail modes, always read the text channel for the actual output.

### Wrong File in Semantic Path

If `locate(semantic_path="logic.go::CompleteLesson")` returns `SYMBOL_NOT_FOUND`, the symbol might be defined in a different file. The semantic path requires the **actual file where the symbol is defined**, not just any file in the module.

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

### `read` detail_level options

| Option | Output | Use Case |
|---|---|---|
| `source_only` | Source code only | Lowest token cost, targeted reading |
| `compact` (default) | Source + flat symbol list | General purpose |
| `symbols` | Symbol tree only, no source | Discover available symbols |
| `full` | Source + nested symbol tree | Deep understanding |

**Additional `read` parameter:**

| Parameter | Default | Effect |
|---|---|---|
| `max_lines_per_file` | `500` | Cap on lines returned per file. Applies to single-file and batch reads. |

> **Alias:** The `filepath` parameter also accepts `path` as an alternative name.

### Converting Grep/Stack Trace Results to Semantic Paths

When you have a file + line from grep, error output, or a stack trace:
1. `locate(file="src/auth.ts", line=42)`
   → Returns semantic path of the symbol at that line
   → null if line is outside any named symbol
   (Note: `file` also accepts `path` as an alias)
2. Use the returned semantic_path with any other Pathfinder tool:
   `inspect(semantic_path="<returned path>")`
   `trace(semantic_path="<returned path>", scope="callers")`

Supported languages: .rs, .ts, .tsx, .go, .py, .vue, .js, .jsx, .java.
For unsupported languages, use `read(filepath="...", detail_level="symbols")` instead.

### `inspect`/`locate` Batch vs Single Parameter Conflict

Do NOT provide both the single-mode and batch-mode addressing parameters simultaneously:

- `inspect`: pass either `semantic_path` (single) OR `semantic_paths` (batch), not both. Providing both returns `INVALID_PARAMS`.
- `locate`: pass either `semantic_path`/`file`+`line` (single mode) OR `locations` (batch), not both. Providing both returns `INVALID_PARAMS`.

## Fallback Patterns

When a tool fails or returns degraded results, use these recovery sequences:

### `inspect`/`trace` returns `SYMBOL_NOT_FOUND`
1. Check `hint` for "Did you mean" suggestion → retry with suggested path
2. If no suggestion: `search(mode="symbol", query="symbol_name")` → get correct `file::symbol`
3. If search finds nothing: `search(mode="text", query="symbol_name")` → broader text search

### `trace` returns `degraded=true` with `null` incoming
1. Check `degraded_reason`:
   - `lsp_warmup_*`: retry after `retry_after_seconds` from `actionable_guidance`
   - `no_lsp_*`: results are heuristic, use `search(mode="text")` for verification
2. Do NOT treat `null` incoming as "zero callers" — it means UNKNOWN

### `explore` truncated (coverage < 100%)
1. Increase `max_tokens` as suggested in hint (or use `suggested_max_tokens` if available)
2. Or narrow scope with `path` parameter to specific subdirectory
3. Or use `detail="files"` for broader coverage at lower token cost

### `health` shows unavailable or stale for a language
1. Check `last_probe_age_secs` — if high (>30s), the probe may be stale and a re-probe can fire on next call; >120s means the cache has not been refreshed in a long time
2. Check `probe_verified` / `navigation_tested` — `false` means LSP reported ready but no live probe confirmed it
3. Try `health(force_probe=true)` to trigger a live re-probe immediately (bypasses cache)
4. Try `health(action="restart")` only if probe still fails after force_probe
5. Fall back to search/read for that language (grep-based, no LSP)

**Key health response fields (per language):**
- `status` — `"ready"`, `"warming_up"`, or `"unavailable"`
- `probe_verified` (bool) — true if status was confirmed by a live definition lookup (not just LSP notification)
- `navigation_tested` (bool or null) — preferred signal over probe_verified; true when call-hierarchy probe also passed
- `last_probe_age_secs` (u32 or null) — seconds since last probe; high values mean results may be stale; null when no probe has run
- `force_probe` (HealthParams field) — pass `force_probe=true` to force a synchronous re-probe regardless of cache age

## Tool Descriptions & Specifications

### Response Channels

Pathfinder returns responses through two channels. Knowing which to read prevents misinterpretation.

- **Text Channel (always present):** The primary output — skeleton text, source code, formatted results. **Always read this first.**
- **Structured Content (metadata):** Machine-parseable JSON with counts, flags, and status fields. Use for programmatic decisions (`coverage_percent`, `degraded`, `truncated`, `dependencies_truncated`, etc.).

#### Per-Tool Response Guide

| Tool | Text Channel Contains | Structured Content Contains |
|---|---|---|
| `explore` | **Skeleton output** (directory tree / file list / symbols) | Metadata: tech_stack, coverage_percent, files_scanned, version_hashes, `mode` (string: "structure" | "files" | "symbols"), `dirs_scanned` (usize, structure mode only); also `suggested_max_tokens` (u32, only when coverage < 100%) and `hint` (string, only when truncated) |
| `search` | **Full JSON response** (matches, counts, metadata — all in one) | Not set |
| `read` | **File content** (source code or raw content) | Metadata: language, symbols, total_lines, version_hash |
| `inspect` | **Symbol source code** (+ dependency signatures if requested). Batch mode: per-symbol entries with status ok/error. Footer: `[completed in Xms, Y/Z symbols inspected]` | Single: start_line, end_line, dependencies, degraded status, `resolution_strategy` (`lsp_call_hierarchy`, `treesitter_direct`, `treesitter_fallback`, `grep_fallback`). Batch: `BatchInspectResult` (results[], succeeded, failed, total_duration_ms). Error entry shape: `{semantic_path, status: "error", error}` — source/start_line/end_line/language/dependencies are absent on error. `include_dependencies` applies to ALL symbols when used in batch mode. |
| `locate` | **Definition location** or **semantic path** (human-readable summary). Batch: per-entry results. | Single: `GetDefinitionResponse` (file, line, column, preview, degraded, resolution_strategy). Batch: `BatchLocateResult` (results[], succeeded, failed, total_duration_ms). Each `LocateResultEntry` includes `input` (echo of the original `LocateEntry`) for correlation. `resolution_strategy` values: `lsp`, `lsp_retry`, `grep_file`, `grep_impl`, `grep_global`, `grep_broad`. |
| `trace` | **Formatted caller/callee/reference listing** | Full typed response (incoming, outgoing, references, degraded status). `resolution_strategy` for callers/overview: `lsp_call_hierarchy`, `grep_file_scoped`, `treesitter_direct`, `treesitter_fallback`. For references: `lsp_references`, `grep_file_scoped`, `lsp_unverified_warmup`. |
| `health` | **Formatted health status** | Full typed response (status, languages, capabilities) |

> **Key difference for `search`:** The entire typed response is serialized as JSON in the text channel — there is no structured_content. Parse it as JSON directly.

> **Key difference for `explore`:** The actual skeleton lives only in the text channel. For `detail="structure"`, metadata shows `files_scanned: 0` — **this is correct**. Check the `mode` field (`"structure"`) and `dirs_scanned` field (directory count) to confirm the call succeeded. Structure mode reads directory names only, not source files. The actual directory tree IS in the text output.

> **Batch vs single:** Use batch mode (`semantic_paths` / `locations`) when reading 3 or more symbols/locations in a single task — it saves LLM round-trips and returns all results ordered. Use single mode for one-off lookups. Both modes share all parameters (`include_dependencies`, `detail_level`, etc.) except the addressing field.

### Search Optimization & Kind Filters

`search` parameters for token efficiency:

| Parameter | Default | Effect |
|---|---|---|
| `mode` | `text` | `text` for literal search; `regex` for patterns; `symbol` for name resolution |
| `path_glob` | `**/*` | Scope search (e.g. `src/**/*.ts`) |
| `exclude_glob` | `""` | Skip files before reading (e.g. `**/*.test.*`) |
| `known_files` | `[]` | Files already in context — matches return metadata only, no content |
| `max_results` | `50` | Cap results. Applies to all modes including symbol. |
| `offset` | `0` | Skip N matches for pagination. Use with `max_results` to page through large result sets. |
| `context_lines` | `2` | Context above/below matches (text/regex modes only) |
| `filter_mode` | `code_only` | `code_only` returns only code lines; `comments_only` (alias: `non_code`) returns comments AND string literals; `all` returns everything unfiltered |
| `group_by_file` | `true` | Groups search results by file. Set to `false` for a flat match list. |

#### Symbol Kind Filter (`kind` parameter)

When using `mode="symbol"`, the optional `kind` parameter filters results by symbol type.

| Kind | Aliases | Matches |
|---|---|---|
| `type` | — | Class, struct, interface, trait, and enum (broadest type-level search) |
| `function` | `method`, `fn` | Functions and methods |
| `class` | — | Class, struct, and interface (broad OOP-style search, but NOT enums) |
| `struct` | — | Struct ONLY |
| `interface` | `trait` | Interface and trait |
| `enum` | — | Enum ONLY |
| `constant` | `const`, `static`, `let` | Constants and static variables |
| `module` | `mod`, `namespace` | Modules and namespaces |
| `impl` | — | Implementation blocks (Rust) |

> To find all type-level constructs, use `kind=type` (covers class+struct+interface+trait+enum in a single query).

**Search Counts & Coverage:**
- `files_searched` — actual files that were searched
- `files_in_scope` — files matching path_glob
- `coverage_percent` — % of in-scope files searched. <100% means some files skipped.
- `total_matches` — post-filter count (equals `matches.len()`). This is the ground truth.
- `raw_match_count` — ripgrep pre-filter count (before code-only filter drops comments/strings).
- `filtered_count` — `raw_match_count - total_matches` (how many rows were removed by the filter).
- `truncated` — result set was capped at `max_results`.

### LSP Degraded Mode

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
| `grep_fallback_dependencies` | inspect(include_dependencies=true) fell back to grep for callee resolution | Verify specific dependencies with locate if precision matters |
| `unsupported_language_filter_bypassed` | Code/comment filter was bypassed (file type not AST-parseable). | Accept results; filter noise manually |
| `unsupported_language` | Language not supported | Use read for raw content |
| `git_error` | Git operation failed | explore changed_since fell back |

**LSP Readiness values:**
- `"ready"` — LSP is fully operational
- `"warming_up"` — LSP still indexing
- `"unavailable"` — No LSP available

> When `degraded: true`, **never treat empty results as confirmed-zero.** Re-run after LSP finishes indexing, or check `health`.

## Detailed Workflows

### Explore (Understand a Codebase)
1. `explore(depth=3, detail="symbols")`
   → Full project skeleton with semantic paths (use `detail="structure"` or `"files"` for high-level overviews)
2. `search(query="<entry point>", path_glob="src/**/*")`
   → Find main handlers, routes, or CLI commands
3. `inspect(semantic_path="<entry point>", include_dependencies=true)`
   → Source + signatures of everything it calls
4. `trace(semantic_path="<key function>", scope="callers", max_depth=3)`
   → Who calls it (incoming) + what it calls (outgoing)

### Impact Assessment (Before Any Change)
1. `inspect(semantic_path="<target>", include_dependencies=true)`
2. `trace(semantic_path="<target>", scope="callers", max_depth=3)`
   → All callers = blast radius
3. For each caller: `inspect(semantic_path="<caller>")`
4. `locate(semantic_path="<suspicious dependency>")`
   → Jump to external dependency source

**Rule:** ALWAYS run `trace(scope="callers")` before recommending a refactor.

### Audit (Review Code Quality)
1. `explore(depth=5, detail="symbols")`
2. For each module:
   - `read(filepath="<file>", detail_level="source_only")`
   - `inspect(semantic_path="<complex function>", include_dependencies=true)`
   - `search(query="unwrap\(\)|expect\(|panic!", mode="regex")`
   - `search(query="TODO|FIXME|HACK", mode="regex")`
3. For critical findings: `trace(semantic_path="<problem>", scope="callers")`

### Debug (Trace a Problem)
1. `inspect(semantic_path="<failing function>", include_dependencies=true)`
2. `locate(semantic_path="<suspicious call>")`
3. `trace(semantic_path="<failing function>", scope="callers", max_depth=1)`
4. `search(query="<error message>")`

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
