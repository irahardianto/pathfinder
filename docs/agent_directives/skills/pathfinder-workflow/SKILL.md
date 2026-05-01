---
name: pathfinder-workflow
description: "MANDATORY after pathfinder-first bootstrap. Detailed tool chains for: exploring codebases (get_repo_map, search_codebase, read_with_deep_context), refactoring (analyze_impact, replace_body, validate_only), implementing features (insert_after, create_file), auditing (read_source_file, analyze_impact), and debugging (get_definition, read_with_deep_context). Load AFTER confirming Pathfinder MCP is available via pathfinder-first skill."
---

# Pathfinder Workflow Skill

## Purpose

Provides task-specific tool chains that leverage Pathfinder's semantic tools for maximum effectiveness. Each workflow is a concrete sequence of tool calls optimized for a specific goal.

## When to Invoke

- Starting work on an **unfamiliar codebase** → use Explore workflow
- **Refactoring** a function or class → use Refactor workflow
- **Implementing a new feature** → use Implement workflow
- **Auditing code quality** → use Audit workflow
- **Debugging** a failing function → use Debug workflow

---

## Foundation: Why Pathfinder?

Pathfinder tools operate at the **semantic level** (symbols, functions, classes) while built-in tools operate at the **text level** (lines, strings, files). Semantic tools give you:

- **Precise targeting** — address a function by name, not by line number
- **Auto-indentation** — edits match surrounding code style
- **LSP validation** — edits are checked for errors before writing to disk
- **Optimistic Concurrency Control (OCC)** — version hashes prevent overwriting concurrent changes

### What is a Semantic Path?

A **Semantic Path** is the unified addressing scheme for Pathfinder tools.
**IMPORTANT:** Unless targeting an entire file (like BOF/EOF insertions), semantic paths MUST ALWAYS include the file path + `::` + the symbol chain. Providing just the symbol name (e.g. `send` or `login`) will FAIL because Pathfinder will mistakenly interpret it as a bare file named `send`.

**Correct Semantic Paths:**
- `src/auth.ts::AuthService.login`
- `crates/pathfinder-lsp/src/client/process.rs::send`
**Incorrect Semantic Paths:**
- `AuthService.login`
- `send`
**Bare File (only for BOF/EOF insertions, config tools):**
- `src/auth.ts`

### Tool Preference Table

> **Canonical source:** The always-on rule `pathfinder-tool-routing.md` is the single source of truth for which tool to use. This table adds the **"Why"** column for deeper understanding but defers to the rule for routing decisions.

