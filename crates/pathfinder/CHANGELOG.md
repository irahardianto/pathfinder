# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.0](https://github.com/irahardianto/pathfinder/compare/v0.3.1...v0.4.0) - 2026-05-01

### Added

- Complete PATCH-001 through PATCH-011 implementation gaps
- *(lsp)* enrich lsp_health with degraded tools and validation latency (PATCH-010)
- *(lsp)* surface install guidance for missing LSPs (PATCH-008)
- *(lsp)* cross-language LSP reliability improvements
- *(lsp)* Python LSP E2E verification test + fix pyright detection (PATCH-009)

### Other

- *(coverage)* add comprehensive tests for lsp_error_to_skip_reason
- add test coverage for apply_filter_mode in search tool
- 🛡️ Shield: Fix panic in test_details_serialization_extra
- 🛡️ Shield: Fix panic in test_details_serialization_extra
- 🛡️ Shield: Increased coverage for error mapping and serialization in pathfinder-common
- 🛡️ Shield: Increased coverage for error mapping and serialization in pathfinder-common
- *(lsp)* add branch coverage for LSP response parsers
- *(lsp)* add process shutdown coverage
- *(search)* add missing coverage for mutex poisoning, utf8 handling, context lines

## [0.3.1](https://github.com/irahardianto/pathfinder/compare/v0.3.0...v0.3.1) - 2026-05-01

### Other

- *(lint)* normalize lint suppressions for consistency and clarity
- *(lint)* add justifying comments for clippy::too_many_lines
- *(cleanup)* remove dead code and stale allow(dead_code)

## [0.3.0](https://github.com/irahardianto/pathfinder/compare/v0.2.2...v0.3.0) - 2026-05-01

### Fixed

- *(navigation)* improve grep fallbacks with visibility modifiers and warmup support
- *(validation)* return uncertain status when both diagnostic snapshots empty
- *(edit)* add blank line before doc comments in insert_after
- *(file-ops)* standardize response envelopes (PATCH-006 + PATCH-007)
- *(search)* populate total_matches in build_file_groups + add regression test (PATCH-004)
- *(search)* schema and serialization fixes for group_by_file + known_files (PATCH-004 + PATCH-005)
- *(navigation)* use name_column in LSP calls + add empty-hierarchy probe (PATCH-002 + PATCH-003)
- *(navigation)* add name_column for correct LSP cursor positioning (PATCH-001)

### Other

- Apply cargo fmt formatting fixes
- Complete PATCH-001 through PATCH-015 implementation
- Fix argument injection in ripgrep execution
- *(edit)* apply cargo fmt to insert_after doc comment detection
- *(validation)* update test to expect uncertain status for empty snapshots
- *(lsp)* Verify LspClient shutdown broadcasts signal
- *(lsp)* Verify LspClient shutdown broadcasts signal

## [0.2.2](https://github.com/irahardianto/pathfinder/compare/v0.2.1...v0.2.2) - 2026-04-29

### Fixed

- *(lsp)* improve reliability of LSP integration by fixing concurrent instances and document lifecycle gaps

### Other

- sync agent directives with updated LSP degraded mode guidance, add badges to README
- update MCP tool descriptions, AGENTS.md, and pathfinder-workflow skill with LSP degraded mode guidance

## [0.2.1](https://github.com/irahardianto/pathfinder/compare/v0.2.0...v0.2.1) - 2026-04-29

### Fixed

- *(api)* restore crate-internal visibility and document side-effect pattern

### Other

- format make_body_range signature with rustfmt
- improve code formatting and remove unnecessary lint suppressions
- *(code-quality)* fix DeepSource and Clippy warnings
- fix 28+ clippy warnings and improve code quality

## [0.2.0](https://github.com/irahardianto/pathfinder/compare/v0.1.9...v0.2.0) - 2026-04-29

### Other

