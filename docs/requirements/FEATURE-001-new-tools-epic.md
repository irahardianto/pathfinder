# Feature Requests: New MCP Tools Epic

**Document ID:** FEATURE-001  
**Status:** Proposed — Not Scheduled  
**Source:** April 2026 Audit Report, Findings F5.4a–F5.4e  
**Related patches:** `docs/requirements/patches/20260429/DEFERRED-001-not-remediated.md` (D-11, D-12)

---

## Overview

This document specifies five new MCP tool proposals that would meaningfully reduce the number of round-trips required for common agentic workflows. Each specification includes:

- **Current workflow** (how agents do it today, with tool call count)
- **Proposed tool** (interface + behavior)
- **Implementation path** (what exists in the codebase to build on)
- **Complexity estimate** and **open questions**

These are **feature requests, not bug fixes.** They are greenfield additions that should be designed, reviewed, and implemented in a dedicated development sprint rather than as patches to the existing codebase.

---

## Codebase Context (Read Before Implementing)

### Existing LSP `Lawyer` trait capabilities

The `Lawyer` trait (`crates/pathfinder-lsp/src/lawyer.rs`) currently exposes:

| Method | LSP Request | Status |
|--------|-------------|--------|
| `goto_definition` | `textDocument/definition` | ✅ Implemented |
| `call_hierarchy_prepare` | `callHierarchy/prepare` | ✅ Implemented |
| `call_hierarchy_incoming` | `callHierarchy/incomingCalls` | ✅ Implemented |
| `call_hierarchy_outgoing` | `callHierarchy/outgoingCalls` | ✅ Implemented |
| `pull_diagnostics` | `textDocument/diagnostic` | ✅ Implemented |
| `pull_workspace_diagnostics` | `workspace/diagnostic` | ✅ Implemented |
| `range_formatting` | `textDocument/rangeFormatting` | ✅ Implemented |
| `did_open/did_change/did_close` | Document sync | ✅ Implemented |
| `did_change_watched_files` | File watcher sync | ✅ Implemented |
| `textDocument/rename` | Rename symbol | ❌ **Not implemented** |
| `textDocument/references` | Find references | ❌ **Not implemented** |
| `textDocument/formatting` | Format entire file | ❌ **Not implemented** |

### Existing tree-sitter `SupportedLanguage` enum

`crates/pathfinder-treesitter/src/language.rs`:

```
Go, TypeScript, Tsx, JavaScript, Python, Rust, Vue
```

### LSP language detection

`crates/pathfinder-lsp/src/client/detect.rs::detect_languages` auto-detects running LSP servers. `LanguageLsp` struct contains `language_id`, `command`, `args`, `root`, and `init_timeout_secs`.

---

## F5.4a: `rename_symbol` Tool

### Problem

Renaming a symbol requires 5–10 tool calls today:

1. `analyze_impact` — find all callers (requires LSP)
2. `read_symbol_scope` × N — read each caller's context
3. `replace_full` or `replace_body` × N — update each occurrence
4. Manual verification that no caller was missed

This is error-prone: agents may miss callers in dynamically-typed code, non-indexed files, or cross-module re-exports. The LSP `textDocument/rename` request handles this atomically and correctly.

### Proposed Tool: `rename_symbol`

#### MCP Tool Definition

```
Tool name: rename_symbol
Description: Rename a symbol and all its references atomically using LSP rename.
             Returns a list of all files modified and changes applied.
```

#### Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `semantic_path` | `string` | ✅ | Symbol to rename (e.g., `src/auth.ts::AuthService.login`) |
| `new_name` | `string` | ✅ | New identifier name (identifier only, no path) |
| `base_version` | `string` | ✅ | OCC hash of the file containing the target symbol |
| `dry_run` | `bool` | ❌ | If true, return planned changes without applying them |

#### Response

```json
{
  "changes_applied": 12,
  "files_modified": ["src/auth.ts", "src/auth.test.ts", "src/api/routes.ts"],
  "dry_run": false,
  "degraded": false,
  "degraded_reason": null
}
```