| Action | Prefer (Pathfinder) | Instead of (Built-in) | Why |
|---|---|---|---|
| Explore project structure | `get_repo_map` | `Glob` + `Read` | One call returns full skeleton with semantic paths + version hashes for immediate editing |
| Search for code patterns | `search_codebase` | `Grep` | Returns `enclosing_semantic_path` + `version_hash` per match — enables Discovery→Edit chaining |
| Read a function or class | `read_symbol_scope` | `Read` | Extracts exactly one symbol — no context waste, returns `version_hash` for OCC |
| Read entire source file + AST hierarchy | `read_source_file` | `Read` | Returns full file content + nested symbol tree with semantic paths + version hash. Three detail levels: `compact` (default — source + flat symbol list), `symbols` (tree only, no source), `full` (source + nested AST). **AST-only** — only call on source files (`.rs`, `.ts`, `.tsx`, `.go`, `.py`, `.vue`, `.jsx`, `.js`); use `read_file` for config/docs files |
| Read function + dependencies | `read_with_deep_context` | Multiple `Read` calls | Returns target code + signatures of all called functions in one call. **Latency:** LSP warmup is unpredictable. First call may take 5–60s, but some LSP servers (especially rust-analyzer on large codebases) may take several minutes to fully index. Pathfinder auto-retries once during warmup, but if the LSP is still cold after the retry, the tool returns 0 dependencies. Check `degraded` in metadata — if `true`, LSP was unavailable and `dependencies` will be empty or incomplete. If `degraded=false` but `dependencies=[]` and the function clearly calls other functions, the LSP may have returned a false confirmation. Re-run the tool after waiting 30s. This is a known edge case. |
| Jump to a definition | `get_definition` | `Grep` (approximation) | LSP-powered, follows imports and re-exports across files. Has multi-strategy grep fallback when LSP is unavailable — check `degraded` in response |
| Assess refactoring impact | `analyze_impact` | No equivalent | Maps all incoming callers + outgoing callees with BFS traversal; returns version hashes for all referenced files. When LSP is warming up, empty results may be unverified — check `degraded` |
| Read a config/docs file | `read_file` | `Read` | Either is fine — roughly equivalent for config files. **Never** call `read_source_file` on config files (YAML, TOML, JSON, Markdown, `.env`, Dockerfile) — it returns `UNSUPPORTED_LANGUAGE` |
| Edit a function body | `replace_body` | `Edit` | Semantic addressing + auto-indentation + LSP validation before disk write |
| Edit an entire declaration | `replace_full` | `Edit` | Semantic addressing, includes signature/decorators/doc comments |
| Batch-edit multiple symbols in one file | `replace_batch` | Multiple `Edit` calls | Atomic single-call with single OCC guard; edits applied back-to-front to avoid offset shifts |
| Add code before/after a symbol | `insert_before` / `insert_after` | `Edit` | Semantic anchor point + auto-spacing |
| Delete a function or class | `delete_symbol` | `Edit` (replace with empty) | Handles decorators, doc comments, whitespace cleanup |
| Delete a file | `delete_file` | No built-in equivalent | OCC-protected — requires `base_version` to prevent deleting a file modified after you last read it |
| Pre-check a risky edit | `validate_only` | No equivalent | Dry-run with LSP diagnostics, zero disk side-effects |
| Create a new file | `create_file` | `Write` | Returns `version_hash` for subsequent OCC-protected edits |
| Edit a config file (.env, Dockerfile, YAML) | `write_file` | `Edit` | OCC protection; supports search-and-replace mode for surgical edits |

### Keep Using Built-in Tools For

These tasks have **no Pathfinder equivalent** — always use built-in tools:

- **Listing directory contents** → `Bash` (with `ls`) or `Glob`
- **Running commands** (tests, linters, builds) → `Bash`
- **Viewing binary files** (images, videos) → `Read`
- **Quick one-line fixes where you already know the exact text** → `Edit` is fine for trivial edits

---

## OCC (Optimistic Concurrency Control)

All Pathfinder edit tools require a `base_version` (SHA-256 hash). This prevents overwriting changes made by other tools or agents.

### Where to Get Version Hashes

| Source tool | Field |
|---|---|
| `read_symbol_scope` | `version_hash` |
| `read_source_file` | `version_hash` |
| `read_with_deep_context` | `version_hash` |
| `search_codebase` | `version_hash` (per match, or per `file_group` when `group_by_file=true`) |
| `get_repo_map` | `version_hashes` (per file) |
| `analyze_impact` | `version_hashes` (for all referenced files — callers and callees) |
| Any edit tool response | `new_version_hash` |

### The Hash Chain Pattern

Edits form a chain — each edit produces a new hash for the next edit:

```
read_symbol_scope → version_hash (v1)
replace_body(base_version=v1) → new_version_hash (v2)
insert_after(base_version=v2) → new_version_hash (v3)
```

**If you get `VERSION_MISMATCH`:** Re-read the symbol to get the latest hash, then retry.

**Important:** `validate_only` does NOT write to disk, so `new_version_hash` is null. Reuse your original `base_version` for the real edit after a successful dry-run.

### Validation Overrides

Every edit tool supports `ignore_validation_failures` (default: `false`). When set to `true`, the edit is written to disk even if LSP validation detects introduced errors.

