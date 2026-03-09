# Epic 7: Observability & Polish Research Log

## 1. Optional LSP RPC tracing
Investigated current state of the codebase. The `--lsp-trace` CLI flag in `mai.rs` is **already implemented**. It enables `pathfinder_lsp::client::transport=debug` for the logging filter. The `transport.rs` file logs `LSP SEND` and `LSP RECV` with `tracing::debug!`. No code changes required, we just need to test and mark it complete.

## 2. Engine-Level Performance Telemetry
Target: Add `ripgrep_ms` and `tree_sitter_parse_ms` to `search_codebase` operations.
Target: Add `resolve_ms`, `validate_ms`, and `flush_ms` to `edit` operations.

In `search.rs`:
- Line 54: `self.scout.search(&search_params).await` -> track `ripgrep_ms` here.
- Line 61: `self.enrich_matches(&mut enriched_matches).await` -> track `tree_sitter_parse_ms` here (or rename the variable to `tree_sitter_ms`).
- In the final `tracing::info!` log, surface these variables.

In `edit.rs`:
- The telemetry for edits (`resolve_ms`, `validate_ms`, `flush_ms`) should be tracked where these operations occur.
- `resolve_ms` is the time spent doing Tree-sitter scope resolution. This might map to surgeon calls.
- `validate_ms` is the `run_lsp_validation` call in `finalize_edit`.
- `flush_ms` is the TOCTOU check and disk write in `finalize_edit`.

## 3. Cross-File Diagnostic Hardening (LSP 3.17)
Need to implement `workspace/diagnostic` in the `Lawyer` trait (`crates/pathfinder-lsp/src/lawyer.rs`) and `LspClient`.
- Investigate `DiagnosticServerCapabilities` in `LspClient::initialize` to detect `workspaceDiagnostics`.
- Add `pull_workspace_diagnostics` method.
- Update `run_lsp_validation` to check workspace diagnostics if supported.
