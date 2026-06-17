# Code Audit: pathfinder, pathfinder-common, pathfinder-lsp

Date: 2026-06-16

## Summary

- **Files reviewed:** 47 source files across 3 crates
- **Issues found:** 28 (0 critical, 10 major, 18 minor)
- **Test coverage:** 1006 unit tests, all passing
- **Dimensions activated:** C (Configuration & Environment), D (Dependency Health), E (Test Coverage Gaps)
- **Dimensions skipped:** A (no frontend), B (no database), F (no mobile app)

---

## Automated Verification Results

- **Clippy:** PASS (zero warnings — pedantic, `unwrap_used = deny`, `unsafe_code = deny`)
- **Format:** PASS (`cargo fmt --check`)
- **Tests:** PASS — 1006 unit tests (pathfinder-mcp: 360, pathfinder-mcp-common: 102, pathfinder-mcp-lsp: 544)
- **cargo deny:** PASS (advisories ok, bans ok, licenses ok, sources ok)
- **Build:** PASS

---

## Critical Issues

None found.

---

## Major Issues

### pathfinder crate

- [ ] **Code duplication in grep fallbacks** — [impact.rs:618-892](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder/src/server/tools/navigation/impact.rs#L618-L892)
  `find_callers_callees_impl` is ~780 lines. Four match arms (NoLsp, Timeout, LspError, WarmupEmpty) each call `grep_reference_fallback` + `grep_outgoing_fallback` with near-identical patterns. Extract a shared `apply_grep_fallbacks()` helper.

- [ ] **Oversized function** — [overview.rs:20-337](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder/src/server/tools/navigation/overview.rs#L20-L337)
  `symbol_overview_impl` is ~320 lines with `#[allow(clippy::too_many_lines)]`. Degraded-reason priority logic (L232-276) and text rendering (L292-336) are independently extractable.

- [ ] **Dead code behind `if false`** — [impact.rs:956-1047](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder/src/server/tools/navigation/impact.rs#L956-L1047)
  ~90 lines of unreachable code gated by literal `false` (Spec 4.2). Remove or gate behind a feature flag.

### pathfinder-common crate

- [ ] **hint() method exceeds 50 lines** — [error.rs:118-233](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-common/src/error.rs#L118-L233)
  115-line match arm. Extract hint generation per variant into dedicated private methods.

### pathfinder-lsp crate

- [ ] **`start_process` exceeds 250 lines** — [lifecycle.rs:435-686](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src/client/lifecycle.rs#L435-L686)
  Handles backoff, coexistence detection, spawning, supervisor wiring, and state insertion. Extract into `apply_backoff()`, `wire_supervisors()`, `create_language_state()`.

- [ ] **`detect_concurrent_lsp` suppresses `too_many_lines`** — [lifecycle.rs:688](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src/client/lifecycle.rs#L688)
  Extract per-language detection (Rust pid-file, Go ps-based, Java jdtls) into separate functions.

- [ ] **`validation_status_from_parts` has 14 parameters** — [mod.rs:186-295](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src/client/mod.rs#L186-L295)
  Suppresses `clippy::too_many_arguments` and `clippy::fn_params_excessive_bools`. Introduce a `ValidationStatusInput` struct.

- [ ] **`unreachable!()` in production match arm** — [mod.rs:244](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src/client/mod.rs#L244)
  `DiagnosticsStrategy::None => unreachable!()` panics in production if hit. Use `debug_assert!` or refactor to eliminate the nested match.

- [ ] **`read_preview_line` reads byte-by-byte** — [response_parsers.rs:26-68](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src/client/response_parsers.rs#L26-L68)
  Creates excessive syscall overhead on large files. Use `AsyncBufReadExt::read_line()` with `enumerate().skip(line_index).next()`.

- [ ] **`spawn_lsp_child` suppresses `too_many_lines`** — [process.rs:473-474](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src/client/process.rs#L473-L474)
  Extract per-language environment setup (Rust target isolation, Go cache, TS tmpdir, Python pycache, Java jdtls) into helper functions.

---

## Minor Issues

### pathfinder crate

- [ ] **Inconsistent `as usize` casts** — [impact.rs:356,364](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder/src/server/tools/navigation/impact.rs#L356)
  Uses `as usize` while other locations use `usize::try_from().unwrap_or()`. Standardize on `usize::from()`.

- [ ] **Avoidable `content.clone()`** — [source_file.rs:268](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder/src/server/tools/source_file.rs#L268)
  Clone can be avoided by building format string before constructing metadata.

- [ ] **Avoidable `final_content.clone()`** — [source_file.rs:326](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder/src/server/tools/source_file.rs#L326)
  Same pattern — restructure to build text from reference, then move.

- [ ] **`debug_assert!(false)` in fallback** — [overview.rs:105,176](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder/src/server/tools/navigation/overview.rs#L105)
  Deserialization bugs invisible in production. Consider `tracing::error!` instead.

- [ ] **Repeated `last_symbol_name()` calls** — [impact.rs (8 locations)](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder/src/server/tools/navigation/impact.rs#L575)
  Same value computed 8 times. Compute once at function top.

- [ ] **Hardcoded client version** — [process.rs:779](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src/client/process.rs#L779)
  `"version": "0.1.0"` in `initialize` request. Should use `env!("CARGO_PKG_VERSION")`.

### pathfinder-common crate

- [ ] **No config value validation** — [config.rs:57-81](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-common/src/config.rs#L57-L81)
  `PathfinderConfig::load()` deserializes JSON with no semantic validation. Invalid values like `log_level: "banana"` or `max_results: 0` pass silently. Add `validate()`.

- [ ] **`trust_level` should be enum** — [types.rs:540](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-common/src/types.rs#L540)
  Free-form `String` where only 4 values are valid. Create `TrustLevel` enum.

- [ ] **`fallback_tool` should be enum** — [types.rs:533](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-common/src/types.rs#L533)
  Same issue — finite known set of values should be typed.

- [ ] **Allocation in sandbox hot path** — [sandbox.rs:256](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-common/src/sandbox.rs#L256)
  `format!("{allowed}/")` allocates on every `check()` call. Pre-compute as constants.

- [ ] **Blocking `exists()` in async fn** — [config.rs:60](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-common/src/config.rs#L60)
  `config_path.exists()` is blocking syscall in async. Use `tokio::fs::try_exists()`. Low priority (startup-only).

- [ ] **`fmt::write` Result discarded** — [types.rs:207-209](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-common/src/types.rs#L207-L209)
  String's `fmt::Write` is infallible but discarding Results is a code smell. Add SAFETY comment.

- [ ] **clone() calls missing CLONE: comments** — config.rs:69, sandbox.rs:144, types.rs:392
  All justified clones but project rules require `// CLONE:` comments.

- [ ] **Config load logs lack workspace context** — [config.rs:61,79](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-common/src/config.rs#L61)
  Add `workspace = %workspace_root.display()` to both tracing calls.

### pathfinder-lsp crate

- [ ] **`read_preview_line` silent on errors** — [response_parsers.rs:28-29](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src/client/response_parsers.rs#L28)
  File open failure returns empty string with no logging. Add `tracing::debug!`.

- [ ] **`line as usize` cast** — [response_parsers.rs:380](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src/client/response_parsers.rs#L380)
  Use `usize::from(line).saturating_sub(1)` for consistency with clippy pedantic.

- [ ] **Duplicate `LspLanguageStatus` construction** — [mod.rs:237-293](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src/client/mod.rs#L237-L293)
  Two near-identical struct literals. Build common fields once, set differing fields per branch.

- [ ] **`pub(crate)` fields on `LspClient`** — [mod.rs:336-348](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src/client/mod.rs#L336-L348)
  All internal fields exposed crate-wide. Consider `#[cfg(test)]` methods instead of raw field access.

---

## Positive Findings

### Security
- Zero `unwrap()` in production code. `unwrap_used = "deny"` enforced at workspace level.
- Zero `unsafe` blocks except one justified `pre_exec` for `prctl(PR_SET_PDEATHSIG)` with proper SAFETY comment.
- Sandbox is solid: hardcoded deny patterns for `.git/*`, `.pem`, `.key`, `.env`, path traversal prevention.
- Git argument injection prevention validates targets don't start with `-`.
- `unsafe_code = "deny"` at workspace level.

### Reliability
- All external calls have timeouts (init: 120s configurable, requests: 10s, shutdown: 2s, send lock: 3s).
- Multi-layered zombie prevention: `prctl`, `kill_on_drop`, `GroupChild`, supervisor reaping.
- Exponential backoff crash recovery (1s-60s) with supervisor task.
- Graceful degradation: LSP unavailability falls back to grep with `degraded/degraded_reason` signaling.
- jdtls data directory isolation with advisory file locking for concurrent instances.

### Testability
- `Lawyer` trait + `MockLawyer` + `NoOpLawyer` — exemplary I/O isolation.
- `Scout` trait + `MockScout`, `Surgeon` trait + `MockSurgeon` — full DI.
- `LspTransport` trait + `FakeTransport` — process management testable without real LSP.
- `GitRunner` trait + `FakeGitRunner` — git operations testable without git.
- `ProcessSpawner` trait — process spawning testable without real child processes.
- 1006 unit tests across three crates.

### Observability
- All tool handlers log start/success/failure with duration.
- Structured logging via `tracing` with JSON output.
- LSP operations log language, process PID, timeouts, crash recovery.

### Code Quality
- `thiserror` in library crates, `anyhow` in application crate — correct pattern.
- Strong plugin system (`LanguagePlugin` trait) for per-language LSP configuration.
- Clean workspace Cargo.toml with shared lint configuration.
- Benchmarks for performance-critical paths.

---

## Cross-Boundary Review

### Dimension C: Configuration & Environment ✅

| Check | Status |
|---|---|
| No hardcoded secrets/tokens/URLs | ✅ Clean — grep for password/secret/token/apikey found zero results |
| No `std::env::var` in audited crates | ✅ Config loaded from JSON file, not env vars |
| Startup validation | ⚠️ Config values not semantically validated (see Minor Issue) |
| Secrets not logged | ✅ No secret handling in these crates |

### Dimension D: Dependency Health ✅

| Check | Status |
|---|---|
| No circular dependencies | ✅ Clean: common → (no deps), lsp → common, pathfinder → both |
| Cross-module imports use public API | ✅ All inter-crate imports via `pub` API |
| No unused top-level dependencies | ✅ All deps used |
| cargo deny (advisories, bans, licenses, sources) | ✅ All PASS |

### Dimension E: Test Coverage Gaps ⚠️

| Check | Status |
|---|---|
| Unit tests for all tool handlers | ✅ All 7 tool handlers have tests |
| Integration tests for storage/LSP adapters | ✅ Feature-gated `lsp_client_integration.rs` + `git_integration.rs` |
| Error path coverage | ✅ Tests for sandbox denials, invalid paths, scout errors, LSP timeouts |
| Unhappy path tests | ✅ Crash recovery, concurrent spawn, idle timeout tested |
| Missing coverage | ⚠️ `health` tool has inline tests but no integration test exercising real LSP probe/restart |
