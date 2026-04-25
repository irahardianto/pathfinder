# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/irahardianto/pathfinder/releases/tag/pathfinder-mcp-v0.1.0) - 2026-04-25

### Added

- wire cache invalidation, direction/depth fields, and LSP process-group hardening
- *(core)* implement LSP warm start and optimize ripgrep hashing
- *(navigation)* integrate ripgrep heuristic fallbacks for degraded LSP
- *(core)* finalize v5.1 replace_batch schema, analyze_impact, and docs
- *(core)* implement v5.1 requirements for reliability and agent experience
- *(treesitter)* implement Vue SFC multi-zone parsing (Epic E1a)
- *(edit)* implement hybrid text+semantic batch edits (Epic E3)
- *(mcp)* add explicit semantic path guardrails to tool descriptions
- *(search)* implement Epic E4 — search intelligence improvements
- *(source_file)* implement compact read modes and line filtering (Pathfinder PRD v5 Epic E2)
- *(server)* implement read_source_file and replace_batch edit tools
- *(repo-map)* raise token defaults and expose max_tokens_per_file param
- *(epic7)* implement get_repo_map visibility filtering and search_codebase degradation tracking
- *(navigation)* wire lsp call hierarchy for analyze_impact and read_deep_context
- *(tools)* implement LSP validation pipeline and resolve clippy warnings
- *(search)* add filter_mode with tree-sitter node classification
- *(lsp)* implement LspClient with JSON-RPC transport and process lifecycle management
- *(epic4/m1)* add pathfinder-lsp crate and implement navigation tools with degraded mode
- *(edit)* implement validate_only dry-run tool with tests
- *(server)* implement remaining AST-aware edit tools (replace_full, insert_before, insert_after, delete_symbol)
- *(treesitter)* implement get_repo_map AST skeleton generator

### Fixed

- *(edit)* resolve logic bugs, missing tests, and visibility in AST edit tools
- *(lsp)* restore Lawyer implementation and add tests
- harden strip_orphaned_doc_comment and resolve clippy warnings
- *(edit)* improve text matching robustness and structural stability
- *(tools)* resolve borrow-after-move errors and clippy warnings
- *(helpers)* add PathTraversal variant to error code match
- *(edit)* implement 7 audit/quality patches for edit tools
- *(tools)* surface critical data in MCP tool text outputs
- *(server)* address review findings on MCP error code changes
- *(server)* map PathfinderErrors to semantic JSON-RPC error codes
- *(audit)* harden production reliability across pathfinder crates
- *(search)* Enforce valid globs and deduplicate known match tokens
- *(server)* Downgrade rmcp to 1.0.0 to fix MCP SDK Node client schema crash
- *(search)* E4 gap remediation — known flag and GroupedMatch shape
- *(repo_map)* increase default file discovery depth to 5
- *(occ)* properly populate lines_changed at TOCTOU check sites
- *(navigation,lsp)* address audit findings from post-PRD v4.6 delta review
- *(audit)* resolve remaining minor and nit audit findings
- *(observability)* add missing start log, sandbox denial logs, and per-engine telemetry
- *(edit)* close LSP document on all validation code paths
- *(edit)* address audit findings for document leaks and logging
- *(lsp)* address observability and redundancy audit findings
- *(lsp)* implement did_close and improve test coverage
- *(search)* bound enrichment concurrency with buffer_unordered
- *(pathfinder-lsp)* address audit findings from 2026-03-07
- *(audit)* address 7 findings from 2026-03-07 full codebase review
- *(edit)* address audit findings F1, F2, F6
- *(edit)* compute body indent delta from file source instead of hardcoding +4 spaces
- *(treesitter,server)* resolve all 7 audit findings from 2026-03-06-1612 review

### Other

- *(workspace)* rename crates to pathfinder-mcp-*
- *(workspace)* resolve integration test and pipeline configuration errors
- *(tools)* fix clippy::useless_conversion and duration_suboptimal_units on Rust 1.95
- *(tools)* apply rustfmt to edit and navigation handlers
- *(pipeline)* add comprehensive github actions workflow and cargo-deny checks
- temp
- remove unintended temporary files
- *(lint)* achieve zero-warning build under -D warnings
- *(edit)* update helper tests for closest_match field
- *(treesitter)* introduce SkeletonConfig struct to reduce generate_skeleton parameter count
- extract shared helpers to reduce duplication across LSP client, mocks, and tool handlers
- *(server)* decompose edit pipeline to reduce complexity and duplication
- *(server)* finalize v5.2 hybrid output and boolean hardening
- update documentation to reflect v5 features and reliability enhancements
- *(audit)* finalize Pathfinder v5.1 reliability hardening
- *(pathfinder)* add unit tests for search context overlap and integration test suite
- *(navigation)* remove dead build_impact_reference helper
- *(audit)* resolve findings from 2026-03-09 0613 audit
- *(tools)* extract duplicated validation tail in edit.rs
- *(pathfinder,search)* resolve 2 remaining 1358 audit findings
