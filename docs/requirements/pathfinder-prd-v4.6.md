# 🚀 PRODUCT REQUIREMENTS DOCUMENT (PRD)

**Product Name:** Pathfinder

**Tagline:** The Headless IDE MCP Server for AI Coding Agents

**Version:** 4.6

**Status:** Approved for Engineering

---

## 1. Executive Summary

Pathfinder is a **request-stateless, workspace-scoped**, high-performance Model Context Protocol (MCP) server that acts as a "Headless IDE" for AI coding agents. It solves the context exhaustion, code hallucination, and destructive editing problems of current AI workflows.

Pathfinder orchestrates a **Tri-Engine Synergy Funnel**:

1. **The Scout (Ripgrep):** Blazing-fast lexical discovery.
2. **The Surgeon (Tree-sitter, native):** Structural precision, dynamic context window pruning, and surgical extraction.
3. **The Lawyer (Polyglot LSPs):** Semantic truth, dependency resolution, and in-memory validation of AI edits before disk write.

Each tool call is self-contained — no conversational state is maintained between calls. Operational infrastructure (AST caches, LSP processes) is maintained per workspace for performance.

### 1.1 State Model

| State Type                   | Where It Lives                                  | Lifecycle                                                  |
| ---------------------------- | ----------------------------------------------- | ---------------------------------------------------------- |
| **Conversational state**     | External orchestrator (Claude, LangChain, etc.) | Per-session                                                |
| **Workspace infrastructure** | Pathfinder process                              | Per-process lifetime                                       |
| **AST parse caches**         | Pathfinder process (in-memory)                  | Evicted on file change or process restart                  |
| **LSP processes**            | Spawned as child processes                      | Lazy-init, auto-terminate after configurable idle timeout  |
| **Edit buffers**             | Transient, per tool call                        | Created and destroyed within a single edit tool invocation |

### 1.2 Workspace Scoping

With stdio transport, the workspace is **implicit in the process**. Each Pathfinder process is started with a single workspace root path as its argument (`pathfinder /path/to/project`). All file operations, AST caches, LSP processes, and sandbox rules are scoped to that root.

There is no workspace ID, no workspace parameter on tool calls, and no multi-workspace routing. The process *is* the workspace. Multiple projects are served by multiple processes — isolation is provided by the OS, not by application logic.

| Aspect              | Implication                             |
| ------------------- | --------------------------------------- |
| File paths          | Resolved relative to workspace root     |
| AST cache           | Scoped to workspace root                |
| LSP processes       | Spawned with `rootUri` = workspace root |
| Sandbox rules       | Applied relative to workspace root      |
| `.pathfinderignore` | Read from workspace root                |

### 1.3 Addressing Model

All symbol-level tools use a unified **semantic path** addressing scheme. This is the **single addressing mode** across discovery, read, navigation, and edit tools — agents never need to split or recombine path components.

```ebnf
semantic_path   = file_path ["::" symbol_chain]
file_path       = relative_path
symbol_chain    = symbol ("." symbol)*
symbol          = identifier [overload_suffix]
overload_suffix = "#" digit+
```

**Examples:**
- `src/auth.ts::AuthService.login` — method
- `src/auth.ts::AuthService.login#2` — second overload
- `src/utils.ts::formatDate` — top-level function
- `src/config.ts::DEFAULT_CONFIG` — top-level const
- `src/auth.ts::default` — export default
- `src/utils.ts` — bare file path (no symbol chain) — targets BOF/EOF for `insert_before`/`insert_after`

This ensures agents never need to provide raw line/column positions — Pathfinder resolves semantic paths to exact AST positions internally via Tree-sitter.

**Fuzzy error hints:** When a semantic path doesn't resolve, Pathfinder computes Levenshtein distance to available symbols in the target file and populates `did_you_mean` in the `SYMBOL_NOT_FOUND` error (e.g., `"did_you_mean": ["stopServer", "startServer"]`). Pathfinder **never** auto-corrects and auto-executes — guessing intent on destructive file operations is a correctness violation.

**Tool chaining:** Discovery tools (`search_codebase`, `get_repo_map`) return `enclosing_semantic_path` and `version_hash` in their responses, enabling direct chaining from Discovery → Edit in a single inference turn without intermediate reads.

---

### 1.4 Competitive Positioning

The current AI coding ecosystem forces developers to choose between proprietary GUI lock-in, stateful CLI applications, or heavy enterprise indexers.

Pathfinder creates a new category: **Deterministic Infrastructure for Autonomous Agents.** It is not an AI agent itself — it is the headless, compiler-verified environment that makes *any* agent (Claude Desktop, LangChain bots, CI/CD scripts) reliable via the open Model Context Protocol (MCP).

#### Pathfinder vs. Aider (The Editing Paradigm)

|                    | Aider                                                           | Pathfinder                                                                          |
| ------------------ | --------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| **Edit mechanism** | LLM generates fuzzy SEARCH/REPLACE text blocks or unified diffs | Agent targets deterministic Semantic Paths (`auth.ts::login`) — zero fuzzy matching |
| **Validation**     | Writes broken code to disk *first*, runs compiler *after*       | Shadow Editor validates in-memory via LSP *before* touching disk                    |
| **Indentation**    | LLM responsible for correct indentation                         | Pathfinder dedents to column 0, re-indents via AST prefix automatically             |
| **Multi-agent**    | Monolithic terminal app, single-user                            | MCP server — composable with any orchestrator, any agent                            |
| **Repo context**   | Pioneered Tree-sitter repo map                                  | Brings Aider-level repo maps to *any* agent, not just Aider's own CLI               |
| **Failure mode**   | LLM miscounts lines → silent file corruption                    | `SYMBOL_NOT_FOUND` + `did_you_mean` → agent self-corrects on next turn              |

#### Pathfinder vs. Cursor / Windsurf (The IDE Paradigm)

|                    | Cursor/Windsurf                                              | Pathfinder                                                                 |
| ------------------ | ------------------------------------------------------------ | -------------------------------------------------------------------------- |
| **Distribution**   | Proprietary VS Code fork (GUI-locked)                        | Single Rust binary via MCP (headless, composable)                          |
| **Intelligence**   | Background vector embeddings (stale on write)                | Real-time Tree-sitter AST — 100% ground-truth at exact millisecond of call |
| **Portability**    | Cannot extract intelligence to a script, CI/CD, or Slack bot | Runs headlessly in GitHub Actions, autonomous agents, or any MCP client    |
| **Context model**  | Merkle tree sync + chunked embeddings                        | Live AST cache + incremental Tree-sitter parse                             |
| **Staleness risk** | Vector DB drifts as code changes                             | No DB — cache updated synchronously on every write                         |

#### Pathfinder vs. Sourcegraph / Cody (The Intelligence Paradigm)

|                      | Sourcegraph/Cody                                           | Pathfinder                                        |
| -------------------- | ---------------------------------------------------------- | ------------------------------------------------- |
| **Indexing**         | Heavy upfront SCIP databases, network-bound                | Zero-config, local-first, no background indexing  |
| **Boot time**        | Minutes to hours for initial index                         | Milliseconds (Tree-sitter parses on demand)       |
| **Optimized for**    | Reading and discovery at massive enterprise scale          | Writing and mutation on a local workspace         |
| **Deployment**       | SaaS or self-hosted server infrastructure                  | Single binary, runs as child process of the agent |
| **Write capability** | None — read-only intelligence                              | Full edit pipeline with validation and formatting |
| **Future**           | Discontinuing free/pro tiers, pivoting to enterprise "Amp" | Open MCP protocol — any agent, any scale          |

#### Pathfinder vs. Naive MCP File Servers

|                  | Standard MCP FS                                    | Pathfinder                                            |
| ---------------- | -------------------------------------------------- | ----------------------------------------------------- |
| **Edit method**  | Raw `write_file` with full content or line numbers | AST-anchored Semantic Paths + search-and-replace      |
| **LLM weakness** | Exploits LLM's worst trait (line counting via BPE) | Exploits LLM's best trait (verbatim text recall)      |
| **Context cost** | Must read entire file to make one edit             | `read_symbol_scope` extracts only the target function |
| **Validation**   | None — writes blindly to disk                      | LSP Pull Diagnostics catch errors *before* disk write |
| **Indentation**  | LLM must produce perfectly indented output         | Auto-dedent + AST re-indent handles it automatically  |

> **Bottom line:** Pathfinder is not competing with agents — it is the *infrastructure layer* that makes them safe. Aider, Cursor, and Cody are tools an engineer uses. Pathfinder is the operating table those tools should be built on.

---

## 2. Tech Stack

