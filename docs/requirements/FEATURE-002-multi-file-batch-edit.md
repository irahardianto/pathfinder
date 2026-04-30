# FEATURE-002: Multi-File Batch Edit

## Status: PROPOSED

## Origin

Agent ergonomics report (2026-04-30) identified that multi-file refactors require N
sequential `replace_body` / `replace_full` / `replace_batch` calls with per-file OCC
hash management. This is the most error-prone workflow in the current API.

## Problem Statement

Current state: `replace_batch` supports multiple edits within a **single file** with
atomic all-or-nothing semantics. But a typical refactoring (e.g., renaming a function,
changing a signature, moving a type) touches 2-5 files. The agent must:

1. Read file A -> get version_hash_A
2. Edit file A -> get new_version_hash_A
3. Read file B -> get version_hash_B
4. Edit file B -> get new_version_hash_B
5. Repeat for each file

Each step is a separate round-trip. The agent must maintain a hash map. If step 4
fails (VERSION_MISMATCH), there's no rollback of step 2 — partial state.

## Proposed API

### `pathfinder_multi_file_batch`

```rust
struct MultiFileBatchParams {
    /// List of edits grouped by file. Each group has:
    ///   - filepath
    ///   - base_version (OCC)
    ///   - edits (same BatchEdit format as replace_batch)
    groups: Vec<FileEditGroup>,
    /// Abort all if any single edit fails validation
    ignore_validation_failures: bool,
}

struct FileEditGroup {
    filepath: String,
    base_version: String,
    edits: Vec<BatchEdit>,
}

struct MultiFileBatchResponse {
    success: bool,
    /// Per-file results with new version hashes
    results: Vec<FileEditResult>,
    /// Aggregated validation across all files
    validation: AggregatedValidation,
}

struct FileEditResult {
    filepath: String,
    new_version_hash: String,
    edits_applied: usize,
}

struct AggregatedValidation {
    status: String, // "passed", "failed", "uncertain", "skipped"
    per_file: HashMap<String, EditValidation>,
}
```

## Design Considerations

1. **Atomicity scope**: Full atomicity (rollback all files on any failure) requires
   keeping original content for all files in memory. For 5 files at 100KB each = 500KB.
   Acceptable. Alternative: best-effort (rollback only failed file) — simpler but
   leaves partial state.

2. **OCC semantics**: Each file group has its own `base_version`. The tool checks all
   versions before applying any edits. If any version mismatches, the entire batch fails
   before any writes.

3. **LSP validation**: Run validation per-file after all edits are applied. Aggregate
   results. If any file has errors, the agent decides (via `ignore_validation_failures`).

4. **Ordering**: Files are edited in the order given. Within each file, edits are applied
   back-to-front (same as `replace_batch`).

5. **Token cost**: The response includes per-file hashes and validation. For 5 files,
   this is ~500 tokens. Acceptable.

## Open Questions

- Maximum number of files per batch? Suggest 10.
- Should there be a dry-run mode (like `validate_only` but for multi-file)?
- How to handle partial LSP warmup (some files validate, others skip)?

## Reconsider Triggers

- Agents routinely make 3+ sequential single-file edits in a single refactoring session
- User reports of partial-state commits (file A edited, file B failed, no rollback)
- DeepSource reports high cyclomatic complexity in agent refactoring workflows

## Priority

Medium. Not a bug — current tools work. This is an ergonomics improvement that reduces
agent error rate and round-trip count.

## Dependencies

- PATCH-001..003 (name_column fix) — navigation tools must work for agents to discover
  cross-file dependencies before editing
- PATCH-008 (validation honesty) — aggregated validation needs reliable per-file status
