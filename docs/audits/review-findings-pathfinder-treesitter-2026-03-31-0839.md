# Code Audit: pathfinder-treesitter
Date: 2026-03-31

## Summary
- **Files reviewed:** 11 (all core Rust sources in `crates/pathfinder-treesitter/src/`)
- **Issues found:** 3 (0 critical, 0 major, 2 minor, 1 nit)
- **Test coverage:** N/A (excellent unit test presence, no explicit % generated)
- **Dimensions activated:** C, D, E (skipped A, B, F)

## Critical Issues
Issues that must be fixed before deployment.
- *None found.*

## Major Issues
Issues that should be fixed in the near term.
- *None found.*

## Minor Issues
Style, naming, or minor improvements.
- [x] **[PAT]** Efficiency: `AstCache` uses an O(N) linear scan for LRU eviction. Consider using a dedicated LRU cache structure (e.g., `lru` crate) if `max_entries` is expected to grow, avoiding `Instant::now()` calls in a tight loop. — `crates/pathfinder-treesitter/src/cache.rs:114`
- [x] **[DATA]** Robustness: `std::str::from_utf8(&source[(start_byte + 1)..end_byte])` relies on `end_byte >= start_byte + 1`. While generally true for tree-sitter braced bodies, using `.get((start_byte + 1)..end_byte)` avoids potential slicing panics on pathological AST node ranges. — `crates/pathfinder-treesitter/src/treesitter_surgeon.rs:354`

## Nit
- [x] **[OBS]** Observability: Add `#[instrument(skip(self))]` to `AstCache::get_or_parse` to natively track cache latency and hits/misses in the request trace. — `crates/pathfinder-treesitter/src/cache.rs:71`

## Verification Results
- Lint: PASS (Zero warnings with `clippy`)
- Tests: PASS (37 passed, 0 failed)
- Build: PASS
- Coverage: NOT COMPUTED (High base suite density observed across all modules)

## Dimensions Covered
<!-- Required when total findings < 3 -->
| Dimension | Status | Files / Queries Examined |
|---|---|---|
| A. Integration Contracts | ⏭ Skipped (N/A) | No frontend/backend APIs in this low-level library crate. |
| B. Database & Schema | ⏭ Skipped (N/A) | No database used. |
| C. Configuration & Environment | ✅ Checked | Verified no hardcoded secrets or environment variables accessed directly. |
| D. Dependency Health | ✅ Checked | Reviewed `Cargo.toml`. No unused or circular deps. Skipped `cargo audit` (not installed locally), but verified standard safe crates used (tokio, tree-sitter, ignore). |
| E. Test Coverage Gaps | ✅ Checked | Examined unit test coverage in `cache.rs`, `parser.rs`, `repo_map.rs`, `surgeon.rs`, `symbols.rs`, and `treesitter_surgeon.rs`. All parsing, extraction, and rendering code paths are well-tested. |
| F. Mobile ↔ Backend | ⏭ Skipped (N/A) | No mobile app. |
