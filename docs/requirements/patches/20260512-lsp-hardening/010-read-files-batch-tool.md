# 010: `read_files` Batch Tool

**Epic**: 4 — New Tools
**Status**: ☐ Pending
**Severity**: Medium (agent ergonomics)
**Risk**: Higher — new MCP tool, schema addition

---

## Problem

Multi-file operations (code audits, refactors, dependency analysis) require agents to make one `read_source_file` or `read_file` call per file. A 5-file audit costs 5 tool calls, each with round-trip latency and context window overhead from the MCP protocol envelope.

### Current Flow

```
# Agent needs to read 4 related files
read_source_file(filepath="src/auth.ts")        → 1 call
read_source_file(filepath="src/auth.spec.ts")    → 1 call
read_source_file(filepath="src/config.ts")       → 1 call
read_file(filepath="tsconfig.json")              → 1 call
# Total: 4 calls, 4 round-trips, 4 protocol envelopes
```

### Desired Flow

```
read_files(paths=["src/auth.ts", "src/auth.spec.ts", "src/config.ts", "tsconfig.json"])
# Total: 1 call, 1 round-trip, 1 protocol envelope
```

---

## Proposed Solution

New MCP tool that reads multiple files in a single call with per-file error resilience:

### Input Schema

```json
{
  "paths": ["src/auth.ts", "src/auth.spec.ts", "src/config.ts", "tsconfig.json"],
  "detail_level": "source_only",   // Optional: "source_only" | "compact" | "full"
  "max_lines_per_file": 500        // Optional: default 500
}
```

### Output Schema

```json
{
  "files": [
    {
      "path": "src/auth.ts",
      "content": "import { Config } from './config';\n...",
      "language": "typescript",
      "total_lines": 142,
      "version_hash": "abc123"
    },
    {
      "path": "src/auth.spec.ts",
      "content": "import { AuthService } from './auth';\n...",
      "language": "typescript",
      "total_lines": 89,
      "version_hash": "def456"
    },
    {
      "path": "src/config.ts",
      "error": "file not found"
    },
    {
      "path": "tsconfig.json",
      "content": "{\n  \"compilerOptions\": {\n...",
      "language": null,
      "total_lines": 25,
      "version_hash": "ghi789"
    }
  ],
  "succeeded": 3,
  "failed": 1
}
```

### Constraints

- **Max 10 files per call** — prevents abuse and keeps response size bounded
- **Per-file error handling** — individual file errors don't fail the batch
- **Sandbox enforced** — each path checked against sandbox rules
- **Detail levels** — same as `read_source_file`: `source_only`, `compact`, `full`
- **Source vs config routing** — files with AST-parseable extensions use `read_source_file` logic; others use `read_file` logic

### Files to Create/Modify

| File | Change |
|------|--------|
| `crates/pathfinder/src/server/tools/read_files.rs` | **[NEW]** Tool implementation |
| `crates/pathfinder/src/server/tools/mod.rs` | Register new module |
| `crates/pathfinder/src/server/types.rs` | Add `ReadFilesParams`, `ReadFilesResponse`, `FileResult` types |
| `crates/pathfinder/src/server.rs` | Register tool in MCP schema |

---

## Acceptance Criteria

- [ ] Tool registered in MCP tool list with JSON schema
- [ ] Accepts 1–10 file paths in a single call
- [ ] Returns `>10 paths` → error with clear message
- [ ] Each file result includes `content`, `language`, `total_lines`, `version_hash` on success
- [ ] Each file result includes `error` string on failure (file not found, sandbox denied, etc.)
- [ ] Individual file errors don't fail the batch — `succeeded` and `failed` counts accurate
- [ ] Sandbox check applied per-file
- [ ] Source files get AST-based `detail_level` processing (symbols, etc.)
- [ ] Config files (`json`, `yaml`, `toml`, `env`, `Dockerfile`) get raw content
- [ ] `version_hash` matches what `read_source_file` would return for the same file
- [ ] Response size bounded by `max_lines_per_file` per file

---

## Test Plan

| Test | Description |
|------|-------------|
| `test_read_files_happy_path` | 3 valid files → all succeed with content |
| `test_read_files_partial_failure` | 2 valid + 1 missing → 2 succeed, 1 error |
| `test_read_files_sandbox_denial` | Path containing `.git/` → individual error |
| `test_read_files_max_limit` | 11 paths → rejected with error |
| `test_read_files_empty_paths` | Empty array → empty result |
| `test_read_files_mixed_source_and_config` | `.rs` + `.toml` → both handled correctly |
| `test_read_files_version_hash_consistency` | Same file via `read_files` and `read_source_file` → same hash |

---

## Verification

```bash
cargo test -p pathfinder-mcp -- read_files
cargo clippy -p pathfinder-mcp -- -D warnings
```

---

## Performance Considerations

Files are read sequentially (not concurrently) to avoid file descriptor exhaustion on large batches. For 10 files of ~500 lines each, total I/O time is negligible (~5ms). The primary cost is tree-sitter parsing for source files (~10ms per file).

If benchmarking shows latency issues, concurrent reads with `futures::future::join_all` can be added as an optimization, bounded by a semaphore (max 5 concurrent reads).
