# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.12.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.11.6...pathfinder-mcp-lsp-v0.12.0) - 2026-06-13

### Added

- *(lsp)* add LspError::ServerError variant to preserve JSON-RPC error codes
- *(lsp)* implement language-aware grace periods, server identity, and capability registration telemetry

### Fixed

- *(lsp)* improve type definitions, trait coverage, and documentation
- *(lsp)* harden language detection with multi-marker consistency and graceful fallbacks
- *(lsp)* add cancel notification on timeout and harden lifecycle layer
- *(lsp)* harden process management, transport, and background task layer
- *(lsp)* harden response parsers with bounded preview and jdt:// URI handling
- *(lsp)* resolve concurrent jdtls process conflicts, enhance java detection markers, and add tests
- *(references)* prevent false-confidence zero references during warmup

### Other

- *(deps)* bump criterion from 0.5.1 to 0.8.2
- *(lsp)* pre-allocate call sites vector in response parser

## [0.11.6](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.11.5...pathfinder-mcp-lsp-v0.11.6) - 2026-06-10

### Other

- updated the following local packages: pathfinder-mcp-common

## [0.11.5](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.11.4...pathfinder-mcp-lsp-v0.11.5) - 2026-06-10

### Added

- *(server)* remediate mcp ergonomics and add get_semantic_path

## [0.11.4](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.11.3...pathfinder-mcp-lsp-v0.11.4) - 2026-06-09

### Fixed

- resolve clippy warnings for doc markdown and case-sensitive extension comparison
- *(lsp)* prevent duplicate didOpen and multiple didClose protocol violations
- *(lsp)* introduce client startup grace period for dynamic capability registration

### Other

- run cargo fmt --all
- *(common,lsp,server)* BATCH-05 add pathfinder-common types, plugin, and server types coverage
- *(lsp-client)* fix cargo fmt violations in lifecycle.rs tests
- *(lsp-client)* fix bugs and add coverage for idle timeout, error paths, and DI chain

## [0.11.3](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.11.2...pathfinder-mcp-lsp-v0.11.3) - 2026-06-09

### Added

- *(lsp)* implement D-1 through D-5 deferred items and fix audit findings

### Other

- *(lsp)* add MockProcessSpawner succeeding mode and comprehensive FakeTransport coverage
- *(lsp)* document D-5 error response delay asymmetry

## [0.11.2](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.11.1...pathfinder-mcp-lsp-v0.11.2) - 2026-06-08

### Fixed

- comprehensive bug and edge case remediation for pathfinder-mcp-lsp

## [0.11.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.11.0...pathfinder-mcp-lsp-v0.11.1) - 2026-06-08

### Fixed

- *(lsp)* implement DELIVERABLE D - Python LSP detection fix and audit resolutions
- complete tool rename from analyze_impact to find_callers_callees

## [0.11.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.10.1...pathfinder-mcp-lsp-v0.11.0) - 2026-06-06

### Fixed

- *(agent-ux)* address LSP reliability and usability issues reported in 24-commit assessment

### Other

- *(deps)* bump serde_json, ignore, rmcp, and which
- *(lsp)* [**breaking**] replace blocking file reads with async tokio::fs and add benchmarks

## [0.10.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.10.0...pathfinder-mcp-lsp-v0.10.1) - 2026-06-03

### Other

- updated the following local packages: pathfinder-mcp-common

## [0.10.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.9.0...pathfinder-mcp-lsp-v0.10.0) - 2026-06-03

### Fixed

- *(lsp)* address 27 audit findings across LSP client lifecycle
- *(lsp)* eliminate cross-language dispatch interference in polyglot workspaces
- *(lsp)* address phase 3 audit findings for cross-language dispatch isolation
- *(pathfinder-lsp)* fix cross-language init race and panic recovery
- Cross-language LSP dispatch isolation (LSP-INIT-002 Phase 1)

### Other

- *(lsp)* add cross-language dispatch observability (LSP-INIT-002 phase 4)

## [0.9.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.8.0...pathfinder-mcp-lsp-v0.9.0) - 2026-06-02

### Fixed

- *(clippy)* resolve all 38 lint errors blocking CI
- *(navigation)* address Phase 5A audit findings
- *(lsp)* address implementation and test gaps from Phase 3 review
- *(git)* harden diff_name_only against argument injection

