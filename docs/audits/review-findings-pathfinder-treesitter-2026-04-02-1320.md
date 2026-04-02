# Code Audit: pathfinder-treesitter
Date: 2026-04-02

## Summary
- **Files reviewed:** 11 (Cargo.toml + 10 Rust source files in src/)
- **Issues found:** 3 (2 critical, 1 major, 0 minor)
- **Test coverage:** N/A (100% of existing logic tested, but large logic gaps found)
- **Dimensions activated:** C, D, E
- **Dimensions skipped:** A (No frontend in this crate), B (No database), F (No mobile app)

## Critical Issues
Issues that must be fixed before deployment (Implementation Gaps from PRD).
- [ ] **E1a.1 Multi-Zone SFC Read Awareness Completely Missing** — `crates/pathfinder-treesitter/src/language.rs:136`
  The Vue SFC parsing continues to rely on the v4 hack of extracting the `<script>` block and replacing the rest with padding text (`extract_vue_script`). There is no implementation of `tree-sitter-html` or `tree-sitter-css` grammars using `Parser::set_included_ranges` as required by the PRD. The required dependencies are also missing from `Cargo.toml`.
- [ ] **E1a.2 AstCache Multi-Zone Support Missing** — `crates/pathfinder-treesitter/src/cache.rs:48`
  The AST cache (`AstCache`) uses only `PathBuf` as its key, which assumes a single tree per file. It must be updated to support caching multiple trees per file (e.g., script, template, style zones) and invalidating them simultaneously upon file changes.

## Major Issues
Issues that should be fixed in the near term.
- [ ] **E1-J.1 JSX Element Extraction Missing** — `crates/pathfinder-treesitter/src/symbols.rs:35`
  The AST symbol extractor does not extract `jsx_element` or `jsx_self_closing_element` nodes under TSX function returns. Semantic addressing of React/JSX elements (e.g., `Component.tsx::ComponentName::return::Button[1]`) is completely unsupported.

## Minor Issues
Style, naming, or minor improvements.
- *No minor issues found in the existing code — current files are well-formulated, modular, and use high-quality idioms.*

## Verification Results
- Lint: PASS
- Tests: PASS (39 passed, 0 failed)
- Build: PASS
- Coverage: N/A

## Dimensions Covered
| Dimension | Status | Files / Queries Examined |
|---|---|---|
| A. Integration Contracts | ⏭ Skipped (no frontend) | Crate is a backend Rust library |
| B. Database & Schema | ⏭ Skipped (no database) | Crate contains no data persistence |
| C. Configuration & Environment | ✅ Checked | Verified no hardcoded secrets, no environment drift |
| D. Dependency Health | ✅ Checked | `Cargo.toml` specifies only active dependencies (`thiserror`, `lru`, `tree-sitter`). Missing `tree-sitter-html` / `tree-sitter-css` flagged as critical |
| E. Test Coverage Gaps | ✅ Checked | Existing code is well-tested (e.g., `test_cache_eviction_lru`, `test_extract_ts_class_with_methods`), but missing tests for E1a and E1-J |
| F. Mobile ↔ Backend | ⏭ Skipped (no mobile) | Not applicable |
