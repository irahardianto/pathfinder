# PATCH-004: Explore Structure Mode Disambiguation

Date: 2026-06-22
Source: Pathfinder report (Rust stack) — "files_scanned: 0 confusion"
Priority: P1 — ergonomic friction; agents waste tokens on unnecessary retries
Status: Spec — awaiting implementation

## Problem Statement

`explore(detail="structure")` returns `files_scanned: 0` in structured
metadata. This is correct — structure mode reads directory names only,
not source files. But agents see `files_scanned: 0` and assume the call
failed, triggering unnecessary retries that waste tokens.

v1 addressed this with documentation only — SKILL.md:134 and tool
description both explain that `files_scanned: 0` is expected for
structure mode. But documentation cannot intercept programmatic
heuristics like `if meta.files_scanned == 0: retry()`.

### Root Cause

File: `crates/pathfinder-treesitter/src/repo_map.rs:664`

```rust
Ok(RepoMapResult {
    skeleton: skeleton_out.trim().to_string(),
    tech_stack: tech_stack.iter().map(|l| l.as_str().to_owned()).collect(),
    files_scanned: 0,        // hardcoded 0 for structure mode
    files_truncated: 0,
    truncated_paths: vec![],
    files_in_scope: manifests.len(),
    coverage_percent: 100,
    version_hashes: HashMap::default(),
})
```

File: `crates/pathfinder/src/server/types.rs:265`

```rust
pub files_scanned: usize,   // serializes as 0, never null
```

No field in `GetRepoMapMetadata` distinguishes "0 because structure mode"
from "0 because failure." Both cases produce identical JSON.

### Agent Impact

Agents that:
- Check `files_scanned == 0` as a failure heuristic → retry unnecessarily
- Aggregate `files_scanned` across calls → report "0 files scanned" for
  structure-mode explore (misleading metric)
- Parse `structured_content` JSON only (no text/SKILL.md access) → no way
  to know structure mode is the cause

---

## DELIVERABLE A: Add `mode` Field to Metadata

Priority: P1
Effort: Low (20 minutes)
Risk: Low (additive field)

**Steps**:

1. In `crates/pathfinder/src/server/types.rs`, add to
   `GetRepoMapMetadata`:

   ```rust
   /// The detail mode used for this response: `"structure"`, `"files"`,
   /// or `"symbols"`.
   ///
   /// Agents can check this to interpret `files_scanned` correctly:
   /// - `"structure"`: `files_scanned` is always 0 (dirs only, not source
   ///   files). Check `dirs_scanned` for directory count.
   /// - `"files"`: `files_scanned` counts files listed.
   /// - `"symbols"`: `files_scanned` counts files with extracted symbols.
   #[serde(skip_serializing_if = "Option::is_none")]
   pub mode: Option<String>,
   ```

2. In `crates/pathfinder/src/server/tools/repo_map.rs`, set `mode` based
   on `params.detail`:

   ```rust
   let mode = match params.detail {
       Detail::Structure => Some("structure".to_string()),
       Detail::Files => Some("files".to_string()),
       Detail::Symbols => Some("symbols".to_string()),
   };

   let metadata = crate::server::types::GetRepoMapMetadata {
       tech_stack: result.tech_stack,
       mode,
       // ... rest unchanged
   };
   ```

3. Also add to the empty-changes response at line 129.

**Files to modify**:
- `crates/pathfinder/src/server/types.rs` — add `mode` field
- `crates/pathfinder/src/server/tools/repo_map.rs` — set `mode`

**Acceptance**:
- `explore(detail="structure")` response includes `mode: "structure"`
- `explore(detail="files")` response includes `mode: "files"`
- `explore(detail="symbols")` response includes `mode: "symbols"`
- Agent can check `mode == "structure"` to know `files_scanned: 0` is
  expected

---

## DELIVERABLE B: Add `dirs_scanned` Field for Structure Mode

Priority: P1
Effort: Low (30 minutes)
Risk: Low (additive field)

**Steps**:

1. In `crates/pathfinder/src/server/types.rs`, add to
   `GetRepoMapMetadata`:

   ```rust
   /// Number of directories scanned during repository mapping.
   ///
   /// Only populated for `detail="structure"` mode (counts directories
   /// in the tree). Absent for `files` and `symbols` modes.
   #[serde(skip_serializing_if = "Option::is_none")]
   pub dirs_scanned: Option<usize>,
   ```

2. In `crates/pathfinder-treesitter/src/repo_map.rs`, modify
   `generate_structure_skeleton` to count directories and return them:

   ```rust
   // In generate_structure_skeleton:
   // Count directories in the skeleton tree
   let dirs_scanned = count_dirs_in_skeleton(&skeleton_out);

   Ok(RepoMapResult {
       skeleton: skeleton_out.trim().to_string(),
       tech_stack: tech_stack.iter().map(|l| l.as_str().to_owned()).collect(),
       files_scanned: 0,
       dirs_scanned: Some(dirs_scanned),   // NEW
       // ... rest unchanged
   })
   ```

   Add `dirs_scanned: Option<usize>` to `RepoMapResult` struct
   (`repo_map.rs:128` area).

