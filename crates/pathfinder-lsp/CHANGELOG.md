# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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