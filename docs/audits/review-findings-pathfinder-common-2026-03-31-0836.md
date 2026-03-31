# Code Audit: pathfinder-common
Date: 2026-03-31

## Summary
- **Files reviewed:** 8
- **Issues found:** 3 (2 critical, 1 major, 0 minor)
- **Test coverage:** N/A
- **Dimensions activated:** C, D. Skipped A, B, E, F (not applicable to a backend library crate).

## Critical Issues
Issues that must be fixed before deployment.
- [x] **[SEC]** Sandbox bypass via path traversal — `Sandbox::check` does not reject strings with `..` components or absolute paths. An attacker can escape the workspace by providing paths like `../../etc/passwd` or `/etc/passwd`. — `crates/pathfinder-common/src/sandbox.rs`:112
- [x] **[SEC]** `WorkspaceRoot::resolve` allows absolute path replacement — If the `relative_path` is absolute, `PathBuf::join` ignores the base path entirely. — `crates/pathfinder-common/src/types.rs`:234

## Major Issues
Issues that should be fixed in the near term.
- [x] **[RES]** Unbounded memory allocation during file hashing — `hash_file` uses `tokio::fs::read` which loads the entire file into memory. A large file could cause OOM. Should stream the file and hash incrementally. — `crates/pathfinder-common/src/file_watcher.rs`:132

## Minor Issues
Style, naming, or minor improvements.
- None

## Verification Results
- Lint: PASS
- Tests: PASS (66 passed, 0 failed)
- Build: PASS
- Coverage: N/A

## Dimensions Covered
| Dimension | Status | Files / Queries Examined |
|---|---|---|
| A. Integration Contracts | ⏭ Skipped (reason) | Not applicable, backend library only |
| B. Database & Schema | ⏭ Skipped (reason) | Not applicable, no database access |
| C. Configuration & Environment | ✅ Checked | Checked config.rs secrets handling |
| D. Dependency Health | ✅ Checked | Checked Cargo.toml for health and vulnerabilities |
| E. Test Coverage Gaps | ⏭ Skipped (reason) | Not applicable, no handlers or endpoints |
| F. Mobile ↔ Backend | ⏭ Skipped (reason) | Not applicable, no mobile app |
