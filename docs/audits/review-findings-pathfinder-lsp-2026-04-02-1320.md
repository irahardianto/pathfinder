# Code Audit: pathfinder-lsp
Date: 2026-04-02

## Summary
- **Files reviewed:** 12
- **Issues found:** 4 (1 critical, 2 major, 1 minor)
- **Test coverage:** Passed (cargo test -p pathfinder-lsp passed)
- **Dimensions activated:** C, D, E. Skipped A (No frontend), B (No database), F (No mobile).

## Critical Issues
Issues that must be fixed before deployment.

- [x] **Unbounded Memory Allocation DOS (Reliability/Security)** — `crates/pathfinder-lsp/src/client/transport.rs:67`
  The `read_message` function uses the parsed `Content-Length` header directly to allocate a buffer: `let mut body = vec![0u8; length];`. If a malicious or misbehaving LSP server (or simply a corrupted stream) sends a huge `Content-Length` (e.g., 4GB), the `pathfinder` process will attempt to allocate this memory and likely panic (OOM). A maximum message size limit (e.g., 10-50MB) must be enforced.

## Major Issues
Issues that should be fixed in the near term.

- [x] **Deadlock Risk due to Unbounded Stdin Lock (Reliability)** — `crates/pathfinder-lsp/src/client/process.rs:155`
  The `send` method takes `process.stdin.lock().await` and writes directly. Timeout handling in `LspClient`'s methods (like `goto_definition`) only wrap the `tokio::sync::oneshot` receiver *after* the request is sent. If the LSP stops reading from its stdin and the OS buffer fills up, `write_all` block indefinitely. This not only hangs the current request, but blocks the `stdin` mutex forever, queuing up/deadlocking all subsequent requests to this LSP. The `send` operation itself needs to be wrapped in a timeout.
- [x] **Missing Operation Logs (Observability)** — `crates/pathfinder-lsp/src/client/mod.rs`
  According to the `Logging and Observability Mandate`, operations calling external services must log start and failure, not just completion. Currently, methods like `goto_definition`, `call_hierarchy_*` only log on request success (e.g. `textDocument/definition complete`). They do not log when the request starts, and silently propagate errors without logging context. Additionally, `did_open`, `did_change`, and `did_close` have no logging at all.

## Minor Issues
Style, naming, or minor improvements.

- [x] **State Machine Retry Logic Inefficiency** — `crates/pathfinder-lsp/src/client/mod.rs:164`
  In `LspClient::ensure_process`, if `start_process(..., 0)` fails immediately (e.g., the LSP binary `gopls` is not installed), the method returns `Err(Io(...))` without updating the `processes` map. Instead of transitioning to `ProcessEntry::Unavailable` on a hard start failure (e.g., `NotFound`), the client attempts the same doomed OS-level `spawn` over and over again for every relevant tool request.

## Verification Results
- Lint: PASS (`cargo clippy -p pathfinder-lsp -- -D warnings` ran cleanly)
- Tests: PASS (51 passed, 0 failed)
- Build: PASS

## Dimensions Covered
| Dimension | Status | Files / Queries Examined |
|---|---|---|
| A. Integration Contracts | ⏭ Skipped | No frontend in this project |
| B. Database & Schema | ⏭ Skipped | No relational database used in `pathfinder-lsp` |
| C. Configuration & Environment | ✅ Checked | Verified no raw secrets, timeout config is hardcoded (30s) or taken via args safely |
| D. Dependency Health | ✅ Checked | Checked `cargo check` & `clippy` for deprecated patterns / issues |
| E. Test Coverage Gaps | ✅ Checked | Mock and no_op implementations have unit test coverage, Client parsing functions are unit tested |
| F. Mobile ↔ Backend | ⏭ Skipped | No mobile app in this project |
