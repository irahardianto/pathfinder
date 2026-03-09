# Code Audit: LSP Navigation & Search Tools (PRD 4.6 / Epic 7)
Date: 2026-03-09
Reviewer: AI Agent (fresh context)

## Summary
- **Files reviewed:** 11
- **Issues found:** 2 (0 critical, 0 major, 1 minor, 1 nit)

## Critical Issues
None.

## Major Issues
None.

## Minor Issues
- [ ] **[PAT]** Global vs Per-File Degradation — [`crates/pathfinder/src/server/tools/search.rs:67`](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder/src/server/tools/search.rs)
  The `search_codebase` tool correctly detects supported languages per-file, but sets `degraded: true` globally on the response if *any* matched file lacks a Tree-sitter grammar. While this matches the current `SearchCodebaseResponse` struct design, the Epic 7 PRD specifically noted "per-file degradation tracking". Consider whether the `SearchMatch` struct should be updated to include a `degraded` flag so agents know exactly which matches lack AST enrichment.

## Nit
- [ ] **[OBS]** Inconsistent Observability Fields — [`crates/pathfinder/src/server/tools/repo_map.rs:60`](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder/src/server/tools/repo_map.rs)
  The `engines_used` tracing field is formatted inconsistently across tools. `search.rs` uses an array `engines_used = ?["ripgrep", "treesitter"]`, while `repo_map.rs` uses a string `engines_used = "treesitter"`. Standardizing to an array format (e.g., `?["tree-sitter"]`) ensures that downstream log aggregators parse the field as a consistent type.

## Verification Results
- Lint: PASS (`cargo clippy --workspace --all-targets -- -D warnings`)
- Tests: PASS (`cargo test --workspace`)
- Build: PASS (`cargo check --workspace`)
- Coverage: N/A

## Rules Applied
- `logging-and-observability-mandate.md` (Checked for missing operation spans, correlation context, and duration)
- `error-handling-principles.md` (Checked for proper `ErrorData` mapping and absence of panics/unwrap)
- `security-mandate.md` (Checked for sandbox validation and path verification)
