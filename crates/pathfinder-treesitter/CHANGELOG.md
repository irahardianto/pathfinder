# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.10.2](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-treesitter-v0.10.1...pathfinder-mcp-treesitter-v0.10.2) - 2026-06-10

### Added

- *(server)* remediate mcp ergonomics and add get_semantic_path

## [0.10.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-treesitter-v0.10.0...pathfinder-mcp-treesitter-v0.10.1) - 2026-06-09

### Added

- *(treesitter)* cap native HTML element depth in Vue template AST to depth < 3

### Other

- *(treesitter,search)* BATCH-03 add comprehensive repo_map and ripgrep coverage

## [0.10.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-treesitter-v0.9.1...pathfinder-mcp-treesitter-v0.10.0) - 2026-06-08

### Added

- Implement batch 4 deliverables for GFB-001 - search coverage, repo map truncation, cache invalidation
- *(navigation)* add outgoing deps to grep fallback (DELIVERABLE F)

### Fixed

- *(treesitter)* Replace unwrap() with expect() for better error messages

## [0.9.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-treesitter-v0.9.0...pathfinder-mcp-treesitter-v0.9.1) - 2026-06-06

### Fixed

- *(deps)* bump tree-sitter 0.25 → 0.26 with parse_with_options migration

### Other

- *(treesitter)* offload CPU-intensive parsing to blocking pool and optimize concurrency

## [0.9.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-treesitter-v0.8.1...pathfinder-mcp-treesitter-v0.9.0) - 2026-06-03

### Fixed

- *(treesitter)* correct mtime race, unblock runtime, and extend Vue preloaded cache

### Other

- apply rustfmt formatting across workspace
- *(treesitter)* eliminate redundant I/O, reduce allocations, and add benchmark suite

## [0.8.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-treesitter-v0.8.0...pathfinder-mcp-treesitter-v0.8.1) - 2026-06-02

### Fixed

- *(git)* harden diff_name_only against argument injection

## [0.8.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-treesitter-v0.7.1...pathfinder-mcp-treesitter-v0.8.0) - 2026-06-01

### Added

- *(agent-experience)* add ActionableGuidance, is_definition enrichment, and warm_start_complete

### Fixed

- Complete spec improvements and bug fixes for pathfinder navigation

### Other

- update dependencies and improve code formatting

## [0.7.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-treesitter-v0.7.0...pathfinder-mcp-treesitter-v0.7.1) - 2026-05-10

### Fixed

- *(treesitter)* resolve Rust impl block merging with lifetimes and generics

## [0.7.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-treesitter-v0.6.1...pathfinder-mcp-treesitter-v0.7.0) - 2026-05-10

### Added

- *(treesitter)* add comprehensive test function detection + preserve all symbols in truncated skeleton
- *(java)* add complete Java support with Tree-sitter and jdtls LSP integration

### Fixed

- resolve clippy doc_markdown warnings and test lints

### Other

- format code with rustfmt

## [0.6.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-treesitter-v0.6.0...pathfinder-mcp-treesitter-v0.6.1) - 2026-05-08

### Other

- updated the following local packages: pathfinder-mcp-common

## [0.6.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-treesitter-v0.5.0...pathfinder-mcp-treesitter-v0.6.0) - 2026-05-07

### Other

- *(treesitter)* Remove edit-centric AST resolution capabilities
- *(core)* Sunset edit traits and utilities in common and treesitter crates

## [0.5.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-treesitter-v0.4.2...pathfinder-mcp-treesitter-v0.5.0) - 2026-05-04

### Other

- *(treesitter)* harden AST resolution + ResolvedFile encapsulation (WP1-4)

## [0.4.2](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-treesitter-v0.4.1...pathfinder-mcp-treesitter-v0.4.2) - 2026-05-01

### Added

- Complete PATCH-001 through PATCH-011 implementation gaps
- *(lsp)* Python LSP E2E verification test + fix pyright detection (PATCH-009)

## [0.4.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-treesitter-v0.4.0...pathfinder-mcp-treesitter-v0.4.1) - 2026-05-01

### Other

- *(cleanup)* remove dead code and stale allow(dead_code)

## [0.4.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-treesitter-v0.3.3...pathfinder-mcp-treesitter-v0.4.0) - 2026-05-01

### Fixed

- *(navigation)* add name_column for correct LSP cursor positioning (PATCH-001)

## [0.3.3](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-treesitter-v0.3.2...pathfinder-mcp-treesitter-v0.3.3) - 2026-04-29

### Other

- improve code formatting and remove unnecessary lint suppressions
- *(code-quality)* fix DeepSource and Clippy warnings
- fix 28+ clippy warnings and improve code quality

## [0.3.2](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-treesitter-v0.3.1...pathfinder-mcp-treesitter-v0.3.2) - 2026-04-29

### Other

- *(core)* implement 2026-04-29 patch batch (7 patches)
- *(cache)* annotate lock-poison arms as structurally untestable in safe Rust

## [0.3.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-treesitter-v0.3.0...pathfinder-mcp-treesitter-v0.3.1) - 2026-04-28

### Fixed

- *(treesitter)* resolve E0432 by moving dev-dependency import to test module

### Other

- replace hardcoded temporary paths with tempfile::tempdir

## [0.3.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-treesitter-v0.2.0...pathfinder-mcp-treesitter-v0.3.0) - 2026-04-27

### Fixed

- *(treesitter)* surface missing-file errors as INVALID_PARAMS not INTERNAL_ERROR

### Other

- *(occ)* migrate to 7-character short version hashes

## [0.2.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-treesitter-v0.1.3...pathfinder-mcp-treesitter-v0.2.0) - 2026-04-27

### Added

- *(treesitter)* implement TypeScript namespace and module visibility support
- *(treesitter)* Enhance AST parsing and extraction for modules and bodies

## [0.1.3](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-treesitter-v0.1.2...pathfinder-mcp-treesitter-v0.1.3) - 2026-04-26

### Other

- replace empty new() calls with default()

## [0.1.2](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-treesitter-v0.1.1...pathfinder-mcp-treesitter-v0.1.2) - 2026-04-26

### Added

- *(cache)* implement singleflight deduplication for AST cache

### Fixed

- resolve all clippy warnings
- remove redundant clones in non-test code
- address DeepSource findings (6/11 issues)

### Other

- apply cargo fmt formatting across all crates
- complete TCV-001 coverage remediation (WP-1 + WP-6)
- Fix DeepSource syntax errors and apply cargo clippy
- add documentation for public items

## [0.1.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-treesitter-v0.1.0...pathfinder-mcp-treesitter-v0.1.1) - 2026-04-25

### Other

- updated the following local packages: pathfinder-mcp-common

## [0.1.0] - 2026-04-24
### Added
- Initial release