# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.8.3](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-common-v0.8.2...pathfinder-mcp-common-v0.8.3) - 2026-06-21

### Added

- *(search)* add type kind filter and non_code filter mode alias

## [0.8.2](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-common-v0.8.1...pathfinder-mcp-common-v0.8.2) - 2026-06-19

### Added

- *(mcp)* support multi-glob exclusions, brace expansion, and improved tool schemas

### Other

- fix clippy warnings in test files to pass CI
- run cargo fmt --all to fix CI formatting check
- add 59 unit tests to cover uncovered production lines (TCV-001)

## [0.8.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-common-v0.8.0...pathfinder-mcp-common-v0.8.1) - 2026-06-18

### Fixed

- *(clippy)* allow expect_used and unwrap_used in tests at crate roots
- *(common)* add target/ to ALWAYS_EXCLUDED_DIRS; add ALWAYS_EXCLUDED_DIR_NAMES

### Other

- Extract tests for pathfinder-lsp, pathfinder-search, and pathfinder-treesitter to separate files
- Extract pathfinder-common tests to separate files

## [0.8.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-common-v0.7.2...pathfinder-mcp-common-v0.8.0) - 2026-06-18

### Fixed

- *(hygiene)* exclude .qlty/ from search ALWAYS_EXCLUDED_DIRS

### Other

- audit remediation and testability improvements across workspace
- *(common)* replace stringly-typed fields with enums; add config validation

## [0.7.2](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-common-v0.7.1...pathfinder-mcp-common-v0.7.2) - 2026-06-14

### Fixed

- *(error)* update agent-facing error hints to reference consolidated tool names

### Other

- run cargo fmt --all to enforce code styling conventions

## [0.7.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-common-v0.7.0...pathfinder-mcp-common-v0.7.1) - 2026-06-13

### Other

- *(deps)* bump criterion from 0.5.1 to 0.8.2
- *(deps)* bump mockall from 0.13.1 to 0.14.0
- *(sandbox)* pre-compute and classify sandbox deny patterns

## [0.7.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-common-v0.6.4...pathfinder-mcp-common-v0.7.0) - 2026-06-10

### Other

- *(get_repo_map)* remove dead include_imports parameter
- minor formatting and doc cleanup in common types and ripgrep
- *(common,lsp,server)* BATCH-05 add pathfinder-common types, plugin, and server types coverage

## [0.6.4](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-common-v0.6.3...pathfinder-mcp-common-v0.6.4) - 2026-06-08

### Fixed

- complete tool rename from analyze_impact to find_callers_callees

## [0.6.3](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-common-v0.6.2...pathfinder-mcp-common-v0.6.3) - 2026-06-06

### Fixed

- *(deps)* bump sha2 0.10 → 0.11 with digest 0.11 migration

## [0.6.2](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-common-v0.6.1...pathfinder-mcp-common-v0.6.2) - 2026-06-03

### Fixed

- *(common,search)* address audit findings from perf session

### Other

- *(common)* reformat sandbox deny pattern matching for readability
- apply rustfmt formatting across workspace
- *(common)* optimize hot-path functions with zero-alloc formatting, pre-allocation, and fast-reject

## [0.6.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-common-v0.6.0...pathfinder-mcp-common-v0.6.1) - 2026-06-02

### Fixed

- *(git)* harden diff_name_only against argument injection

### Other

- apply cargo fmt across entire workspace
- *(TCV-001)* cover Tier 2 quick-win files (Phase 1)

## [0.6.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-common-v0.5.2...pathfinder-mcp-common-v0.6.0) - 2026-06-01

### Added

- *(pathfinder)* complete agent-experience-remediation patch (26 specs, 5 epics)
- *(agent-experience)* add ActionableGuidance, is_definition enrichment, and warm_start_complete
- *(pathfinder)* PATCH-003 repo_map LSP status + PATCH-002 search hint + PATCH-001 grep fallback extraction

### Fixed

- Complete spec improvements and bug fixes for pathfinder navigation

### Other

- apply cargo fmt formatting

## [0.5.2](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-common-v0.5.1...pathfinder-mcp-common-v0.5.2) - 2026-05-10

### Added

- *(java)* add complete Java support with Tree-sitter and jdtls LSP integration

### Fixed

- resolve clippy doc_markdown warnings and test lints

### Other

- format code with rustfmt
- *(core)* finalize read-only semantic navigation architecture and tool ergonomics

## [0.5.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-common-v0.5.0...pathfinder-mcp-common-v0.5.1) - 2026-05-08

### Fixed

- *(common)* improve symbol not found hint to suggest search_codebase

## [0.5.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-common-v0.4.0...pathfinder-mcp-common-v0.5.0) - 2026-05-07

### Other

- *(core)* Sunset edit traits and utilities in common and treesitter crates

## [0.4.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-common-v0.3.0...pathfinder-mcp-common-v0.4.0) - 2026-05-04

### Other

- *(treesitter)* harden AST resolution + ResolvedFile encapsulation (WP1-4)
- format codebase and remediate Clippy lints across all crates

## [0.3.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-common-v0.2.0...pathfinder-mcp-common-v0.3.0) - 2026-05-01

### Added

- *(lsp)* cross-language LSP reliability improvements

### Other

- 🛡️ Shield: Fix panic in test_details_serialization_extra
- 🛡️ Shield: Fix panic in test_details_serialization_extra
- 🛡️ Shield: Increased coverage for error mapping and serialization in pathfinder-common
- 🛡️ Shield: Increased coverage for error mapping and serialization in pathfinder-common

## [0.2.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-common-v0.1.6...pathfinder-mcp-common-v0.2.0) - 2026-05-01

### Fixed

- *(navigation)* add name_column for correct LSP cursor positioning (PATCH-001)

## [0.1.6](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-common-v0.1.5...pathfinder-mcp-common-v0.1.6) - 2026-04-29

### Other

- *(code-quality)* fix DeepSource and Clippy warnings
- fix 28+ clippy warnings and improve code quality

## [0.1.5](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-common-v0.1.4...pathfinder-mcp-common-v0.1.5) - 2026-04-29

### Other

- *(core)* implement 2026-04-29 patch batch (7 patches)
- *(coverage)* add targeted unit tests to eliminate TCV-001 gaps
- *(integration)* establish mock LSP test harness and fix SLSA provenance

## [0.1.4](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-common-v0.1.3...pathfinder-mcp-common-v0.1.4) - 2026-04-27

### Other

- *(occ)* migrate to 7-character short version hashes
- *(config)* add coverage for successful load path and default idle timeout
- *(error)* add coverage for InvalidTarget variant

## [0.1.3](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-common-v0.1.2...pathfinder-mcp-common-v0.1.3) - 2026-04-26

### Other

- replace empty new() calls with default()

## [0.1.2](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-common-v0.1.1...pathfinder-mcp-common-v0.1.2) - 2026-04-26

### Added

- *(file-watcher)* log event drops for observability

### Fixed

- resolve all clippy warnings
- address DeepSource findings (6/11 issues)

### Other

- Fix DeepSource syntax errors and apply cargo clippy
- Fix DeepSource syntax errors and apply cargo fmt
- add documentation for public items

## [0.1.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-common-v0.1.0...pathfinder-mcp-common-v0.1.1) - 2026-04-25

### Other

- *(ci)* fix release-plz publish errors and changelog warnings

## [0.1.0] - 2026-04-24
### Added
- Initial release