**When to use `ignore_validation_failures: true`:**
- LSP is flaky or unavailable for the target language (check `validation_skipped_reason`)
- You are making a deliberate change that temporarily breaks type checking (e.g., updating an interface before updating all callers)
- The LSP reports false positives for your specific pattern

**When NOT to use it:**
- For normal edits where LSP is healthy — let validation catch genuine errors
- As a blanket habit — always try the edit with validation first

---

## Workflow 1: Explore (Understand a Codebase)

**Goal:** Build a mental model of the project from zero context.

```
Step 1: get_repo_map(path=".", depth=5, visibility="public")
        → Get the full project skeleton with semantic paths
        → Note the tech_stack and files_scanned to understand project scale
        → If coverage_percent is low: increase max_tokens (more files)
        → If files show [TRUNCATED DUE TO SIZE]: increase max_tokens_per_file (more detail per file)
        → Use visibility="all" to include private symbols (better for auditing)
        → Use include_imports="third_party" (default) to see external dependencies,
          or "all" to see internal imports too, or "none" for a minimal skeleton
        → Use changed_since="3h" (or a git ref like "HEAD~5") to scope the map
          to only recently modified files — useful when reviewing a PR or
          picking up where a previous session left off.
        → Use include_extensions=["ts","tsx"] to focus on a specific language
          in mixed-language repos. Mutually exclusive with exclude_extensions.

Step 2: search_codebase(query="<entry point pattern>", path_glob="src/**/*")
        → Find main entry points, API handlers, or CLI commands
        → Use enclosing_semantic_path from results for next step

Step 3: read_with_deep_context(semantic_path="<chosen entry point>")
        → Read the entry point + all functions it calls
        → Follow the dependency chain to understand data flow
        → NOTE: First call after LSP start may take 5–60s while the server
          indexes. Pathfinder auto-retries once during warmup.
        → If degraded=true in metadata, LSP was unavailable or warming up;
          dependencies will be empty or incomplete but source is still returned.

        Alternative: read_source_file(filepath="<key file>", detail_level="compact")
        → When you need the full file context + symbol tree, not just one function

Step 4: analyze_impact(semantic_path="<key function>", max_depth=2)
        → Understand who calls this function (incoming)
        → Understand what it depends on (outgoing)
```

**When to stop:** You can explain the project's architecture, identify its core modules, and trace a request through the system.

---

## Workflow 2: Refactor (Safely Change Code)

**Goal:** Modify existing code without breaking callers or dependencies.

```
Step 1: read_with_deep_context(semantic_path="<target>")
        → Read the target function and everything it calls
        → Save the version_hash for editing

Step 2: analyze_impact(semantic_path="<target>", max_depth=2)
        → Identify ALL callers — these are your blast radius
        → Version hashes are returned for all referenced files

Step 3: validate_only(semantic_path="<target>", edit_type="replace_body",
                      new_code="<your refactored code>", base_version="<hash>")
        → Dry-run the edit to check for LSP errors BEFORE writing

Step 4: replace_body(semantic_path="<target>",
                     new_code="<your refactored code>", base_version="<hash>")
        → Apply the edit with semantic addressing + auto-indentation
        → Check the validation result in the response

Step 5: (If callers need updating) For each caller from Step 2:
        read_symbol_scope → replace_body → verify

Step 6: Bash("cargo test" / "npm test")
        → Verify the refactoring didn't break anything
```

**Key rule:** ALWAYS run `analyze_impact` before refactoring. Agents that skip this step risk breaking unknown callers.

---

## Workflow 3: Implement (Add New Code)

**Goal:** Add a new function, class, or feature to an existing codebase.