#### Behavior

1. Resolve `semantic_path` → file path + line/column of the symbol declaration.
2. Call `Lawyer::rename` (new method) → `textDocument/rename` → returns `WorkspaceEdit`.
3. Apply `WorkspaceEdit` to disk (map LSP text edits to file writes).
4. Return the list of modified files and total change count.

**Degraded mode:** If no LSP available for the file type, fall back to `search_codebase`-based rename with a warning. The fallback searches for the exact identifier and replaces occurrences, but cannot handle:
- Dynamic references (string-based lookups)
- Shadowed names in nested scopes
- Cross-package renames in compiled languages

The response must set `"degraded": true` and `"degraded_reason": "no_lsp_available_using_text_search"` in fallback mode.

#### LSP Layer Changes Required

1. **Add to `Lawyer` trait** (`crates/pathfinder-lsp/src/lawyer.rs`):
   ```rust
   async fn rename(
       &self,
       workspace_root: &Path,
       file_path: &Path,
       line: u32,
       column: u32,
       new_name: &str,
   ) -> Result<WorkspaceEdit, LspError>;
   ```

2. **Add `WorkspaceEdit` type** to `crates/pathfinder-lsp/src/types.rs`:
   ```rust
   pub struct WorkspaceEdit {
       pub changes: Vec<FileEdit>,
   }
   pub struct FileEdit {
       pub file: PathBuf,
       pub edits: Vec<TextEdit>,
   }
   pub struct TextEdit {
       pub start_line: u32,
       pub start_column: u32,
       pub end_line: u32,
       pub end_column: u32,
       pub new_text: String,
   }
   ```

3. **Add `NoOpLawyer::rename`** that returns `LspError::NoLspAvailable`.

4. **Add `MockLawyer` fixture** for testing.

#### Implementation in Tool Layer

- **File:** `crates/pathfinder/src/server/tools/` → new file `rename.rs`
- **Registration:** Add to `tool_router` in `crates/pathfinder/src/server.rs`

#### Open Questions

- Should `dry_run=true` return a diff preview or just the file list?
- What happens when the LSP returns a `WorkspaceEdit` that conflicts with an OCC-guarded file that changed between the `base_version` check and the write? → Recommend: apply changes file by file, abort and rollback on first OCC conflict.
- Should the tool accept the symbol position by line/column instead of semantic path, for callers who don't know the semantic path?

#### Complexity: **MEDIUM-HIGH**
New LSP method + `WorkspaceEdit` application + fallback logic + OCC integration across multiple files.

---

## F5.4b: `find_all_references` Tool

### Problem

`analyze_impact` (call hierarchy) requires LSP and returns call *hierarchy*, which is richer than needed for simple reference listing. It also doesn't find non-call usages (field accesses, type references, imports, string constants referencing the name).

There is no lightweight alternative for finding all references when:
- LSP is unavailable
- The symbol is a type/interface/constant (not callable)
- The agent needs to count usages before deciding whether to inline a function

### Proposed Tool: `find_all_references`

#### MCP Tool Definition

```
Tool name: find_all_references
Description: Find all references to a symbol. Uses LSP references when available,
             falls back to semantic-aware text search.
```

#### Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `semantic_path` | `string` | ✅ | Symbol to find references for |
| `include_declaration` | `bool` | ❌ | Include the declaration site in results (default: false) |
| `max_results` | `u32` | ❌ | Cap results (default: 100) |

#### Response

```json
{
  "references": [
    {
      "file": "src/routes.ts",
      "line": 42,
      "column": 15,
      "context": "  const handler = new AuthService();",
      "enclosing_symbol": "src/routes.ts::setupRoutes"
    }
  ],
  "total_count": 12,
  "truncated": false,
  "source": "lsp",
  "degraded": false
}
```

`source` is `"lsp"` when using `textDocument/references`, `"search"` when using the fallback.

#### Behavior

**Primary path (LSP available):**
1. Resolve `semantic_path` → position.
2. Call `Lawyer::references` (new) → `textDocument/references`.
3. Map `Location[]` → `ReferenceResult[]` with context lines.

