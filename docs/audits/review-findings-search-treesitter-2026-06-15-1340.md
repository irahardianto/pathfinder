# Code Audit: pathfinder-search & pathfinder-treesitter
Date: 2026-06-15

## Summary
- **Files reviewed:** 18 source files + 2 Cargo.toml + 4 test files + 6 bench files = 30 total
- **Issues found:** 11 (0 critical, 3 major, 5 minor, 3 nit)
- **Test coverage:** 191 tests pass (0 failures), ~250 test functions across both crates
- **Dimensions activated:** C, D, E (Skipping A — no frontend/backend boundary; B — no database; F — no mobile app)

## Verification Results
- Lint: **PASS** (`cargo clippy -p pathfinder-mcp-search -p pathfinder-mcp-treesitter -- -D warnings` — zero warnings)
- Tests: **PASS** (191 passed, 0 failed, 0 ignored across 4 test binaries + 2 doc-test suites)
- Format: **PASS** (`cargo fmt -- --check` — no diffs)
- Build: **PASS** (compiled cleanly)

---

## Major Issues

### M1. `cache.rs` — Error-cloning `match` block duplicated 4 times (~80 lines)

**File:** `crates/pathfinder-treesitter/src/cache.rs` — lines 234, 360, 512, 654

`SurgeonError` does not implement `Clone`, forcing a 22-line manual match block to clone each variant at 4 call sites. This is the largest DRY violation across both crates.

```rust
// This exact block appears at L234, L360, L512, L654:
match result.as_ref() {
    Ok((tree, source)) => Ok((tree.clone(), source.clone())),
    Err(SurgeonError::FileNotFound(p)) => Err(SurgeonError::FileNotFound(p.clone())),
    Err(SurgeonError::UnsupportedLanguage(p)) => Err(SurgeonError::UnsupportedLanguage(p.clone())),
    Err(SurgeonError::ParseError { path: p, reason }) => Err(SurgeonError::ParseError {
        path: p.clone(), reason: reason.clone(),
    }),
    Err(SurgeonError::SymbolNotFound { path: p, did_you_mean: dym }) => Err(SurgeonError::SymbolNotFound {
        path: p.clone(), did_you_mean: dym.clone(),
    }),
    Err(SurgeonError::Io(e)) => Err(SurgeonError::ParseError {
        path: path.to_path_buf(), reason: e.to_string(),
    }),
}
```

**Fix options:**
1. Derive `Clone` on `SurgeonError` (replace `Io(std::io::Error)` with a string-wrapped variant or `Arc<io::Error>` since `io::Error` is not `Clone`)
2. Extract `fn clone_surgeon_result<T: Clone>(result: &Result<T, SurgeonError>, fallback_path: &Path) -> Result<T, SurgeonError>` helper

---

### M2. `ripgrep.rs` — `walk_files()` double-walk with duplicated filtering (137 lines)

**File:** `crates/pathfinder-search/src/ripgrep.rs` — lines 386–522

The function walks the entire workspace **twice** — once with `.gitignore` disabled (to count `files_in_scope` pre-gitignore) and once with `.gitignore` enabled (for actual files). The filtering logic (ALWAYS_EXCLUDED_DIRS, glob matching, exclude_glob, binary extension check) is copy-pasted between the two loops (L432-467 vs L469-514).

Additional bug: `binary_skipped` count is only incremented in the second walker (L509), not the first (L462 silently skips). This means `gitignored_skipped` calculation at L518-520 may be inaccurate — it subtracts `binary_skipped` from the first walker's count but `binary_skipped` only reflects the second walker.

**Fix:** Extract a `fn filter_entry(relative: &str, glob_matcher: &GlobSet, exclude_matcher: &Option<GlobSet>, path: &Path) -> FilterResult` helper used by both loops.

---

### M3. `repo_map.rs` — `SymbolKind` → prefix match block duplicated verbatim

**File:** `crates/pathfinder-treesitter/src/repo_map.rs` — lines 204–221 and 239–255

The 15-arm `match sym.kind` → `&'static str` block is duplicated between `render_symbols_recursive()` and `render_truncated_file_skeleton()`.

**Fix:** Extract `fn symbol_prefix(kind: SymbolKind) -> &'static str`.

---

## Minor Issues

### m1. `ripgrep.rs` — Duplicate entry "o" in `BINARY_EXTENSIONS`

**File:** `crates/pathfinder-search/src/ripgrep.rs` — line 35

`"o"` appears at positions within both the `.o` (object file) group and again later. Harmless but sloppy.

```rust
// L35: "o" appears twice in the same const array
"dll", "so", "dylib", "o", "a", "lib", "obj", "wasm", "class", "jar", "pyc", "pyo", "o",
//                    ^^^                                                             ^^^
```

---

### m2. `ripgrep.rs` — Missing duration logging in search operation

**File:** `crates/pathfinder-search/src/ripgrep.rs` — lines 531-617

The search operation logs start (L531-537) and completion (L610-617) but does not record `duration` in the completion log. The logging mandate requires duration measurement on all operation entry points.

