---
name: pathfinder-workflow
description: Use when exploring a codebase, refactoring code, implementing features, or auditing code quality effectively.
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

### Tool Preference Table

> **Canonical source:** The always-on rule `pathfinder-tool-routing.md` is the single source of truth for which tool to use. This table adds the **"Why"** column for deeper understanding but defers to the rule for routing decisions.

| Action | Prefer (Pathfinder) | Instead of (Built-in) | Why |
|---|---|---|---|
| Explore project structure | `get_repo_map` | `list_dir` + `view_file` | One call returns full skeleton with semantic paths + version hashes for immediate editing |
| Search for code patterns | `search_codebase` | `grep_search` | Returns `enclosing_semantic_path` + `version_hash` per match — enables Discovery→Edit chaining |
| Read a function or class | `read_symbol_scope` | `view_file` | Extracts exactly one symbol — no context waste, returns `version_hash` for OCC |
| Read function + dependencies | `read_with_deep_context` | Multiple `view_file` calls | Returns target code + signatures of all called functions in one call |
| Jump to a definition | `get_definition` | `grep_search` (approximation) | LSP-powered, follows imports and re-exports across files |
| Assess refactoring impact | `analyze_impact` | No equivalent | Maps all incoming callers + outgoing callees with BFS traversal |
| Edit a function body | `replace_body` | `replace_file_content` | Semantic addressing + auto-indentation + LSP validation before disk write |
| Edit an entire declaration | `replace_full` | `replace_file_content` | Semantic addressing, includes signature/decorators/doc comments |
| Add code before/after a symbol | `insert_before` / `insert_after` | `replace_file_content` | Semantic anchor point + auto-spacing |
| Delete a function or class | `delete_symbol` | `replace_file_content` (empty) | Handles decorators, doc comments, whitespace cleanup |
| Pre-check a risky edit | `validate_only` | No equivalent | Dry-run with LSP diagnostics, zero disk side-effects |
| Create a new file | `create_file` | `write_to_file` | Returns `version_hash` for subsequent OCC-protected edits |
| Edit a config file (.env, Dockerfile, YAML) | `write_file` | `replace_file_content` | OCC protection; supports search-and-replace mode for surgical edits |
| Read a config file | `read_file` | `view_file` | Either is fine — roughly equivalent for config files |

### Keep Using Built-in Tools For

These tasks have **no Pathfinder equivalent** — always use built-in tools:

- **Listing directory contents** → `list_dir`, `find_by_name`
- **Running commands** (tests, linters, builds) → `run_command`
- **Viewing binary files** (images, videos) → `view_file`
- **Multiple non-contiguous edits in one file** → Sequential Pathfinder `replace_body` calls (preferred) or `multi_replace_file_content` (fallback). See **Same-File Multi-Symbol Edit** in Advanced Patterns.
- **Quick one-line fixes where you already know the exact line** → `replace_file_content` is acceptable for trivial edits

---

## OCC (Optimistic Concurrency Control)

All Pathfinder edit tools require a `base_version` (SHA-256 hash). This prevents overwriting changes made by other tools or agents.

### Where to Get Version Hashes

| Source tool | Field |
|---|---|
| `read_symbol_scope` | `version_hash` |
| `read_with_deep_context` | `version_hash` |
| `search_codebase` | `version_hash` (per match) |
| `get_repo_map` | `version_hashes` (per file) |
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

---

## Workflow 1: Explore (Understand a Codebase)

**Goal:** Build a mental model of the project from zero context.

```
Step 1: get_repo_map(path=".", depth=3, visibility="public")
        → Get the full project skeleton with semantic paths
        → Note the tech_stack and files_scanned to understand project scale
        → If coverage_percent is low: increase max_tokens (more files)
        → If files show [TRUNCATED DUE TO SIZE]: increase max_tokens_per_file (more detail per file)
        → Use visibility="all" to include private symbols (better for auditing)

Step 2: search_codebase(query="<entry point pattern>", path_glob="src/**/*")
        → Find main entry points, API handlers, or CLI commands
        → Use enclosing_semantic_path from results for next step

Step 3: read_with_deep_context(semantic_path="<chosen entry point>")
        → Read the entry point + all functions it calls
        → Follow the dependency chain to understand data flow

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

Step 6: run_command("cargo test" / "npm test")  [built-in tool]
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

Step 5: run_command("cargo test" / "npm test")  [built-in tool]
```