**Fallback path (no LSP):**
1. Extract the symbol name from the semantic path (the part after `::`, last segment).
2. Call `Scout::search` with the symbol name as query, `is_regex=false`.
3. Filter results by file type match and semantic proximity (exclude results inside comments using tree-sitter enrichment).
4. Return with `"source": "search"` and `"degraded": true`.

#### LSP Layer Changes Required

1. **Add to `Lawyer` trait**:
   ```rust
   async fn references(
       &self,
       workspace_root: &Path,
       file_path: &Path,
       line: u32,
       column: u32,
       include_declaration: bool,
   ) -> Result<Vec<ReferenceLocation>, LspError>;
   ```

2. **Add `ReferenceLocation` type** to `crates/pathfinder-lsp/src/types.rs`:
   ```rust
   pub struct ReferenceLocation {
       pub file: PathBuf,
       pub start_line: u32,
       pub start_column: u32,
       pub end_line: u32,
       pub end_column: u32,
   }
   ```

#### Open Questions

- The fallback search by name will produce false positives for common names (e.g., `new`, `get`, `id`). Should there be a minimum name length threshold for fallback?
- Should results include the enclosing symbol path (requires tree-sitter enrichment per file)?

#### Complexity: **MEDIUM**
New LSP method + fallback to existing `Scout::search` + result enrichment.

---

## F5.4c: `move_symbol` Tool

### Problem

Moving a function or type between files requires 4+ chained tool calls:

1. `read_symbol_scope` — read the source symbol
2. `delete_symbol` — remove from source file
3. `insert_after` or `insert_into` — add to target file
4. Manual import update in the target file
5. Manual import update in every file that imported from the source file

Step 5 is the hardest: finding all files that imported the moved symbol requires `analyze_impact` or `find_all_references`, then updating each import manually.

### Proposed Tool: `move_symbol`

#### MCP Tool Definition

```
Tool name: move_symbol
Description: Move a symbol from one file to another. Updates the symbol's location
             and adjusts import/export statements. Requires LSP for full accuracy.
```

#### Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `source_path` | `string` | ✅ | Semantic path of symbol to move (e.g., `src/utils.ts::formatDate`) |
| `target_file` | `string` | ✅ | Destination file (relative path, e.g., `src/helpers/date.ts`) |
| `base_version` | `string` | ✅ | OCC hash of source file |
| `target_base_version` | `string` | ❌ | OCC hash of target file (required if target file exists) |
| `update_imports` | `bool` | ❌ | Whether to update import sites (default: true, requires LSP) |
| `dry_run` | `bool` | ❌ | Preview changes without applying |

#### Response

```json
{
  "symbol_moved": true,
  "source_file": "src/utils.ts",
  "target_file": "src/helpers/date.ts",
  "import_sites_updated": 7,
  "files_modified": ["src/utils.ts", "src/helpers/date.ts", "src/routes.ts", "..."],
  "warnings": ["src/legacy.js: could not update import (unsupported language)"],
  "degraded": false
}
```

#### Behavior

1. Read source symbol content via `Surgeon::read_symbol_scope`.
2. Get all references via `Lawyer::references` (F5.4b) or `Scout::search` fallback.
3. Delete symbol from source file via `Surgeon::delete_symbol`.
4. Insert symbol into target file via `Surgeon::insert_into` or `insert_after`.
5. Update import statements in all reference files:
   - For typed languages (TS, Rust, Go): use LSP `rename`-like edit or tree-sitter to locate the import statement and rewrite the path.
   - For untyped/unsupported languages: emit a warning.

#### Implementation Note

This tool is the most complex of the five. The import update step is language-specific and cannot be fully generalized without per-language import rewriting logic. Consider a phased approach:

- **Phase 1:** Move symbol only (no import updates). `update_imports: false` by default.
- **Phase 2:** Add import update for TypeScript/JavaScript (most common use case).
- **Phase 3:** Add Rust `use` statement updates, Go `import` updates.