### Other

- apply cargo fmt across entire workspace
- *(infra)* enhance MockScout and MockLawyer for multi-call scenarios
- *(lsp-client)* split monolithic mod.rs into focused sub-modules
- *(TCV-001)* cover LSP client residual gaps (Phase 4D)
- *(TCV-001)* cover reader_supervisor_task and init lock serialization
- *(TCV-001)* build FakeTransport and cover LSP client operations (Phase 3B-3E)
- *(lsp)* extract LspTransport trait for testable I/O boundary (Phase 3A)

## [0.8.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.7.1...pathfinder-mcp-lsp-v0.8.0) - 2026-06-01

### Added

- *(pathfinder)* complete agent-experience-remediation patch (26 specs, 5 epics)
- *(agent-experience)* add ActionableGuidance, is_definition enrichment, and warm_start_complete
- *(lsp)* PATCH-004 LSP warm start tracking and improved diagnostics

### Fixed

- *(lsp-hardening)* resolve gaps and bugs from LSP hardening audit
- *(lsp-hardening)* [**breaking**] address critical bugs and gaps from LSP hardening audit
- *(lsp)* populate indexing_progress_percent from workDoneProgress events
- *(pathfinder-lsp)* warm_start_complete flag not set when zero languages

### Other

- update dependencies and improve code formatting
- apply cargo fmt formatting across workspace
- *(lsp)* add warm_start_complete flag tests and fix doctest reporting

## [0.7.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.7.0...pathfinder-mcp-lsp-v0.7.1) - 2026-05-10

### Added

- *(lsp)* add goto_implementation and improve references output

## [0.7.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.6.2...pathfinder-mcp-lsp-v0.7.0) - 2026-05-10

### Added

- *(lsp)* add textDocument/references support to Lawyer trait (R5)
- *(java)* add complete Java support with Tree-sitter and jdtls LSP integration

## [0.6.2](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.6.1...pathfinder-mcp-lsp-v0.6.2) - 2026-05-08

### Fixed

- *(lint)* resolve clippy warnings in error.rs and helpers.rs

### Other

- *(lsp)* add transport edge case tests improving coverage to 89%
- *(lsp)* add client module tests for validation status and in-flight guard
- *(lsp)* add comprehensive error.rs tests achieving 100% coverage

## [0.6.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.6.0...pathfinder-mcp-lsp-v0.6.1) - 2026-05-07

### Other

- apply rustfmt formatting
- *(mcp)* standardize tool outputs and enhance text ergonomics

## [0.6.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.5.2...pathfinder-mcp-lsp-v0.6.0) - 2026-05-07

### Other

- *(lsp)* Drop diagnostics, formatting, and validation types
- *(lsp)* Remove shadow editor and edit logic from LSP client

## [0.5.2](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.5.1...pathfinder-mcp-lsp-v0.5.2) - 2026-05-04

### Fixed

- *(lsp)* prevent false-positive coexistence mode in parallel test environments

### Other

- *(fmt)* apply rustfmt to closure in detect_concurrent_lsp

## [0.5.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.5.0...pathfinder-mcp-lsp-v0.5.1) - 2026-05-04

### Fixed

- *(lsp)* prevent zombie processes and refine coexistence detection

## [0.5.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.4.0...pathfinder-mcp-lsp-v0.5.0) - 2026-05-04

### Added

- *(lsp)* complete phase 0-5 of pathfinder LSP roadmap
- *(lsp)* DS-1 DocumentGuard RAII lifecycle for navigation tools
- *(lsp)* pre-flight manifest validation and python venv detection
- *(mcp)* surface lsp_readiness and validation_confidence signals (IW-2)
- *(lsp)* core lifecycle, diagnostics reliability, and backoff improvements

### Fixed

- *(lsp)* resolve all CI Clippy -D warnings and fmt violations
- *(lsp)* apply rustfmt style to assert_eq! in plugin tests

### Other

- *(lsp)* resolve flakiness in process lifecycle tests

## [0.4.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.3.0...pathfinder-mcp-lsp-v0.4.0) - 2026-05-02

### Fixed

