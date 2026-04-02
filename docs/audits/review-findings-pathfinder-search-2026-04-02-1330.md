# Code Audit: pathfinder-search
Date: 2026-04-02

## Summary
- **Files reviewed:** 4 (`lib.rs`, `types.rs`, `searcher.rs`, `ripgrep.rs`) in `pathfinder-search`, plus MCP wrapper `server/tools/search.rs` and `server/types.rs` in the `pathfinder` crate
- **Issues found:** 2 (1 major, 1 minor)
- **Test coverage:** Checked
- **Dimensions activated:** C, D, E (Skipping A, B, F as this is a backend pure Rust library)

## Critical Issues
None.

## Major Issues
Issues that should be fixed in the near term.

- [ ] **Silent Failure on Invalid Globs** — `crates/pathfinder-search/src/ripgrep.rs`
  When parsing `path_glob` or `exclude_glob` using `globset::GlobBuilder`, the implementation uses `.ok()` which silently consumes syntax errors (e.g., an unmatched bracket like `[invalid`). Instead of failing the search with an `InvalidPattern` error alerting the user (agent), the search engine silently ignores the filter and searches all files. This wastes tokens, CPU, and emits excessive unexpected output for broken inputs.

- [ ] **Redundant Tokens in Grouped Known Output** — `crates/pathfinder/src/server/types.rs` & `server/tools/search.rs`
  The E4.2 PRD strict requirement states that `group_by_file` must "deduplicate `file` and `version_hash` per group". The `GroupedMatch` schema correctly omits these. However, for `known_files`, the grouping logic inserts `KnownFileMatch` into `group.known_matches`. Because `KnownFileMatch` contains BOTH `file` and `version_hash`, these fields are redundantly re-serialized for every single matched line in a known file inside a group, violating the core token-efficiency intent of E4.

## Minor Issues
Style, naming, or minor improvements.
None found.

## Verification Results
- Lint: PASS
- Tests: PASS (all tests present correctly target intended behaviors)
- Build: PASS
- Coverage: No explicit check run, but gaps identified in test suite regarding invalid globs

## Dimensions Covered
| Dimension | Status | Files / Queries Examined |
|---|---|---|
| A. Integration Contracts | ⏭ Skipped | No frontend/backend integration in this crate |
| B. Database & Schema | ⏭ Skipped | No database in use |
| C. Configuration & Environment | ✅ Checked | Scanned and confirmed no hardcoded secrets |
| D. Dependency Health | ✅ Checked | Dependencies (`grep-searcher`, `globset`, etc.) are appropriate and used |
| E. Test Coverage Gaps | ✅ Checked | Tests valid inputs, lacks test asserting invalid `exclude_glob`/`path_glob` rejection |
| F. Mobile ↔ Backend | ⏭ Skipped | No mobile component |