#### Open Questions

- If the target file doesn't exist, should the tool create it?
- How should circular imports be detected and rejected?
- Should the tool handle re-exports (e.g., if `src/utils.ts` re-exports `formatDate` from `src/utils/date.ts`)?

#### Complexity: **HIGH**
Multi-file atomic operation + language-specific import rewriting + OCC coordination across N files. Recommend splitting into Phase 1 (move only) and Phase 2 (import update).

---

## F5.4d: `format_file` Tool

### Problem

When agents use `write_file` on source files (e.g., updating a config-adjacent `.ts` file), the resulting file may not conform to the project's formatter settings. There is no standalone format tool — formatting only happens as a side effect of `replace_body`, `replace_full`, etc., via the edit validation pipeline's `range_formatting` call.

Agents that want to normalize an entire file after a batch of changes have no single-call path.

### Proposed Tool: `format_file`

#### MCP Tool Definition

```
Tool name: format_file
Description: Format an entire file using the LSP formatter. Falls back to no-op
             with a warning if no LSP is available for the file type.
```

#### Parameters

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `filepath` | `string` | ✅ | Relative path to the file to format |
| `base_version` | `string` | ✅ | OCC hash (prevents formatting a file that changed mid-operation) |

#### Response

```json
{
  "formatted": true,
  "new_version_hash": "a3f7c21",
  "lines_changed": "+0/-3",
  "degraded": false,
  "degraded_reason": null
}
```

If no LSP available:
```json
{
  "formatted": false,
  "new_version_hash": null,
  "lines_changed": null,
  "degraded": true,
  "degraded_reason": "no_lsp_available"
}
```

#### Behavior

1. Validate file path via `Sandbox::check`.
2. Read file content + compute OCC hash.
3. Check `base_version` OCC.
4. Call `Lawyer::range_formatting` with line range `1..=total_lines` (whole-file format).
   - This reuses the existing `range_formatting` method without any LSP layer changes.
5. If formatted content differs, write to disk and compute new hash.
6. Return result with `lines_changed` using `compute_lines_changed`.

#### LSP Layer Changes Required

**None.** The existing `Lawyer::range_formatting` method covers the whole-file case when called with `start_line=1, end_line=total_lines`. This is purely a new tool in the tool layer.

#### Implementation in Tool Layer

- **File:** `crates/pathfinder/src/server/tools/file_ops.rs` (add `format_file_impl`) or new file `format.rs`
- **Registration:** Add to `tool_router`

#### Open Questions

- Should `format_file` update the OCC hash atomically, or should the caller re-read after formatting?
- What if formatting produces no changes? Return `formatted: false` or `formatted: true` with `lines_changed: "+0/-0"`?
- Should `format_file` work on unsupported-language files (e.g., YAML)? Currently `range_formatting` is LSP-gated, so YAML with no LSP would get `degraded: true`.

#### Complexity: **LOW**
No LSP layer changes required. New tool handler that orchestrates existing `Lawyer::range_formatting`, OCC guard, and file write.

---

## F5.4e: `list_languages` Tool (or `get_repo_map` Extension)

### Problem

Agents have no programmatic way to discover:
1. Which languages have tree-sitter grammar support (AST tools will work)
2. Which LSP servers are currently running (LSP-dependent tools will work)
3. Which file extensions map to which language IDs

Agents must infer this from `get_repo_map` output or from tool errors (`UNSUPPORTED_LANGUAGE`). This is error-prone and costs at least one failed tool call per discovery.

### Option A: New `list_languages` Tool

#### MCP Tool Definition

```
Tool name: list_languages
Description: List all languages Pathfinder supports, with tree-sitter and LSP
             availability status for the current workspace.
```

#### Parameters

None. Zero-argument tool.

#### Response

