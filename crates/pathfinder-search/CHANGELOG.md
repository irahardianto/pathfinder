# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.8](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.5.7...pathfinder-mcp-search-v0.5.8) - 2026-06-21

### Other

- updated the following local packages: pathfinder-mcp-common

## [0.5.7](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.5.6...pathfinder-mcp-search-v0.5.7) - 2026-06-19

### Added

- *(mcp)* support multi-glob exclusions, brace expansion, and improved tool schemas

### Other

- run cargo fmt --all to fix CI formatting check
- add 59 unit tests to cover uncovered production lines (TCV-001)

## [0.5.6](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.5.5...pathfinder-mcp-search-v0.5.6) - 2026-06-18

### Fixed

- *(clippy)* allow expect_used and unwrap_used in tests at crate roots

### Other

- Extract tests for pathfinder-lsp, pathfinder-search, and pathfinder-treesitter to separate files
- *(search)* prune excluded dirs at walker level; remove DRY violation in closures
- *(search)* add criterion benchmark for grep fallback worst-case traversal

## [0.5.5](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.5.4...pathfinder-mcp-search-v0.5.5) - 2026-06-18

### Fixed

- *(search)* unify walk_files counting path through filter_entry_impl
- *(hygiene)* exclude .qlty/ from search ALWAYS_EXCLUDED_DIRS

### Other

- audit remediation and testability improvements across workspace

## [0.5.4](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.5.3...pathfinder-mcp-search-v0.5.4) - 2026-06-15

### Fixed

- *(search)* exclude binary and gitignored files from files_in_scope denominator

### Other

- *(pathfinder-search)* extract filter_entry, fix gitignored/binary count logic, and fix win_dir prefix match bug

## [0.5.3](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.5.2...pathfinder-mcp-search-v0.5.3) - 2026-06-14

### Other

- updated the following local packages: pathfinder-mcp-common

## [0.5.2](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.5.1...pathfinder-mcp-search-v0.5.2) - 2026-06-13

### Other

- updated the following local packages: pathfinder-mcp-common

## [0.5.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.5.0...pathfinder-mcp-search-v0.5.1) - 2026-06-10

### Other

- updated the following local packages: pathfinder-mcp-common

## [0.5.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.4.3...pathfinder-mcp-search-v0.5.0) - 2026-06-08

### Added

- *(search)* Add compiled regex caching for improved performance
- Implement batch 4 deliverables for GFB-001 - search coverage, repo map truncation, cache invalidation

### Fixed

- *(search)* Remove flaky cache hit counter from regex cache test

## [0.4.3](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.4.2...pathfinder-mcp-search-v0.4.3) - 2026-06-06

### Fixed

- *(deps)* bump sha2 0.10 → 0.11 with digest 0.11 migration

## [0.4.2](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.4.1...pathfinder-mcp-search-v0.4.2) - 2026-06-03

### Fixed

- *(common,search)* address audit findings from perf session

### Other

- apply rustfmt formatting across workspace
- *(search)* optimize pathfinder-search with incremental hashing and zero-alloc line decoding

## [0.4.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.4.0...pathfinder-mcp-search-v0.4.1) - 2026-06-02

### Other

- updated the following local packages: pathfinder-mcp-common

## [0.4.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.3.0...pathfinder-mcp-search-v0.4.0) - 2026-06-01

### Added

- *(agent-experience)* add ActionableGuidance, is_definition enrichment, and warm_start_complete

### Other

- *(lsp)* add warm_start_complete flag tests and fix doctest reporting

## [0.3.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.2.9...pathfinder-mcp-search-v0.3.0) - 2026-05-10

### Added

- *(search)* add files_searched/files_in_scope/coverage_percent to SearchResult (R8)

## [0.2.9](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.2.8...pathfinder-mcp-search-v0.2.9) - 2026-05-08

### Fixed

- *(search)* exclude .git/, node_modules/, vendor/ from search results

## [0.2.8](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.2.7...pathfinder-mcp-search-v0.2.8) - 2026-05-07

### Other

- *(search)* Strip edit-related metadata from search crate
- *(search)* eliminate redundant Mutex wrappers in RipgrepScout

## [0.2.7](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.2.6...pathfinder-mcp-search-v0.2.7) - 2026-05-04

### Other

- updated the following local packages: pathfinder-mcp-common

## [0.2.6](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.2.5...pathfinder-mcp-search-v0.2.6) - 2026-05-04

### Fixed

- *(search)* bump pathfinder-mcp-search to v0.2.6 for offset field

### Other

- format codebase and remediate Clippy lints across all crates

## [0.2.5](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.2.4...pathfinder-mcp-search-v0.2.5) - 2026-05-01

### Other

- updated the following local packages: pathfinder-mcp-common

## [0.2.4](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.2.3...pathfinder-mcp-search-v0.2.4) - 2026-05-01

### Other

- updated the following local packages: pathfinder-mcp-common

## [0.2.3](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.2.2...pathfinder-mcp-search-v0.2.3) - 2026-04-29

### Other

- updated the following local packages: pathfinder-mcp-common

## [0.2.2](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.2.1...pathfinder-mcp-search-v0.2.2) - 2026-04-29

### Other

- updated the following local packages: pathfinder-mcp-common

## [0.2.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.2.0...pathfinder-mcp-search-v0.2.1) - 2026-04-27

### Other

- *(occ)* migrate to 7-character short version hashes

## [0.2.0](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.1.3...pathfinder-mcp-search-v0.2.0) - 2026-04-27

### Other

- *(search)* replace RipgrepScout constructor with derived Default

## [0.1.3](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.1.2...pathfinder-mcp-search-v0.1.3) - 2026-04-26

### Other

- *(search)* fix rustfmt and clippy warnings in ripgrep tests
- replace empty new() calls with default()

## [0.1.2](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.1.1...pathfinder-mcp-search-v0.1.2) - 2026-04-26

### Fixed

- resolve all clippy warnings
- address DeepSource findings (6/11 issues)

### Other

- apply cargo fmt formatting across all crates
- complete TCV-001 coverage remediation (WP-1 + WP-6)
- add documentation for public items

## [0.1.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-mcp-search-v0.1.0...pathfinder-mcp-search-v0.1.1) - 2026-04-25

### Other

- updated the following local packages: pathfinder-mcp-common

## [0.1.0] - 2026-04-24
### Added
- Initial release