# Monorepo LSP Support Improvements

> **Status**: Planned  
> **Priority**: Medium  
> **Created**: 2026-06-15  

## Problem Statement

Pathfinder starts exactly **1 LSP instance per `language_id`** and sends **1 entry** in `workspaceFolders` during LSP initialization. For monorepos with multiple applications of the same language (e.g., `apps/frontend1/tsconfig.json` + `apps/frontend2/tsconfig.json`), the LSP server receives the workspace root as `rootUri` with a single `workspaceFolders` entry. This works but is suboptimal — there is no per-app intellisense isolation.

### Current Architecture

```
LspClient.processes: DashMap<String, ProcessEntry>  // keyed by language_id
```

- Key is `language_id` string (e.g., `"typescript"`) → exactly 1 entry per language
- `build_initialize_request()` sends single `rootUri` + single `workspaceFolders` entry
- When multiple marker files found (e.g., multiple `tsconfig.json`), detection falls back to workspace root
- Config escape hatch: `root_override` in `pathfinder.config.json` — but only 1 root per language

### Per-Language Monorepo Behavior (Current)

| Language | Marker Files | Multi-Project Handling | Native Workspace Support |
|----------|-------------|----------------------|-------------------------|
| **Rust** | `Cargo.toml` | Works well | `rust-analyzer` handles `[workspace]` members natively |
| **Go** | `go.mod` | Falls back to root | Works with `go.work` at root (gopls workspace mode) |
| **TypeScript** | `tsconfig.json`, `package.json` | Falls back to root | tsserver resolves per-file `tsconfig.json` by walking up |
| **Python** | `pyproject.toml`, `setup.py`, `requirements.txt` | Falls back to root | Most LSPs adequate from workspace root |
| **Java** | `pom.xml`, `build.gradle*` | Falls back to root | jdtls handles multi-module Maven/Gradle |

## Phased Improvement Plan

### Phase 1: Multiple `workspaceFolders` in Single Instance

**Impact**: HIGH | **Complexity**: MEDIUM | **Risk**: LOW

The LSP protocol (3.6+) supports `workspace/workspaceFolders` — multiple folders per single server instance. Pathfinder already declares `workspaceFolders: true` in client capabilities. Both `typescript-language-server` and `vtsls` accept multiple entries.

**Changes required:**

1. **`detect.rs`** — When `find_marker()` finds multiple matching directories, return ALL of them instead of falling back to workspace root
2. **`mod.rs` / `LanguageLsp`** — Add `workspace_folders: Vec<PathBuf>` field alongside existing `root` field
3. **`process.rs` / `build_initialize_request()`** — Accept `Vec<(Uri, String)>` for workspace folders, populate `workspaceFolders` array with all entries
4. **`lifecycle.rs`** — Forward workspace folders through `start_process()` → `spawn_and_initialize()`

**Example initialize request (after):**

```json
{
  "rootUri": "file:///workspace",
  "workspaceFolders": [
    { "uri": "file:///workspace/apps/frontend1", "name": "frontend1" },
    { "uri": "file:///workspace/apps/frontend2", "name": "frontend2" }
  ]
}
```

**Benefits all languages**: Go with multiple `go.mod` files would also benefit from explicit folder entries.

### Phase 2: `vtsls` as Preferred TS/JS LSP

**Impact**: MEDIUM | **Complexity**: LOW | **Risk**: MEDIUM

`vtsls` wraps VS Code's native TS language service (not tsserver directly). Used by Zed editor. Better native monorepo support than `typescript-language-server`.

**Changes required:**

1. **`plugin.rs`** — Add `vtsls` as LSP candidate ahead of `typescript-language-server` (like Python has 5 candidates)
2. **`detect.rs`** — Map `vtsls` init options format (different from typescript-language-server)
3. **Config** — Allow user to pick: `"lsp.typescript.command": "vtsls"` in `pathfinder.config.json`

### Phase 3: Multi-Instance Support (Last Resort)

**Impact**: HIGH | **Complexity**: HIGH | **Risk**: HIGH

Only pursue if empirical evidence shows single-instance with multi-folders is insufficient.

**Changes required:**

1. Refactor `DashMap<String, ProcessEntry>` key from `language_id` to `(language_id, instance_id)`
2. File-to-instance routing: map each file path to the correct LSP instance based on nearest marker file
3. Process lifecycle management for N instances of same language

**Trade-offs:**
- HIGH memory (N tsserver processes)
- No cross-project navigation
- Complex routing logic
- Major refactor

### Recommended User Pattern (No Code Changes)

For TS monorepos, recommend a root `tsconfig.json` with project references:

```jsonc
// Root tsconfig.json
{
  "files": [],
  "references": [
    { "path": "./apps/frontend1" },
    { "path": "./apps/frontend2" }
  ]
}
```

Each sub-project adds `"composite": true, "declaration": true`. This gives tsserver the full project graph — enables cross-package go-to-definition and find-references.

## Key Files

| File | Role |
|------|------|
| `crates/pathfinder-lsp/src/client/process.rs` | `build_initialize_request()` — LSP handshake |
| `crates/pathfinder-lsp/src/client/detect.rs` | `detect_languages()`, `find_marker()` — language detection |
| `crates/pathfinder-lsp/src/client/mod.rs` | `LspClient`, `ProcessEntry`, `LanguageLsp` — process management |
| `crates/pathfinder-lsp/src/client/lifecycle.rs` | `start_process()`, `ensure_process()` — lifecycle |
| `crates/pathfinder-lsp/src/plugin.rs` | Language plugin registry (markers, binaries, search depth) |

## Research References

- LSP 3.6+ `workspace/workspaceFolders` specification
- `typescript-language-server` multi-root workspace support
- `vtsls` (VS Code TypeScript Language Server wrapper) — native monorepo handling
- TypeScript project references documentation
