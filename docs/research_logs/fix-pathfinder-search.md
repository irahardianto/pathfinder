# Research Log: Fix Pathfinder Search Review Findings

## Context
Following the orchestrator workflow for Audit findings in `docs/audits/review-findings-pathfinder-search-2026-04-02-1330.md`.

## Major Findings

1. **Silent Failure on Invalid Globs** (`crates/pathfinder-search/src/ripgrep.rs`):
   - Current implementation uses `ok()` when building glob sets, which falls back to `None` silently for invalid globs.
   - We must surface invalid globs as a `SearchError::InvalidPattern` to prevent the agent interpreting full codebase runs as the filtered result.
   - The fix requires altering `walk_files` to return a `Result` and unpacking the builder cleanly.

2. **Redundant Tokens in Grouped Known Output** (`crates/pathfinder/src/server/types.rs` & `server/tools/search.rs`):
   - `group_by_file` implementation clusters into `SearchResultGroup`.
   - `SearchResultGroup` has `known_matches: Vec<KnownFileMatch>`.
   - `KnownFileMatch` contains `file` and `version_hash`.
   - This duplicates the `file` and `version_hash` already present on `SearchResultGroup`, wasting tokens (violating E4.2).
   - The fix requires introducing a `GroupedKnownMatch` type that excludes `file` and `version_hash`.

## Alignment with Rules & PRD
- **E4.2 PRD:** "Deduplicate `file` and `version_hash` per group" - This aligns directly with finding #2.
- **Rugged Software Constitution:** "Assume every input is malformed, malicious, or incorrect until proven otherwise." - Aligns with finding #1, we must explicitly fail on bad globs instead of degrading silently.

The plan is to address these issues and then follow up with comprehensive tests (`cargo clippy` and `cargo test`).
