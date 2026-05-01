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

This section is a step-by-step reference for extending Pathfinder's LSP support.
It captures every lesson from the Rust remediation, cross-language diagnostics
work (PATCH-001 through PATCH-011), and the Go/TypeScript/Python gap analysis.

Read this BEFORE writing any code. The agnostic channel means most new languages
need ZERO changes to tool handlers, the Lawyer trait, or validation logic.

---

### Architecture Invariants (Never Break These)

These properties hold for ALL languages. Violating them reintroduces bugs
that took weeks to diagnose.

1. **Single Lawyer trait, single LspClient**. All languages share the same
code path through `goto_definition`, `analyze_impact`, `read_with_deep_context`,
and `validate_only`. There is no per-language branching in tool handlers.

2. **Column indexing is 0-based, UTF-16 offset**. LSP uses 0-based columns.
Tree-sitter uses 0-based columns. The `name_column` field in `SymbolInfo` must
point to the FIRST character of the symbol name, not the keyword before it.
Example: `def compute(x)` -> name_column=4 (the 'c'), NOT 0 (the 'd').

3. **didOpen lifecycle is mandatory**. Every LSP operation (goto_definition,
pull_diagnostics, collect_diagnostics) requires the file to be opened via
`did_open` first. Closing via `did_close` after prevents memory leaks in the
LSP server.

4. **Empty hierarchy probe**. When `call_hierarchy_prepare` returns an empty
array, it does NOT mean the LSP failed. It means the symbol at that position
has no callers. The tool must return an empty result, not an error.

5. **Validation honesty**. When validation is skipped (no LSP, timeout, etc.),
the response MUST include `validation_skipped_reason` with a human-readable
explanation. Silent skipping hides real problems.

6. **Diagnostics strategy is detected, not configured**. The strategy (pull vs
push) comes from the LSP's `initialize` response capabilities. Never hardcode
it per language.

---

### Adding a New TypeScript Plugin (Framework Support)

Example: adding Svelte support via `svelte2tsx` + `svelte-language-server`.

**Files to modify:**

| # | File | Change |
|---|------|--------|
| 1 | `crates/pathfinder-lsp/src/client/detect.rs` | Plugin detection + file presence check |
| 2 | `crates/pathfinder-lsp/src/client/detect.rs` tests | Fake node_modules test fixtures |

No changes needed in `process.rs`, `capabilities.rs`, `lawyer.rs`, or `navigation.rs`.

**Step 1: Add plugin detection**

In `detect.rs`, the `detect_typescript_plugins` function handles plugin discovery.
Add your plugin to the auto-detection block:

```rust
// Inside detect_typescript_plugins():

// Auto-detect Svelte plugin when .svelte files are present
if workspace_has_svelte_files(workspace_root).await {
    if let Some(plugin) = detect_ts_plugin(workspace_root, "svelte2tsx").await {
        tracing::info!("Auto-detected svelte2tsx for Svelte support");
        plugins.push(plugin);
    }
}
```

Write a `workspace_has_svelte_files()` function following the `workspace_has_vue_files()`
pattern: scan `src/` (preferred) or workspace root up to 2 levels deep for `.svelte` files.

**Step 2: Ensure extension is registered**

If the framework uses a new file extension, add it to `language_id_for_extension()`:

```rust
"ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "vue" | "svelte" => Some("typescript"),
```

**Step 3: Add file extension to initialize params**

In `build_initialize_request()` in `process.rs`, if the plugin needs tsserver to
recognize the new extension, add it to `extraFileExtensions`:

```json
"extraFileExtensions": [
    { "extension": "vue", "scriptKind": 3 },
    { "extension": "svelte", "scriptKind": 3 }
]
```

Note: `scriptKind: 3` is TypeScript's internal enum for TS. Not the string "TS".

**Step 4: Tests**

Follow the existing Vue plugin test pattern:
- `test_auto_detects_svelte_plugin_when_present`
- `test_no_svelte_plugin_without_svelte_files`
- `test_no_svelte_plugin_when_absent`

