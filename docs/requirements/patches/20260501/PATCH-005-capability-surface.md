# PATCH-005: Surface Per-Language Capabilities in lsp_health

## Group: C (Observability) — Capability Surface
## Depends on: None

## Objective

Extend the `lsp_health` response to include per-language diagnostics strategy and
which LSP operations are supported (definition, call hierarchy, diagnostics, formatting).
This gives agents the information they need to choose their tool strategy at session
start, rather than discovering limitations through failed tool calls.

## Severity: LOW — improves agent self-adaptation

## Scope

| # | File | Change | Description |
|---|------|--------|-------------|
| 1 | `crates/pathfinder-lsp/src/types.rs` | Extend `LspLanguageStatus` | Add capability fields |
| 2 | `crates/pathfinder/src/server/types.rs` | Extend `LspLanguageHealth` | Add strategy and capability fields |
| 3 | `crates/pathfinder/src/server/tools/navigation.rs` | Populate new fields in `lsp_health_impl` | Map LspLanguageStatus to response |

## Step 1: Extend LspLanguageStatus

**File:** `crates/pathfinder-lsp/src/types.rs`

Add to `LspLanguageStatus`:

```rust
/// How this LSP provides diagnostics ("pull", "push", "none", or null if no process).
pub diagnostics_strategy: Option<String>,

/// LSP supports textDocument/definition (get_definition).
pub supports_definition: Option<bool>,

/// LSP supports textDocument/prepareCallHierarchy (analyze_impact, read_with_deep_context).
pub supports_call_hierarchy: Option<bool>,

/// LSP supports textDocument/diagnostic or publishDiagnostics (validate_only, edit validation).
pub supports_diagnostics: Option<bool>,

/// LSP supports textDocument/rangeFormatting (edit formatting).
pub supports_formatting: Option<bool>,
```

Populate these in `mod.rs` `capability_status` from `DetectedCapabilities`.

## Step 2: Extend Response Type

**File:** `crates/pathfinder/src/server/types.rs`

Add to `LspLanguageHealth`:

```rust
/// How diagnostics work for this language.
pub diagnostics_strategy: Option<String>,

/// Whether call hierarchy is supported (affects analyze_impact, read_with_deep_context).
pub supports_call_hierarchy: Option<bool>,

/// Whether validation is supported (affects validate_only, edit tools).
pub supports_diagnostics: Option<bool>,
```

## Step 3: Wire Up in lsp_health_impl

**File:** `crates/pathfinder/src/server/tools/navigation.rs`

Map from `LspLanguageStatus` to `LspLanguageHealth`:

```rust
languages.push(crate::server::types::LspLanguageHealth {
    language: lang.clone(),
    status: status_str.to_owned(),
    uptime,
    diagnostics_strategy: status.diagnostics_strategy.clone(),
    supports_call_hierarchy: status.supports_call_hierarchy,
    supports_diagnostics: status.supports_diagnostics,
});
```

## Step 4: Tests

- `test_lsp_health_includes_diagnostics_strategy` — response has strategy field
- `test_lsp_health_shows_push_for_go` — mock Go LSP -> strategy = "push"
- `test_lsp_health_shows_pull_for_rust` — mock Rust LSP -> strategy = "pull"
- `test_lsp_health_shows_capabilities` — supports_call_hierarchy, supports_diagnostics present

## EXCLUSIONS

- `validation.rs` — no changes
- `detect.rs` — no changes
- `process.rs` — no changes

## Verification

```bash
cargo build --all
cargo test --all

# New fields present in types
grep -n "diagnostics_strategy\|supports_call_hierarchy\|supports_diagnostics" \
  crates/pathfinder-lsp/src/types.rs crates/pathfinder/src/server/types.rs
```

## Expected Impact

Agents can call `lsp_health` once at session start and know:
- Whether `analyze_impact` will use LSP or grep for each language
- Whether `validate_only` will work for each language
- What diagnostics strategy is in use
- No more discovering limitations through failed calls
