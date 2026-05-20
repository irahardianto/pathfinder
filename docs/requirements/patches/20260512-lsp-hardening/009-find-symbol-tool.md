# 009: `find_symbol` Tool

**Epic**: 4 — New Tools
**Status**: ☐ Pending
**Severity**: High (agent ergonomics)
**Risk**: Higher — new MCP tool, schema addition, prompt inventory update
**Depends on**: 007 (language-aware definition patterns)

---

## Problem

All Pathfinder navigation tools require the caller to provide a `file::symbol` semantic path (e.g., `src/auth.ts::AuthService.login`). But agents often start with only a bare symbol name (e.g., `AuthService`) and don't know which file it's defined in.

### Current Ceremony

```
Step 1: get_repo_map(path=".") → scan skeleton for symbol name (often truncated)
Step 2: search_codebase(query="AuthService", filter_mode="code_only") → find file
Step 3: Manually construct semantic path from search results
Step 4: get_definition("src/auth.ts::AuthService") → actual navigation
```

This 3–4 call ceremony wastes agent time and context window on boilerplate path discovery.

### Desired Flow

```
Step 1: find_symbol(name="AuthService") → returns semantic paths with file, line, kind
Step 2: get_definition("src/auth.ts::AuthService") → actual navigation
```

---

## Proposed Solution

New MCP tool that resolves a bare symbol name to its semantic path(s):

### Input Schema

```json
{
  "name": "AuthService",
  "kind": "class",           // Optional: filter by symbol kind
  "path_glob": "src/**/*",   // Optional: limit search scope
  "max_results": 10           // Optional: default 10
}
```

### Output Schema

```json
{
  "symbols": [
    {
      "semantic_path": "src/auth.ts::AuthService",
      "kind": "class",
      "file": "src/auth.ts",
      "line": 15,
      "preview": "export class AuthService {"
    },
    {
      "semantic_path": "src/auth.ts::AuthService.login",
      "kind": "method",
      "file": "src/auth.ts",
      "line": 42,
      "preview": "async login(credentials: LoginDTO): Promise<Token> {"
    }
  ],
  "total_found": 2,
  "search_strategy": "treesitter"
}
```

### Implementation Strategy

1. Run `search_codebase` with language-aware definition patterns (from spec 007)
2. Enrich with tree-sitter to get `enclosing_semantic_path` and `kind`
3. Deduplicate by semantic path
4. Sort by relevance: exact name match > prefix match > contains match
5. Return top N results

### Scope Constraint

Workspace-only search (consistent with `project_only=true` default). Does not search vendored/node_modules/target directories.

### Files to Create/Modify

| File | Change |
|------|--------|
| `crates/pathfinder/src/server/tools/find_symbol.rs` | **[NEW]** Tool implementation |
| `crates/pathfinder/src/server/tools/mod.rs` | Register new module |
| `crates/pathfinder/src/server/types.rs` | Add `FindSymbolParams`, `FindSymbolResponse`, `FoundSymbol` types |
| `crates/pathfinder/src/server.rs` | Register tool in MCP schema |

---

## Acceptance Criteria

- [ ] Tool registered in MCP tool list with JSON schema
- [ ] Bare name `"LspClient"` resolves to correct `crates/pathfinder-lsp/src/client/mod.rs::LspClient`
- [ ] Optional `kind` filter limits results (e.g., `kind: "function"` excludes structs)
- [ ] Optional `path_glob` scopes search (e.g., `src/**/*.ts` for TypeScript only)
- [ ] Results sorted by relevance (exact match first)
- [ ] `search_strategy` field indicates how results were found (`treesitter`, `grep`, `lsp`)
- [ ] Sandbox check applied to all returned paths
- [ ] Max 10 results by default
- [ ] Handles no-results gracefully (empty `symbols` array, no error)

---

## Test Plan

| Test | Description |
|------|-------------|
| `test_find_symbol_exact_match` | Search for `PathfinderServer` in own codebase → exact match |
| `test_find_symbol_with_kind_filter` | Search for `LspClient` with `kind: "struct"` → excludes method matches |
| `test_find_symbol_with_path_glob` | Search scoped to `crates/pathfinder-lsp/**` → only LSP crate results |
| `test_find_symbol_no_results` | Search for `NonExistentSymbol12345` → empty array |
| `test_find_symbol_sandbox_enforced` | Search returning `.git/` results → filtered out |
| `test_find_symbol_deduplication` | Same symbol matched by multiple patterns → single result |

---

## Verification

```bash
cargo test -p pathfinder-mcp -- find_symbol
cargo clippy -p pathfinder-mcp -- -D warnings
```

---

## Agent Prompt Update

After implementation, update the MCP tool description in the pathfinder server's tool schema:

```
find_symbol — Resolve a bare symbol name to its file::symbol semantic path(s).
Use when you know a symbol's name but not its file. Returns matching definitions
with file, line, kind, and a code preview. Faster than get_repo_map + search_codebase.
```