3. In `generate_files_skeleton` and `generate_symbols_skeleton`, set
   `dirs_scanned: None` (not applicable for those modes).

4. In `crates/pathfinder/src/server/tools/repo_map.rs`, propagate
   `dirs_scanned` to `GetRepoMapMetadata`:

   ```rust
   let metadata = crate::server::types::GetRepoMapMetadata {
       tech_stack: result.tech_stack,
       mode,
       dirs_scanned: result.dirs_scanned,
       // ... rest unchanged
   };
   ```

5. For the empty-changes response (line 129), set `dirs_scanned: None`.

**Files to modify**:
- `crates/pathfinder-treesitter/src/repo_map.rs` — add field to
  `RepoMapResult`, populate in `generate_structure_skeleton`
- `crates/pathfinder/src/server/types.rs` — add field to
  `GetRepoMapMetadata`
- `crates/pathfinder/src/server/tools/repo_map.rs` — propagate field

**Acceptance**:
- `explore(detail="structure")` response includes `dirs_scanned: <N>`
  where N > 0 (count of directories)
- `explore(detail="files")` response: `dirs_scanned` absent (None)
- `explore(detail="symbols")` response: `dirs_scanned` absent (None)
- Agent can check `dirs_scanned > 0` to confirm structure mode succeeded

---

## DELIVERABLE C: Update Tool Description and SKILL.md

Priority: P2
Effort: Low (15 minutes)
Risk: None

**Steps**:

1. In `crates/pathfinder/src/server.rs` (or wherever the explore tool
   description is defined), update the tool description:

   ```
   NOTE: files_scanned=0 in metadata is EXPECTED for detail="structure"
   mode — structure mode reads directory names only. Check the `mode`
   field to confirm which mode was used. For structure mode, `dirs_scanned`
   indicates the directory count.
   ```

2. In `docs/agent_directives/skills/pathfinder/SKILL.md`, update the
   gotcha section (around line 134):

   Before:
   ```markdown
   For `detail="structure"`, metadata shows `files_scanned: 0` — **this is correct**.
   Structure mode reads directory names and manifest files only, not source files.
   The actual directory tree IS in the text output.
   ```

   After:
   ```markdown
   For `detail="structure"`, metadata shows `files_scanned: 0` — **this is correct**.
   Check the `mode` field (`"structure"`) and `dirs_scanned` field (directory count)
   to confirm the call succeeded. Structure mode reads directory names only, not
   source files. The actual directory tree IS in the text output.
   ```

**Files to modify**:
- `crates/pathfinder/src/server.rs` (tool description) or relevant schema file
- `docs/agent_directives/skills/pathfinder/SKILL.md`

**Acceptance**:
- Tool description mentions `mode` and `dirs_scanned` fields
- SKILL.md references the new fields instead of only prose explanation

---

## DELIVERABLE D: Tests

Priority: P1
Effort: Low (20 minutes)
Risk: None

**Steps**:

Add tests to `crates/pathfinder/src/server/tools/repo_map_test.rs`:

1. `test_explore_structure_mode_has_mode_and_dirs_scanned`
   - Call explore with `detail="structure"`
   - Assert: `mode == Some("structure")`
   - Assert: `dirs_scanned` is `Some(n)` where `n > 0`
   - Assert: `files_scanned == 0` (unchanged)

2. `test_explore_files_mode_has_mode_no_dirs_scanned`
   - Call explore with `detail="files"`
   - Assert: `mode == Some("files")`
   - Assert: `dirs_scanned == None`
   - Assert: `files_scanned > 0`

3. `test_explore_symbols_mode_has_mode_no_dirs_scanned`
   - Call explore with `detail="symbols"`
   - Assert: `mode == Some("symbols")`
   - Assert: `dirs_scanned == None`

4. `test_explore_structure_dirs_scanned_matches_tree`
   - Call explore with `detail="structure"` on a known directory structure
   - Assert: `dirs_scanned` matches the count of directory entries in the
     skeleton text output

**Files to modify**:
- `crates/pathfinder/src/server/tools/repo_map_test.rs`
- `crates/pathfinder-treesitter/src/repo_map_test.rs` (if unit tests for
  `generate_structure_skeleton` are there)

**Acceptance**:
- All 4 tests pass
- Tests verify `mode` and `dirs_scanned` are correctly populated per mode

---

## Dependency Order

```
A (mode field) → C (docs)
B (dirs_scanned field) → C (docs)
A + B → D (tests)
```

A and B are independent — can be done in parallel.
C depends on both A and B.
D depends on A and B.

## Verification Plan

```bash
cargo test -p pathfinder repo_map
cargo test -p pathfinder-mcp-treesitter repo_map
cargo clippy -- -D warnings
```

Manual verification:
- Call `explore(detail="structure")` on any project
- Verify response JSON has `mode: "structure"` and `dirs_scanned: <N>`
- Call `explore(detail="files")`
- Verify response has `mode: "files"` and `dirs_scanned` absent

Total effort: ~1 hour
