# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.13.2](https://github.com/irahardianto/pathfinder/compare/v0.13.1...v0.13.2) - 2026-06-09

### Added

- *(lsp)* implement D-1 through D-5 deferred items and fix audit findings

### Other

- *(lsp)* add MockProcessSpawner succeeding mode and comprehensive FakeTransport coverage
- *(lsp)* document D-5 error response delay asymmetry

## [0.13.1](https://github.com/irahardianto/pathfinder/compare/v0.13.0...v0.13.1) - 2026-06-08

### Fixed

- comprehensive bug and edge case remediation for pathfinder-mcp-lsp

## [0.13.0](https://github.com/irahardianto/pathfinder/compare/v0.12.0...v0.13.0) - 2026-06-08

### Added

- Implement batch 4 deliverables for GFB-001 - search coverage, repo map truncation, cache invalidation
- *(GFB-001-G)* add graceful fallback for unsupported languages in read_source_file
- *(navigation)* add outgoing deps to grep fallback (DELIVERABLE F)
- *(navigation)* implement DELIVERABLE B - grep fallback for find_all_references
- *(search)* Add compiled regex caching for improved performance

### Fixed

- *(lsp)* implement DELIVERABLE D - Python LSP detection fix and audit resolutions
- complete tool rename from analyze_impact to find_callers_callees
- *(search)* Remove flaky cache hit counter from regex cache test
- *(treesitter)* Replace unwrap() with expect() for better error messages

## [0.12.0](https://github.com/irahardianto/pathfinder/compare/v0.11.2...v0.12.0) - 2026-06-06

### Fixed

- *(agent-ux)* address LSP reliability and usability issues reported in 24-commit assessment
- *(deps)* bump sha2 0.10 → 0.11 with digest 0.11 migration
- *(deps)* bump tree-sitter 0.25 → 0.26 with parse_with_options migration

### Other

- *(deps)* bump serde_json, ignore, rmcp, and which
- *(treesitter)* offload CPU-intensive parsing to blocking pool and optimize concurrency
- *(lsp)* [**breaking**] replace blocking file reads with async tokio::fs and add benchmarks

## [0.11.2](https://github.com/irahardianto/pathfinder/compare/v0.11.1...v0.11.2) - 2026-06-03

### Fixed

- *(pathfinder)* address edge cases and improve efficiency in find_symbol
- *(common,search)* address audit findings from perf session
- *(treesitter)* correct mtime race, unblock runtime, and extend Vue preloaded cache

### Other

- apply rustfmt formatting across workspace
- *(pathfinder)* add criterion benchmark suite for find_symbol
- *(pathfinder)* optimize find_symbol with parallel execution and zero-alloc hot paths
- *(common)* reformat sandbox deny pattern matching for readability
- *(common)* optimize hot-path functions with zero-alloc formatting, pre-allocation, and fast-reject
- *(search)* optimize pathfinder-search with incremental hashing and zero-alloc line decoding
- *(treesitter)* eliminate redundant I/O, reduce allocations, and add benchmark suite

## [0.11.1](https://github.com/irahardianto/pathfinder/compare/v0.11.0...v0.11.1) - 2026-06-03

### Fixed

- *(lsp)* address 27 audit findings across LSP client lifecycle
- *(lsp)* eliminate cross-language dispatch interference in polyglot workspaces
- *(lsp)* address phase 3 audit findings for cross-language dispatch isolation
- *(pathfinder-lsp)* fix cross-language init race and panic recovery
- Cross-language LSP dispatch isolation (LSP-INIT-002 Phase 1)

### Other

- *(lsp)* add cross-language dispatch observability (LSP-INIT-002 phase 4)

## [0.11.0](https://github.com/irahardianto/pathfinder/compare/v0.10.0...v0.11.0) - 2026-06-02

### Added

- *(overview)* add warm_start_in_progress field to SymbolOverviewResponse

### Fixed

- *(clippy)* resolve all 38 lint errors blocking CI
- *(navigation)* address Phase 5B audit findings across all navigation tools
- *(navigation)* address Phase 5A audit findings
- *(git)* harden diff_name_only against argument injection
- *(lsp)* address implementation and test gaps from Phase 3 review

### Other

