# Code Audit: pathfinder-search & pathfinder-treesitter

Date: 2026-06-16
Auditor: Antigravity (Claude Opus 4.6)

## Summary

- **Files reviewed:** 16 production source files, 3 integration test files, 6 benchmark files
- **Issues found:** 12 (0 critical, 5 major, 7 minor)
- **Test coverage:** 222 tests passing (206 treesitter + 16 search)
- **Dimensions activated:** C, D, E. Skipped A (library crates, no frontend/backend boundary), B (no database), F (no mobile app)

## Verification Results

- Lint: PASS (`cargo clippy -p pathfinder-mcp-search -p pathfinder-mcp-treesitter -- -D warnings` — zero warnings)
- Tests: PASS (222 passed, 0 failed)
- Build: PASS (included in clippy)
- Advisories: PASS (`cargo deny check advisories` — clean)

---

## Critical Issues

None.

---

## Major Issues

### M-1: `pathfinder-search` has zero `#[instrument]` spans — `ripgrep.rs`

The search crate depends on `tracing` but uses only raw `tracing::debug!` / `tracing::warn!` calls. There are no `#[instrument]` spans on any function. By contrast, `pathfinder-treesitter` uses `#[instrument]` extensively (13 spans across `cache.rs`, `parser.rs`, `treesitter_surgeon.rs`).

**Impact:** Search operations cannot be traced through span hierarchies. No duration recording, no structured field propagation.

**Recommendation:** Add `#[instrument]` to `RipgrepScout::search`, `build_matcher`, and `walk_files`. The existing `tracing::debug!` start/finish logs can remain as span events.

- [ripgrep.rs](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-search/src/ripgrep.rs)

---

### M-2: `walk_files` duplicates filter logic inline instead of calling `filter_entry` — `ripgrep.rs:491-532`

The first walk (no-gitignore pass, L491-535) manually re-implements the always-excluded-dirs check, glob matching, and exclude-glob matching instead of calling `filter_entry`. The second walk (L538-558) correctly calls `filter_entry`. This violates DRY and risks divergence.

The only intentional difference is that the first walk handles binary extensions separately (to count them). This can be preserved by splitting `filter_entry` into a base filter + binary check, or by adding a `skip_binary` parameter.

**Impact:** If filter logic changes (new always-excluded dir, new edge case), only one walk may be updated.