| Component         | Technology                             | Rationale                                                                                                   |
| ----------------- | -------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| **Language**      | Rust                                   | First-class tree-sitter integration (native, same as Helix/Zed). No GC pauses. Predictable `<50ms` latency. |
| **Async runtime** | Tokio                                  | Industry standard for Rust async I/O, subprocess management                                                 |
| **Tree-sitter**   | `tree-sitter` crate (native C via FFI) | Compiles C runtime into binary. No WASM overhead.                                                           |
| **LSP types**     | `lsp-types` crate                      | Actively maintained, tracks LSP spec                                                                        |
| **LSP client**    | Custom (~500-800 LOC)                  | JSON-RPC over stdio to child LSP processes. No framework needed.                                            |
| **JSON**          | `serde` + `serde_json`                 | Best-in-class serialization                                                                                 |
| **MCP transport** | **stdio** (v1)                         | Natural 1-process-per-workspace isolation. Native support in Claude Desktop, Cursor, Cline.                 |
| **File watching** | `notify` crate                         | Cross-platform file system events                                                                           |

### 2.1 Build Requirements

- Rust toolchain (stable)
- C compiler (`cc`) — required by tree-sitter native build
- LSP servers installed on user's machine (`typescript-language-server`, `pyright`, `gopls`)

### 2.2 Distribution

Single static binary per platform. No runtime dependencies beyond installed LSPs.

---

## 3. Functional Requirements (MCP Tool API)

### Tool Overview

| Category   | Tool                     | Purpose                                         |
| ---------- | ------------------------ | ----------------------------------------------- |
| Discovery  | `search_codebase`        | Text search with AST-aware filtering            |
| Discovery  | `get_repo_map`           | AST-based project skeleton                      |
| Read       | `read_symbol_scope`      | Extract exact AST block                         |
| Read       | `read_with_deep_context` | Extract block + dependency signatures           |
| Navigation | `get_definition`         | Jump to symbol definition                       |
| Navigation | `analyze_impact`         | Call hierarchy (who uses / what uses)           |
| Edit       | `replace_body`           | Replace function body                           |
| Edit       | `replace_full`           | Replace entire declaration                      |
| Edit       | `insert_before`          | Insert code before symbol                       |
| Edit       | `insert_after`           | Insert code after symbol                        |
| Edit       | `delete_symbol`          | Remove symbol entirely                          |
| Edit       | `validate_only`          | Dry-run validation (no disk write)              |
| File       | `create_file`            | Create new file with content                    |
| File       | `delete_file`            | Delete a file                                   |
| File       | `read_file`              | Raw file read (any file type)                   |
| File       | `write_file`             | Raw file write with full or search-replace mode |

---

### 3.1 Context & Discovery Tools

#### `search_codebase`

Search the workspace for a text pattern.

| Parameter       | Type   | Default       | Description                                 |
| --------------- | ------ | ------------- | ------------------------------------------- |
| `query`         | string | *required*    | Search pattern (literal or regex)           |
| `is_regex`      | bool   | `false`       | Treat query as regex                        |
| `path_glob`     | string | `**/*`        | Limit search scope (e.g., `src/**/*.ts`)    |
| `filter_mode`   | enum   | `"code_only"` | `"code_only"` / `"comments_only"` / `"all"` |
| `max_results`   | int    | `50`          | Maximum matches returned                    |
| `context_lines` | int    | `2`           | Lines of context above/below each match     |

**Engine pipeline:**
1. Ripgrep: raw text search
2. If `filter_mode != "all"`: Tree-sitter parses each matched file, checks AST node type at match location, drops matches in `comment` / `string_literal` nodes (or keeps only those for `"comments_only"`)
3. For each match: Tree-sitter walks up from match position to find the enclosing named AST node → compute `enclosing_semantic_path`

**Returns:** JSON array of matches:
```json
{
  "matches": [
    {
      "file": "src/auth.ts",
      "line": 42,
      "column": 8,
      "content": "const token = jwt.sign(payload)",
      "context_before": ["  async login(user: User) {", "    const payload = { id: user.id };"],
      "context_after": ["    return { token, expiresIn: 3600 };", "  }"],
      "enclosing_semantic_path": "src/auth.ts::AuthService.login",  // null for non-AST files
      "version_hash": "sha256:abc123..."
    }
  ],
  "total_matches": 12,
  "truncated": false
}
```

| Field                     | Purpose                                                                                                                                                      |
| ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `enclosing_semantic_path` | Copy-pasteable handle for edit tools. Bridges Discovery → Edit without guessing. **Nullable** — `null` for matches in non-AST files (YAML, Markdown, `.env`) |
| `version_hash`            | SHA-256 of the file. Use as `base_version` for immediate edit — no intermediate read needed                                                                  |

---

#### `get_repo_map`

Generate an AST-based skeleton of a directory.

| Parameter         | Type   | Default              | Description                                                                                                         |
| ----------------- | ------ | -------------------- | ------------------------------------------------------------------------------------------------------------------- |
| `path`            | string | `.` (workspace root) | Directory to map                                                                                                    |
| `max_tokens`      | int    | `4096`               | Token budget (counted as `ceil(chars / 4)`)                                                                         |
| `depth`           | int    | `3`                  | Max directory traversal depth                                                                                       |
| `visibility`      | enum   | `"public"`           | `"public"` — exported/public symbols only. `"all"` — include private/internal symbols                               |
| `include_imports` | enum   | `"third_party"`      | `"none"` — omit all imports. `"third_party"` — include only external/package imports. `"all"` — include all imports |

**Pruning algorithm:**
1. Walk directory breadth-first
2. Per file: Tree-sitter extracts symbols based on `visibility`:
   - `"public"` (default): public classes, interfaces, exported functions (signatures only, no bodies)
   - `"all"`: all classes, interfaces, and functions regardless of visibility (signatures only, no bodies)
   - Note: `"all"` consumes significantly more tokens — use when investigating internal call chains
3. After adding each file, check `accumulated_tokens <= max_tokens`
   - If exceeded → stop, append `[... N more files]`
4. Within a file, if its skeleton exceeds 512 tokens:
   - Keep only class names + public method signatures (no parameters)
   - Append `[... N methods omitted]`
5. Import handling (based on `include_imports`):
   - `"none"`: omit all imports
   - `"third_party"` (default): include only imports from external packages (detected by: not starting with `.` or `..` in JS/TS, not relative in Python, not within the module path in Go/Rust)
   - `"all"`: include all import statements
   - Imports are counted toward the token budget like any other content.

**Skeleton output format — explicit semantic paths:**

Each symbol in the skeleton includes its full copy-pasteable semantic path as a trailing comment:

```
src/auth.ts
  class AuthService                    // src/auth.ts::AuthService
    login(user: User): Promise<Token>  // src/auth.ts::AuthService.login
    logout(): void                     // src/auth.ts::AuthService.logout
    refreshToken(token: string)        // src/auth.ts::AuthService.refreshToken
    refreshToken(token: string, force) // src/auth.ts::AuthService.refreshToken#2
  [... 3 methods omitted]

src/utils.ts
  formatDate(date: Date): string       // src/utils.ts::formatDate
  parseConfig(path: string): Config    // src/utils.ts::parseConfig
```

This ensures agents can identify the exact semantic path (including overload disambiguation like `#2`) for use in subsequent tool calls.

**Returns:** Indented text skeleton (with semantic path comments). Optionally JSON with `format: "json"`:

```json
{
  "skeleton": "...",
  "tech_stack": ["express", "prisma", "zod", "jsonwebtoken"],
  "files_scanned": 42,
  "files_truncated": 8,
  "files_in_scope": 200,
  "coverage_percent": 21,
  "version_hashes": {
    "src/auth.ts": "sha256:abc123...",
    "src/utils.ts": "sha256:def456..."
  }
}
```

| Field              | Purpose                                                                                        |
| ------------------ | ---------------------------------------------------------------------------------------------- |
| `version_hashes`   | Per-file SHA-256. Use as `base_version` for immediate edit — enables Discovery → Edit chaining |
| `files_in_scope`   | Total files in the target directory (cheap directory listing, no parsing)                      |
| `coverage_percent` | `files_scanned / files_in_scope * 100`. If low, increase `max_tokens` or narrow `path`         |

---

### 3.2 Read & Extraction Tools

#### `read_symbol_scope`

Extract an exact AST block via semantic path.

| Parameter       | Type   | Default    | Description                            |
| --------------- | ------ | ---------- | -------------------------------------- |
| `semantic_path` | string | *required* | e.g., `src/auth.ts::AuthService.login` |

**Returns:** `{ content, start_line, end_line, version_hash, language }`. The `version_hash` is SHA-256 of the full file content, used for OCC.

---

