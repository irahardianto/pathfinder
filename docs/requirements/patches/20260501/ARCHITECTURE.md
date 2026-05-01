# LSP Integration Architecture

This document describes the cross-language LSP (Language Server Protocol) integration
architecture in Pathfinder. It covers the agnostic channel design, diagnostics strategy,
plugin system, detection pipeline, and how to extend the system for new languages.

## Table of Contents

1. [Agnostic Channel Design](#agnostic-channel-design)
2. [Diagnostics Strategy](#diagnostics-strategy)
3. [TypeScript Plugin System](#typescript-plugin-system)
4. [Detection and Provisioning](#detection-and-provisioning)
5. [Health and Observability](#health-and-observability)
6. [Future Plugin Guide](#future-plugin-guide)

---

## Agnostic Channel Design

Pathfinder uses a language-agnostic LSP integration layer. All tool handlers
(`get_definition`, `analyze_impact`, `validate_only`, etc.) interact with LSP
servers through a single trait boundary.

### Lawyer Trait

The `Lawyer` trait (`crates/pathfinder-lsp/src/lawyer.rs`) defines the integration
boundary between Pathfinder's tool handlers and the LSP layer:

- `goto_definition` — find where a symbol is defined
- `call_hierarchy_prepare/incoming/outgoing` — build dependency graphs
- `did_open/did_change/did_close` — document synchronization
- `pull_diagnostics/pull_workspace_diagnostics` — diagnostic retrieval
- `range_formatting` — code formatting
- `collect_diagnostics` — push diagnostics collection
- `capability_status` — query running LSP capabilities
- `missing_languages` — query languages with markers but no LSP binary
- `warm_start` — eagerly spawn LSP processes

### Implementations

| Implementation | Purpose |
|---|---|
| `LspClient` | Production implementation that spawns and manages real LSP processes |
| `MockLawyer` | Testing implementation with configurable responses |
| `NoOpLawyer` | Degraded mode that returns `NoLspAvailable` for all methods |
| `UnsupportedDiagLawyer` | Wrapper that degrades diagnostics for unsupported languages |

### Per-Language Process Management

`LspClient` manages one LSP process per detected language:

- **Spawn**: Lazy initialization on first LSP method call, or eager via `warm_start()`
- **Lifecycle**: `spawn_and_initialize()` -> running -> idle timeout -> shutdown
- **Recovery**: Failed processes are marked `Unavailable` with a cooldown period
- **Retry**: Exponential backoff (1s, 2s, 4s) up to `MAX_RESTART_ATTEMPTS` (3)

### Language Routing

File paths are mapped to language IDs via file extensions:

```
.rs -> "rust"
.go -> "go"
.ts/.tsx/.js/.jsx/.vue -> "typescript"
.py -> "python"
```

See `language_id_for_extension()` in `detect.rs`.

---

## Diagnostics Strategy

LSP servers advertise diagnostics capability differently. Pathfinder supports
two strategies:

### Pull Diagnostics

- Server provides a `diagnosticProvider` capability
- Client explicitly requests diagnostics via `textDocument/diagnostic`
- Used by: rust-analyzer
- Latency: ~2s per request
- More reliable — client controls when to request

### Push Diagnostics

- Server pushes diagnostics via `textDocument/diagnostic` notifications
- Detected from `textDocumentSync.save` or `diagnostics` capability
- Used by: gopls, pyright, typescript-language-server
- Latency: ~10s (5s pre + 5s post collection window)
- Requires a collection window to gather all notifications

### Strategy Detection

Strategy is detected from the server's `initialize` response capabilities:

1. Check for `diagnosticProvider` (pull)
2. Check for `textDocumentSync.save` (push)
3. Check boolean `diagnostics` capability (push)
4. Default: none

See `detect_diagnostics_strategy()` in `capabilities.rs`.

### Validation Pipeline

```
Pull -> request diagnostics -> compare pre/post snapshots
Push -> collect notifications -> wait for collection window -> compare
None -> skip validation entirely
```

Fallback chain for edit validation: Pull -> Push -> Skip

---

## TypeScript Plugin System

Some frameworks (Vue, Svelte) need plugins loaded into the TypeScript language
server to provide accurate type information and navigation.

### Plugin Detection

Plugins are detected automatically from the workspace:

1. **Config override**: If `typescript_plugins` is set in `.pathfinder.toml`, use that
2. **Auto-detection**: Scan `node_modules/@vue/typescript-plugin` (npm and pnpm)
3. **Vue file trigger**: Auto-detection only activates when `.vue` files exist in the workspace

### Supported Plugins

| Plugin | Framework | Auto-detected |
|---|---|---|
| `@vue/typescript-plugin` | Vue 3 | Yes (when .vue files present) |

### How to Add New Plugins

To add a new framework plugin (e.g., Svelte):

1. Add detection logic in `detect.rs` (search `node_modules` for the plugin)
2. Add the plugin name to the auto-detection list
3. Add a test with fake `node_modules` structure
4. No changes needed in the protocol layer — plugins are passed as initialization options

### Plugin Configuration

Plugins are passed to the TypeScript server in the `initializationOptions`:

```json
{
  "plugins": [
    { "name": "@vue/typescript-plugin" }
  ]
}
```

---

## Detection and Provisioning

### Marker File Scanning

Languages are detected by scanning for marker files in the workspace:

| Language | Marker Files |
|---|---|
| Rust | `Cargo.toml` |
| Go | `go.mod` |
| TypeScript | `tsconfig.json`, `package.json` |
| Python | `pyproject.toml`, `requirements.txt`, `setup.py`, `setup.cfg` |

Marker files are searched up to depth 2 (workspace root + one level of subdirectories).

### Binary Resolution

For each detected language, the LSP binary is resolved via `which`:

1. If `command` is set in `.pathfinder.toml` config, use that
2. If `command` is an empty string, fall back to `which` for the default binary
3. If no binary is found, the language is listed as "missing" with install guidance

### Python Fallback Chain

Python has multiple LSP servers. Pathfinder tries them in order of preference:

1. `pyright-langserver` — Fast, strict type checking (npm: `pyright`)
2. `pylsp` — Community standard, plugin ecosystem (pip: `python-lsp-server`)
3. `ruff-lsp` — Extremely fast, growing adoption (pip: `ruff-lsp`)
4. `jedi-language-server` — Mature, lightweight (pip: `jedi-language-server`)

Note: `pyright` is the CLI tool; `pyright-langserver` is the stdio LSP server.
Both are installed by `npm install -g pyright`.

### Config Overrides

Users can override LSP settings in `.pathfinder.toml`:

```toml
[lsp.python]
command = "pyright-langserver"
args = ["--stdio"]
idle_timeout_minutes = 30

[lsp.typescript]
command = "typescript-language-server"
args = ["--stdio"]
typescript_plugins = ["@vue/typescript-plugin"]
```

---

## Health and Observability

### `lsp_health` Tool

The `lsp_health` tool provides a comprehensive view of all LSP servers:

**Per-language information:**
- Status: `ready`, `warming_up`, `starting`, `unavailable`
- Uptime: How long the LSP process has been running
- Diagnostics strategy: `pull`, `push`, or none
- Capabilities: call hierarchy, diagnostics, definition, formatting
- Degraded tools: Which tools lose LSP support
- Validation latency: Estimated time for validation (push: ~10s, pull: ~2s)
- Install hint: How to install the LSP when unavailable

**Probe-based readiness:**
- Languages running over 10s but still "warming_up" get a live probe
- Probe sends `goto_definition` to a workspace file to verify LSP responsiveness
- Successful probe upgrades status to "ready"

### Missing Language Detection

When marker files exist but no LSP binary is found on PATH:
- Language appears in `lsp_health` as "unavailable"
- Install hint provides actionable commands
- All tools listed as degraded

---

## Future Plugin Guide

### Adding a New TypeScript Plugin

1. **Detect**: Add detection logic in `detect.rs`
   - Search for the plugin in `node_modules`
   - Only activate when relevant files exist (e.g., `.svelte` files)

2. **Configure**: The plugin name is passed to the TypeScript server
   - No protocol changes needed

3. **Test**: Add unit tests with fake `node_modules` structure

### Adding a New Language

1. **Register extension**: Add to `language_id_for_extension()` in `detect.rs`

2. **Add marker files**: Add marker file patterns to the detection logic

3. **Configure LSP binary**: Add default binary name and args
   - For languages with multiple LSP options, add a fallback chain

4. **Add tree-sitter grammar**: Ensure the tree-sitter crate supports the language
   - Symbol extraction, name_column, etc.

5. **Test**: Add detection tests, integration tests, and name_column tests

6. **Install hint**: Add install guidance in `install_hint()` helper

### Testing Checklist

For each new language:

- [ ] Detection: marker file found -> language detected
- [ ] Binary resolution: binary on PATH -> command resolved
- [ ] Missing detection: marker found but no binary -> missing language with hint
- [ ] Integration: full pipeline test (gated on binary availability)
- [ ] Name column: tree-sitter extracts correct `name_column` for functions
- [ ] Health: `lsp_health` shows correct status, capabilities, degraded tools
