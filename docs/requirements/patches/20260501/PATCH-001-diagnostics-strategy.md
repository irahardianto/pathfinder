# PATCH-001: Diagnostics Strategy Abstraction

## Group: A (Foundation) — Diagnostics Protocol
## Depends on: None

## Objective

Introduce a `DiagnosticsStrategy` enum that captures whether each LSP supports pull
diagnostics (LSP 3.17 `textDocument/diagnostic`), push diagnostics
(`textDocument/publishDiagnostics`), or neither. This is the foundation for making
`validate_only` and edit validation work for Go and TypeScript, which don't support
pull diagnostics but DO support push diagnostics.

Today, `DetectedCapabilities` has a boolean `diagnostic_provider` that only tracks
pull diagnostics. When this is false, validation is skipped entirely — even though
gopls and tsserver CAN provide diagnostics via push. This patch adds the abstraction
to detect and represent both strategies.

## Severity: HIGH — unblocks validation for Go/TS

## Scope

| # | File | Change | Description |
|---|------|--------|-------------|
| 1 | `crates/pathfinder-lsp/src/client/capabilities.rs` | Add `DiagnosticsStrategy` enum + detect push support | Parse `textDocumentSync` and `diagnosticProvider` to determine strategy |
| 2 | `crates/pathfinder-lsp/src/client/mod.rs` | Update `DetectedCapabilities` usage | Replace `diagnostic_provider: bool` with `diagnostics_strategy: DiagnosticsStrategy` |
| 3 | `crates/pathfinder-lsp/src/types.rs` | Add strategy to `LspLanguageStatus` | Surface strategy in capability_status |
| 4 | `crates/pathfinder/src/server/tools/edit/validation.rs` | Use strategy enum | Update `run_lsp_validation` to handle both strategies |

## Step 1: Define DiagnosticsStrategy

**File:** `crates/pathfinder-lsp/src/client/capabilities.rs`

Add after `DetectedCapabilities`:

```rust
/// How an LSP server provides diagnostics.
///
/// Determines the validation pipeline strategy for edit tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagnosticsStrategy {
    /// LSP supports `textDocument/diagnostic` (LSP 3.17 pull model).
    /// Most capable: request diagnostics on demand for any file.
    Pull,

    /// LSP supports `textDocument/publishDiagnostics` (push model).
    /// Requires subscribing to notifications after didOpen/didChange.
    /// Used by gopls, typescript-language-server, and most LSPs.
    Push,

    /// LSP does not support any diagnostics capability.
    None,
}
```

## Step 2: Update DetectedCapabilities

**File:** `crates/pathfinder-lsp/src/client/capabilities.rs`

Replace `diagnostic_provider: bool` and `workspace_diagnostic_provider: bool` with:

```rust
/// How this LSP provides diagnostics (pull, push, or none).
pub diagnostics_strategy: DiagnosticsStrategy,
```

Update `from_response_json` to detect both strategies:

```rust
// Check pull diagnostics first (preferred — more deterministic)
let has_pull = caps
    .get("diagnosticProvider")
    .is_some_and(|v| v.as_bool().unwrap_or_else(|| !v.is_null()));

let has_push = if has_pull {
    false // Don't need push if pull is available
} else {
    // Push diagnostics: check if textDocumentSync includes open/close/change
    // Most LSPs that support document sync also push diagnostics
    caps.get("textDocumentSync")
        .is_some_and(|v| {
            // textDocumentSync can be a number (TextDocumentSyncKind) or an object
            // If present at all, the LSP tracks documents and likely pushes diagnostics
            !v.is_null()
        })
};

let diagnostics_strategy = if has_pull {
    DiagnosticsStrategy::Pull
} else if has_push {
    DiagnosticsStrategy::Push
} else {
    DiagnosticsStrategy::None
};
```

Note: Push diagnostics detection is heuristic. An LSP that advertises `textDocumentSync`
but never sends `publishDiagnostics` will be incorrectly detected as `Push`. This is
acceptable because the push listener (PATCH-002) handles the "no diagnostics received"
case gracefully (returns empty, same as current behavior).

## Step 3: Propagate Through LspClient

**File:** `crates/pathfinder-lsp/src/client/mod.rs`

Update `validation_status_from_parts` to accept `DiagnosticsStrategy` instead of
`diagnostic_provider: bool`:

```rust
fn validation_status_from_parts(
    command: &str,
    running: bool,
    diagnostics_strategy: DiagnosticsStrategy,
    indexing_complete: bool,
    uptime_seconds: u64,
) -> crate::types::LspLanguageStatus {
    if !running {
        return crate::types::LspLanguageStatus {
            validation: false,
            reason: format!("{command} failed to start or crashed repeatedly"),
            indexing_complete: None,
            uptime_seconds: None,
        };
    }
    match diagnostics_strategy {
        DiagnosticsStrategy::Pull | DiagnosticsStrategy::Push => crate::types::LspLanguageStatus {
            validation: true,
            reason: format!(
                "LSP connected and supports validation ({})",
                match diagnostics_strategy {
                    DiagnosticsStrategy::Pull => "pull diagnostics",
                    DiagnosticsStrategy::Push => "push diagnostics",
                    DiagnosticsStrategy::None => unreachable!(),
                }
            ),
            indexing_complete: Some(indexing_complete),
            uptime_seconds: Some(uptime_seconds),
        },
        DiagnosticsStrategy::None => crate::types::LspLanguageStatus {
            validation: false,
            reason: "LSP connected but does not support diagnostics".to_owned(),
            indexing_complete: Some(indexing_complete),
            uptime_seconds: Some(uptime_seconds),
        },
    }
}
```