**Fix:** Capture `Instant::now()` at operation start, record elapsed at completion:
```rust
let start = std::time::Instant::now();
// ... search logic ...
tracing::debug!(
    duration_ms = start.elapsed().as_millis() as u64,
    total_matches, returned, truncated, files_searched, files_in_scope,
    "Scout: search complete"
);
```

---

### m3. `ripgrep.rs` — `MatchCollector::new()` takes 8 positional arguments

**File:** `crates/pathfinder-search/src/ripgrep.rs` — line 177

Suppressed with `#[allow(clippy::too_many_arguments)]`. While the struct is internal, a config struct or builder would improve readability.

---

### m4. `test_symbols.rs` — Dead no-op test file

**File:** `crates/pathfinder-treesitter/src/test_symbols.rs` — 3 lines

```rust
#[test]
fn test_enclosing_symbol_impl_block() {}
```

This test has an empty body — it asserts nothing. Either implement it or delete the file.

---

### m5. `ripgrep.rs` — `make_workspace` test helper duplicated across test modules

**File:** `crates/pathfinder-search/src/ripgrep.rs` — L645-656 and L1557-1568

The same `fn make_workspace(files: &[(&str, &str)]) -> TempDir` helper is defined identically in both `mod tests` and `mod batch03c_tests`. Should be a shared test utility (e.g., a `#[cfg(test)]` module at crate level or in a `tests/common.rs`).

---

## Nit Issues

### n1. Clone calls lack `// CLONE:` justification comments

**Files:** `crates/pathfinder-search/src/ripgrep.rs` (6 clone sites in production code)

Per Rust idioms rule: "Never `.clone()` to silence the borrow checker without a `// CLONE:` comment." All clones are justified by usage patterns (move into closures, cache extraction, dual-use strings) but none have the mandated comments. The treesitter crate does include `// CLONE:` comments at its clone sites — search crate should follow the same convention.

### n2. `SearchParams` has no input validation

**File:** `crates/pathfinder-search/src/types.rs`

No bounds checking on `max_results`, `context_lines`, or `offset`. While validation presumably happens upstream at the MCP handler, defensive validation at the type boundary would be more rugged. Not blocking since `max_results` is capped at 256 in `Vec::with_capacity()` (L544 of ripgrep.rs).

### n3. `SurgeonError` missing `Clone` derive affects downstream ergonomics

**File:** `crates/pathfinder-treesitter/src/error.rs`

Noted as the root cause of M1. The `Io(std::io::Error)` variant prevents `#[derive(Clone)]`. If `Clone` were added (wrapping `Io` in `Arc` or converting to a string representation), the 4 manual clone blocks in cache.rs would collapse to `.clone()`.

---

## Positive Observations

Both crates demonstrate strong engineering quality:

1. **Zero `unsafe` code** — entire search crate and treesitter crate have no unsafe blocks. Tree-sitter FFI safety achieved via `thread_local!` + `RefCell` + `parking_lot::Mutex`.
2. **Zero unwrap/expect in production code** — only in tests and mathematically infallible `NonZeroUsize::new(32)`.
3. **Comprehensive test suites** — ~64 tests in search, ~191 total across both crates. Coverage includes edge cases (UTF-8 boundaries, context overlap, concurrent stress, cache eviction, singleflight deduplication).
4. **Clean trait-based I/O boundaries** — `Scout` trait (search) and `Surgeon` trait (treesitter) with production + mock implementations. Textbook testability-first architecture.
5. **Proper error types** — `thiserror` derive, typed enums, `Result`-based throughout. No string errors.
6. **Good observability** — `#[instrument]` spans with `cache_hit` fields, `tracing::warn!` on recoverable errors, `tracing::debug!` on expected failures.
7. **Performance-conscious design** — thread-local regex cache with LRU, 3-tier cache invalidation (mtime → content-hash → singleflight), `TeeHasher` for zero-copy hashing, `Rc<str>` for shared file paths.
8. **Mutex poison recovery** — `MockScout` uses `unwrap_or_else(PoisonError::into_inner)` instead of panicking.
9. **Parse timeout** — tree-sitter parser has 500ms timeout preventing DoS from adversarial inputs.
10. **Benchmark suites** — both crates include criterion benchmarks for performance regression detection.

---

## Dimensions Covered

| Dimension | Status | Files / Queries Examined |
|---|---|---|
| A. Integration Contracts | ⏭ Skipped | No frontend/backend HTTP boundary in these crates |
| B. Database & Schema | ⏭ Skipped | No database usage |
| C. Configuration & Environment | ✅ Checked | No hardcoded secrets; cache sizes configurable via constructor params; no env vars read directly |
| D. Dependency Health | ✅ Checked | Both Cargo.toml reviewed. 12 deps (search) + 20 deps (treesitter). All pinned to major versions. No unused top-level deps detected. No circular dependencies — clean layered architecture (search → common, treesitter → common). |
| E. Test Coverage Gaps | ✅ Checked | All public API methods have test coverage. Mock implementations exist for both traits. Integration tests exist for treesitter (Java fixtures, Rust impl blocks). Gap: `test_symbols.rs` is a dead no-op test (m4). |
| F. Mobile ↔ Backend | ⏭ Skipped | No mobile app |
