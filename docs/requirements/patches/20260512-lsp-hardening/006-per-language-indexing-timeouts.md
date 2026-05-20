# 006: Per-Language Indexing Timeouts

**Epic**: 2 — LSP Hardening
**Status**: ✅ Complete
**Severity**: Medium
**Risk**: Low — configuration change, no behavioral logic change
**Enhances**: LSP-HEALTH-001 Task 6 (`spawn_indexing_timeout_fallback`, already implemented with flat 30s)

---

## Problem

The current `spawn_indexing_timeout_fallback` in `LspClient` uses a flat 30-second timeout for all non-Rust language servers. When the timeout fires without receiving a `$/progress` `WorkDoneProgressEnd` notification, it assumes indexing is complete.

This causes issues for:

- **Java (jdtls)**: Eclipse JDT Language Server regularly needs 60–120s for initial workspace indexing on large Java projects. A 30s timeout prematurely marks indexing as complete, leading to incomplete symbol resolution.
- **TypeScript**: Large monorepos with `tsconfig.json` project references can take 30–45s. The current timeout is borderline.
- **Python (pyright)**: Typically fast (10–15s), but virtual environments with many installed packages can extend this.

### Current Code

```rust
// In spawn_indexing_timeout_fallback:
tokio::time::sleep(Duration::from_secs(30)).await;
```

No language differentiation.

---

## Proposed Solution

Replace the flat timeout with a language-specific lookup:

```rust
fn indexing_timeout_for_language(lang: &str) -> Duration {
    match lang {
        "java" => Duration::from_secs(120),
        "typescript" | "javascript" => Duration::from_secs(45),
        "go" => Duration::from_secs(30),
        "python" => Duration::from_secs(30),
        "rust" => Duration::from_secs(60), // rust-analyzer handles its own progress
        _ => Duration::from_secs(30),
    }
}
```

### Files to Modify

| File | Change |
|------|--------|
| `crates/pathfinder-lsp/src/client/mod.rs` | Add `indexing_timeout_for_language()` function; use in `spawn_indexing_timeout_fallback` |

---

## Acceptance Criteria

- [x] `indexing_timeout_for_language` returns language-specific `Duration`
- [x] Java timeout is 120s
- [x] TypeScript/JavaScript timeout is 45s
- [x] Default timeout remains 30s for unrecognized languages
- [x] Timeout log message includes the language and configured duration
- [x] No change to the timeout behavior — still marks `indexing_complete = true` on expiry

---

## Test Plan

| Test | Description |
|------|-------------|
| `test_indexing_timeout_java_is_120s` | Direct function test |
| `test_indexing_timeout_typescript_is_45s` | Direct function test |
| `test_indexing_timeout_unknown_is_30s` | Direct function test |
| `test_indexing_timeout_log_includes_language` | Verify tracing span includes language field |

---

## Verification

```bash
cargo test -p pathfinder-mcp-lsp -- indexing_timeout
cargo clippy -p pathfinder-mcp-lsp -- -D warnings
```

---

## Rationale for Specific Values

| Language | Timeout | Justification |
|----------|---------|---------------|
| Java | 120s | Eclipse jdtls needs full classpath resolution, maven/gradle dependency graphs. Based on VSCode Java extension benchmarks for medium projects (~50k LOC). |
| TypeScript | 45s | tsserver project loading + type checking. Based on measured startup times for projects with 3–5 `tsconfig.json` references. |
| Go | 30s | gopls is fast; `go/packages` loading is bounded by `go mod download`. |
| Python | 30s | pyright type checking is fast; main bottleneck is venv package scanning. |
| Rust | 60s | rust-analyzer manages its own `$/progress` notifications, so this timeout is rarely hit. Set higher as a safety net. |
