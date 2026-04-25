# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/irahardianto/pathfinder/releases/tag/pathfinder-mcp-lsp-v0.1.0) - 2026-04-25

### Added

- wire cache invalidation, direction/depth fields, and LSP process-group hardening
- *(core)* implement LSP warm start and optimize ripgrep hashing
- *(lsp)* add reader task supervisor for crash detection
- *(lsp)* add cooldown-based recovery for permanently unavailable processes
- *(core)* implement v5.1 requirements for reliability and agent experience
- *(source_file)* implement compact read modes and line filtering (Pathfinder PRD v5 Epic E2)
- *(navigation)* wire lsp call hierarchy for analyze_impact and read_deep_context
- *(tools)* implement LSP validation pipeline and resolve clippy warnings
- *(lsp)* implement LspClient with JSON-RPC transport and process lifecycle management
- *(epic4/m1)* add pathfinder-lsp crate and implement navigation tools with degraded mode

### Fixed

- *(lsp)* restore Lawyer implementation and add tests
- *(lsp)* resolve stdout-read deadlock that prevented LSP initialization
- *(lsp)* increase initialization timeout and make it configurable
- *(lsp)* prevent idle timeout from killing processes with in-flight ops
- *(lsp)* health-check reader task before sending requests
- *(lsp)* Correct state machine retry logic and observability gaps in audit fixes
- *(search)* Enforce valid globs and deduplicate known match tokens
- *(lsp)* Resolve audit findings regarding timeout DOS, deadlock, observability, and state machine
- *(navigation,lsp)* address audit findings from post-PRD v4.6 delta review
- *(lsp)* address observability and redundancy audit findings
- *(lsp)* implement did_close and improve test coverage
- *(pathfinder-lsp)* address audit findings from 2026-03-07

### Other

- *(workspace)* rename crates to pathfinder-mcp-*
- *(tools)* fix clippy::useless_conversion and duration_suboptimal_units on Rust 1.95
- *(lsp)* resolve clippy warnings for Duration and Result
- *(pipeline)* add comprehensive github actions workflow and cargo-deny checks
- extract shared helpers to reduce duplication across LSP client, mocks, and tool handlers
- *(lsp)* improve robustness of LSP validation, tracking, and connection handling
- *(lsp-detect)* add monorepo depth-2 and root_override detection tests
- *(lsp)* address observability and code quality audit findings
- *(audit)* resolve findings from 2026-03-09 0613 audit
- *(tools)* extract duplicated validation tail in edit.rs
