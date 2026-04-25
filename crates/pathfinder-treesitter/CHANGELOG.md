# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/irahardianto/pathfinder/releases/tag/pathfinder-treesitter-v0.1.0) - 2026-04-25

### Added

- wire cache invalidation, direction/depth fields, and LSP process-group hardening
- *(treesitter)* implement JSX/TSX symbol extraction (Epic E1-J)
- *(core)* implement v5.1 requirements for reliability and agent experience
- *(treesitter)* implement Vue SFC multi-zone parsing (Epic E1a)
- *(repo-map)* raise token defaults and expose max_tokens_per_file param
- *(epic7)* implement get_repo_map visibility filtering and search_codebase degradation tracking
- add semantic path overload handling
- *(search)* add filter_mode with tree-sitter node classification
- *(server)* implement remaining AST-aware edit tools (replace_full, insert_before, insert_after, delete_symbol)
- *(treesitter)* implement get_repo_map AST skeleton generator

### Fixed

- *(lsp)* restore Lawyer implementation and add tests
- *(treesitter)* remediate clippy and fmt findings from Phase 2 verification
- *(audit)* harden production reliability across pathfinder crates
- *(search)* Enforce valid globs and deduplicate known match tokens
- *(repo_map)* increase default file discovery depth to 5
- *(treesitter)* refine symbol extraction for Go types, TS arrow funcs, and Vue SFCs
- *(navigation,lsp)* address audit findings from post-PRD v4.6 delta review
- *(audit)* resolve remaining minor and nit audit findings
- *(audit)* address 7 findings from 2026-03-07 full codebase review
- *(edit)* address audit findings F1, F2, F6
- *(edit)* compute body indent delta from file source instead of hardcoding +4 spaces
- *(treesitter,server)* resolve all 7 audit findings from 2026-03-06-1612 review

### Other

- *(pipeline)* add comprehensive github actions workflow and cargo-deny checks
- *(treesitter)* extract detect_body_indent to reduce resolve_body_range complexity
- *(treesitter)* introduce SkeletonConfig struct to reduce generate_skeleton parameter count
- *(treesitter)* decompose extract_symbols_recursive via SymbolExtractionContext
- extract shared helpers to reduce duplication across LSP client, mocks, and tool handlers
- *(audit)* finalize Pathfinder v5.1 reliability hardening
