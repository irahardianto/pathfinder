# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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