- *(core)* implement 2026-04-29 patch batch (7 patches)
- *(coverage)* add targeted unit tests to eliminate TCV-001 gaps
- *(integration)* establish mock LSP test harness and fix SLSA provenance
- *(cache)* annotate lock-poison arms as structurally untestable in safe Rust

## [0.1.9](https://github.com/irahardianto/pathfinder/compare/v0.1.8...v0.1.9) - 2026-04-29

### Fixed

- *(coverage)* exclude test-mock-lsp from coverage analysis and fix DeepSource issues

### Other

- *(coverage)* add targeted unit tests to eliminate TCV-001 gaps
- *(integration)* establish mock LSP test harness and fix SLSA provenance
- *(cache)* annotate lock-poison arms as structurally untestable in safe Rust

## [0.1.8](https://github.com/irahardianto/pathfinder/compare/v0.1.7...v0.1.8) - 2026-04-28

### Fixed

- *(lsp)* add required-features to prevent integration test binary from running in unit-test mode
- *(lsp)* gate `mod common` behind `#[cfg(feature = "integration")]`

### Other

- *(integration)* establish mock LSP test harness and fix SLSA provenance

## [0.1.7](https://github.com/irahardianto/pathfinder/compare/v0.1.6...v0.1.7) - 2026-04-28

### Added

- *(lsp)* track indexing complete and uptime status

### Fixed

- *(navigation)* probe LSP readiness before declaring zero callers
- *(edit)* signal vacuous validation passes during LSP warmup
- *(search)* bypass filter mode on degraded languages
- *(treesitter)* resolve E0432 by moving dev-dependency import to test module

### Other

- replace hardcoded temporary paths with tempfile::tempdir

## [0.1.6](https://github.com/irahardianto/pathfinder/compare/v0.1.5...v0.1.6) - 2026-04-27

### Fixed

- *(treesitter)* surface missing-file errors as INVALID_PARAMS not INTERNAL_ERROR

### Other

- *(occ)* migrate to 7-character short version hashes
- *(config)* add coverage for successful load path and default idle timeout
- *(error)* add coverage for InvalidTarget variant

## [0.1.5](https://github.com/irahardianto/pathfinder/compare/v0.1.4...v0.1.5) - 2026-04-27

### Added

- *(treesitter)* implement TypeScript namespace and module visibility support
- *(server)* Implement tooling primitives, insert_into, and OCC short hash logic
- *(treesitter)* Enhance AST parsing and extraction for modules and bodies

### Other

- *(search)* replace RipgrepScout constructor with derived Default
- *(config)* add coverage for successful load path and default idle timeout
- *(error)* add coverage for InvalidTarget variant

## [0.1.4](https://github.com/irahardianto/pathfinder/compare/v0.1.3...v0.1.4) - 2026-04-27

### Other

- *(server)* replace wildcard import with explicit imports
- *(config)* add coverage for successful load path and default idle timeout
- *(error)* add coverage for InvalidTarget variant

## [0.1.3](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-v0.1.2...pathfinder-mcp-v0.1.3) - 2026-04-26

### Other

- replace empty new() calls with default()

## [0.1.2](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-v0.1.1...pathfinder-mcp-v0.1.2) - 2026-04-26

### Added

- *(edit)* block invalid UTF-8 edits to prevent corruption

### Fixed

- *(ci)* fix release-plz regex, add ARM Linux builds, update README install docs
- resolve all clippy warnings
- remove redundant clones in non-test code

### Other

- apply rustfmt formatting across workspace and update coverage report
- apply cargo fmt formatting across all crates
- *(batch)* add comprehensive overlap detection tests
- complete TCV-001 coverage remediation (WP-1 + WP-6)
- Fix DeepSource syntax errors and apply cargo fmt
- add documentation for public items

## [0.1.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-v0.1.0...pathfinder-mcp-v0.1.1) - 2026-04-25

### Other

- updated the following local packages: pathfinder-mcp-common, pathfinder-mcp-lsp, pathfinder-mcp-search, pathfinder-mcp-treesitter

## [0.1.0] - 2026-04-24
### Added
- Initial release