- [ripgrep.rs:491-532](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-search/src/ripgrep.rs#L491-L532)

---

### M-3: `generate_skeleton_text` is 437 lines — `repo_map.rs:410-736`

This function has an `#[expect(clippy::too_many_lines)]` suppression with a reason string. The stated rationale ("splitting would obscure linear data flow") is partially valid, but the function handles 3 distinct modes (Structure, Files, Symbols) and could be decomposed further. The Structure and Files modes are already dispatched to separate functions, but the Symbols mode body (L536-736) is still ~200 lines.

**Impact:** High cognitive load for future maintainers. The function mixes file-walking, symbol extraction, visibility filtering, token budgeting, and output assembly.

- [repo_map.rs:410-736](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-treesitter/src/repo_map.rs#L410-L736)

---

### M-4: Several functions in `symbols.rs` exceed 150 lines

| Function | Lines | Description |
|---|---|---|
| `extract_symbols_from_tree` | ~237 | Main symbol extraction with per-language dispatch |
| `merge_rust_impls` | ~214 | Rust impl block merging |
| `extract_template_elements` | ~238 | Vue template element extraction |
| `extract_css_symbols` | ~167 | CSS symbol extraction |
| `extract_jsx_children` | ~158 | JSX/TSX extraction |

These are complex tree-walking functions where some length is inherent, but several contain repetitive match arms or nested loops that could be extracted into helper functions.

- [symbols.rs](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-treesitter/src/symbols.rs)

---

### M-5: Integration test coverage gaps in `pathfinder-treesitter`

The `tests/` directory has fixture-based integration tests only for:
- Java (13 tests, comprehensive)
- Rust (3 tests, minimal — only impl blocks and top-level fn)

Missing fixture-based integration tests for:
- Go symbol extraction
- TypeScript/TSX symbol extraction
- Python symbol extraction
- JavaScript symbol extraction
- Vue SFC multi-zone symbol extraction
- CSS/HTML symbol extraction
- `find_enclosing_symbol` for non-Rust languages

The inline `#[cfg(test)]` modules cover these languages for unit-level checks, but there are no integration tests exercising the full public API (`extract_symbols_from_tree`) with real source fixtures for most languages.

- [tests/](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-treesitter/tests/)

---

## Minor Issues

### m-1: Duplicate doc comments in `lib.rs` — `pathfinder-treesitter`

Two modules have duplicated doc comments:

```rust
/// Module containing error types and utilities.
/// Module for error types and utilities.
pub mod error;
```

```rust
/// Provides utilities for surgically manipulating Tree-sitter parse trees.
/// Public module providing Tree-sitter-based surgery utilities.
pub mod treesitter_surgeon;
```

- [lib.rs:11-12](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-treesitter/src/lib.rs#L11-L12)
- [lib.rs:25-27](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-treesitter/src/lib.rs#L25-L27)

---

### m-2: ~70 `.clone()` calls without `// CLONE:` justification — `cache.rs`, `treesitter_surgeon.rs`

Per `rust-idioms-and-patterns.md` §1: "Never `.clone()` to silence the borrow checker without a `// CLONE:` comment explaining why."

`cache.rs` has 4 instances WITH the comment (good), but ~35 without. Most are `Arc` clones (cheap) or moves into `spawn_blocking` (required by ownership). `treesitter_surgeon.rs` has ~12 without comments.

**Recommendation:** Add `// CLONE: Arc ref-count bump` or `// CLONE: move into spawn_blocking` comments to the most prominent ones. Skip trivially obvious cases.

- [cache.rs](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-treesitter/src/cache.rs)
- [treesitter_surgeon.rs](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-treesitter/src/treesitter_surgeon.rs)

---

### m-3: `cache.rs` has 4 singleflight functions (97-116 lines each) with near-identical structure

`get_or_parse`, `get_or_parse_preloaded`, `get_or_parse_vue`, `get_or_parse_vue_preloaded` share the same cache-check → singleflight → parse → insert pattern. The preloaded variants differ only in how they read file content (from disk vs from Arc). The Vue variants differ in parser call and cache map.

**Impact:** Code duplication makes bug fixes error-prone (must update 4 places). Consider a generic helper parameterized by parse strategy and cache map.

- [cache.rs](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-treesitter/src/cache.rs)

---

### m-4: `pathfinder-search` has no `#[cfg(test)]` modules in `types.rs` or `searcher.rs`

`searcher.rs` is a trait-only file and `types.rs` is pure data — neither has testable logic, so this is acceptable. However, `types.rs` implements `Default` for `SearchParams` with specific values (e.g., `max_results: 200`, `path_glob: "**/*"`) that could benefit from a regression test.

- [types.rs](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-search/src/types.rs)

---

### m-5: `extract_vue_script` in `language.rs` is 52 lines (slightly over 50-line guideline)

Minor overshoot. Function is a byte-level scanner for `<script>` tags — low cognitive complexity despite length.

- [language.rs](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-treesitter/src/language.rs)

---

### m-6: `mock.rs` (search crate) clones without `// CLONE:` comment

Two instances: `self.calls` vec clone at L74 and `params.clone()` at L91. Both in test infrastructure, low priority.

- [mock.rs](file:///home/irahardianto/works/projects/pathfinder/crates/pathfinder-search/src/mock.rs)

---

### m-7: No doc-tests in either crate

Both crates show `running 0 tests` for doc-tests. Adding `# Examples` sections to key public API items (`Scout::search`, `Surgeon::extract_symbols`, `AstCache::get_or_parse`) would improve discoverability and serve as living documentation.

---

## Positive Findings

These are notable strengths worth preserving:

1. **Workspace-level lint strictness:** `unwrap_used = deny`, `expect_used = warn`, `unsafe_code = deny`, `clippy::pedantic = warn`. Both crates pass with zero warnings.

2. **Zero unsafe code** across all files. No `unsafe` blocks in either crate.

3. **Zero TODO/FIXME/HACK** comments. Clean codebase with no deferred work items.

4. **Clean trait-based I/O abstraction:** `Scout` trait + `MockScout` in search; `Surgeon` trait + `MockSurgeon` in treesitter. Both crates are independently testable.

5. **Error handling consistency:** `thiserror`-based typed error enums throughout. No stringly-typed errors. Proper `?` propagation. Graceful skip-and-log for per-file errors.

6. **Parser timeout:** `AstParser` enforces a 500ms timeout via `ControlFlow::Break` — prevents tree-sitter from blocking on malformed/huge files.

7. **Singleflight pattern in `AstCache`:** Prevents thundering herd on concurrent parses of the same file.

8. **Comprehensive Java fixture tests:** 13 integration tests covering classes, records, sealed types, enums, annotations, inner classes, generics, lambdas, module-info.

9. **Benchmark coverage:** 6 benchmark files covering parsing, extraction, caching, fuzzy matching, and Vue zone scanning at multiple scales.

10. **`cargo deny` advisory check passes:** No known CVEs in dependency tree.

---

## Dimensions Covered

| Dimension | Status | Files / Queries Examined |
|---|---|---|
| A. Integration Contracts | ⏭ Skipped | Library crates — no frontend/backend HTTP boundary |
| B. Database & Schema | ⏭ Skipped | No database usage in either crate |
| C. Configuration & Environment | ✅ Checked | Scanned for hardcoded secrets, env vars. None found. Both crates are library crates with no config files. |
| D. Dependency Health | ✅ Checked | `cargo deny check advisories` — clean. Reviewed Cargo.toml deps for both crates. No unused deps detected. `futures` in dev-dependencies also in main deps for treesitter (acceptable). |
| E. Test Coverage Gaps | ✅ Checked | 222 tests pass. Identified gap: integration tests exist only for Java and Rust (minimal). Go/TS/Python/JS/Vue/CSS lack fixture-based integration tests. |
| F. Mobile ↔ Backend | ⏭ Skipped | No mobile app |
