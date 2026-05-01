# PATCH-002: Implement Push Diagnostics Listener

## Group: A (Foundation) — Diagnostics Protocol
## Depends on: PATCH-001

## Objective

Implement the push diagnostics path in the validation pipeline. When an LSP uses
`DiagnosticsStrategy::Push`, Pathfinder subscribes to `textDocument/publishDiagnostics`
notifications after `didOpen`/`didChange`, collects diagnostics for the target file
within a timeout window, and diffs them the same way pull diagnostics are diffed today.

This is the implementation that makes `validate_only` and edit validation work for
Go (gopls) and TypeScript (typescript-language-server).

## Severity: HIGH — completes validation for Go/TS

## Scope

| # | File | Change | Description |
|---|------|--------|-------------|
| 1 | `crates/pathfinder-lsp/src/client/protocol.rs` | Add diagnostics subscription to RequestDispatcher | Channel for collecting publishDiagnostics notifications |
| 2 | `crates/pathfinder-lsp/src/client/mod.rs` | Add `collect_push_diagnostics` method to LspClient | Subscribe, wait, collect diagnostics for a file |
| 3 | `crates/pathfinder-lsp/src/lawyer.rs` | Add `collect_diagnostics` method to Lawyer trait | New trait method for push diagnostics |
| 4 | `crates/pathfinder/src/server/tools/edit/validation.rs` | Implement push validation path | Replace stub from PATCH-001 with real implementation |
| 5 | `crates/pathfinder-lsp/src/mock.rs` | Add mock for `collect_diagnostics` | Test support |
| 6 | `crates/pathfinder-lsp/src/no_op.rs` | Add no-op for `collect_diagnostics` | Graceful degradation |

## Step 1: Add Diagnostics Collection to RequestDispatcher

**File:** `crates/pathfinder-lsp/src/client/protocol.rs`

The `RequestDispatcher` already has a `subscribe_notifications()` method for the
progress watcher task. Reuse the same pattern for diagnostics.

Add a method to filter and collect `textDocument/publishDiagnostics` notifications
for a specific file URI within a timeout:

```rust
impl RequestDispatcher {
    /// Subscribe to `textDocument/publishDiagnostics` notifications for a
    /// specific file URI. Returns collected diagnostics after `timeout`.
    ///
    /// This is a one-shot collector: it subscribes, waits for notifications,
    /// and returns whatever was received within the timeout window.
    pub async fn collect_push_diagnostics(
        &self,
        file_uri: &str,
        timeout: Duration,
    ) -> Vec<serde_json::Value> {
        let mut rx = self.subscribe_notifications();
        let mut collected = Vec::new();
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }

            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Ok(msg)) => {
                    let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
                    if method != "textDocument/publishDiagnostics" {
                        continue;
                    }
                    let uri = msg
                        .pointer("/params/uri")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if uri == file_uri {
                        collected.push(msg);
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                Err(_) => break, // timeout
            }
        }

        collected
    }
}
```

## Step 2: Add Lawyer Trait Method

**File:** `crates/pathfinder-lsp/src/lawyer.rs`

Add new method to the `Lawyer` trait:

```rust
/// Collect diagnostics for a file using push model.
///
/// Sends `didOpen` (if needed), waits for `textDocument/publishDiagnostics`
/// notifications targeting the file, and returns the collected diagnostics.
///
/// This is used as a fallback when `pull_diagnostics` is not supported.
///
/// # Errors
/// - `LspError::NoLspAvailable` — no language server for this file type
/// - `LspError::Timeout` — no diagnostics received within timeout
async fn collect_diagnostics(
    &self,
    workspace_root: &Path,
    file_path: &Path,
    content: &str,
    version: i32,
    timeout_ms: u64,
) -> Result<Vec<LspDiagnostic>, LspError>;
```

## Step 3: Implement in LspClient

**File:** `crates/pathfinder-lsp/src/client/mod.rs`

```rust
async fn collect_diagnostics(
    &self,
    workspace_root: &Path,
    file_path: &Path,
    content: &str,
    version: i32,
    timeout_ms: u64,
) -> Result<Vec<LspDiagnostic>, LspError> {
    let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let language_id = language_id_for_extension(ext).ok_or(LspError::NoLspAvailable)?;
    self.ensure_process(language_id).await?;

    let file_uri = Url::from_file_path(workspace_root.join(file_path))
        .map_err(|()| LspError::Protocol("cannot convert file path to URI".to_owned()))?
        .to_string();

    // Send didOpen or didChange
    if version <= 1 {
        self.did_open(workspace_root, file_path, content).await?;
    } else {
        self.did_change(workspace_root, file_path, content, version).await?;
    }

    // Collect push diagnostics within timeout
    let raw_diags = self
        .dispatcher
        .collect_push_diagnostics(&file_uri, Duration::from_millis(timeout_ms))
        .await;

    // Parse the collected notifications
    let mut all_diags = Vec::new();
    for notif in raw_diags {
        if let Some(items) = notif.pointer("/params/diagnostics").and_then(|v| v.as_array()) {
            all_diags.extend(parse_diagnostic_items(items, file_path));
        }
    }

    self.touch(language_id);
    Ok(all_diags)
}
```

