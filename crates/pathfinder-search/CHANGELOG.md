# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/irahardianto/pathfinder/releases/tag/pathfinder-search-v0.1.0) - 2026-04-25

### Added

- *(core)* implement LSP warm start and optimize ripgrep hashing
- *(core)* finalize v5.1 replace_batch schema, analyze_impact, and docs
- *(search)* implement Epic E4 — search intelligence improvements
- *(treesitter)* implement get_repo_map AST skeleton generator

### Fixed

- *(lsp)* restore Lawyer implementation and add tests
- *(audit)* harden production reliability across pathfinder crates
- *(search)* Enforce valid globs and deduplicate known match tokens
- *(search)* E4 gap remediation — known flag and GroupedMatch shape
- *(audit)* address 7 findings from 2026-03-07 full codebase review

### Other

- *(pipeline)* add comprehensive github actions workflow and cargo-deny checks
- *(search,common)* mutex recovery helper + resolve_strict variant
- *(pathfinder)* add unit tests for search context overlap and integration test suite
- *(search)* improve ripgrep handling and search result robustness
- *(pathfinder,search)* resolve 2 remaining 1358 audit findings
