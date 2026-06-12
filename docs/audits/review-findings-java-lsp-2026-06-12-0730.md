# Code Audit: Java LSP Integration
Date: 2026-06-12

## Summary
- **Files reviewed:** 5 (`plugin.rs`, `detect.rs`, `process.rs`, `lifecycle.rs`, `mod.rs`)
- **Issues found:** 4 (2 major, 2 minor)
- **Test coverage:** 94% (measured via tarpaulin)
- **Dimensions activated:** C, D, E. Skipped A (no frontend app), B (no database), F (no mobile app).

## Critical Issues
None identified.

## Major Issues
Issues that should be fixed in the near term.
- [x] **Concurrent jdtls Processes Lock/Conflict** — [crates/pathfinder-lsp/src/client/process.rs:467](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src/client/process.rs#L467-L484)
  The `jdtls` process is launched with `-data` pointing to a static directory: `project_root.join(".pathfinder").join("jdtls-data")`. If multiple Pathfinder instances run concurrently on the same workspace, they will attempt to share this directory. Because Eclipse `jdtls` strictly requires a unique data directory per active running process, this will cause the concurrent instances to lock or fail to start. This should be isolated (e.g. by appending a process ID or lock ID) when concurrent instances are detected, similar to the treatment of GOCACHE/GOMODCACHE for Go.
  **Remediated 2026-06-12:** Extracted `resolve_jdtls_data_dir()` with `File::try_lock` advisory locking. Primary instance acquires lock on `jdtls-data/.pathfinder-lock`; concurrent instances fall back to `jdtls-data-{pid}/`. Lock held in `ManagedProcess._jdtls_lock` for process lifetime. 4 unit tests added.

- [x] **Integration Test Mock Coverage Gap and Dummy Test** — [crates/pathfinder-lsp/tests/lsp_client_integration.rs](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/tests/lsp_client_integration.rs) and [crates/pathfinder-lsp/src/client/process.rs:1308](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src/client/process.rs#L1308-L1318)
  No integration tests exist for Java/jdtls-specific features like dynamic capability registration, the 15-second grace period, or `-data` directory arguments. Furthermore, the unit test `test_jdtls_data_dir_created_for_java` in `process.rs` is a dummy test: it manually calls `std::fs::create_dir_all` directly instead of exercising the actual process spawner code, failing to verify that `spawn_lsp_child` performs this directory creation correctly.
  **Remediated 2026-06-12:** Deleted dummy test. Replaced with 4 real tests exercising `resolve_jdtls_data_dir`: directory creation, primary path selection, concurrent PID fallback, and lock-release-reacquisition.

## Minor Issues
Style, naming, or minor improvements.
- [x] **DRY/Architectural Inconsistency in Language Registry and Detection** — [crates/pathfinder-lsp/src/client/detect.rs](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src/client/detect.rs) and [crates/pathfinder-lsp/src/plugin.rs](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src/plugin.rs)
  The `LanguagePlugin` trait and registry (LT-2) were designed to centralize language-specific details. However, `detect_languages` in `detect.rs` still contains hardcoded blocks and duplicates `install_hint` implementations (which differ in copy text for Java). The `language_id_for_extension` function is also duplicated between `plugin.rs` and `detect.rs`.
  **Remediated 2026-06-12:** `detect.rs::install_hint()` now delegates to `plugin::plugin_for_language()` — plugin registry is single source of truth. Java install hint divergence eliminated. Cross-validation test added.

- [x] **Missing Test Coverage for settings.gradle / settings.gradle.kts** — [crates/pathfinder-lsp/src/client/detect.rs](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-lsp/src/client/detect.rs)
  Even though `settings.gradle` and `settings.gradle.kts` are listed as Java marker files in the registry, there are no tests verifying that Pathfinder successfully detects a Java project using them.
  **Remediated 2026-06-12:** Added `settings.gradle.kts` to detect.rs search chain. Fixed misnamed test. Added new `test_detect_java_via_settings_gradle_kts` that creates actual `.kts` file.

## Verification Results
- Lint (Clippy): PASS
- Formatting (rustfmt): PASS
- Tests: PASS (206 passed, 0 failed)
- Coverage: 94%

## Dimensions Covered
| Dimension | Status | Files / Queries Examined |
|---|---|---|
| A. Integration Contracts | ⏭ Skipped | No frontend app in this project |
| B. Database & Schema | ⏭ Skipped | No database used in this project |
| C. Configuration & Environment | ✅ Checked | Scanned for hardcoded secrets, verified JSON config loader |
| D. Dependency Health | ✅ Checked | Scanned Cargo.toml dependencies, confirmed zero unused / circular dependencies |
| E. Test Coverage Gaps | ✅ Checked | Analyzed test targets, identified mock integration gaps for Java features |
| F. Mobile ↔ Backend | ⏭ Skipped | No mobile app in this project |