#### `read_with_deep_context`

Extract the target scope plus signatures of all external dependencies called within it.

| Parameter       | Type   | Default    | Description                 |
| --------------- | ------ | ---------- | --------------------------- |
| `semantic_path` | string | *required* | Same as `read_symbol_scope` |

**Engine pipeline:**
1. Tree-sitter: extract target function body
2. Tree-sitter: find all `call_expression` nodes inside it
3. LSP: `textDocument/definition` → locate each call's definition
4. Tree-sitter: extract only the function **signature** (not body) from the definition site

**Returns:** `{ content, version_hash, context_appendix: [{ symbol, file, signature, docstring }] }`.

---

### 3.3 Semantic Navigation

#### `get_definition`

Jump to where a symbol is defined.

| Parameter       | Type   | Default    | Description                                                             |
| --------------- | ------ | ---------- | ----------------------------------------------------------------------- |
| `semantic_path` | string | *required* | Semantic path to the reference (e.g., `src/auth.ts::AuthService.login`) |

**Engine pipeline:**
1. Tree-sitter: resolve `semantic_path` to file + exact AST position
2. LSP: `textDocument/definition` at resolved position

**Returns:** `{ file, line, column, preview, version_hash }` where `preview` is the first line of the definition.

---

#### `analyze_impact`

Aggregated call hierarchy — who uses this and what does this use.

| Parameter       | Type   | Default    | Description                                                          |
| --------------- | ------ | ---------- | -------------------------------------------------------------------- |
| `semantic_path` | string | *required* | Semantic path to the target (e.g., `src/auth.ts::AuthService.login`) |
| `max_depth`     | int    | `2`        | Traversal depth (max: 5)                                             |

**Cycle prevention:** Visited-set tracking. If a node is already traversed, it's marked and skipped.

**Returns:** Flat JSON (optimized for AI consumption):
```json
{
  "target": { "symbol": "...", "file": "...", "line": 42, "version_hash": "sha256:..." },
  "incoming": [{ "symbol": "...", "file": "...", "line": 15, "depth": 1, "version_hash": "sha256:..." }],
  "outgoing": [{ "symbol": "...", "file": "...", "line": 88, "depth": 1, "via": "...", "version_hash": "sha256:..." }],
  "cycles_detected": ["A → B → A"],
  "truncated": false
}
```

**Graceful degradation:** If LSP doesn't support `callHierarchy`, falls back to Tree-sitter `call_expression` scanning for outgoing calls. Incoming calls return empty with `"degraded": true`.

---

### 3.4 Safe Editing & Verification

All edit tools share a common execution pipeline with **in-memory validation before disk write**. The edit is first applied in memory, sent to the LSP for diagnostic analysis, and only written to disk if no new errors are introduced. If the edit would break the code, it is rejected — the file on disk is never left in a broken state.

#### Common Parameters

All edit tools accept these parameters:

| Parameter                    | Type   | Default    | Description                                                                                                                                                                                                                                  |
| ---------------------------- | ------ | ---------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `semantic_path`              | string | *required* | Full semantic path to the target (e.g., `src/auth.ts::AuthService.login`)                                                                                                                                                                    |
| `base_version`               | string | *required* | SHA-256 hash from previous read or discovery tool (OCC)                                                                                                                                                                                      |
| `ignore_validation_failures` | bool   | `false`    | When `true`, write to disk even if validation detects new errors. Response still includes `introduced_errors` so agent knows what to fix. **Use only during multi-file refactors** where dependent files will be updated in subsequent edits |

> **Note:** `semantic_path` is the same unified format used by all other tools. Pathfinder parses the file path and symbol chain internally via the `::` separator.
>
> **When to use `ignore_validation_failures`:** If the agent changes a function signature in `auth.ts`, workspace-wide validation will flag broken callers in `api.ts`. Without the override, the agent is permanently blocked from saving `auth.ts` — a deadlock. This flag breaks the deadlock while keeping the agent informed of downstream breakage.

#### Common Return Schema

All edit tools return the same response structure on success:

```json
{
  "success": true,
  "new_version_hash": "sha256:abc123...",
  "formatted": true,
  "validation": {
    "status": "passed",
    "introduced_errors": [],
    "resolved_errors": [
      { "severity": 1, "code": "TS2304", "message": "Cannot find name 'x'", "file": "src/auth.ts" }
    ]
  }
}
```

| Field                          | Type    | Description                                                                                                                     |
| ------------------------------ | ------- | ------------------------------------------------------------------------------------------------------------------------------- |
| `success`                      | bool    | Whether edit was applied                                                                                                        |
| `new_version_hash`             | string  | SHA-256 of file after edit — use as `base_version` for next edit                                                                |
| `formatted`                    | bool    | Whether LSP formatting was applied. `false` if LSP lacks formatting support (Tree-sitter indentation still applied)             |
| `validation.status`            | enum    | `"passed"` / `"failed"` / `"skipped"`                                                                                           |
| `validation.introduced_errors` | array   | **New** Severity 1 errors not present before the edit. Empty = pass                                                             |
| `validation.resolved_errors`   | array   | Pre-existing errors that the edit fixed                                                                                         |
| `validation_skipped`           | bool    | `true` if validation was skipped (no LSP, or timeout). Agent should run external build check (`cargo check`, `tsc`, `go build`) |
| `validation_skipped_reason`    | string? | Present when `validation_skipped` is `true`. Values: `"no_lsp"`, `"diagnostic_timeout"`, `"lsp_crash"`                          |

---

#### `replace_body`

Replace a function/method body (content inside braces), keeping the signature intact.

| Parameter  | Type   | Description                                     |
| ---------- | ------ | ----------------------------------------------- |
| `new_code` | string | Replacement body content (without outer braces) |

**Scope definition — what counts as "body":**

| Construct                                 | Body Range                              | Signature Preserved               |
| ----------------------------------------- | --------------------------------------- | --------------------------------- |
| Function/method with block body           | Content inside `{ }` (excluding braces) | Full signature + braces           |
| Arrow function with block: `=> { ... }`   | Content inside `{ }`                    | `(params) => {` preserved         |
| Arrow function with expression: `=> expr` | The expression itself                   | `(params) =>` preserved           |
| Class body                                | Content inside `{ }`                    | `class Name {` preserved          |
| Rust `impl` block body                    | Content inside `{ }`                    | `impl Trait for Type {` preserved |
| Getter/setter                             | Content inside `{ }`                    | `get prop() {` preserved          |

**What `new_code` should contain:**
- The body content **without** outer braces — Pathfinder preserves the existing braces
- For expression-bodied arrow functions, provide the replacement expression (Pathfinder handles the `=>`)
- Indentation: provide at base level (zero indent). Pathfinder re-indents via Tree-sitter baseline + optional LSP formatting

**Brace-leniency (forgiving input normalization):**

LLMs are heavily trained to produce syntactically complete code and will frequently wrap `new_code` in `{ }` despite being instructed not to. Instead of rejecting this, Pathfinder applies **automatic brace stripping**: if the target is a block-bodied construct and `new_code` starts with `{` and ends with `}`, the outermost braces are stripped before insertion. This prevents the `{{ ... }}` double-brace failure mode.

**Edge cases:**

| Situation                               | Behavior                                                                   |
| --------------------------------------- | -------------------------------------------------------------------------- |
| Target is a constant/variable (no body) | Returns `INVALID_TARGET: "replace_body requires a block-bodied construct"` |
| Target is an abstract method (no body)  | Returns `INVALID_TARGET`                                                   |
| Target has empty body `{ }`             | Replaces empty body with `new_code`                                        |

---

#### `replace_full`

Replace an entire declaration — signature, body, and associated elements.

| Parameter  | Type   | Description                      |
| ---------- | ------ | -------------------------------- |
| `new_code` | string | Complete replacement declaration |

**Scope definition — what counts as "full":**

| Associated Element                            | Included in Replacement | Example                                                                                                                      |
| --------------------------------------------- | ----------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| Decorators / attributes above the declaration | ✅ Yes                   | `@Injectable()`, `#[derive(Debug)]`, `//go:generate`                                                                         |
| Doc comments directly above                   | ✅ Yes                   | `/** ... */`, `/// ...`, `# ...` (Python docstring is inside body — not included in scope, agent controls it via `new_code`) |
| Export keyword                                | ✅ Yes                   | `export function`, `pub fn`                                                                                                  |
| The declaration itself                        | ✅ Yes                   | Signature + body                                                                                                             |

**What `new_code` should contain:**
- The **complete** replacement including any decorators/attributes/doc comments the agent wants to keep
- If the original had `@Injectable()` and the agent omits it from `new_code`, it is removed
- Indentation: provide at base level. Pathfinder re-indents via Tree-sitter baseline + optional LSP formatting