## Step 4: Implement Push Validation Path

**File:** `crates/pathfinder/src/server/tools/edit/validation.rs`

Replace the `DiagnosticsStrategy::Push` stub from PATCH-001:

```rust
DiagnosticsStrategy::Push => {
    // Push diagnostics: didOpen -> collect -> didChange -> collect -> diff
    let push_timeout_ms = 5000; // 5 seconds to collect diagnostics

    // Step 1: Open and collect pre-edit diagnostics
    let pre_diags = match self
        .lawyer
        .collect_diagnostics(
            workspace, relative, original_content, 1, push_timeout_ms,
        )
        .await
    {
        Ok(d) => d,
        Err(e) => {
            let reason = Self::lsp_error_to_skip_reason(&e);
            tracing::warn!(error = %e, "validation: push pre-diagnostics collection failed");
            return return_skip(reason);
        }
    };

    // Step 2: Apply change and collect post-edit diagnostics
    let post_diags = match self
        .lawyer
        .collect_diagnostics(
            workspace, relative, new_content, 2, push_timeout_ms,
        )
        .await
    {
        Ok(d) => d,
        Err(e) => {
            let reason = Self::lsp_error_to_skip_reason(&e);
            tracing::warn!(error = %e, "validation: push post-diagnostics collection failed");
            self.lsp_revert_and_close(workspace, relative, original_content).await;
            return return_skip(reason);
        }
    };

    // Step 3: Revert and close
    self.lsp_revert_and_close(workspace, relative, original_content).await;

    // Step 4: Same diff logic as pull diagnostics
    build_validation_outcome(
        &pre_diags,
        &post_diags,
        ignore_validation_failures,
        file_path,
    )
}
```

## Step 5: Add Mock and NoOp Implementations

**File:** `crates/pathfinder-lsp/src/mock.rs`

```rust
async fn collect_diagnostics(
    &self,
    _workspace_root: &Path,
    _file_path: &Path,
    _content: &str,
    _version: i32,
    _timeout_ms: u64,
) -> Result<Vec<LspDiagnostic>, LspError> {
    // Return empty by default — tests can override via results queue
    Ok(vec![])
}
```

**File:** `crates/pathfinder-lsp/src/no_op.rs`

```rust
async fn collect_diagnostics(
    &self,
    _workspace_root: &Path,
    _file_path: &Path,
    _content: &str,
    _version: i32,
    _timeout_ms: u64,
) -> Result<Vec<LspDiagnostic>, LspError> {
    Err(LspError::NoLspAvailable)
}
```

## Step 6: Tests

### Unit Tests (capabilities.rs)

- `test_push_diagnostics_from_text_document_sync_number` — `textDocumentSync: 1` with
  no `diagnosticProvider` -> strategy = Push
- `test_push_diagnostics_from_text_document_sync_object` — `textDocumentSync: { openClose: true, change: 1 }`
  -> strategy = Push
- `test_pull_takes_precedence_over_push` — both present -> strategy = Pull

### Unit Tests (protocol.rs or mod.rs)

- `test_collect_push_diagnostics_timeout_returns_empty` — no notifications within
  timeout -> returns empty vec
- `test_collect_push_diagnostics_collects_matching_uri` — notification with matching
  URI is collected
- `test_collect_push_diagnostics_ignores_other_files` — notification for different
  URI is skipped

### Integration-style Tests (validation.rs tests)

- `test_push_validation_no_errors` — pre and post both empty -> validation passes
- `test_push_validation_introduced_error` — post has new error -> validation fails
- `test_push_validation_timeout_skips` — timeout with no response -> skipped

## EXCLUSIONS — Do NOT Modify These

- `navigation.rs` — navigation tools don't use diagnostics
- `detect.rs` — detection unchanged
- `process.rs` — spawn unchanged
- Pull diagnostics path — keep working exactly as before
- `MockLawyer::push_prepare_call_hierarchy_result` etc — existing mocks untouched

## Verification

```bash
# 1. Build
cargo build --all

# 2. All tests pass
cargo test --all

# 3. Push diagnostics path exists in validation
grep -n "DiagnosticsStrategy::Push" crates/pathfinder/src/server/tools/edit/validation.rs
# Expected: match arm with full implementation (not stub)

# 4. collect_diagnostics exists on Lawyer trait
grep -n "collect_diagnostics" crates/pathfinder-lsp/src/lawyer.rs
# Expected: trait method definition

# 5. Push collection in dispatcher
grep -n "collect_push_diagnostics" crates/pathfinder-lsp/src/client/protocol.rs
# Expected: method implementation

# 6. Manual test with Go project:
#    - Open a Go workspace
#    - Call validate_only on a Go file
#    - Expected: validation.result != "skipped", validation_skipped_reason != "pull_diagnostics_unsupported"
```

## Expected Impact

- `validate_only` works for Go files (gopls push diagnostics)
- `validate_only` works for TypeScript files (tsserver push diagnostics)
- Edit tools (`replace_body`, `replace_full`, etc.) get LSP validation for Go/TS
- Rust validation unchanged (still uses pull diagnostics)
- Validation latency increases slightly for Go/TS (5s collection window per snapshot)
  but this is acceptable for a validation step
