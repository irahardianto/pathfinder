# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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