**Edge cases:**

| Situation                                     | Behavior                                                                                        |
| --------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| Target is a top-level constant                | Full range: from any doc comment/attribute above to the end of the declaration (`;` or newline) |
| Target is a type alias / interface            | Full range: from doc comment/attribute to closing `}` or `;`                                    |
| Blank lines between decorator and declaration | Included in the replacement range                                                               |

---

#### `insert_before`

Insert new code before the target symbol. When `semantic_path` is a bare file path (no `::`), inserts at the **beginning of the file** (BOF).

| Parameter  | Type   | Description          |
| ---------- | ------ | -------------------- |
| `new_code` | string | Code block to insert |

**Insertion point:** Immediately before the target's **first associated element** (decorator, attribute, or doc comment). If no associated elements, before the declaration itself.

**Bare file path (BOF):** When `semantic_path` has no `::` (e.g., `src/utils.ts`), code is prepended to the very top of the file. Use this to add imports, module docstrings, or file-level comments.

**Whitespace handling:**
- Pathfinder inserts exactly **one blank line** between the inserted code and the existing content
- If `new_code` doesn't end with a newline, one is appended
- Indentation: `new_code` is re-indented via Tree-sitter baseline + optional LSP formatting

**Edge cases:**

| Situation                                     | Behavior                                                               |
| --------------------------------------------- | ---------------------------------------------------------------------- |
| `semantic_path` is bare file path             | Code is prepended to line 1 (BOF)                                      |
| Target is the first item in the file          | Code is inserted at line 1 (before any file-level comments/headers)    |
| Target is the first item in a class/impl body | Code is inserted after the opening `{` of the class, before the target |
| Target is inside a namespace/module           | Indentation matches the enclosing scope                                |

---

#### `insert_after`

Insert new code after the target symbol. When `semantic_path` is a bare file path (no `::`), appends to the **end of the file** (EOF).

| Parameter  | Type   | Description          |
| ---------- | ------ | -------------------- |
| `new_code` | string | Code block to insert |

**Insertion point:** Immediately after the target's **closing boundary** (closing brace, semicolon, or last line).

**Bare file path (EOF):** When `semantic_path` has no `::` (e.g., `src/utils.ts`), code is appended to the very end of the file. Use this to add new classes, functions, or exports.

**Whitespace handling:**
- Pathfinder inserts exactly **one blank line** between the existing content and the inserted code
- Indentation: `new_code` is re-indented via Tree-sitter baseline + optional LSP formatting

**Edge cases:**

| Situation                                           | Behavior                                                       |
| --------------------------------------------------- | -------------------------------------------------------------- |
| `semantic_path` is bare file path                   | Code is appended to end of file (EOF)                          |
| Target is the last item in a file                   | Code is appended with one blank line separator                 |
| Target is the last item in a class/impl body        | Code is inserted before the closing `}` of the enclosing scope |
| Trailing comments on the same line as closing brace | Inserted after the full line (including the comment)           |

---

#### `delete_symbol`

Remove the target symbol entirely. No `new_code` parameter.

**Deletion scope:**

| Element                                                         | Deleted?                                                          |
| --------------------------------------------------------------- | ----------------------------------------------------------------- |
| Decorators / attributes directly above                          | ✅ Yes                                                             |
| Doc comments directly above                                     | ✅ Yes                                                             |
| The declaration itself (signature + body)                       | ✅ Yes                                                             |
| Blank lines directly above (between target and previous symbol) | ✅ Yes — cleaned up to leave exactly one blank line                |
| Import statements that reference the symbol                     | ❌ No — not auto-removed (LSP validation will flag unused imports) |

**Whitespace cleanup:**
- After deletion, Pathfinder collapses consecutive blank lines to a maximum of one
- If the deleted symbol was the only item in a class/impl body, the body becomes empty `{ }`

**Edge cases:**

| Situation                                              | Behavior                                                        |
| ------------------------------------------------------ | --------------------------------------------------------------- |
| Target is the only symbol in a file                    | File becomes empty (or contains only imports/module-level code) |
| Target is a class — delete the class or just its body? | Deletes the **entire class declaration** including all methods  |
| Target is a class method (e.g., `AuthService.login`)   | Deletes only the method, leaves the class intact                |

---

#### `validate_only`

Dry-run validation — check if an edit **would** succeed without writing to disk. Use this to pre-check complex or risky edits.

| Parameter       | Type    | Description                                                                             |
| --------------- | ------- | --------------------------------------------------------------------------------------- |
| `semantic_path` | string  | Full semantic path to the target                                                        |
| `edit_type`     | enum    | `"replace_body"` / `"replace_full"` / `"insert_before"` / `"insert_after"` / `"delete"` |
| `new_code`      | string? | Replacement code (required for all types except `"delete"`)                             |
| `base_version`  | string  | SHA-256 hash from previous read (OCC)                                                   |

**Execution:** Runs steps 0–9 of the edit pipeline (everything except disk write and cache update). Returns the same response schema as edit tools, but `success: true` means "edit would succeed" not "edit was applied."

> **CRITICAL:** `new_version_hash` is always `null` in the response because nothing was written to disk. The agent **must reuse its original `base_version`** when executing the real edit — not the null value from validate_only.

**Use cases:**
- Pre-validate a large refactor before committing
- Agent can try multiple approaches and choose the one with fewest warnings
- Safe to call repeatedly — no side effects

---

#### Edit Execution Flow

All edit tools follow this pipeline:

```
0. Input Normalization (runs on new_code before any processing):
   a. Markdown fence stripping → remove ```lang ... ``` wrappers
   b. Brace-leniency → strip outermost { } if target is block-bodied (replace_body only)
   c. Normalize \r\n → \n (LF). Git core.autocrlf handles Windows conversion at commit time.
1. Read file from disk → compute SHA-256
2. Compare SHA-256 with base_version
   ├─ Mismatch → return VERSION_MISMATCH (include current hash for zero-turn retry)
   └─ Match → continue
3. Resolve semantic path:
   ├─ Bare file path (no ::) → BOF/EOF mode for insert_before/insert_after
   ├─ With symbol chain → Tree-sitter: locate target AST node
   │   ├─ Not found → return SYMBOL_NOT_FOUND + did_you_mean (Levenshtein candidates)
   │   └─ Found → compute scope boundaries (body range, full range, or insertion point)
4. Apply edit operation:
   - replace_body → splice new_code into body range (preserving braces/signature)
   - replace_full → splice new_code into full declaration range (including decorators/docstrings)
   - insert_before → insert at BOF (bare path) or before target's first associated element
   - insert_after → append at EOF (bare path) or after target's closing boundary
   - delete_symbol → remove target + associated elements, collapse blank lines
5. Tree-sitter indentation pre-pass:
   a. Dedent: compute minimum leading whitespace across all non-empty lines in new_code
      → strip it (normalize AI output to column 0 baseline)
   b. Determine the starting column of the target AST node
   c. Pad every line of dedented new_code with the target's whitespace prefix
   d. This guarantees correct indentation even without LSP formatting
      (critical for whitespace-significant languages like Python)
6. Send textDocument/didChange to LSP with new content
7. LSP: textDocument/rangeFormatting on changed region (optional refinement on top of step 5)
   ├─ Supported → apply LSP formatting, set formatted = true
   └─ Not supported → skip, set formatted = false (Tree-sitter indentation from step 5 is sufficient)
8. Diagnostic Validation (WORKSPACE-WIDE via LSP 3.17 Pull Diagnostics):
   a. Send textDocument/diagnostic request → await response (blocks, no timers)
   b. Send workspace/diagnostic request → await response (catches cross-file breakage)
   c. If Pull Diagnostics not supported → set validation_skipped = true,
      reason = "pull_diagnostics_unsupported" (no Push fallback)
9. Multiset Diagnostic Diffing:
   a. Build multiset: HashMap<DiagnosticHash, count> for pre and post snapshots
      - Hash key = [severity, code, message, source_file] (EXCLUDING line/column)
      - Value = number of occurrences
   b. Compute: introduced = for each key, max(0, post_count - pre_count)
   c. Compute: resolved = for each key, max(0, pre_count - post_count)
   ├─ introduced contains any Severity 1 items AND ignore_validation_failures is false
   │   → send didChange back to original → return VALIDATION_FAILED + introduced
   ├─ introduced contains Severity 1 items AND ignore_validation_failures is true
   │   → log warning, continue to disk write (response still includes introduced_errors)
   └─ No new Severity 1 errors (or validation_skipped) → continue
10. Late-check TOCTOU guard (skip for validate_only):
    a. Re-read file from disk, compute SHA-256
    b. Compare to base_version (NOT to in-memory buffer)
    c. Mismatch → abort, return VERSION_MISMATCH (file modified during validation)
    d. Match → proceed to disk write
11. Write to disk (skip for validate_only):
    - Use in-place std::fs::write (preserves inode — keeps HMR/Vite/Docker happy)
    - NOT rename() — rename creates new inode, breaks file watchers and dev servers
12. Synchronous cache update:
    - Update AST cache from the in-memory buffer (NOT from disk re-read)
    - When delayed file watcher event arrives later: hash file on disk,
      compare to cache → match = ignore, mismatch = external change → re-parse
13. Compute new version_hash → return success + new hash
```

