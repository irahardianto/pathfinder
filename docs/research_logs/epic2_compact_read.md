# Epic E2 — Compact Read Modes Research Log

## Requirements
From `docs/requirements/pathfinder-v5-requirements.md`:

### E2.1 Detail Level
Add `detail_level` parameter (`compact`, `symbols`, `full`). Default to `compact`.
- `compact`: Returns full source + flat list of top-level names/kinds.
- `symbols`: Returns `{ version_hash, language, symbols }` without file content.
- `full`: Returns full source + full nested AST.

### E2.2 Line-Range Read
Add `start_line` and `end_line` (optional).
Truncate content and filter symbols overlapping the range. `version_hash` covers the full file.

## Implementation Plan
1. Update `ReadSourceFileArgs` in `crates/pathfinder/src/server/types.rs` or `source_file.rs`.
2. Update `call` logic in `source_file.rs`. Ensure it delegates to `Surgeon` or processes the extracted symbols properly.
3. Write/update tests in `source_file.rs`.
