# Research Log: Epic 5 — LSP Validation Pipeline for Edit Tools

## Date
2026-03-09

## Context
All 16 MCP tools are implemented with 232+ tests. Edit tools (replace_body, replace_full, insert_before, insert_after, delete_symbol, validate_only) currently hardcode `validation_skipped: true` with `reason: "no_lsp"`. The PRD (§3.4, steps 6-9) specifies a full validation pipeline using LSP Pull Diagnostics before disk write.

## Current State

### What Exists
- `Lawyer` trait with `goto_definition` only
- `LspClient` production implementation with:
  - Lazy process spawning (PRD §6.1)
  - Exponential backoff crash recovery (PRD §6.3)
  - Idle timeout auto-termination (PRD §6.2)
  - Capability detection for `diagnostic_provider`, `formatting_provider`, `call_hierarchy_provider` (PRD §6.4)
  - JSON-RPC transport via `RequestDispatcher`
  - `make_notification()` method (ready for didOpen/didChange)
- `MockLawyer` test double with fixture pattern
- `NoOpLawyer` for graceful degradation
- `DetectedCapabilities` struct already parsed from initialize response

### What's Missing
1. **Lawyer trait methods:** `did_open`, `did_change`, `pull_diagnostics`, `range_formatting`
2. **LspClient implementations** of these methods
3. **Multiset diagnostic diffing** (PRD §5.10)
4. **Wiring** in all 6 edit tool handlers

## Key Patterns Discovered

### Notification vs Request
- `didOpen`/`didChange`/`didClose` are **notifications** (no response expected)
- `textDocument/diagnostic` and `textDocument/rangeFormatting` are **requests** (await response)
- `RequestDispatcher.make_notification()` exists for notifications
- `LspClient.request()` exists for requests with timeout

### Capability Guards
The `DetectedCapabilities` struct already tracks:
- `diagnostic_provider: bool` → gates Pull Diagnostics
- `formatting_provider: bool` → gates range formatting
Both are parsed from the LSP initialize response.

### Diagnostic Diffing (PRD §5.10)
Hash key = `[severity, code, message, source_file]` (exclude line/column since edits shift them).
Use `HashMap<DiagnosticHash, count>` for multiset comparison:
- `introduced = max(0, post_count - pre_count)`
- `resolved = max(0, pre_count - post_count)`

### Process `send` function
The existing `send()` function works for both requests and notifications — it just writes JSON-RPC to stdin. Notifications use `make_notification()` (no id), requests use `make_request()` (with id + await response).

## Existing Test Patterns
- Unit tests use `MockLawyer` for controllable results
- `MockSurgeon` uses `Arc<Mutex<Vec<Result<...>>>>` queues for sequenced results
- Edit tool tests use `tempdir()` fixtures with real files on disk
- All tests use `PathfinderServer::with_engines()` or `with_all_engines()` for DI

## Technologies
- LSP 3.17 Pull Diagnostics: `textDocument/diagnostic` request/response
- LSP `textDocument/didOpen`, `textDocument/didChange` notifications
- LSP `textDocument/rangeFormatting` for post-edit formatting
- Multiset diffing algorithm for diagnostic comparison

## Risks and Mitigations
- **Risk:** LSP may not support Pull Diagnostics → **Mitigation:** Already handled — capability detection sets `validation_skipped: true`
- **Risk:** Diagnostic timeout → **Mitigation:** Use existing `request()` with configurable timeout
- **Risk:** Large diagnostic sets → **Mitigation:** Hash-based diffing is O(n), memory-efficient