**Validation reliability notes:**
- **Input normalization** (step 0): Strips markdown fences, extraneous braces, and normalizes line endings. Only corrects **structural** LLM quirks — no content-altering transforms (no trailing whitespace stripping, no CRLF detection)
- **Dedent before pad** (step 5a): LLMs frequently output already-indented code. Pathfinder strips the common leading whitespace to column 0, then re-indents with the AST node's prefix. This prevents double-indentation bugs (e.g., 4 + 4 = 8 spaces in Python)
- **Pull Diagnostics** (step 8): LSP 3.17 `textDocument/diagnostic` is synchronous — send request, await response. No timers, no sliding windows, no race conditions. If Pull Diagnostics not supported → `validation_skipped: true` (no Push fallback — 100% synchronous pipeline)
- **Multiset diffing** (step 9): Diagnostics counted by occurrence. Two identical `"Missing comma"` errors are counted as 2, not 1
- **`ignore_validation_failures`** (step 9): Escape hatch for multi-file refactors. Writes to disk regardless, but response still reports `introduced_errors`
- **TOCTOU late-check** (step 10): Re-reads + re-hashes the file immediately before disk write. Closes the race window between OCC check (step 2) and write (step 11). If a human saved during LSP validation, Pathfinder aborts cleanly instead of overwriting
- **In-place write** (step 11): `std::fs::write` preserves the file's inode. HMR servers (Vite, Next.js, Webpack), Docker bind mounts, and `nodemon` track files by inode — `rename()` would break them
- **Synchronous cache update** (step 12): After disk write, Pathfinder updates its AST cache from the in-memory buffer immediately. No `expected_writes` HashMap, no race conditions with delayed file watcher events
- **Bare file paths** (step 3): `insert_before` and `insert_after` accept bare file paths (no `::`) for BOF/EOF insertion — enables adding imports and new declarations without targeting a symbol
- **validate_only** (step 10–13): Skips TOCTOU check, disk write, and returns `new_version_hash: null`. Agent must reuse its original `base_version` for the real edit
- **Line-number exclusion** (step 9a): Diagnostics hashed WITHOUT line/column because edits shift line numbers
- **Tree-sitter indentation** (step 5): Always runs before LSP formatting. Guarantees correct indentation for Python even when LSP lacks formatting
- **Severity 1 only**: Only errors block. Warnings and hints are reported but don't prevent the edit


---

### 3.5 File-Level Operations

For project scaffolding and non-AST files (configs, `.env`, `Dockerfile`, `YAML`). These tools operate on raw file content — no AST parsing or semantic paths required.

#### `create_file`

Create a new file with initial content.

| Parameter  | Type   | Description                                                   |
| ---------- | ------ | ------------------------------------------------------------- |
| `filepath` | string | Relative file path (parent directories created automatically) |
| `content`  | string | Initial file content                                          |

**Pipeline:**
1. Check sandbox rules → `ACCESS_DENIED` if blocked
2. Create parent directories if needed
3. Atomically create file using `OpenOptions::new().write(true).create_new(true)` → `FILE_ALREADY_EXISTS` if present
   - OS-level guarantee: no TOCTOU race between existence check and creation
4. Write content to the file descriptor
5. If file has a supported language: send `textDocument/didOpen` to LSP, request Pull Diagnostics (`textDocument/diagnostic`)
6. Return validation results (LSP may flag syntax errors in the new file)

**Returns:**
```json
{
  "success": true,
  "version_hash": "sha256:abc123...",
  "validation": {
    "status": "passed",
    "introduced_errors": []
  }
}
```

---

#### `delete_file`

Delete a file from the workspace.

| Parameter      | Type   | Description                           |
| -------------- | ------ | ------------------------------------- |
| `filepath`     | string | Relative file path                    |
| `base_version` | string | SHA-256 hash from previous read (OCC) |

**Pipeline:**
1. Check sandbox rules → `ACCESS_DENIED` if blocked
2. Verify file exists → `FILE_NOT_FOUND` if missing
3. Verify `base_version` matches → `VERSION_MISMATCH` if changed
4. Send `textDocument/didClose` to LSP
5. Delete file from disk
6. Synchronously evict AST cache entry for the deleted file

**Returns:** `{ "success": true }`

---

#### `read_file`

Read raw file content. Works on any file type — no AST parsing required.

| Parameter    | Type   | Default    | Description                                                              |
| ------------ | ------ | ---------- | ------------------------------------------------------------------------ |
| `filepath`   | string | *required* | Relative file path                                                       |
| `start_line` | int    | `1`        | First line to return (1-indexed). Use for pagination through large files |
| `max_lines`  | int    | `500`      | Maximum lines to return from `start_line`                                |

**Returns:**
```json
{
  "content": "...",
  "start_line": 1,
  "lines_returned": 250,
  "total_lines": 250,
  "truncated": false,
  "version_hash": "sha256:abc123...",
  "language": "yaml"
}
```

