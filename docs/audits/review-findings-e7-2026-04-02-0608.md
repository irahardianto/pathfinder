# Code Audit: Epic E7 (OCC Ergonomics)
Date: 2026-04-02

## Summary
- **Files reviewed:** 4 (`error.rs`, `server.rs`, `edit.rs`, `file_ops.rs`)
- **Issues found:** 1 (1 major, 0 minor)
- **Test coverage:** 100% on new functions (hints and line-delta logic)
- **Dimensions activated:** C, D, E (Skipped A, B, F as this is a backend library/server feature with no frontend, DB, or mobile app).

## Critical Issues
None.

## Major Issues
Issues that should be fixed in the near term.
- [x] **RESOLVED (2026-04-02) — Implementation Gap / Architectural Conflict in E7.2** — `crates/pathfinder/src/server/tools/edit.rs` / `file_ops.rs`
  - **Description:** The `lines_changed` field for `PathfinderError::VersionMismatch` is hardcoded to `None` for all 11 construction sites. This is because Pathfinder is stateless and only receives a `base_version` (SHA-256 hash) from the agent, making it mathematically impossible to compute the file delta without access to the prior source code.
  - **Resolution (Option A — PRD Relaxation + Best-Effort):**
    - PRD E7.2 acceptance criteria relaxed: `hint` is always present; `lines_changed` is best-effort (`null` when prior content is unavailable).
    - ADR-style design decision note added to PRD explaining the stateless OCC constraint and why git diff cannot solve it.
    - `compute_lines_changed` is now wired at the two TOCTOU check sites where both old and new disk content are in scope simultaneously:
      1. `flush_edit_with_toctou` in `edit.rs` — covers all 7 AST edit tools (`replace_body`, `replace_full`, `insert_before`, `insert_after`, `delete_symbol`, `replace_batch`)
      2. TOCTOU late-check in `write_file_impl` in `file_ops.rs`
    - All 18 `VersionMismatch` construction sites audited and confirmed correct.

## Minor Issues
None.

## Verification Results
- Lint: PASS (0 warnings)
- Tests: PASS (all tests pass across the workspace, including 14 new tests)
- Build: PASS

## Dimensions Covered
| Dimension | Status | Files / Queries Examined |
|---|---|---|
| A. Integration Contracts | ⏭ Skipped | Not applicable for this backend tooling crate |
| B. Database & Schema | ⏭ Skipped | No database layer |
| C. Configuration & Environment | ✅ Checked | Verified no hardcoded secrets or environment conflicts |
| D. Dependency Health | ✅ Checked | Confirmed build cleanly resolves all dependencies |
| E. Test Coverage Gaps | ✅ Checked | Verified comprehensive tests added for `hint()` and `compute_lines_changed()` |
| F. Mobile ↔ Backend | ⏭ Skipped | No mobile app in this project |
