# PATCH-006: Add Probe-Based Readiness to lsp_health

## Group: C (Observability) — Capability Surface
## Depends on: PATCH-005

## Objective

Replace the current `indexing_complete` flag (which depends on `$/progress` notifications
that not all LSPs emit) with an optional probe-based readiness check. When an agent
calls `lsp_health` with a language that's been running for more than 10 seconds but
still shows `warming_up`, fire a lightweight `goto_definition` probe to check if the
LSP is actually ready. This fixes the common case where gopls or tsserver don't emit
progress notifications but are fully indexed.

## Severity: LOW — improves lsp_health accuracy for Go/TS

## Scope

| # | File | Change | Description |
|---|------|--------|-------------|
| 1 | `crates/pathfinder-lsp/src/lawyer.rs` | No change (goto_definition already exists) | Probe uses existing method |
| 2 | `crates/pathfinder/src/server/tools/navigation.rs` | Add probe logic to `lsp_health_impl` | Fire probe when warming_up for too long |
| 3 | `crates/pathfinder/src/server/types.rs` | Add `probe_verified` field to `LspLanguageHealth` | Signal that status was probe-confirmed |

## Step 1: Add Probe Logic

**File:** `crates/pathfinder/src/server/tools/navigation.rs`

In `lsp_health_impl`, after building the language list, check for stale warming_up:

```rust
// For languages that have been running for a while but still show warming_up,
// fire a probe to verify actual readiness.
for lang_health in &mut languages {
    if lang_health.status == "warming_up" {
        // Find the uptime
        let uptime_secs = lang_health.uptime.as_deref().and_then(|u| {
            // Parse "Xs", "XmYs", "XhYm" back to seconds approximately
            parse_uptime_to_seconds(u)
        });

        if let Some(secs) = uptime_secs {
            if secs > 10 {
                // LSP has been running for 10+ seconds but still warming_up.
                // This likely means progress notifications aren't being emitted.
                // Fire a lightweight probe.
                let probe_result = self.probe_language_readiness(&lang_health.language).await;
                if probe_result {
                    lang_health.status = "ready".to_owned();
                    lang_health.probe_verified = true;
                }
            }
        }
    }
}
```

Add helper:

```rust
/// Probe whether an LSP is actually ready by attempting a lightweight operation.
async fn probe_language_readiness(&self, language_id: &str) -> bool {
    // Find a well-known file in the workspace for this language
    let probe_file = match language_id {
        "rust" => self.find_probe_file(&["src/main.rs", "src/lib.rs"]),
        "go" => self.find_probe_file(&["main.go", "cmd/main.go"]),
        "typescript" => self.find_probe_file(&["src/index.ts", "index.ts", "src/main.ts"]),
        "python" => self.find_probe_file(&["src/__init__.py", "main.py", "setup.py"]),
        _ => None,
    };

    let Some(file_path) = probe_file else {
        return false; // No file to probe with
    };

    // Open the file, try goto_definition on line 1 column 1
    let content = tokio::fs::read_to_string(self.workspace_root.path().join(&file_path))
        .await
        .unwrap_or_default();

    let _ = self.lawyer.did_open(
        self.workspace_root.path(),
        &file_path,
        &content,
    ).await;

    let result = self.lawyer.goto_definition(
        self.workspace_root.path(),
        &file_path,
        1,
        1,
    ).await;

    let _ = self.lawyer.did_close(
        self.workspace_root.path(),
        &file_path,
    ).await;

    // Any response (even Ok(None)) means the LSP is alive and processing requests.
    // Only Err(ConnectionLost) or Err(Timeout) means it's not ready.
    result.is_ok()
}

fn find_probe_file(&self, candidates: &[&str]) -> Option<std::path::PathBuf> {
    for candidate in candidates {
        let path = self.workspace_root.path().join(candidate);
        if path.exists() {
            return Some(std::path::PathBuf::from(candidate));
        }
    }
    None
}
```

## Step 2: Add Field to Response Type

**File:** `crates/pathfinder/src/server/types.rs`

```rust
pub struct LspLanguageHealth {
    // ... existing fields ...

    /// Whether the status was verified by a live probe (rather than just
    /// progress notifications). When true, the agent can trust the status.
    pub probe_verified: bool,
}
```

## Step 3: Tests

- `test_lsp_health_probe_upgrades_warming_up_to_ready` — mock LSP running for 30s,
  goto_definition succeeds -> status upgraded to "ready", probe_verified = true
- `test_lsp_health_probe_keeps_warming_up_when_probe_fails` — mock LSP running for 30s,
  goto_definition returns ConnectionLost -> stays "warming_up", probe_verified = false
- `test_lsp_health_no_probe_for_recently_started` — uptime < 10s -> no probe attempted
- `test_lsp_health_no_probe_for_already_ready` — status "ready" -> no probe attempted

## EXCLUSIONS

- `mod.rs` in pathfinder-lsp — no changes to LspClient internals
- `process.rs` — spawn logic unchanged
- `detect.rs` — unchanged

## Verification

```bash
cargo build --all
cargo test --all

grep -n "probe_language_readiness\|probe_verified" \
  crates/pathfinder/src/server/tools/navigation.rs \
  crates/pathfinder/src/server/types.rs
```

## Expected Impact

- lsp_health shows "ready" for Go/TS once they're actually indexed (even without progress notifications)
- Agents get accurate status without guessing
- No more "permanently warming_up" for gopls/tsserver
- Probe is only fired when status is ambiguous (warming_up + uptime > 10s)