```json
{
  "languages": [
    {
      "name": "TypeScript",
      "extensions": [".ts", ".tsx"],
      "treesitter_available": true,
      "lsp_available": true,
      "lsp_command": "typescript-language-server",
      "lsp_status": "indexed"
    },
    {
      "name": "Rust",
      "extensions": [".rs"],
      "treesitter_available": true,
      "lsp_available": true,
      "lsp_command": "rust-analyzer",
      "lsp_status": "warming_up"
    },
    {
      "name": "Go",
      "extensions": [".go"],
      "treesitter_available": true,
      "lsp_available": false,
      "lsp_command": null,
      "lsp_status": null
    },
    {
      "name": "Python",
      "extensions": [".py"],
      "treesitter_available": true,
      "lsp_available": false,
      "lsp_command": null,
      "lsp_status": null
    },
    {
      "name": "Vue",
      "extensions": [".vue"],
      "treesitter_available": true,
      "lsp_available": false,
      "lsp_command": null,
      "lsp_status": null
    }
  ]
}
```

`lsp_status` values: `"indexed"`, `"warming_up"`, `"not_running"`, `null` (no LSP).

#### Implementation

1. Enumerate `SupportedLanguage` variants (tree-sitter layer).
2. Call `Lawyer::capability_status` to get per-language LSP status.
3. Merge into `LanguageEntry` structs and serialize.

**Data sources already exist:**
- Tree-sitter languages: `SupportedLanguage::detect()` covers all supported extensions
- LSP status: `Lawyer::capability_status()` already returns `HashMap<String, LspLanguageStatus>`

Complexity: **LOW** — pure aggregation of existing data.

### Option B: Extend `get_repo_map` response

Add a `supported_languages` field to `RepoMapResult`:

```json
{
  "skeleton": "...",
  "tech_stack": ["TypeScript", "Rust"],
  "supported_languages": {
    "TypeScript": { "treesitter": true, "lsp": true },
    "Rust": { "treesitter": true, "lsp": true },
    "Go": { "treesitter": true, "lsp": false }
  }
}
```

**Recommendation:** Prefer **Option A** (standalone tool) because:
- Agents shouldn't need to call `get_repo_map` just to check language support
- `get_repo_map` is expensive (walks the filesystem); language status is cheap
- Dedicated tool is easier to mock in tests

#### Open Questions

- Should `list_languages` include languages NOT present in the current workspace (e.g., show Vue even if there are no `.vue` files)?
- Should it report the LSP `root` directory for monorepos where different subdirectories use different LSP instances?

#### Complexity: **LOW**
Pure aggregation of `SupportedLanguage` enum + `Lawyer::capability_status()`.

---

## Implementation Priority

| Tool | Value | Complexity | Priority |
|------|-------|------------|----------|
| **F5.4e** `list_languages` | High (reduces discovery errors) | Low | **1st** |
| **F5.4d** `format_file` | Medium (common need) | Low | **2nd** |
| **F5.4b** `find_all_references` | High (enables safe refactoring) | Medium | **3rd** |
| **F5.4a** `rename_symbol` | High (biggest workflow reduction) | Medium-High | **4th** |
| **F5.4c** `move_symbol` | High (but Phase 1 only) | High | **5th** |

---

## Cross-Cutting Requirements

All new tools must follow the existing Pathfinder conventions:

1. **Sandbox check:** Every file path must be validated via `Sandbox::check` before access.
2. **OCC guard:** Any tool that writes to disk must validate `base_version` via `VersionHash::matches`.
3. **Structured response:** Use `CallToolResult` with `structured_content` JSON metadata alongside text content.
4. **Tracing spans:** Wrap the impl function in `tracing::info!` at start + end, logging `duration_ms` and `engines_used`.
5. **Degraded mode:** Any tool that uses LSP must gracefully degrade with `"degraded": true` and a `"degraded_reason"` string.
6. **`NoOpLawyer` impl:** Any new `Lawyer` trait methods must have a `NoOpLawyer` implementation that returns `LspError::NoLspAvailable`.
7. **`MockLawyer` fixture:** Any new `Lawyer` trait methods must have a testable `MockLawyer` implementation.
8. **Tests:** Each tool must have at minimum: one success test, one sandbox-denial test, one LSP-unavailable degraded test.
