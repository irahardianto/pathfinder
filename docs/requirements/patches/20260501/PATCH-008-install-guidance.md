# PATCH-008: Surface Install Guidance for Missing LSPs

## Group: D (Provisioning) — Python Support
## Depends on: PATCH-007

## Objective

When a workspace contains files of a language but no LSP binary is detected,
surface an actionable install guidance message in `lsp_health` and in tool error
responses. This turns the silent "no_lsp" degradation into a helpful nudge.

## Severity: LOW — improves developer experience

## Scope

| # | File | Change | Description |
|---|------|--------|-------------|
| 1 | `crates/pathfinder-lsp/src/client/detect.rs` | Track "detected but no binary" languages | Return attempted languages with their binaries |
| 2 | `crates/pathfinder-lsp/src/types.rs` | Add install guidance to LspLanguageStatus | Include suggested install command |
| 3 | `crates/pathfinder/src/server/tools/navigation.rs` | Include guidance in lsp_health response | Surface in per-language status |
| 4 | `crates/pathfinder/src/server/types.rs` | Add guidance field to LspLanguageHealth | Response field for install hint |

## Step 1: Track Undetected Languages

**File:** `crates/pathfinder-lsp/src/client/detect.rs`

Add a return type that includes both detected and "marker found but no binary" languages:

```rust
/// Result of language detection.
pub struct DetectionResult {
    /// Languages with markers AND binaries found.
    pub detected: Vec<LanguageLsp>,
    /// Languages with markers but no binary on PATH.
    pub missing: Vec<MissingLanguage>,
}

/// A language whose marker files were found but whose LSP binary is not on PATH.
pub struct MissingLanguage {
    pub language_id: String,
    pub marker_file: String,
    pub tried_binaries: Vec<String>,
    pub install_hint: String,
}

/// Install hints for common LSP servers.
fn install_hint(language_id: &str) -> String {
    match language_id {
        "rust" => "Install rust-analyzer: https://rust-analyzer.github.io/".to_owned(),
        "go" => "Install gopls: go install golang.org/x/tools/gopls@latest".to_owned(),
        "typescript" => "Install typescript-language-server: npm install -g typescript-language-server typescript".to_owned(),
        "python" => "Install pyright: npm install -g pyright\nOr install pylsp: pip install python-lsp-server".to_owned(),
        _ => format!("Install a language server for {language_id}"),
    }
}
```

Update `detect_languages` return type to `DetectionResult`. Populate `missing` when
marker is found but binary resolution fails for all candidates.

## Step 2: Add Guidance to Status

**File:** `crates/pathfinder-lsp/src/types.rs`

```rust
pub struct LspLanguageStatus {
    // ... existing fields ...

    /// Install guidance when LSP is unavailable.
    /// None when LSP is running or language not detected at all.
    pub install_hint: Option<String>,
}
```

## Step 3: Wire into lsp_health

**File:** `crates/pathfinder/src/server/tools/navigation.rs`

In `lsp_health_impl`, include missing languages:

```rust
// After capability_status for detected languages:
let detection = self.lawyer.detection_result();  // New method on LspClient

for missing in &detection.missing {
    if let Some(ref filter) = params.language {
        if &missing.language_id != filter { continue; }
    }
    languages.push(LspLanguageHealth {
        language: missing.language_id.clone(),
        status: "unavailable".to_owned(),
        uptime: None,
        diagnostics_strategy: None,
        supports_call_hierarchy: None,
        supports_diagnostics: None,
        probe_verified: false,
        install_hint: Some(missing.install_hint.clone()),
    });
}
```

## Step 4: Tests

- `test_missing_python_shows_install_hint` — workspace with pyproject.toml but no pyright
  -> lsp_health shows Python with install_hint containing "npm install -g pyright"
- `test_missing_go_shows_gopls_hint` — workspace with go.mod but no gopls
  -> lsp_health shows Go with install_hint containing "go install"
- `test_no_missing_when_binary_found` — workspace with all binaries
  -> no missing entries in lsp_health

## EXCLUSIONS

- `validation.rs` — no changes (it already shows skip reasons)
- `process.rs` — no changes
- Error responses in tool handlers — future enhancement

## Verification

```bash
cargo build --all
cargo test --all

grep -n "install_hint\|MissingLanguage" crates/pathfinder-lsp/src/client/detect.rs
grep -n "install_hint" crates/pathfinder-lsp/src/types.rs
```

## Expected Impact

- lsp_health shows "Python: unavailable — Install pyright: npm install -g pyright"
- Agents can surface this to users when tools degrade
- Developers know exactly what to install