Use the `create_vue_plugin()` test helper as a template for creating fake
`node_modules` structures.

---

### Adding a New Language (Complete LSP Integration)

This is the full checklist for adding a completely new language (e.g., Java, Ruby,
C#, Kotlin). The agnostic channel means you only touch 4 areas: detection,
tree-sitter, health probes, and tests.

**Architecture diagram (data flow for a new language):**

```
                    .pathfinder.toml
                         |
                         v
 detect_languages() -> DetectionResult { detected, missing }
                         |
           +--------------+--------------+
           |                             |
     detected: Vec<LanguageLsp>    missing: Vec<MissingLanguage>
           |                             |
           v                             v
    LspClient.warm_start()       lsp_health (unavailable)
           |                             + install_hint
           v
    spawn_and_initialize(plugins)
           |
           v
    DetectedCapabilities::from_response_json()
           |
     +-----+------+-----------+
     |            |            |
  definition  call_hierarchy  diagnostics_strategy
  _provider   _provider      (Pull|Push|None)
     |            |            |
     v            v            v
  lsp_health response (per-language capabilities)
     |
     v
  Tool handlers use Lawyer trait (no per-language code)
```

**Files to modify:**

| # | File | Change |
|---|------|--------|
| 1 | `crates/pathfinder-lsp/src/client/detect.rs` | Extension mapping, marker files, binary resolution, install hint |
| 2 | `crates/pathfinder-lsp/src/client/detect.rs` tests | Detection, fallback, missing tests |
| 3 | `crates/pathfinder-treesitter/src/symbols.rs` | Tree-sitter grammar + name_column |
| 4 | `crates/pathfinder-treesitter/src/language.rs` | `SupportedLanguage` enum variant |
| 5 | `crates/pathfinder/src/server/tools/navigation.rs` | Probe file candidates |
| 6 | `crates/pathfinder-lsp/tests/lsp_client_integration.rs` | Full pipeline integration test |

No changes needed: `lawyer.rs`, `validation.rs`, `capabilities.rs`, `process.rs`,
`mock.rs`, `no_op.rs`, `protocol.rs`, tool handler files.

**Step 1: Register the file extension**

File: `crates/pathfinder-lsp/src/client/detect.rs`

Add to `language_id_for_extension()`:

```rust
"java" => Some("java"),
"rb" => Some("ruby"),
"cs" => Some("csharp"),
```

Language IDs should match the LSP server's expected `languageId` in `textDocument/didOpen`.
Common convention: use the language name ("ruby", "java") not the extension ("rb", "java").

**Step 2: Add marker file detection**

In `detect_languages()`, add a new detection block following the existing pattern.
Every block has this structure:

```rust
// ── Marker detection ──
let lang_root = match get_override!("java") {
    Some(r) => Some(r),
    None => find_marker(workspace_root, "pom.xml", 2).await
        .or(find_marker(workspace_root, "build.gradle", 2).await),
};

if let Some(root) = lang_root {
    let has_override = get_command_override!("java").is_some();
    let cmd = get_command_override!("java")
        .or_else(|| resolve_command("jdtls", "java"));

    if let Some(command) = cmd {
        detected.push(LanguageLsp {
            language_id: "java".to_owned(),
            command,
            args: get_args!("java", vec![]),
            root,
            init_timeout_secs: None,
            auto_plugins: vec![],
        });
    } else if !has_override {
        // Marker found but no binary -> report as missing with install hint
        missing.push(MissingLanguage {
            language_id: "java".to_owned(),
            marker_file: "pom.xml or build.gradle".to_string(),
            tried_binaries: vec!["jdtls".to_string()],
            install_hint: install_hint("java"),
        });
    }
}
```

Important details:
- `get_override!` checks `.pathfinder.toml` for `root_override` (monorepo support)
- `get_command_override!` checks for custom `command` in config
- `has_override` prevents false "missing" reports when user configured an empty command
- `get_args!` returns config args if set, otherwise falls back to `default_args`
- `find_marker(..., 2)` scans up to depth 2 (workspace root + one subdir level)

**Step 2a: Multiple LSP binaries (fallback chain)**

If the language has multiple popular LSP servers, use a fallback chain:

```rust
let lsp_candidates = [
    ("jdtls", vec![]),              // Eclipse JDT Language Server
    ("java-language-server", vec![]), // Alternative
];

let maybe_command = get_command_override!("java").or_else(|| {
    for (binary, args) in &lsp_candidates {
        if let Some(resolved) = resolve_command(binary, "java") {
            return Some((resolved, args.clone()));
        }
    }
    None
});
```

See the Python detection block for a working example with 4 fallback candidates.

**Step 3: Add tree-sitter support**

File: `crates/pathfinder-treesitter/src/language.rs`

Add the language to the `SupportedLanguage` enum and wire the tree-sitter grammar.

File: `crates/pathfinder-treesitter/src/symbols.rs`

Ensure the symbol extraction produces correct `name_column` values. This is
the #1 source of bugs when adding new languages. Test it:

```rust
#[test]
fn test_java_name_column_points_to_method_name() {
    let source = "public void compute(int x) { return x * 2; }";
    // name_column must point to 'c' in 'compute', not 'p' in 'public'
    // Verify: the LSP uses 0-based columns where 'public' starts at 0
    // and 'compute' starts at column 12 (after "public void ")
}
```

**Step 4: Add probe file candidates**

File: `crates/pathfinder/src/server/tools/navigation.rs`

In `find_probe_file()`, add candidate files for the new language:

```rust
"java" => vec!["src/main/java/Main.java", "src/Main.java"],
"ruby" => vec!["lib/main.rb", "main.rb"],
```

These are used by the probe-based readiness check. When an LSP has been running
for 10+ seconds but still shows "warming_up" (because it doesn't emit `$/progress`
notifications), Pathfinder sends a lightweight `goto_definition` probe to one of
these files. If the probe succeeds, status is upgraded to "ready".

Pick files that are very likely to exist in a typical project. Don't pick obscure
files. The probe is best-effort; if no candidate file exists, the probe is skipped
and the language stays "warming_up" until it naturally reports ready.

**Step 5: Add install hint**

File: `crates/pathfinder-lsp/src/client/detect.rs`

Add to the `install_hint()` function:

```rust
"java" => "Install Eclipse JDT Language Server: https://github.com/eclipse-jdtls/eclipse.jdt.ls".to_owned(),
"ruby" => "Install solargraph: gem install solargraph".to_owned(),
```

**Step 6: Integration test**

File: `crates/pathfinder-lsp/tests/lsp_client_integration.rs`

Add a gated integration test following the Python integration pattern:

```rust
#[cfg(feature = "integration")]
#[cfg(test)]
mod java_integration {
    fn jdtls_available() -> bool {
        which::which("jdtls").is_ok()
    }

    #[tokio::test]
    async fn test_java_lsp_full_pipeline() {
        if !jdtls_available() {
            eprintln!("Skipping Java integration test: jdtls not installed");
            return;
        }
        // Create temp dir with pom.xml
        // Create Java source file with known symbol
        // Test: detection, did_open, goto_definition, call_hierarchy_prepare
        // All assertions should handle LSPs that don't support every capability
    }
}
```

The `#[cfg(feature = "integration")]` gate ensures the test doesn't run in
normal CI. Run with: `cargo test --features integration`

**Step 7: Detection unit tests**

File: `crates/pathfinder-lsp/src/client/detect.rs` (in the `tests` module)

Use the `test_with_fake_python_binaries` helper as a template. Create a similar
helper for your language that:
1. Creates a temp directory with symlinks to fake binaries
2. Temporarily adds the temp dir to PATH
3. Runs the test closure
4. Cleans up PATH

Tests to write:
- `test_detects_<lang>_via_<marker>`
- `test_detects_<lang>_fallback_to_<alt_lsp>` (if fallback chain exists)
- `test_<lang>_not_detected_without_binary`
- `test_prefers_<primary_lsp>_over_<secondary>`

---

### Diagnostics Strategy for New Languages

When adding a new language, you do NOT need to implement anything special for
diagnostics. The system auto-detects the strategy from the LSP's capabilities.

However, you should VERIFY which strategy the new LSP uses:

| LSP Server | Diagnostics Strategy | Why |
|---|---|---|
| rust-analyzer | Pull | Advertises `diagnosticProvider` in capabilities |
| gopls | Push | Only `textDocumentSync`, no `diagnosticProvider` |
| typescript-language-server | Push | Only `textDocumentSync`, no `diagnosticProvider` |
| pyright-langserver | Push (likely) | Only `textDocumentSync` |
| jdtls (Java) | Unknown | Test empirically with `lsp_health` |

To verify: start Pathfinder, open a workspace for the language, call `lsp_health`.
The response shows `diagnostics_strategy` per language.

**If the LSP supports PULL diagnostics** (advertises `diagnosticProvider`):
- No extra work needed. The existing pull path in `validation.rs` handles it.
- Validation latency: ~2s.

**If the LSP supports PUSH diagnostics** (only `textDocumentSync`):
- No extra work needed. The push collection path in `collect_diagnostics()` handles it.
- Validation latency: ~10s (two 5s collection windows).
- The push path is heuristic: it subscribes to `textDocument/publishDiagnostics`
  notifications and collects them within a timeout. If the LSP never sends
  diagnostics, the path returns an empty vec (same as "no errors").

**If the LSP supports NEITHER** (no `textDocumentSync`, no `diagnosticProvider`):
- No extra work needed. Validation is skipped with reason `no_diagnostics_support`.
- `lsp_health` shows `diagnostics_strategy: "none"`.
- `degraded_tools` includes `validate_only`.

---

### Common Pitfalls

These are bugs that have actually occurred. Do not reintroduce them.

1. **name_column pointing to keyword instead of name**.
   Tree-sitter's `field_name: "name"` node gives the correct position.
   Do NOT use the parent node's start column (which points to `def`, `fn`, `func`).
   Always verify with a test: `assert_eq!(sym.name_column, 4)` not `0`.

2. **1-based vs 0-based line numbers**.
   LSP uses 0-based lines in the protocol but most editors show 1-based.
   Pathfinder's internal types use 0-based (matching LSP). Tree-sitter also
   uses 0-based. The `DefinitionLocation.line` field is 0-based.
   When displaying to users, add +1.

3. **Assuming all LSPs emit `$/progress` notifications**.
   Many don't (gopls, tsserver). The probe-based readiness check in `lsp_health`
   handles this by sending a `goto_definition` probe after 10 seconds of "warming_up".

4. **Push diagnostics timeout too short**.
   The push collection window is 5 seconds per snapshot (10s total for validation).
   Some slow LSPs may need more. The timeout is hardcoded but could be made
   configurable if needed.

5. **Not handling `UnsupportedCapability` errors**.
   Not all LSPs support call hierarchy. The `call_hierarchy_prepare` method returns
   `LspError::UnsupportedCapability` when the server doesn't advertise
   `callHierarchyProvider`. Tool handlers must degrade gracefully (use grep fallback).

6. **Opening a file without closing it**.
   Every `did_open` must be paired with `did_close`. Otherwise the LSP server
   accumulates open documents and leaks memory. The validation pipeline handles
   this in `lsp_revert_and_close`.

7. **Forgetting `auto_plugins: vec![]`** when constructing `LanguageLsp`.
   Every detection block must include this field. The TypeScript block is the
   only one that populates it.

---

### Testing Checklist

For each new language or plugin, verify ALL of these:

**Detection:**
- [ ] Marker file found -> language detected
- [ ] Binary on PATH -> command resolved correctly
- [ ] Marker found but no binary -> `DetectionResult.missing` populated with install hint
- [ ] Config override -> user's command takes precedence over auto-detection
- [ ] Multiple binaries -> fallback chain tries in preference order
- [ ] Monorepo -> `root_override` in config finds marker in subdirectory

**Tree-sitter:**
- [ ] `name_column` points to first char of symbol name (not keyword)
- [ ] `SupportedLanguage` enum has the new variant
- [ ] Symbols extracted: functions, methods, classes, structs, enums
- [ ] Nested symbols have correct parent-child relationships

**LSP Integration:**
- [ ] `spawn_and_initialize` succeeds (LSP starts without error)
- [ ] `goto_definition` resolves a known symbol
- [ ] `call_hierarchy_prepare` works or returns `UnsupportedCapability`
- [ ] Diagnostics strategy auto-detected correctly (check `lsp_health`)
- [ ] `validate_only` produces a result (not always "skipped")
- [ ] Push diagnostics collected within timeout (if applicable)

**Health and Observability:**
- [ ] `lsp_health` shows correct `status` (ready/warming_up/unavailable)
- [ ] `lsp_health` shows correct `diagnostics_strategy`
- [ ] `lsp_health` shows correct `supports_*` capabilities
- [ ] `lsp_health` shows correct `degraded_tools`
- [ ] `lsp_health` shows correct `install_hint` when binary missing
- [ ] Probe upgrades "warming_up" to "ready" after 10s (if LSP doesn't emit progress)
- [ ] `probe_verified` field is `true` only after a successful probe

**Edge Cases:**
- [ ] Empty workspace (no files) -> no languages detected, no crashes
- [ ] Multiple languages in one workspace (e.g., Go backend + Vue frontend)
- [ ] LSP crashes during operation -> marked unavailable, tools degrade gracefully
- [ ] Very large file -> no timeout on symbol extraction
- [ ] Non-UTF8 file -> tree-sitter handles without panic

---

### Reference: Key Structs and Functions

| Struct/Function | File | Purpose |
|---|---|---|
| `LanguageLsp` | `detect.rs` | Detection result: language_id, command, args, root, auto_plugins |
| `DetectionResult` | `detect.rs` | `{ detected: Vec<LanguageLsp>, missing: Vec<MissingLanguage> }` |
| `MissingLanguage` | `detect.rs` | Language with marker but no binary: tried_binaries, install_hint |
| `DiagnosticsStrategy` | `capabilities.rs` | Pull, Push, or None — auto-detected from LSP capabilities |
| `DetectedCapabilities` | `capabilities.rs` | LSP capabilities from `initialize` response |
| `LspLanguageStatus` | `types.rs` (pathfinder-lsp) | Capability status per language (validation, strategy, supports_*) |
| `LspLanguageHealth` | `types.rs` (pathfinder) | Health response per language (all fields, serialized to JSON) |
| `Lawyer` trait | `lawyer.rs` | Integration boundary between tools and LSP |
| `LspClient` | `mod.rs` | Production Lawyer impl: spawn, lifecycle, routing |
| `MockLawyer` | `mock.rs` | Test Lawyer impl with configurable responses |
| `install_hint()` | `detect.rs` | Per-language install guidance strings |
| `language_id_for_extension()` | `detect.rs` | Extension to language_id mapping |
| `detect_languages()` | `detect.rs` | Main detection entry point |
| `detect_typescript_plugins()` | `detect.rs` | Auto-detect TS plugins (Vue, future Svelte) |
| `find_probe_file()` | `navigation.rs` | Well-known files for probe-based readiness |
| `compute_degraded_tools()` | `navigation.rs` | Compute which tools are degraded from capabilities |
| `validation_status_from_parts()` | `mod.rs` | Map process state to `LspLanguageStatus` |
| `collect_push_diagnostics()` | `protocol.rs` | Subscribe and collect push diagnostics with timeout |
| `build_initialize_request()` | `process.rs` | LSP initialize request with plugins and capabilities |