**Pagination:** For a 1500-line Kubernetes YAML, the agent calls `read_file(filepath, start_line=1)` then `read_file(filepath, start_line=501)` then `read_file(filepath, start_line=1001)`. Each call returns the same `version_hash` (file hasn't changed), which the agent can use for `write_file`.

> Use `read_file` for config files, Dockerfiles, YAML, TOML, `.env`, `requirements.txt`, and any non-source-code file. Use `read_symbol_scope` for precise AST extraction from source code.

---

#### `write_file`

Write to an existing non-AST file with OCC. Supports two modes: **full replacement** or **search-and-replace**.

| Parameter      | Type    | Description                                                          |
| -------------- | ------- | -------------------------------------------------------------------- |
| `filepath`     | string  | Relative file path                                                   |
| `base_version` | string  | SHA-256 hash from previous read (OCC)                                |
| `content`      | string? | Full replacement content. **Mutually exclusive with `replacements`** |
| `replacements` | array?  | Search-and-replace operations. **Mutually exclusive with `content`** |

> **Exactly one** of `content` or `replacements` must be provided. If both or neither → error.

**Replacement object (when using `replacements` mode):**

| Field      | Type   | Description                                              |
| ---------- | ------ | -------------------------------------------------------- |
| `old_text` | string | Exact text to find in the file (verbatim match)          |
| `new_text` | string | Replacement text. Empty string = delete the matched text |

**Pipeline (full replacement mode — `content`):**
1. Check sandbox rules → `ACCESS_DENIED` if blocked
2. Verify file exists → `FILE_NOT_FOUND` if missing
3. Verify `base_version` matches → `VERSION_MISMATCH` if changed
4. Late-check: re-read + re-hash file from disk right before write → `VERSION_MISMATCH` if changed during processing
5. Write content to disk (in-place `std::fs::write`)
6. Synchronous cache update

**Pipeline (search-and-replace mode — `replacements`):**
1. Check sandbox rules → `ACCESS_DENIED` if blocked
2. Verify file exists → `FILE_NOT_FOUND` if missing
3. Verify `base_version` matches → `VERSION_MISMATCH` if changed
4. For each replacement in order:
   a. Count occurrences of `old_text` in file content
   b. `count == 0` → return `MATCH_NOT_FOUND` (prevents hallucinated edits)
   c. `count > 1` → return `AMBIGUOUS_MATCH` with `details.occurrences` count ("Provide more context lines")
   d. `count == 1` → apply `file_content.replace(old_text, new_text)`
5. Late-check: re-read + re-hash file from disk right before write → `VERSION_MISMATCH` if changed during processing
6. Write result to disk (in-place `std::fs::write`)
7. Synchronous cache update

**Returns:** `{ "success": true, "new_version_hash": "sha256:def456..." }`

**Example — updating an image tag in docker-compose.yml:**
```json
{
  "filepath": "docker-compose.yml",
  "base_version": "sha256:abc...",
  "replacements": [
    { "old_text": "image: postgres:15", "new_text": "image: postgres:16-alpine" }
  ]
}
```

**Why search-and-replace instead of line numbers:** LLMs have near-zero spatial awareness of line numbers due to BPE tokenization. If an agent says "patch line 42," it will frequently miscount and corrupt the wrong line. Search-and-replace uses the LLM's **strongest** trait (verbatim text recall) and acts as an implicit content-level OCC — if the text doesn't exist, the edit can't proceed.

> **Agent guidance:** Use `write_file` only for configuration and non-source-code files. For source code, always prefer the AST-aware edit tools. When changing specific text in large config files, use `replacements` mode to avoid reproducing the entire file content.

---

### 3.6 Tool Schema Descriptions

AI agents **do not read PRDs** — they only read the `description` string in the MCP JSON schema. These descriptions are the agent's **system prompt** for each tool. The engineering team MUST use these exact strings:

#### Discovery Tools

**`search_codebase`:** *"Search the codebase for a text pattern. Returns matching lines with surrounding context. Each match includes an 'enclosing_semantic_path' (the AST symbol containing the match) and 'version_hash' (for immediate editing without a separate read). Use path_glob to narrow the search scope."*

**`get_repo_map`:** *"Returns the structural skeleton of the project as an indented tree of classes, functions, and type signatures. IMPORTANT: Each symbol has its full semantic path in a trailing comment (e.g., '// src/auth.ts::AuthService.login'). You MUST copy-paste these EXACT paths into read/edit tools. Also returns version_hashes per file for immediate editing. Check coverage_percent — it shows what percentage of project files were included. If low, increase max_tokens or narrow the path."*

#### Read Tools

**`read_symbol_scope`:** *"Extract the exact source code of a single symbol (function, class, method) by its semantic path. Returns the code, line range, and version_hash for OCC."*

**`read_with_deep_context`:** *"Extract a symbol's source code PLUS the signatures of all functions it calls. Use this when you need to understand a function's dependencies before editing it."*

#### Navigation Tools

**`get_definition`:** *"Jump to where a symbol is defined. Provide a semantic path to a reference and get back the definition's file, line, and a code preview."*

**`analyze_impact`:** *"Find all callers of a symbol (incoming) and all symbols it calls (outgoing). Use this BEFORE refactoring to understand the blast radius of a change. Returns version_hashes for all referenced files."*

#### Edit Tools

**`replace_body`:** *"Replace the internal logic of a block-scoped construct (function, method, class body, impl block), keeping the signature intact. Provide ONLY the body content — DO NOT include the outer braces or function signature. DO NOT wrap your code in markdown code blocks. Pathfinder handles indentation automatically. For multi-file refactors where validation would fail due to downstream callers, set ignore_validation_failures to true."*

**`replace_full`:** *"Replace an entire declaration including its signature, body, decorators, and doc comments. Provide the COMPLETE replacement — anything you omit (decorators, doc comments) will be removed. DO NOT wrap your code in markdown code blocks. Pathfinder handles indentation automatically."*

**`insert_before`:** *"Insert new code BEFORE a target symbol. To insert at the TOP of a file (e.g., adding imports), use a bare file path without '::' (e.g., 'src/utils.ts'). Pathfinder automatically adds one blank line between your code and the target, and handles indentation. DO NOT wrap your code in markdown code blocks."*

**`insert_after`:** *"Insert new code AFTER a target symbol. To append to the BOTTOM of a file (e.g., adding new classes), use a bare file path without '::' (e.g., 'src/utils.ts'). Pathfinder automatically adds one blank line between the target and your code, and handles indentation. DO NOT wrap your code in markdown code blocks."*

**`delete_symbol`:** *"Delete a symbol and all its associated decorators, attributes, and doc comments. If the target is a class, the ENTIRE class is deleted. If the target is a method (e.g., 'AuthService.login'), only that method is deleted — the class remains."*

**`validate_only`:** *"Dry-run an edit WITHOUT writing to disk. Use this to pre-check risky changes. Returns the same validation results as a real edit. IMPORTANT: new_version_hash will be null because nothing was written. Reuse your original base_version for the real edit. Safe to call repeatedly."*

#### File Tools

**`create_file`:** *"Create a new file with initial content. Parent directories are created automatically. Returns a version_hash for subsequent edits."*

**`delete_file`:** *"Delete a file. Requires base_version (OCC) to prevent deleting a file that was modified after you last read it."*

**`read_file`:** *"Read raw file content. Use ONLY for configuration files (.env, Dockerfile, YAML, TOML, package.json). For source code, use read_symbol_scope instead. Supports pagination via start_line for large files."*

**`write_file`:** *"WARNING: This bypasses AST validation and formatting. DO NOT use for source code (TypeScript, Python, Go, Rust). ONLY use for configuration files (.env, .gitignore, Dockerfile, YAML). For source code, use replace_body or replace_full instead. Provide EITHER 'content' for full replacement OR 'replacements' for surgical search-and-replace edits (e.g., {old_text: 'postgres:15', new_text: 'postgres:16'}). Use replacements when changing specific text in large files. Requires base_version (OCC)."*

---

## 4. Non-Functional Requirements

### 4.1 Performance Targets

| Operation                                  | Target    | Notes                          |
| ------------------------------------------ | --------- | ------------------------------ |
| Ripgrep search                             | `< 50ms`  | Realistic for typical repos    |
| Tree-sitter parse (cached/incremental)     | `< 50ms`  | Native Rust, not WASM          |
| Tree-sitter parse (cold start, large file) | `< 200ms` | First parse of >5000-line file |
| LSP semantic query (warm)                  | `< 500ms` | Depends on LSP implementation  |
| Full edit tool cycle                       | `< 3s`    | Includes LSP diagnostic wait   |

### 4.2 Concurrency Control (OCC)

- All read and discovery operations return a `version_hash` (SHA-256 of file content at read time)
- Edit tools require `base_version`. If mismatch → `VERSION_MISMATCH` error with `current_version_hash` for zero-turn retry
- No persistent VFS. Concurrency is file-level via hash comparison.

### 4.3 Sandboxing — Three-Tier Model

| Tier               | Files                                                                                       | Behavior                                     | Configurable?            |
| ------------------ | ------------------------------------------------------------------------------------------- | -------------------------------------------- | ------------------------ |
| **HARDCODED DENY** | `.git/objects/`, `.git/HEAD`, `*.pem`, `*.key`, `*.pfx`                                     | Always excluded. Returns `ACCESS_DENIED`     | No                       |
| **DEFAULT DENY**   | `.env`, `.env.*`, `secrets/`, `node_modules/`, `vendor/`, `__pycache__/`, `dist/`, `build/` | Excluded by default. Returns `ACCESS_DENIED` | Yes — override in config |
| **USER-DEFINED**   | `.pathfinderignore` patterns                                                                | `.gitignore` syntax                          | Yes                      |

> **Allowed from `.git/`:** `.gitignore`, `.github/workflows/`, `.github/actions/` — useful for AI to understand project structure.

### 4.4 File Watcher & Synchronous Cache Eviction

Pathfinder uses **synchronous cache eviction** — no shared mutable state, no race conditions.

**On Pathfinder-initiated writes (edit tools, write_file):**
1. Write content to disk via in-place `std::fs::write`
2. Synchronously update AST cache from the in-memory buffer (not disk re-read)
3. When delayed file watcher event arrives: hash file on disk, compare to cached hash
   - Match → ignore (our write)
   - Mismatch → external change occurred after our write → re-parse from disk

**On external file changes (human editor, git operations):**
1. File watcher fires
2. Hash file on disk, compare to cached hash
   - Match → ignore (no change)
   - Mismatch → invalidate AST cache, re-parse on next access

No `expected_writes` HashMap. No Mutex. No cleanup logic. The cache is always consistent because it's updated synchronously before the tool call returns.

---

## 5. Error Taxonomy

Every tool returns errors in a standardized format:

```json
{ "error": "ERROR_CODE", "message": "Human-readable explanation", "details": {} }
```

| Code                    | Meaning                                                                                                                          |
| ----------------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| `FILE_NOT_FOUND`        | File path doesn't exist                                                                                                          |
| `FILE_ALREADY_EXISTS`   | File already exists (for `create_file`)                                                                                          |
| `SYMBOL_NOT_FOUND`      | Semantic path doesn't resolve. `details.did_you_mean` lists Levenshtein-closest symbols                                          |
| `AMBIGUOUS_SYMBOL`      | Multiple matches. `details.matches` lists all                                                                                    |
| `VERSION_MISMATCH`      | File changed since last read. `details.current_version_hash` provided for zero-turn retry                                        |
| `VALIDATION_FAILED`     | Edit introduced new errors. `details.introduced_errors` lists them                                                               |
| `NO_LSP_AVAILABLE`      | No language server for this file type                                                                                            |
| `LSP_ERROR`             | Language server crashed or returned error                                                                                        |
| `LSP_TIMEOUT`           | LSP didn't respond within timeout (including 30s init timeout)                                                                   |
| `ACCESS_DENIED`         | File is in sandbox deny-list                                                                                                     |
| `PARSE_ERROR`           | Tree-sitter couldn't parse the file                                                                                              |
| `INVALID_TARGET`        | Target symbol exists but is incompatible with edit type (e.g., `replace_body` on a constant)                                     |
| `TOKEN_BUDGET_EXCEEDED` | Response would exceed `max_tokens`                                                                                               |
| `MATCH_NOT_FOUND`       | `write_file` replacements: `old_text` not found in file content (prevents hallucinated edits)                                    |
| `AMBIGUOUS_MATCH`       | `write_file` replacements: `old_text` found multiple times. `details.occurrences` = count. Agent must provide more context lines |

---

## 6. LSP Lifecycle Management

### 6.1 Blocking Initialization

LSPs start **only** when the first semantic tool is called for a given language. The tool call **blocks** until the LSP is ready — the agent never sees initialization state.

**Startup sequence:**

1. Spawn LSP process (`typescript-language-server --stdio`, etc.)
2. Send `initialize` with `rootUri` = workspace root
3. **Block** (Tokio `.await` on LSP ready channel) until `initialized` response
4. Send `textDocument/didOpen` for the requested file
5. Tool call proceeds

**Hard timeout:** 30s. If the LSP doesn't initialize within 30s → return `LSP_TIMEOUT` (genuine failure, not a retry hint).

**Why block instead of `LSP_INITIALIZING` error:** AI orchestrators (LangChain, Claude Desktop, Cursor) are bad at state machines. If told "wait 3s and retry," agents typically panic, hallucinate, or switch tools. Letting the Rust Tokio runtime handle the wait produces a slightly slower first call (~2-5s) but 100% reliable behavior. The agent simply sees a tool call that took longer than usual.

**Telemetry:** When the LSP was cold-started during a tool call, the response includes `"lsp_init_wait_ms": N` for observability.

### 6.2 Auto-Idle Termination

| LSP Category | Default Timeout | Examples                    |
| ------------ | --------------- | --------------------------- |
| Heavy        | 30 minutes      | `tsserver`, `rust-analyzer` |
| Standard     | 15 minutes      | `pyright`, `gopls`          |
| Lightweight  | 5 minutes       | JSON, YAML, TOML            |

Configurable per-language in `pathfinder.config.json`.

### 6.3 Crash Recovery

1. Detect LSP process exit (non-zero exit code or broken pipe)
2. Log the crash with LSP stderr output
3. Retry: restart LSP with exponential backoff (1s, 2s, 4s)
4. Max 3 retries within 5 minutes
5. After 3 failures → mark language as `LSP_UNAVAILABLE`, tools gracefully degrade

### 6.4 Capability Detection

On `initialize`, parse the LSP's `ServerCapabilities`:

| Capability                        | Used By                                    | Fallback If Missing                                                  |
| --------------------------------- | ------------------------------------------ | -------------------------------------------------------------------- |
| `definitionProvider`              | `get_definition`, `read_with_deep_context` | Tree-sitter heuristic search                                         |
| `callHierarchyProvider`           | `analyze_impact`                           | Tree-sitter `call_expression` scan (outgoing only)                   |
| `documentFormattingProvider`      | Edit tools formatting                      | Skip LSP formatting (Tree-sitter indentation baseline is sufficient) |
| `diagnosticProvider` (Pull, 3.17) | Edit tools validation                      | `validation_skipped: true`, `reason: "pull_diagnostics_unsupported"` |

### 6.5 Zero-Config Workspace Detection

| Marker File                                      | Language      | LSP                          | Auto-Config                             |
| ------------------------------------------------ | ------------- | ---------------------------- | --------------------------------------- |
| `tsconfig.json` or `package.json`                | TypeScript/JS | `typescript-language-server` | `rootUri` = workspace root              |
| `pyproject.toml`, `setup.py`, `requirements.txt` | Python        | `pyright`                    | Auto-detect venv from `.venv/`, `venv/` |
| `go.mod`                                         | Go            | `gopls`                      | `rootUri` = dir containing `go.mod`     |
| `Cargo.toml`                                     | Rust          | `rust-analyzer`              | `rootUri` = dir containing `Cargo.toml` |

---

## 7. Language Support Matrix

| Tier            | Languages                         | Tree-sitter    | `.scm` Queries                 | LSP         | Shipped       |
| --------------- | --------------------------------- | -------------- | ------------------------------ | ----------- | ------------- |
| **Tier 1**      | Go, TypeScript/JavaScript, Python | ✅ Native crate | ✅ Bundled                      | ✅ Required  | v1.0          |
| **Tier 2**      | Rust, Java, C/C++                 | ✅ Native crate | ✅ Bundled from nvim-treesitter | ✅ Available | v1.1          |
| **Tier 3**      | Ruby, PHP, Kotlin, Swift          | ✅ Available    | 🟡 Community                    | ✅ Available | v1.2+         |
| **Unsupported** | Others                            | 🟡 May exist    | 🔴 None                         | 🟡 May exist | Fallback mode |

### Fallback Mode (Unsupported Languages)

1. Ripgrep: text search works (language-agnostic)
2. Tree-sitter: parse if grammar exists, but no `.scm` queries → no semantic path resolution
3. Regex heuristic: detect functions via `^(def|func|function|fn)\s+(\w+)` patterns
4. All responses include `"degraded": true`
5. LSP tools return `NO_LSP_AVAILABLE`

---

## 8. Graceful Degradation

When a higher-tier engine is unavailable, Pathfinder degrades to the next tier:

| Tool                     | Full Mode (all engines)                               | Degraded: No LSP                                           | Degraded: No Tree-sitter                           |
| ------------------------ | ----------------------------------------------------- | ---------------------------------------------------------- | -------------------------------------------------- |
| `search_codebase`        | Ripgrep + Tree-sitter filtering + semantic paths      | Ripgrep + Tree-sitter filtering + semantic paths           | Ripgrep raw results (no `enclosing_semantic_path`) |
| `get_repo_map`           | Tree-sitter AST skeleton with semantic paths          | Tree-sitter AST skeleton with semantic paths               | Directory listing only                             |
| `read_symbol_scope`      | Tree-sitter extraction                                | Tree-sitter extraction                                     | `NOT_SUPPORTED`                                    |
| `read_with_deep_context` | Tree-sitter + LSP definitions                         | Tree-sitter scope only (no appendix)                       | `NOT_SUPPORTED`                                    |
| `get_definition`         | Tree-sitter + LSP                                     | `NOT_SUPPORTED`                                            | `NOT_SUPPORTED`                                    |
| `analyze_impact`         | LSP + Tree-sitter                                     | Tree-sitter outgoing only                                  | `NOT_SUPPORTED`                                    |
| Edit tools (all 5)       | Full pipeline (indentation + formatting + validation) | Tree-sitter indentation + edit, `validation_skipped: true` | `NOT_SUPPORTED`                                    |
| `validate_only`          | Full validation (dry-run)                             | Tree-sitter validation, no diagnostic check                | `NOT_SUPPORTED`                                    |
| `create_file`            | Write + LSP validation                                | Write only (no validation)                                 | Write only                                         |
| `delete_file`            | LSP cleanup + delete                                  | Delete only                                                | Delete only                                        |
| `read_file`              | Raw read                                              | Raw read                                                   | Raw read                                           |
| `write_file`             | Raw write (full or search-replace)                    | Raw write (full or search-replace)                         | Raw write (full or search-replace)                 |

All degraded responses include `"degraded": true` and `"degraded_reason": "..."`.

---

## 9. Observability

### 9.1 Structured Logging

Every tool invocation logs a structured JSON entry:

```json
{
  "timestamp": "2026-02-23T07:00:00Z",
  "tool": "replace_body",
  "duration_ms": 2450,
  "engines_used": ["tree-sitter", "lsp"],
  "degraded": false,
  "validation_method": "pull_diagnostics",
  "diagnostics_scope": "workspace_wide",
  "input_normalization": { "markdown_stripped": true, "braces_stripped": false, "crlf_normalized": false },
  "lsp_init_wait_ms": 0,
  "error": null
}
```

### 9.2 Performance Telemetry

Per-engine timing within each tool call:

```json
{
  "ripgrep_ms": 8,
  "tree_sitter_parse_ms": 12,
  "tree_sitter_query_ms": 5,
  "tree_sitter_indent_ms": 2,
  "lsp_definition_ms": 280,
  "lsp_diagnostic_wait_ms": 1800,
  "total_ms": 2110
}
```

### 9.3 LSP Tracing

Optional `--lsp-trace` flag dumps all JSON-RPC messages to/from LSP processes for debugging.

---

## 10. Configuration

### 10.1 Zero-Config Default

Pathfinder works out of the box with no config file. Auto-detection (Section 6.5) handles setup.

### 10.2 Optional `pathfinder.config.json`

For non-standard setups, place in workspace root:

```json
{
  "lsp": {
    "typescript": {
      "command": "typescript-language-server",
      "args": ["--stdio"],
      "idle_timeout_minutes": 30
    },
    "python": {
      "command": "pyright-langserver",
      "args": ["--stdio"],
      "settings": { "python.pythonPath": ".venv/bin/python" }
    }
  },
  "sandbox": {
    "additional_deny": ["*.generated.ts", "terraform.tfstate"],
    "allow_override": [".env.example"]
  },
  "search": {
    "max_results": 100,
    "default_filter_mode": "code_only"
  },
  "repo_map": {
    "max_tokens": 8192,
    "token_method": "char_div_4"
  },
  "validation": {
    "scope": "workspace_wide"
  },
  "log_level": "info"
}
```

### 10.3 MCP Client Configuration

```json
{
  "mcpServers": {
    "pathfinder": {
      "command": "pathfinder",
      "args": ["/path/to/project"]
    }
  }
}
```

One Pathfinder process per project. Multiple projects = multiple processes (naturally isolated).

---

## 11. Implementation Epics

### Epic 1: Core Foundation

| Story | Description                               | AC                                                               |
| ----- | ----------------------------------------- | ---------------------------------------------------------------- |
| 1.1   | MCP stdio transport                       | Pathfinder starts via stdio, registers all 16 tools with MCP SDK |
| 1.2   | Workspace auto-detection                  | Scan for project markers, populate language registry             |
| 1.3   | Configuration loading                     | Load `pathfinder.config.json` if present, merge with defaults    |
| 1.4   | File watcher + synchronous cache eviction | Watch workspace, hash-compare on events, no shared state         |
| 1.5   | Sandbox enforcement                       | Three-tier deny model, `.pathfinderignore` support               |

### Epic 2: The Scout (Ripgrep)

| Story | Description                                                          |
| ----- | -------------------------------------------------------------------- |
| 2.1   | `search_codebase` with all parameters                                |
| 2.2   | Integration with Tree-sitter for `filter_mode`                       |
| 2.3   | `enclosing_semantic_path` resolution per match (Tree-sitter walk-up) |
| 2.4   | `version_hash` inclusion per referenced file                         |

### Epic 3: The Surgeon (Tree-sitter)

| Story | Description                                                              |
| ----- | ------------------------------------------------------------------------ |
| 3.1   | Language grammar loading (native crates for Tier 1)                      |
| 3.2   | `.scm` query file loading and compilation                                |
| 3.3   | Semantic path resolution (used by all symbol-level tools)                |
| 3.4   | `get_repo_map` with pruning algorithm + explicit semantic path rendering |
| 3.5   | `read_symbol_scope`                                                      |
| 3.6   | AST cache with incremental re-parse on file change                       |
| 3.7   | Indentation pre-pass — compute target column + pad `new_code` lines      |

### Epic 4: The Lawyer (LSP)

| Story | Description                                                          |
| ----- | -------------------------------------------------------------------- |
| 4.1   | LSP process spawning + `initialize` handshake                        |
| 4.2   | Capability detection + graceful degradation registry                 |
| 4.3   | Idle timeout + auto-termination                                      |
| 4.4   | Crash recovery with exponential backoff                              |
| 4.5   | `get_definition` (semantic path → LSP definition)                    |
| 4.6   | `read_with_deep_context` (context appendix)                          |
| 4.7   | `analyze_impact` (call hierarchy + cycle detection + `version_hash`) |

### Epic 5: The Shadow Editor

| Story | Description                                                                                  |
| ----- | -------------------------------------------------------------------------------------------- |
| 5.1   | OCC: version hash computation and comparison                                                 |
| 5.2   | Shared edit pipeline: scope boundary detection + AST-anchored splicing                       |
| 5.3   | `replace_body` — body scope detection + brace-leniency auto-stripping                        |
| 5.4   | `replace_full` — full scope detection (decorators, doc comments, export keywords)            |
| 5.5   | `insert_before` / `insert_after` — insertion point detection + whitespace handling           |
| 5.6   | `delete_symbol` — deletion scope + whitespace cleanup                                        |
| 5.7   | `validate_only` — dry-run mode (shared pipeline, skip disk write)                            |
| 5.8   | Tree-sitter indentation pre-pass (baseline indent before LSP formatting)                     |
| 5.9   | Pull Diagnostics only (`textDocument/diagnostic` + `workspace/diagnostic`). No Push fallback |
| 5.10  | Multiset diagnostic diffing (HashMap<Hash, count>, exclude line/column)                      |
| 5.11  | Range formatting via LSP (optional refinement on top of Tree-sitter indent)                  |
| 5.12  | In-place disk write (`std::fs::write`) + synchronous AST cache update                        |
| 5.13  | Bare file path support in insert_before/insert_after (BOF/EOF mode)                          |
| 5.14  | TOCTOU late-check: re-hash file from disk immediately before write (closes race window)      |


### Epic 6: File-Level Operations

| Story | Description                                                                         |
| ----- | ----------------------------------------------------------------------------------- |
| 6.1   | `create_file` — atomic `OpenOptions::create_new(true)` + optional LSP validation    |
| 6.2   | `delete_file` — LSP cleanup + deletion                                              |
| 6.3   | `read_file` — raw file read with pagination (`start_line` + `max_lines`)            |
| 6.4   | `write_file` — raw file write with OCC (full replacement + search-and-replace mode) |

### Epic 7: Observability & Polish

| Story | Description                                 |
| ----- | ------------------------------------------- |
| 7.1   | Structured JSON logging per tool invocation |
| 7.2   | Per-engine performance telemetry            |
| 7.3   | Optional LSP RPC tracing                    |
| 7.4   | Error taxonomy enforcement across all tools |

---

## 12. Deployment Topology

```
┌──────────────────────────────────┐
│  AI Agent (Claude, Cursor, etc.) │
│  ┌──────────────────────────┐    │
│  │  MCP Client              │    │
│  └──────────┬───────────────┘    │
└─────────────┼────────────────────┘
              │ stdio (JSON-RPC)
              ▼
┌──────────────────────────────────┐
│  Pathfinder (1 process)          │
│  ┌────────┐ ┌────────┐ ┌──────┐  │     ┌──────────────┐
│  │Ripgrep │ │Tree-   │ │ LSP  │──┼────▶│ tsserver     │
│  │        │ │sitter  │ │Mgr   │──┼────▶│ pyright      │
│  │        │ │(native)│ │      │──┼────▶│ gopls        │
│  └────────┘ └────────┘ └──────┘  │     └──────────────┘
│  ┌─────────────────────────────┐ │
│  │ File Watcher + AST Cache    │ │
│  └─────────────────────────────┘ │
└──────────────────────────────────┘
              │
              ▼
       📁 Project Files (disk)
```

**1 Pathfinder process = 1 workspace root.** Multiple projects = multiple processes.

### 12.1 Resource Considerations

Each Pathfinder process spawns **independent** LSP server instances as child processes. LSP servers are not shared across Pathfinder processes because they maintain workspace-specific state (project configuration, dependency graphs, diagnostics).

**Expected memory footprint per Pathfinder process:**

| Component                    | Memory (typical) | Notes                                      |
| ---------------------------- | ---------------- | ------------------------------------------ |
| Pathfinder core              | 30–80 MB         | AST cache grows with file count            |
| `typescript-language-server` | 300–600 MB       | Depends on project size and `node_modules` |
| `rust-analyzer`              | 500 MB–1.5 GB    | Depends on dependency tree depth           |
| `pyright`                    | 200–400 MB       | Depends on venv size                       |
| `gopls`                      | 150–300 MB       | Depends on module graph                    |

**Multi-project example (3 concurrent projects):**

```
Project A (TS+Python) ≈ 530–1080 MB (Pathfinder + tsserver + pyright)
Project B (Rust)      ≈ 530–1580 MB (Pathfinder + rust-analyzer)
Project C (Go+TS)     ≈ 480–980 MB  (Pathfinder + gopls + tsserver)
Total                 ≈ 1.5–3.6 GB
```

**Mitigation:** The idle timeout system (Section 6.2) automatically terminates inactive LSP processes, reclaiming memory for projects not actively being worked on. In practice, only the actively-edited project's LSPs remain resident.

---