---

## Workflow 4: Audit (Review Code Quality)

**Goal:** Systematically review a codebase for issues.

```
Step 1: get_repo_map(path=".", depth=4, visibility="all")
        → Get complete project overview including private symbols

Step 2: For each module/feature area:
        a. read_symbol_scope(semantic_path="<public API function>")
           → Review the public interface
        b. read_with_deep_context(semantic_path="<complex function>")
           → Check that dependencies are reasonable
        c. search_codebase(query="<language-specific danger pattern>", path_glob="src/**/*")
           → Find potential crash/error points. Examples by language:
             - Go: `panic|log.Fatal`
             - Rust: `unwrap|expect|panic`
             - Python: `except:|pass  # noqa`
             - TypeScript: `as any|@ts-ignore`
        d. search_codebase(query="TODO|FIXME|HACK", filter_mode="comments_only")
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

When editing **multiple non-contiguous symbols in the same file** (e.g., fixing 3 unrelated functions), chain `new_version_hash` between edits:

```
# Same file: fix function A, then function B, then function C
read_symbol_scope("main.py::_collect_words") → hash_v1
replace_body("main.py::_collect_words", base_version=hash_v1) → hash_v2
replace_body("main.py::_patch_audio_urls", base_version=hash_v2) → hash_v3
replace_full("main.py::_generate_tts", base_version=hash_v3) → hash_v4
```

**Why this beats `multi_replace_file_content`:**
- Each edit targets a **symbol name**, not a fragile text string — no risk of matching the wrong occurrence
- LSP validation runs after **each** edit — errors are caught immediately, not discovered after all edits land
- OCC ensures no concurrent modification between edits

**When to fall back to `multi_replace_file_content`:**
- Pathfinder tools are unavailable (server offline)
- Edits are inside non-symbol regions (e.g., top-level constants, inline comments)
- Trivial single-line changes across many locations (regex-safe `AllowMultiple=True`)

### Discovery→Edit Chaining

`search_codebase` and `get_repo_map` return version hashes, so you can skip the read step and edit directly:

```
search_codebase(query="deprecated_function") → results with version_hash per file
replace_full(semantic_path=result.enclosing_semantic_path,
             base_version=result.version_hash, new_code="<fixed code>")
```

**Use sparingly** — only when the search result gives you enough context to write the replacement code without reading the full function.

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

---

## Tool Chain Quick Reference

| I want to... | Tool chain |
|---|---|
| Understand a new project | `get_repo_map` → `read_with_deep_context` |
| Find and read a function | `search_codebase` → `read_symbol_scope` |
| Edit a function body | `read_symbol_scope` → `replace_body` |
| Add a new function to a file | `read_symbol_scope` (neighbor) → `insert_after` |
| Rename/restructure a function | `analyze_impact` → `replace_full` (+ update callers) |
| Delete a function safely | `analyze_impact` → `delete_symbol` |
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
| `read_symbol_scope` / `read_with_deep_context` | `view_file` + `view_code_item` |
| `search_codebase` | `grep_search` |
| `replace_body` / `replace_full` | `replace_file_content` |
| `insert_before` / `insert_after` | `replace_file_content` |
| `get_repo_map` | `list_dir` + `view_file_outline` |
| `analyze_impact` | Manual grep + `view_code_item` (approximate) |

**Rules:**
- Do not block on Pathfinder being unavailable — complete the work with built-in tools
- Note the degradation to the user if asked
- When Pathfinder comes back online, resume using it immediately — no need to redo prior work