- *(clippy)* resolve all clippy warnings in LSP health implementation
- *(lsp)* harden probe cache with TTL and auto-gitignore for isolated caches

### Other

- *(lsp)* apply rustfmt formatting

## [0.3.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.2.7...pathfinder-mcp-lsp-v0.3.0) - 2026-05-01

### Added

- Complete PATCH-001 through PATCH-011 implementation gaps
- *(lsp)* Python LSP E2E verification test + fix pyright detection (PATCH-009)
- *(lsp)* surface install guidance for missing LSPs (PATCH-008)
- *(lsp)* cross-language LSP reliability improvements

### Other

- *(lsp)* add branch coverage for LSP response parsers
- *(lsp)* add process shutdown coverage

## [0.2.7](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.2.6...pathfinder-mcp-lsp-v0.2.7) - 2026-05-01

### Other

- *(lint)* normalize lint suppressions for consistency and clarity
- *(cleanup)* remove dead code and stale allow(dead_code)

## [0.2.6](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.2.5...pathfinder-mcp-lsp-v0.2.6) - 2026-05-01

### Other

- updated the following local packages: pathfinder-mcp-common

## [0.2.5](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.2.4...pathfinder-mcp-lsp-v0.2.5) - 2026-04-29

### Fixed

- *(lsp)* improve reliability of LSP integration by fixing concurrent instances and document lifecycle gaps

## [0.2.4](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.2.3...pathfinder-mcp-lsp-v0.2.4) - 2026-04-29

### Fixed

- *(api)* restore crate-internal visibility and document side-effect pattern

### Other

- improve code formatting and remove unnecessary lint suppressions
- *(code-quality)* fix DeepSource and Clippy warnings
- fix 28+ clippy warnings and improve code quality

## [0.2.3](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.2.2...pathfinder-mcp-lsp-v0.2.3) - 2026-04-29

### Other

- *(core)* implement 2026-04-29 patch batch (7 patches)

## [0.2.2](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.2.1...pathfinder-mcp-lsp-v0.2.2) - 2026-04-29

### Fixed

- *(coverage)* exclude test-mock-lsp from coverage analysis and fix DeepSource issues

### Other

- *(coverage)* add targeted unit tests to eliminate TCV-001 gaps

## [0.2.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.2.0...pathfinder-mcp-lsp-v0.2.1) - 2026-04-28

### Fixed

- *(lsp)* add required-features to prevent integration test binary from running in unit-test mode
- *(lsp)* gate `mod common` behind `#[cfg(feature = "integration")]`

### Other

- *(integration)* establish mock LSP test harness and fix SLSA provenance

## [0.2.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.1.4...pathfinder-mcp-lsp-v0.2.0) - 2026-04-28

### Added

- *(lsp)* track indexing complete and uptime status

## [0.1.4](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.1.3...pathfinder-mcp-lsp-v0.1.4) - 2026-04-27

### Other

- updated the following local packages: pathfinder-mcp-common

## [0.1.3](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.1.2...pathfinder-mcp-lsp-v0.1.3) - 2026-04-26

### Other

- replace empty new() calls with default()

## [0.1.2](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.1.1...pathfinder-mcp-lsp-v0.1.2) - 2026-04-26

### Added

- *(lsp)* implement graceful shutdown for LSP processes

### Fixed

- resolve all clippy warnings
- address DeepSource findings (6/11 issues)
- *(release)* stabilize SLSA provenance and resolve macOS target compilation
- *(lsp)* gate prctl on linux only, not unix

### Other

- apply rustfmt formatting across workspace and update coverage report
- apply cargo fmt formatting across all crates
- *(lsp)* add comprehensive SAFETY comment for prctl unsafe code
- complete TCV-001 coverage remediation (WP-1 + WP-6)
- Fix DeepSource syntax errors and apply cargo clippy
- Fix DeepSource syntax errors and apply cargo fmt
- add documentation for public items
- *(lsp)* fix cargo fmt formatting
- defer fallback computation with unwrap_or_else

## [0.1.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-lsp-v0.1.0...pathfinder-mcp-lsp-v0.1.1) - 2026-04-25

### Other

- updated the following local packages: pathfinder-mcp-common

## [0.1.0] - 2026-04-24
### Added
- Initial release