# Audit Remediation 0026

## Findings
- **F1 (range_formatting silent discard):** Will add `tracing::debug!` to log `edits.len()` if the response is an array.
- **F2 (_restart_count unused):** Will rename to `restart_count` and log it during `idle_timeout_task`.
- **F3 (touch lock contention):** Recommendation confirms no action needed. Will leave `RwLock` as-is.
- **F4 (last_used redundancy):** Will remove `LanguageState.last_used` and exclusively use `ManagedProcess.last_used`.

## Implementation Points
- [x] `mod.rs:48` -> rename `_restart_count` to `restart_count`.
- [x] `mod.rs:50` -> remove `last_used` from `LanguageState` and its initialization in `start_process`.
- [x] `mod.rs:202` -> `touch` will only update `state.process.last_used`.
- [x] `mod.rs:530` -> `range_formatting` will parse `response.as_array()` and emit a debug log with the length before returning `Ok(None)`.
- [x] `mod.rs:748` -> `idle_timeout_task` checks `state.process.last_used` instead of `state.last_used`.
- [x] `mod.rs:761` -> `idle_timeout_task` adds `restarts = state.restart_count` to the `tracing::info!` log.
- [x] `replace_full_impl` and other edit tools: Added runtime durations (`resolve_ms`, `validate_ms`, `flush_ms`) and `pull_workspace_diagnostics`.
- [x] Added `review-findings-pathfinder-all-2026-03-09-0026.md`
1. `mod.rs:48` -> rename `_restart_count` to `restart_count`.
2. `mod.rs:50` -> remove `last_used` from `LanguageState` and its initialization in `start_process` (`mod.rs:191`).
3. `mod.rs:202` -> `touch` will only update `state.process.last_used`.
4. `mod.rs:530` -> `range_formatting` will parse `response.as_array()` and emit a debug log with the length before returning `Ok(None)`.
5. `mod.rs:748` -> `idle_timeout_task` checks `state.process.last_used` instead of `state.last_used`.