- apply cargo fmt across entire workspace
- *(navigation)* split monolithic 9031-line file into focused sub-modules
- *(TCV-001)* cover LSP client residual gaps (Phase 4D)
- *(TCV-001)* cover navigation tool grep-fallback and BFS paths (Phase 2)
- *(TCV-001)* cover Tier 2 quick-win files (Phase 1)
- *(infra)* enhance MockScout and MockLawyer for multi-call scenarios
- *(lsp-client)* split monolithic mod.rs into focused sub-modules
- *(TCV-001)* cover reader_supervisor_task and init lock serialization
- *(TCV-001)* build FakeTransport and cover LSP client operations (Phase 3B-3E)
- *(lsp)* extract LspTransport trait for testable I/O boundary (Phase 3A)

## [0.10.0](https://github.com/irahardianto/pathfinder/compare/v0.9.1...v0.10.0) - 2026-06-01

### Added

- *(pathfinder)* complete agent-experience-remediation patch (26 specs, 5 epics)
- *(pathfinder)* add find_symbol and read_files MCP tools (specs 009, 010)
- *(pathfinder)* add symbol_overview composite tool
- add find_symbol and read_files tools with agent-experience spec docs
- *(agent-experience)* add ActionableGuidance, is_definition enrichment, and warm_start_complete
- *(pathfinder)* PATCH-003 repo_map LSP status + PATCH-002 search hint + PATCH-001 grep fallback extraction
- *(lsp)* PATCH-004 LSP warm start tracking and improved diagnostics

### Fixed

- *(lsp-hardening)* resolve gaps and bugs from LSP hardening audit
- *(lsp-hardening)* [**breaking**] address critical bugs and gaps from LSP hardening audit
- *(search)* add line index guard to is_definition check
- *(navigation)* paginate find_all_references correctly
- *(tests)* resolve 4 failing tests in navigation and repo_map modules
- Complete spec improvements and bug fixes for pathfinder navigation
- *(pathfinder)* find_symbol/read_files type and fallback fixes
- *(pathfinder)* search hint wording improvements
- *(pathfinder)* derive_lsp_status should match lsp_health_impl logic
- *(lsp)* populate indexing_progress_percent from workDoneProgress events
- *(pathfinder-lsp)* warm_start_complete flag not set when zero languages

### Other

- update dependencies and improve code formatting
- *(server)* add error format examples to semantic-path tool descriptions
- apply cargo fmt formatting across workspace
- *(readme)* update tool count from 10 to 13 and sync documentation
- *(mcp)* improve tool descriptions for find_symbol, read_files, symbol_overview, find_all_references, lsp_health, find_callers_callees
- apply cargo fmt formatting
- *(pathfinder)* extract grep_reference_fallback helper (specs 001, 007, 008)
- sync documentation with v0.9.1 codebase state
- *(lsp)* add warm_start_complete flag tests and fix doctest reporting

## [0.9.1](https://github.com/irahardianto/pathfinder/compare/v0.9.0...v0.9.1) - 2026-05-10

### Added

- *(lsp)* add goto_implementation and improve references output

### Fixed

- *(treesitter)* resolve Rust impl block merging with lifetimes and generics

## [0.9.0](https://github.com/irahardianto/pathfinder/compare/v0.8.1...v0.9.0) - 2026-05-10

### Added

- *(server)* rename analyze_impact to find_callers_callees, register find_all_references, update descriptions
- *(nav)* standardize degraded format, confirmed-zero messages, lsp_readiness
- *(mcp-tools)* add source_only detail_level, search coverage %, thread include_tests
- *(mcp-types)* add include_tests, search coverage, lsp_readiness, find_all_references types; max_depth 2→3
- *(java)* add complete Java support with Tree-sitter and jdtls LSP integration
- *(lsp)* add textDocument/references support to Lawyer trait (R5)
- *(search)* add files_searched/files_in_scope/coverage_percent to SearchResult (R8)
- *(treesitter)* add comprehensive test function detection + preserve all symbols in truncated skeleton

### Fixed

- resolve clippy doc_markdown warnings and test lints

### Other

- apply cargo fmt + add .opencode to gitignore
- resolve 19 clippy lint violations in mcp tools
- *(core)* finalize read-only semantic navigation architecture and tool ergonomics
- Update README.md
- format code with rustfmt

