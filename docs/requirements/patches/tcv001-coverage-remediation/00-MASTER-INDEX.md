# TCV-001 Coverage Remediation -- Master Index

Date: 2026-06-08
Supersedes: 20260601-tcv001-test-coverage.md
DeepSource Issue: TCV-001 (Lines not covered in tests)
Severity: CRITICAL | Category: COVERAGE | Analyzer: test-coverage
Current State: 916 occurrences | LCV 92.3% | NLCV 100% | BCV 0%

---

## Context

916 uncovered lines despite 92.3% line coverage. Each uncovered line is a separate CRITICAL issue in DeepSource. The old plan (20260601) is stale -- `client/mod.rs` was refactored into `lifecycle.rs` + `process.rs` + `detect.rs` + `capabilities.rs`, and `navigation.rs` was split into `impact.rs` + `health.rs` + `references.rs` + `overview.rs` + `deep_context.rs` + `definition.rs`.

This plan is file-grouped, batched by dependency chain and complexity. Each batch is independently deliverable. Order matters -- later batches may build on test infrastructure from earlier ones.

---

## Batch Execution Order

| Batch | Crate | Files | Est. Uncovered Lines | Complexity | Depends On |
|---|---|---|---|---|---|
| [BATCH-01](./BATCH-01-lsp-client-coverage.md) | pathfinder-lsp | lifecycle.rs, detect.rs, process.rs, capabilities.rs, plugin.rs | ~134 | HIGH | None (biggest offender, start here) |
| [BATCH-01 Design Gaps](./BATCH-01-DESIGN-GAP-REMEDATION.md) | pathfinder-lsp | mod.rs, lifecycle.rs, fake_transport.rs | 0 (testability) | MEDIUM | BATCH-01 (DI improvement for integration testing) |
| [BATCH-02](./BATCH-02-navigation-impact-coverage.md) | pathfinder | impact.rs | ~30 | HIGH | None (largest single navigation file) |
| [BATCH-03](./BATCH-03-treesitter-search-coverage.md) | pathfinder-treesitter, pathfinder-search | repo_map.rs, ripgrep.rs | ~31 | MEDIUM | None |
| [BATCH-04](./BATCH-04-navigation-remaining-coverage.md) | pathfinder | health.rs, references.rs, overview.rs, mod.rs | ~49 | MEDIUM | BATCH-02 (shared test helpers) |
| [BATCH-05](./BATCH-05-common-types-plugin-coverage.md) | pathfinder-common, pathfinder, pathfinder-lsp | types.rs, server/types.rs, plugin.rs | ~10 | LOW | None |

**Total estimated coverage improvement:** ~254 lines from 3 sampled batches. Remaining ~662 lines follow same file distribution pattern. Completing all 5 batches addresses the core structural gaps.

---

## Coverage Metrics -- Current State

| Metric | Key | Value | Threshold | Status |
|---|---|---|---|---|
| Line Coverage (LCV) | Rust | 92.3% | 88% | PASSING |
| New Line Coverage (NLCV) | Rust | 100% | -- | PASSING |
| Branch Coverage (BCV) | Rust | 0.0% | -- | NOT TRACKED |
| Composite Coverage (CPCV) | Rust | 92.3% | -- | PASSING |
| Documentation Coverage | Rust | 100% | -- | PASSING |

---

## Affected Files by Size and Gap Density

| File | Total Lines | Uncovered Lines (est.) | Gap Density | Crate |
|---|---|---|---|---|
| pathfinder-lsp/src/client/lifecycle.rs | 2003 | ~52 | 2.6% | pathfinder-lsp |
| pathfinder-lsp/src/client/detect.rs | 1963 | ~41 | 2.1% | pathfinder-lsp |
| pathfinder-lsp/src/client/process.rs | 1168 | ~40 | 3.4% | pathfinder-lsp |
| pathfinder/src/server/tools/navigation/impact.rs | 3219 | ~30 | 0.9% | pathfinder |
| pathfinder-treesitter/src/repo_map.rs | 1259 | ~19 | 1.5% | pathfinder-treesitter |
| pathfinder/src/server/tools/navigation/health.rs | 2161 | ~18 | 0.8% | pathfinder |
| pathfinder/src/server/tools/navigation/mod.rs | 1634 | ~14 | 0.9% | pathfinder |
| pathfinder-search/src/ripgrep.rs | 1547 | ~12 | 0.8% | pathfinder-search |
| pathfinder/src/server/tools/navigation/references.rs | 2134 | ~9 | 0.4% | pathfinder |
| pathfinder/src/server/tools/navigation/overview.rs | 1039 | ~8 | 0.8% | pathfinder |
| pathfinder-common/src/types.rs | 934 | ~7 | 0.7% | pathfinder-common |
| pathfinder-lsp/src/plugin.rs | 634 | ~2 | 0.3% | pathfinder-lsp |
| pathfinder-lsp/src/client/capabilities.rs | 635 | ~1 | 0.2% | pathfinder-lsp |
| pathfinder/src/server/types.rs | ~1200 | ~1 | <0.1% | pathfinder |

---

## Root Cause Analysis

1. **Infrastructure gap** -- LSP client code (lifecycle, process, detect) requires live child process for most paths. Tests use `fake_transport` and `MockLawyer` but don't cover real process lifecycle paths.

2. **Grep fallback paths** -- Navigation tools (impact, references, overview) have both LSP-powered and grep-fallback code paths. Tests cover LSP paths via mocks, grep-fallback paths are untested.

3. **Error branches** -- Error handling in match arms (process crash, timeout, malformed response) creates coverage gaps. Happy path tested, unhappy paths not.

4. **Feature velocity** -- NLCV=100% proves TDD discipline is solid for new code. Legacy code accumulated gaps before coverage tracking was enabled.

---

## Existing Test Infrastructure

| Crate | Test Files | Inline Tests | Mock Infrastructure |
|---|---|---|---|
| pathfinder-lsp | `tests/lsp_client_integration.rs`, `tests/common/mod.rs` | Yes (in each module) | `fake_transport.rs`, `MockLawyer` |
| pathfinder | `tests/test_read_source_file_integration.rs` | Yes (navigation/test_helpers.rs) | `make_server()`, `MockScout`, `MockLawyer` |
| pathfinder-treesitter | `tests/test_impl.rs`, `tests/test_java.rs`, `tests/test_rust_top_level.rs` | Yes (`test_impl.rs`, `test_symbols.rs`) | In-tree test fixtures |
| pathfinder-search | None (only benches) | No | `mock.rs` |
| pathfinder-common | `tests/git_integration.rs` | Yes | `FakeGitRunner` |

---

## Success Criteria

After all 5 batches complete:

1. TCV-001 occurrence count drops below 200 (from 916)
2. LCV rises to 95%+ (from 92.3%)
3. All new tests pass `cargo test`
4. All code passes `cargo clippy -- -D warnings`
5. No regression in existing tests
6. DeepSource analysis run shows improvement

---

## Delivery Protocol

Each batch is a self-contained deliverable:

1. Read the batch document
2. Implement tests per the strategy section
3. Run `cargo test` and `cargo clippy -- -D warnings`
4. Verify coverage improvement via `cargo llvm-cov` locally
5. Commit with message: `test(batch-N): improve coverage for [scope]`
6. Push and verify DeepSource TCV-001 count reduction