```
Step 1: get_repo_map(path="<relevant directory>")
        → Understand existing structure and naming patterns
        → Identify the right file to add the new code to

Step 2: read_symbol_scope(semantic_path="<neighboring function>")
        → Read an existing function in the same file for style reference
        → Save the version_hash

Step 3: insert_after(semantic_path="<anchor symbol>",
                     new_code="<your new function>", base_version="<hash>")
        → Add the new code after an appropriate symbol
        → Use bare file path (no "::") to append at EOF

Step 4: (If adding imports) insert_before(semantic_path="<filepath>",
                     new_code="<import statements>", base_version="<new hash>")
        → Use bare file path to insert at the top of the file

Step 5: Bash("cargo test" / "npm test")
```

---

## Workflow 4: Audit (Review Code Quality)

**Goal:** Systematically review a codebase for issues.

```
Step 1: get_repo_map(path=".", depth=5, visibility="all")
        → Get complete project overview including private symbols

Step 2: For each module/feature area:
        a. read_source_file(filepath="<module entry file>", detail_level="compact")
           → Get the full file with its symbol tree for a structural overview
        b. read_symbol_scope(semantic_path="<public API function>")
           → Review the public interface in detail
        c. read_with_deep_context(semantic_path="<complex function>")
           → Check that dependencies are reasonable
        d. search_codebase(query="<language-specific danger pattern>",
                          path_glob="src/**/*", is_regex=true)
           → Find potential crash/error points. Examples by language:
             - Go: `panic|log\.Fatal`
             - Rust: `unwrap\(\)|expect\(|panic!`
             - Python: `except:|pass\s+# noqa`
             - TypeScript: `as any|@ts-ignore`
        e. search_codebase(query="TODO|FIXME|HACK", filter_mode="comments_only")
           → Find technical debt markers

Step 3: For critical findings:
        analyze_impact(semantic_path="<problematic function>")
        → Assess blast radius before recommending changes
```

---

## Workflow 5: Debug (Trace a Problem)

**Goal:** Understand why a specific function is failing.

```
Step 1: read_with_deep_context(semantic_path="<failing function>")
        → See the function AND all its dependencies

Step 2: get_definition(semantic_path="<suspicious call within the function>")
        → Jump to the definition of a called function to inspect its contract
        → If degraded=true, result is from grep fallback — verify with read_source_file

Step 3: analyze_impact(semantic_path="<failing function>", max_depth=1)
        → Find all callers to understand what inputs are being passed

Step 4: search_codebase(query="<error message or pattern>")
        → Find where the error originates in the codebase
```

---

## Advanced Patterns

### Multi-File Edit Chain

When a refactor touches multiple files (e.g., update interface + all implementations + tests), maintain the version hash chain per file:

```
# File A: Update the interface
read_symbol_scope("fileA.go::Storage") → hash_A1
replace_full("fileA.go::Storage", base_version=hash_A1) → hash_A2

# File B: Update implementation (uses its OWN hash, not File A's)
read_symbol_scope("fileB.go::PostgresStorage.Create") → hash_B1
replace_body("fileB.go::PostgresStorage.Create", base_version=hash_B1) → hash_B2

# File C: Update tests
read_symbol_scope("fileC_test.go::TestCreate") → hash_C1
replace_body("fileC_test.go::TestCreate", base_version=hash_C1) → hash_C2
```

**Key insight:** Each file has its own independent hash chain. Don't mix hashes across files.

### Same-File Multi-Symbol Edit

When editing **multiple non-contiguous symbols in the same file** (e.g., fixing 3 unrelated functions), use `replace_batch`:

```
# Preferred: single atomic call with replace_batch
read_symbol_scope("main.py::_collect_words") → hash_v1
replace_batch(filepath="main.py", base_version=hash_v1, edits=[
  { semantic_path: "main.py::_collect_words", edit_type: "replace_body", new_code: "..." },
  { semantic_path: "main.py::_patch_audio_urls", edit_type: "replace_body", new_code: "..." },
  { semantic_path: "main.py::_generate_tts", edit_type: "replace_full", new_code: "..." },
]) → hash_v2
```

**All five `edit_type` values for `replace_batch` Option A:**
- `replace_body` — replace internal logic, keep signature
- `replace_full` — replace entire declaration (signature + body + decorators + doc comments)
- `insert_before` — insert new code before the target symbol
- `insert_after` — insert new code after the target symbol
- `delete` — delete the target symbol (no `new_code` needed)

**Why `replace_batch` is preferred:**
- **Atomic** — all edits land in one write with a single OCC guard
- Each edit targets a **symbol name**, not a fragile text string
- Edits are applied back-to-front (by byte offset) to avoid offset shifting
- LSP validation runs once on the combined result
- If any edit fails, the **entire batch is rolled back** atomically

**Fallback: sequential chaining** (when edits depend on each other's results):

```
read_symbol_scope("main.py::_collect_words") → hash_v1
replace_body("main.py::_collect_words", base_version=hash_v1) → hash_v2
replace_body("main.py::_patch_audio_urls", base_version=hash_v2) → hash_v3
```

**When to fall back to multiple `Edit` calls:**
- Pathfinder tools are unavailable (server offline)
- Edits are inside non-symbol regions (e.g., top-level constants, inline comments)
- Trivial single-line changes across many locations

### Vue SFC Text Targeting

Vue Single-File Components have three zones: `<script>`, `<template>`, and `<style>`. The `<script>` zone is fully AST-aware (TypeScript symbols are extracted and addressable). The `<template>` and `<style>` zones have **no AST symbols** — use `replace_batch` with **Option B (text targeting)** for edits there:

```
# Edit a template element by surrounding text context
read_source_file("src/views/Dashboard.vue") → hash_v1
replace_batch(filepath="src/views/Dashboard.vue", base_version=hash_v1, edits=[

  # Option A — Script zone: semantic targeting works normally
  { semantic_path: "src/views/Dashboard.vue::setup", edit_type: "replace_body",
    new_code: "..." },

  # Option B — Template zone: text targeting required
  { old_text: "<div class=\"card\">", context_line: 42,
    replacement_text: "<div class=\"card elevated\">" },

  # Option B with normalize_whitespace for multi-line HTML
  { old_text: "<Button @click=\"submit\">",
    context_line: 55, normalize_whitespace: true,
    replacement_text: "<Button @click=\"handleSubmit\">" },
])
```

**Rules for text targeting:**
- `context_line` (1-indexed) anchors the search — Pathfinder scans ±10 lines around it
- Set `normalize_whitespace: true` to collapse `\s+` → single space (safe for HTML; **do NOT** use for Python or YAML where indent is significant)
- Both Option A and Option B edits may be mixed in a single `replace_batch` call
- If any edit fails (e.g., `TEXT_NOT_FOUND`), the **entire batch is rolled back** atomically

### Efficient Search

`search_codebase` has several parameters that significantly reduce token waste:

| Parameter | Default | Purpose |
|---|---|---|
| `filter_mode` | `code_only` | AST-aware filtering: `code_only` excludes comments/strings, `comments_only` for TODOs/FIXMEs, `all` for everything |
| `exclude_glob` | `""` | Exclude files before search (e.g., `**/*.test.*`) — files are never read, saving I/O |
| `known_files` | `[]` | List of file paths already in your context. Matches in these files return minimal metadata (no content), saving tokens |
| `group_by_file` | `false` | Group matches by file with a single shared `version_hash` per group — cleaner for multi-file edits |
| `is_regex` | `false` | Treat query as regex (e.g., `unwrap\(\)|expect\(` to find Rust panics) |
| `path_glob` | `**/*` | Limit search scope (e.g., `src/**/*.ts` to search only TypeScript files in src/) |
| `max_results` | `50` | Cap results returned |
| `context_lines` | `2` | Lines of context above/below each match |

**Token efficiency pattern:**
```
# After reading fileA.ts and fileB.ts, search without re-reading their content:
search_codebase(query="deprecated_api",
                known_files=["src/fileA.ts", "src/fileB.ts"],
                exclude_glob="**/*.test.*",
                group_by_file=true)
```

**Known issue:** When ALL matches are in `known_files` files, `file_groups` may appear empty despite `total_matches > 0`. This is a serialization bug. Workaround: if `total_matches > 0` but `file_groups` is empty, re-run with `group_by_file=false` and `known_files=[]` to get the full flat `matches` list.

### Discovery→Edit Chaining

`search_codebase` and `get_repo_map` return version hashes, so you can skip the read step and edit directly:

```
search_codebase(query="deprecated_function") → results with version_hash per file
replace_full(semantic_path=result.enclosing_semantic_path,
             base_version=result.version_hash, new_code="<fixed code>")
```

**Use sparingly** — only when the search result gives you enough context to write the replacement code without reading the full function.

---

## LSP Navigation Tools and Degraded Mode

Three Pathfinder tools depend on LSP for precise results:

| Tool | LSP Feature Used | Degraded Fallback |
|---|---|---|
| `get_definition` | `textDocument/definition` | Multi-strategy ripgrep (file-scoped → impl-scoped → global) |
| `analyze_impact` | `callHierarchy/incomingCalls` + `outgoingCalls` | Grep heuristics for callers |
| `read_with_deep_context` | `callHierarchy/outgoingCalls` | Tree-sitter only (no dependency signatures) |

### Interpreting Degraded Results

Every LSP-dependent response includes `degraded` (boolean) and `degraded_reason` (string):

| `degraded_reason` | Meaning | What to do |
|---|---|---|
| `null` (not degraded) | LSP confirmed the result | Trust fully |
| `no_lsp` | No language server available | Install the LSP or accept limited results |
| `lsp_warmup_empty_unverified` | LSP still indexing; empty results unverified | Do NOT treat empty as confirmed-zero. Re-run after a few seconds |
| `lsp_warmup_grep_fallback` | LSP returned null; result from grep | Verify with `read_source_file` |
| `grep_fallback_file_scoped` | No LSP; file-scoped grep result | Good confidence if file path was correct |
| `grep_fallback_impl_scoped` | No LSP; impl-block grep result | Good for Rust methods |
| `grep_fallback_global` | No LSP; global grep result | Least precise. Verify before relying on it |
| `lsp_error` | LSP returned an error | Results from Tree-sitter/grep only |

### Key Rule

**When `degraded: true`, never treat empty results as confirmed-zero.** The LSP may not have finished indexing. Re-run the tool after a few seconds, or check `get_repo_map` capabilities to see if `indexing_complete` is true.

---

## Error Recovery Patterns

### SYMBOL_NOT_FOUND

```
Error: SYMBOL_NOT_FOUND for "src/auth.ts::AuthServce.login"
       did_you_mean: ["AuthService.login", "AuthService.logout"]

Recovery:
→ Use the corrected path from did_you_mean
→ Retry: read_symbol_scope(semantic_path="src/auth.ts::AuthService.login")
```

### VERSION_MISMATCH

```
Error: VERSION_MISMATCH — file was modified since your last read

Recovery:
→ Re-read the file: read_symbol_scope(semantic_path="<target>")
→ Get the fresh version_hash
→ Retry the edit with the new base_version
```

### Validation Failures (introduced_errors)

```
Response: validation.status = "failed"
          introduced_errors: [{ message: "cannot find name 'foo'", ... }]

Recovery:
→ Read the introduced_errors to understand what broke
→ Fix your new_code to address the errors
→ Use validate_only to dry-run before committing the fix
→ Apply the corrected edit
```

### Validation Skipped or Uncertain

```
Response: validation.validation_skipped = true
          validation.validation_skipped_reason = "no_lsp" | "lsp_not_on_path" |
              "lsp_start_failed" | "lsp_crash" | "lsp_timeout" |
              "pull_diagnostics_unsupported" |
              "empty_diagnostics_both_snapshots"

This means: For validate_only — the edit was NOT written to disk (dry-run only)
and LSP validation could not confirm the code is clean. For real edit tools —
the edit WAS written to disk but was NOT validated by LSP.

Special case: `"empty_diagnostics_both_snapshots"` — both pre- and post-edit
diagnostics were empty. This could mean the code is genuinely clean, OR the LSP
hasn't finished indexing. The `validation.status` will be `"uncertain"` (not
`"passed"`) to signal this ambiguity. Do NOT treat `"uncertain"` as confirmation.

