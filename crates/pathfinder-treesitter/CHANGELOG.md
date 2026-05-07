# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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