Update `ProcessEntry::to_validation_status` to use `state.process.capabilities.diagnostics_strategy`.

## Step 4: Add strategy to LspLanguageStatus

**File:** `crates/pathfinder-lsp/src/types.rs`

Add field to `LspLanguageStatus`:

```rust
/// How this LSP provides diagnostics (pull, push, or none).
pub diagnostics_strategy: Option<String>,
```

Populate from the capabilities in `capability_status()`. When no process is running
(lazy start), set to `None`. When running, set to the strategy name ("pull", "push", "none").

## Step 5: Update validation.rs to Use Strategy

**File:** `crates/pathfinder/src/server/tools/edit/validation.rs`

In `run_lsp_validation`, after getting capabilities, check strategy:

```rust
let ext = relative.extension().and_then(|e| e.to_str()).unwrap_or("");
let language_id = pathfinder_lsp::client::language_id_for_extension(ext);

// Determine diagnostics strategy from capabilities
let strategy = match language_id {
    Some(lang) => self.lawyer.capability_status().await
        .get(lang)
        .and_then(|s| s.diagnostics_strategy.as_deref())
        .and_then(|s| match s {
            "pull" => Some(pathfinder_lsp::client::capabilities::DiagnosticsStrategy::Pull),
            "push" => Some(pathfinder_lsp::client::capabilities::DiagnosticsStrategy::Push),
            _ => None,
        })
        .unwrap_or(pathfinder_lsp::client::capabilities::DiagnosticsStrategy::None),
    None => pathfinder_lsp::client::capabilities::DiagnosticsStrategy::None,
};

match strategy {
    DiagnosticsStrategy::Pull => {
        // Current behavior: pull diagnostics flow
        // (existing code, unchanged)
    }
    DiagnosticsStrategy::Push => {
        // NEW: will be implemented in PATCH-002
        // For now, return skipped with reason "push_diagnostics_not_yet_implemented"
        return return_skip("push_diagnostics_not_yet_implemented");
    }
    DiagnosticsStrategy::None => {
        return return_skip("no_diagnostics_support");
    }
}
```

This keeps the patch safe — push diagnostics is detected but not yet wired.
PATCH-002 fills in the implementation.

## Step 6: Update tests

Update all test constructions of `DetectedCapabilities` to use `diagnostics_strategy`
instead of `diagnostic_provider: bool` and `workspace_diagnostic_provider: bool`.

In `capabilities.rs` tests:
- `test_empty_capabilities` -> assert `diagnostics_strategy == DiagnosticsStrategy::None`
- `test_bool_true_capabilities` -> assert `diagnostics_strategy == DiagnosticsStrategy::Pull`
- `test_object_form_capabilities` -> assert `diagnostics_strategy == DiagnosticsStrategy::Pull`
- `test_bool_false_capabilities` -> assert `diagnostics_strategy == DiagnosticsStrategy::None`

Add new tests:
- `test_push_diagnostics_detected` — LSP with `textDocumentSync: 1` but no `diagnosticProvider`
  -> should detect `DiagnosticsStrategy::Push`
- `test_pull_preferred_over_push` — LSP with both `diagnosticProvider` and `textDocumentSync`
  -> should detect `DiagnosticsStrategy::Pull`

In `mod.rs` tests, update `validation_status_from_parts` calls to pass strategy.

## EXCLUSIONS — Do NOT Modify These

- `navigation.rs` — navigation tools don't use diagnostics
- `detect.rs` — detection logic unchanged; strategy is determined at capability negotiation
- `process.rs` — spawn logic unchanged
- The actual push diagnostics implementation — that's PATCH-002

## Verification

```bash
# 1. Build succeeds
cargo build --all

# 2. Existing tests still pass
cargo test --all

# 3. New strategy enum is used in capabilities
grep -n "DiagnosticsStrategy" crates/pathfinder-lsp/src/client/capabilities.rs
# Expected: enum definition + from_response_json usage

# 4. validation.rs references strategy
grep -n "DiagnosticsStrategy" crates/pathfinder/src/server/tools/edit/validation.rs
# Expected: match block with Pull/Push/None branches

# 5. LspLanguageStatus includes strategy
grep -n "diagnostics_strategy" crates/pathfinder-lsp/src/types.rs
# Expected: field definition
```

## Expected Impact

After this patch:
- `lsp_health` shows "push diagnostics" for Go and TypeScript (instead of "does not support")
- `run_lsp_validation` routes to strategy-appropriate validation path
- The validation still skips for Go/TS (push not implemented yet), but the reason
  is now "push_diagnostics_not_yet_implemented" instead of "pull_diagnostics_unsupported"
- PATCH-002 will fill in the push implementation