When you see this:
→ Check capabilities.lsp.per_language via get_repo_map to understand LSP status
→ The edit landed successfully (if not validate_only) — but you have no compile-time safety net
→ Compensate by running tests (Bash) or manual review
→ If the reason is "lsp_not_on_path", suggest the user install the language server
→ If the reason is "lsp_crash" or "lsp_timeout", the LSP may recover on next edit
```

### TEXT_NOT_FOUND (replace_batch Option B)

```
Error: TEXT_NOT_FOUND for old_text="<div class=\"card\">"

Recovery:
→ The old_text was not found within ±10 lines of context_line
→ Re-read the file with read_source_file to find the correct text and line number
→ Adjust old_text to match exactly, or update context_line
→ Consider normalize_whitespace: true if whitespace differences are the issue
→ Retry the replace_batch call
```

---

## Tool Chain Quick Reference

| I want to... | Tool chain |
|---|---|
| Understand a new project | `get_repo_map` → `read_with_deep_context` |
| Read an entire source file with AST | `read_source_file` |
| Find and read a function | `search_codebase` → `read_symbol_scope` |
| Edit a function body | `read_symbol_scope` → `replace_body` |
| Edit multiple symbols in one file | `read_symbol_scope` → `replace_batch` |
| Add a new function to a file | `read_symbol_scope` (neighbor) → `insert_after` |
| Rename/restructure a function | `analyze_impact` → `replace_full` (+ update callers) |
| Delete a function safely | `analyze_impact` → `delete_symbol` |
| Delete a file safely | `read_file` → `delete_file` (use version_hash as base_version) |
| Check an edit before applying | `read_symbol_scope` → `validate_only` → (if ok) → `replace_body` |
| Find all usages before refactoring | `analyze_impact` (max_depth=2) |
| Add imports to a file | `insert_before` (bare file path, no `::`) |
| Append a class to end of file | `insert_after` (bare file path, no `::`) |
| Edit a config file surgically | `read_file` → `write_file` (with `replacements`) |
| Edit multiple files in sequence | Per-file hash chains (see Advanced Patterns) |

---

## Graceful Fallback

If Pathfinder MCP tools are **not available** (server offline, tools not surfaced in the function list), fall back to built-in tools transparently:

| Pathfinder tool | Built-in fallback |
|---|---|
| `read_symbol_scope` / `read_with_deep_context` | `Read` (with line ranges for focused reading) |
| `read_source_file` | `Read` |
| `read_file` | `Read` |
| `search_codebase` | `Grep` |
| `replace_body` / `replace_full` | `Edit` |
| `replace_batch` | Multiple `Edit` calls |
| `insert_before` / `insert_after` | `Edit` |
| `delete_symbol` | `Edit` (replace with empty) |
| `delete_file` | `Bash` (`rm`) |
| `get_repo_map` | `Glob` or `Bash` (with `ls`) |
| `analyze_impact` | `Grep` (search for function name — approximate) |
| `create_file` | `Write` |
| `write_file` | `Edit` |
| `validate_only` | No equivalent — rely on `Bash` with linter/compiler |

**Rules:**
- Do not block on Pathfinder being unavailable — complete the work with built-in tools
- Note the degradation to the user if asked
- When Pathfinder comes back online, resume using it immediately — no need to redo prior work
