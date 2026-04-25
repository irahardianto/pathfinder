# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/irahardianto/pathfinder/compare/pathfinder-common-v0.1.0...pathfinder-common-v0.1.1) - 2026-04-25

### Added

- *(core)* finalize v5.1 replace_batch schema, analyze_impact, and docs
- *(core)* implement v5.1 requirements for reliability and agent experience
- *(edit)* implement hybrid text+semantic batch edits (Epic E3)
- *(repo-map)* raise token defaults and expose max_tokens_per_file param
- *(treesitter)* implement get_repo_map AST skeleton generator

### Fixed

- *(edit)* resolve logic bugs, missing tests, and visibility in AST edit tools
- *(lsp)* restore Lawyer implementation and add tests
- *(sandbox)* robust workspace path normalization and wildcard evaluation
- *(server)* map PathfinderErrors to semantic JSON-RPC error codes
- *(audit)* harden production reliability across pathfinder crates
- *(occ)* properly populate lines_changed at TOCTOU check sites
- *(navigation,lsp)* address audit findings from post-PRD v4.6 delta review
- *(audit)* resolve remaining minor and nit audit findings
- *(search)* bound enrichment concurrency with buffer_unordered
- *(audit)* address 7 findings from 2026-03-07 full codebase review
- *(edit)* compute body indent delta from file source instead of hardcoding +4 spaces

### Other

- *(tools)* fix clippy::useless_conversion and duration_suboptimal_units on Rust 1.95
- *(pipeline)* add comprehensive github actions workflow and cargo-deny checks
- *(search,common)* mutex recovery helper + resolve_strict variant
- *(audit)* finalize Pathfinder v5.1 reliability hardening
- *(common)* improve sandbox, file watcher, and config resilience
