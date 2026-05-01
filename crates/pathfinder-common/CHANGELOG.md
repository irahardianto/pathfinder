# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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