## [0.8.1](https://github.com/irahardianto/pathfinder/compare/v0.8.0...v0.8.1) - 2026-05-08

### Added

- *(navigation)* add source file filtering and workspace detection for LSP fallback
- *(server)* add structured DegradedToolInfo for degraded tool reporting

### Fixed

- *(lint)* resolve clippy warnings in error.rs and helpers.rs
- *(common)* improve symbol not found hint to suggest search_codebase
- *(search)* exclude .git/, node_modules/, vendor/ from search results

### Other

- *(helpers)* apply rustfmt formatting to assert_eq! statements
- *(server)* update tool descriptions with missing guidance
- *(readme)* fix inaccuracies in tool count, agent directives, and development commands
- *(server)* add comprehensive helpers module tests achieving 95% coverage
- *(lsp)* add transport edge case tests improving coverage to 89%
- *(lsp)* add client module tests for validation status and in-flight guard
- *(lsp)* add comprehensive error.rs tests achieving 100% coverage
- *(treesitter)* add parser tests for all supported languages achieving 95% coverage

## [0.8.0](https://github.com/irahardianto/pathfinder/compare/v0.7.0...v0.8.0) - 2026-05-07

### Other

- apply rustfmt formatting
- *(mcp)* standardize tool outputs and enhance text ergonomics

## [0.7.0](https://github.com/irahardianto/pathfinder/compare/v0.6.2...v0.7.0) - 2026-05-07

### Fixed

- *(clippy)* suppress dead_code lint on tool_router field
- resolve Dependabot config error and patch CVE-2026-42559 in rmcp

### Other

- *(server)* Remove OCC metadata from tool responses
- *(lsp)* Drop diagnostics, formatting, and validation types
- Formalize read-only architectural pivot and update agent directives
- *(server)* Decommission edit tools and validation pipeline in pathfinder server
- *(core)* Sunset edit traits and utilities in common and treesitter crates
- *(lsp)* Remove shadow editor and edit logic from LSP client
- *(search)* Strip edit-related metadata from search crate
- *(search)* eliminate redundant Mutex wrappers in RipgrepScout
- *(treesitter)* Remove edit-centric AST resolution capabilities

## [0.6.2](https://github.com/irahardianto/pathfinder/compare/v0.6.1...v0.6.2) - 2026-05-04

### Fixed

- *(lsp)* prevent false-positive coexistence mode in parallel test environments

### Other

- *(treesitter)* harden AST resolution + ResolvedFile encapsulation (WP1-4)
- format codebase and remediate Clippy lints across all crates
- *(fmt)* apply rustfmt to closure in detect_concurrent_lsp

## [0.6.1](https://github.com/irahardianto/pathfinder/compare/v0.6.0...v0.6.1) - 2026-05-04

### Fixed

- *(navigation)* add grep fallback to generic LSP errors in get_definition
- *(lsp)* prevent zombie processes and refine coexistence detection

### Other

- *(lint)* address clippy warnings and format across workspaces
- format codebase and remediate Clippy lints across all crates

## [0.6.0](https://github.com/irahardianto/pathfinder/compare/v0.5.0...v0.6.0) - 2026-05-04

### Added

- *(lsp)* complete phase 0-5 of pathfinder LSP roadmap
- *(lsp)* DS-1 DocumentGuard RAII lifecycle for navigation tools
- *(mcp)* surface lsp_readiness and validation_confidence signals (IW-2)
- *(lsp)* pre-flight manifest validation and python venv detection
- *(lsp)* core lifecycle, diagnostics reliability, and backoff improvements

### Fixed

- *(lsp)* resolve all CI Clippy -D warnings and fmt violations
- *(lsp)* apply rustfmt style to assert_eq! in plugin tests

### Other

- *(server)* expand get_repo_map test coverage
- format codebase and remediate Clippy lints across all crates
- *(lsp)* resolve flakiness in process lifecycle tests

## [0.5.0](https://github.com/irahardianto/pathfinder/compare/v0.4.0...v0.5.0) - 2026-05-02

### Fixed

- *(clippy)* resolve all clippy warnings in LSP health implementation
- *(lsp)* harden probe cache with TTL and auto-gitignore for isolated caches

### Other

- *(navigation)* apply rustfmt to reformat long line
- *(lsp)* apply rustfmt formatting

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