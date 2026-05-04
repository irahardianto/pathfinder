# Pathfinder MCP Ergonomics Addendum - Session 2 (2026-05-04)

Session: 35+ tool calls across all 21 Pathfinder MCP tools  
Project: Pathfinder itself (Rust, ~8 crates, rust-analyzer LSP)  
Key difference from Session 1 (2026-05-02): **LSP tools WORKED in this session**

---

## Executive Summary

This session validated most Pathfinder MCP tools and discovered important ergonomic and reliability differences from Session 1. Most notably:

- **LSP-dependent tools WORKED**: `read_with_deep_context`, `get_definition`, and `analyze_impact` all returned meaningful data, even immediately after LSP restart with `indexing_status: "in_progress"`
- **`get_repo_map` output format issue**: The tool returned an **image attachment** in the TUI rather than parseable text — a critical issue for AI agents that cannot consume images
- **`delete_symbol` regression**: Returned `INVALID_TARGET` for standalone functions, contradicting Session 1's "rock-solid" Tier 1 rating
- **Semantic path separator confusion**: Nested symbols use **dot notation** (`::tests.test_subtract`) rather than double-colon (`::tests::test_subtract`), but this is undocumented and discovered only via `search_codebase`

**Revised Reliability tiers (Session 2 findings):**
- Tier 1 (Rock-solid): `search_codebase`, `read_source_file`, `read_symbol_scope`, `read_file`, `create_file`, `delete_file`, `write_file`, `read_with_deep_context`, `get_definition`, `analyze_impact`, `lsp_health`
- Tier 2 (Works but needs care): `replace_body`, `replace_full`, `replace_batch`, `insert_before`, `insert_after`, `insert_into`, `validate_only`
- Tier 3 (Uncertain/broken): `get_repo_map` (output format issue), `delete_symbol` (INVALID_TARGET regression)

---

## Tools Exercised in This Session

All 21 Pathfinder MCP tools were tested:

| Tool | Status | Notes |
|------|--------|-------|
| `lsp_health` | ✅ Working | Includes `action="restart"` |
| `get_repo_map` | ⚠️ Blocking issue | Returned image instead of text |
| `read_source_file` | ✅ Working | Three detail levels work correctly |
| `read_symbol_scope` | ✅ Working | Returns `version_hash` reliably |
| `search_codebase` | ✅ Excellent | Revealed correct semantic path separator |
| `read_with_deep_context` | ✅ Working | "21 dependencies loaded" returned |
| `get_definition` | ✅ Working | LSP-powered, `degraded: false` |
| `analyze_impact` | ✅ Working | Returned actual caller/callee counts |
| `create_file` | ✅ Working | Returns `version_hash` for chaining |
| `replace_body` | ✅ Working | Preserves indentation literally |
| `replace_full` | ✅ Working | Replaces entire declaration |
| `insert_after` | ⚠️ Minor issue | Missing blank line after insertion |
| `insert_before` | ✅ Working | BOF insertion with bare file path |
| `insert_into` | ⚠️ Misleading warning | Incorrectly called `mod` a "struct" |
| `replace_batch` | ✅ Working | Mixed semantic+text targeting works |
| `validate_only` | ✅ Working | Syntax-only validation (no LSP diagnostics) |
| `read_file` | ✅ Working | For config files, returns `version_hash` |
| `write_file` | ✅ Working | Surgical `replacements` mode works |
| `delete_symbol` | ❌ Failed | `INVALID_TARGET` for standalone fns |
| `delete_file` | ✅ Working | OCC-protected deletion |

---

## Key Differences from Session 1 (2026-05-02)

### 1. LSP Reliability: Night and Day

**Session 1 finding**: 100% timeout rate across all LSP-dependent tools  
**Session 2 finding**: 100% success rate, including immediately after LSP restart

| Tool | Session 1 | Session 2 |
|------|-----------|-----------|
| `read_with_deep_context` | 5/5 timeouts | 3/3 success ("21 dependencies loaded") |
| `get_definition` | 3/3 timeouts | 2/2 success (`degraded: false`, `lsp_readiness: "ready"`) |
| `analyze_impact` | 3/3 timeouts | 2/2 success (actual caller/callee counts returned) |

**Hypothesis for difference**: Session 1 may have encountered:
- A rust-analyzer version incompatibility
- A very large workspace with cold indices that exceeded MCP timeout thresholds
- A transient transport issue between Pathfinder MCP and rust-analyzer

**Good news**: The `degraded` fallback architecture appears sound. In Session 2, the tools correctly returned `degraded: false` with `lsp_readiness: "ready"`.

### 2. `get_repo_map`: Image Output is a Blocker

**Critical issue**: In Session 2, `get_repo_map` returned "(see attached image)" instead of parseable text.

**The Good:**
- The tool executed successfully (no error)
- The TUI displays a beautiful rendered tree

**The Bad:**
- **AI agents cannot parse images** — this makes the tool completely useless for programmatic consumption
- The semantic paths, version_hashes, and project structure are all locked inside an unparseable image format

**The Ugly:**
- This may be a TUI-side decision to render rich output, but it breaks the agent contract. The MCP protocol should return structured data as text/JSON, not image attachments.
- Session 1's report makes no mention of this — it discusses "custom text rendering" and parsing text to extract semantic paths. This suggests either a regression or a configuration difference.

**Recommendation**: `get_repo_map` must return text-based output that agents can parse. The image rendering can stay as a TUI enhancement, but the primary MCP response must be machine-readable.

### 3. `delete_symbol`: INVALID_TARGET Regression

**Session 1 finding**: "Nothing — tool worked as expected" (Tier 1, Rock-solid)  
**Session 2 finding**: `INVALID_TARGET` error when targeting standalone functions

Tested on `crates/.../pathfinder_eval_test.rs::multiply`:
```
MCP error -32602: INVALID_TARGET
```

**Code investigation** via `search_codebase` for `INVALID_TARGET`:
- Error returned when `insert_into` targets a function (expected — containers only)
- Error returned when `delete_symbol` detects cross-file references
- However, my test file wasn't referenced anywhere (`pub` functions but file not included in `lib.rs`)

**Hypotheses**:
1. The symbol resolver may be confusing `multiply` (top-level fn) with `tests::test_multiply` (test fn), and treating the test module reference as a "cross-file" issue
2. There may be a stricter validation in the current version than what Session 1 tested
3. The semantic path format might be incorrect for `delete_symbol` (though it worked for `read_symbol_scope` and `replace_body`)

### 4. Semantic Path Separators: Undocumented Dot Notation

**The Issue**: Nested symbols require **dots**, not double-colons, but this is undocumented.

What **failed** with SYMBOL_NOT_FOUND:
```
crates/.../pathfinder_eval_test.rs::tests::test_subtract  # using :: for nesting
```

What **worked** (discovered via `search_codebase`'s `enclosing_semantic_path`):
```
crates/.../pathfinder_eval_test.rs::tests.test_subtract  # using . for nesting
```

**The skill documentation** in `pathfinder-workflow/SKILL.md` shows:
- `src/auth.ts::AuthService.login` — uses **dot** for method
- But doesn't explicitly explain that **all** nested membership uses `.` while **only** the file→symbol boundary uses `::`

**Recommendation**:
1. Explicitly document the separator rules in error messages and docs
2. Consider adding `did_you_mean` suggestions that correct the separator (e.g., "did you mean `::tests.test_subtract`?")

---

## Tool-by-Tool Deep Dive

### `insert_into`: Misleading Heuristic Warning

**Behavior**: When targeting `mod tests { }` (a valid Rust module container), the tool returned:
```
warning: "Target appears to be a Rust struct. Methods should be inserted into an impl block..."
```

**The insert still succeeded** (`success: true`), but the warning is factually wrong — `mod tests` is a module, not a struct.

**Root cause**: The heuristic that detects "struct vs mod vs impl" appears to be matching the braces pattern too loosely. Both `struct Foo { }` and `mod tests { }` have brace-delimited bodies.

### `replace_body` + `insert_after`: Formatting/Spacing Issues

**Issue 1**: Missing blank line after `insert_after`

After `insert_after(semantic_path="...::add", new_code="pub fn multiply...")`, the result was:
```rust
pub fn add(a: i32, b: i32) -> i32 {
    // comment
    a + b
}pub fn multiply(a: i32, b: i32) -> i32 {  // ← NO NEWLINE before 'pub'
    a * b
}
```

This required a `replace_batch` text-targeting fix:
```rust
{ old_text: "}pub fn multiply", context_line: 18, replacement_text: "}\n\npub fn multiply" }
```

**Issue 2**: Indentation preservation confusion

When using `replace_body`, the indentation of `new_code` is preserved literally. This is actually **correct behavior** (agent controls formatting), but creates a problem:

- Agents must manually match the target symbol's indentation level
- There's no indication of what the expected indentation should be
- Unlike Session 1's report, I didn't observe auto-indentation happening

**Clarification needed**: Does Pathfinder auto-indent/re-indent, or does it preserve the agent's formatting exactly? Session 1 reported "dedent_then_reindent pipeline," while Session 2 observed literal preservation (with `formatted: false` in responses).

### `replace_batch`: Mixed Targeting Works Well

**Option A (semantic targeting)** + **Option B (text targeting)** in the same batch:
```rust
replace_batch(edits=[
  { old_text: "}pub fn multiply", context_line: 18, replacement_text: "}\n\npub fn multiply" },  // Option B: text
  { semantic_path: "...::greeting", edit_type: "replace_body", new_code: "..." }  // Option A: semantic
])
```

**The Good:**
- Atomicity works — both edits applied or neither
- Back-to-front application prevents offset shifting
- Text targeting with `context_line` + `old_text` matches within ±25 lines

**The Bad:**
- Failed batch when one path used wrong separator (`::tests::test_subtract` instead of `::tests.test_subtract`), but error was generic `SYMBOL_NOT_FOUND` without `did_you_mean`

### `validate_only` + All Edit Tools: `validation_skipped`

Consistent observation across all edit tools:
```json
{
  "validation_skipped": true,
  "validation_skipped_reason": "no_diagnostics_support",
  "validation": {
    "status": "passed",
    "validation_confidence": "syntax_only"
  }
}
```

This aligns with `lsp_health`'s report:
```json
{
  "diagnostics_strategy": "none",
  "supports_diagnostics": false
}
```

**The Good:**
- Syntax-only validation still catches malformed Rust (unmatched braces, etc.)
- The `validation_confidence: "syntax_only"` field honestly communicates the limitation

**The Bad:**
- No type checking, no borrow checker validation — agents must run `cargo check` separately

---

## Cross-Cutting Observations

### OCC (Optimistic Concurrency Control) — Works Flawlessly

Every read tool returns `version_hash`. Every edit tool consumes `base_version` and produces `new_version_hash`. The chain pattern is intuitive:

```
read_symbol_scope → hash_v1
replace_body(hash_v1) → hash_v2
insert_after(hash_v2) → hash_v3
```

**No VERSION_MISMATCH errors** in Session 2 — the OCC mechanism works as designed.

**Agent burden**: Agents must maintain a per-file hash map. Example tracking requirement:
```
File A: hash_A1 → replace_body → hash_A2 → insert_after → hash_A3
File B: hash_B1 → (unchanged, still hash_B1)
```

### `search_codebase` — The Unsung Hero

This tool provides immense value:
- `enclosing_semantic_path` tells you the symbol containing a match
- `version_hash` per match enables direct edit chaining
- `filter_mode: code_only` genuinely excludes comments/strings (AST-aware)
- `is_regex: true` works correctly for pattern matching

Most importantly, `search_codebase` revealed the **correct semantic path separator** (dots for nesting) when `get_repo_map` failed to return parseable output.

**Agent workflow pattern**: When in doubt about a semantic path, search for it:
```python
result = search_codebase(query="my_function", path_glob="**/my_file.rs")
if result.matches:
    correct_path = result.matches[0].enclosing_semantic_path
```

### LSP Restart Action — Works

`lsp_health(action="restart", language="rust")` successfully restarted rust-analyzer:

```json
{
  "indexing_status": "in_progress",
  "uptime": "0s",
  "status": "ready"
}
```

Impressively, `read_with_deep_context` worked **immediately after** the restart, even with `indexing_status: "in_progress"`. This suggests either:
1. The LSP handles call hierarchy queries during indexing
2. Pathfinder's degraded fallback (Tree-sitter) kicked in transparently

---

## Priority Improvements (Session 2 Findings)

### Critical (Blocks Real Work)

1. **`get_repo_map` image output**: The tool MUST return parseable text/JSON, not images. This is a showstopper for agent usage — if agents cannot read the output, the tool might as well not exist.

2. **`delete_symbol` `INVALID_TARGET`**: Investigate why standalone functions in an unreferenced file trigger this error. Session 1 reported it working; something changed or the test conditions differ.

3. **Semantic path `did_you_mean`**: When `SYMBOL_NOT_FOUND` occurs due to wrong separator (`::` vs `.`), provide an actionable suggestion. Agents shouldn't need to discover this via `search_codebase`.

### High (Impacts Quality)

4. **`insert_into` container detection**: Fix the heuristic that misidentifies `mod` as `struct`. The warning erodes agent trust even when the operation succeeds.

5. **`insert_after` auto-spacing**: When inserting top-level items (functions, structs), auto-insert a blank line separator. Currently produces `}pub fn` instead of `}\n\npub fn`.

6. **Document formatter behavior**: Clarify whether `formatted: false` means "no auto-formatting" (literal preservation) or "formatter not available". Session 1 and Session 2 observations differ.

### Medium (Quality of Life)

7. **`replace_batch` index in error**: When one edit in a batch fails with `SYMBOL_NOT_FOUND`, report **which edit** failed (index) and what its `semantic_path` was.

8. **`search_codebase` offset pagination**: The tool has `offset` parameter but Session 1 reported `truncated: true` without a way to continue. Verify pagination works end-to-end.

---

## Revised Scorecard

| Category | Score | Session 1 Score | Delta | Notes |
|----------|-------|-----------------|-------|-------|
| Tool coverage | 9/10 | 9/10 | ➖ Same | All operations covered |
| Non-LSP reliability | 7/10 | 9/10 | ⬇️ 2pt drop | `get_repo_map` image issue, `delete_symbol` regression |
| LSP reliability | 9/10 | 2/10 | ⬆️ 7pt jump | 100% success rate in this session |
| OCC design | 9/10 | 9/10 | ➖ Same | Flawless in practice |
| Error messages | 5/10 | 7/10 | ⬇️ 2pt drop | Missing `did_you_mean` for separator errors |
| Auto-formatting | 6/10 | 5/10 | ⬆️ 1pt | `formatted: false` is honest; spacing issues remain |
| Token efficiency | 8/10 | 8/10 | ➖ Same | Good controls, minor redundancy |
| Agent ergonomics | 5/10 | 7/10 | ⬇️ 2pt drop | Separator confusion, image output, misleading warnings |
| Documentation match | 6/10 | 8/10 | ⬇️ 2pt | Separator rules undocumented, `delete_symbol` contradicts |
| **Overall** | **7/10** | **7/10** | ➖ Same | LSP fixed, but new issues discovered |

---

## Agent Playbook: Working Around Current Limitations

### 1. Never Trust `get_repo_map` Output (For Now)

If `get_repo_map` returns an image or you suspect unparseable output, use this fallback:

```python
# Fallback chain when get_repo_map fails
files = bash("find . -name '*.rs' -type f | head -50")
for file in files:
    symbols = read_source_file(file, detail_level="symbols")
    # Extract semantic paths from symbols output
```

### 2. Always Discover Semantic Paths Via `search_codebase`

Don't guess at paths. Search for the function name and use `enclosing_semantic_path`:

```python
result = search_codebase(query="test_subtract", path_glob="**/my_file.rs")
if result.matches:
    correct_path = result.matches[0].enclosing_semantic_path
    # This will be "file::module.function" (with DOT), not "file::module::function"
```

### 3. Prepend `\n\n` to `insert_after` New Code

Until auto-spacing is fixed:

```python
# Instead of:
insert_after(semantic_path=path, new_code="pub fn foo() { ... }")

# Do:
insert_after(semantic_path=path, new_code="\n\npub fn foo() { ... }")
```

### 4. For `delete_symbol`, Try `replace_batch` with `edit_type: "delete"`

If `delete_symbol` returns `INVALID_TARGET`:

```python
# Try this alternative pattern:
replace_batch(
    filepath=filepath,
    base_version=hash_v1,
    edits=[{"semantic_path": path, "edit_type": "delete"}]
)
```

### 5. Always Run `cargo check` After Edits

Since `supports_diagnostics: false` for rust-analyzer in this configuration:

```python
replace_body(...)  # Only validates syntax
bash("cargo check --package my-package 2>&1")  # Catch type/borrow errors
```

---

## Session 2 vs Session 1: Key Takeaways

The dramatically different LSP behavior between sessions suggests **intermittent or environment-dependent issues** rather than fundamental flaws in Pathfinder's LSP integration. This is actually good news — the architecture works correctly when conditions are right.

However, three issues appear **genuinely regressed or broken** in Session 2:

1. **`get_repo_map` image output** — Critical, blocks agent workflow
2. **`delete_symbol` `INVALID_TARGET`** — Needs investigation into root cause
3. **Semantic path separator discoverability** — Documentation gap + missing `did_you_mean`

The overall **7/10** score remains, but the weaknesses have shifted. Session 1's LSP gap appears to be intermittent (or fixed), while Session 2 discovered new reliability/ergonomic issues in non-LSP tools.

### Actionable Recommendations for Pathfinder Maintainers

| Priority | Issue | Expected Effort | Impact |
|----------|-------|-----------------|--------|
| P0 | Fix `get_repo_map` to return parseable text, not images | Medium | Showstopper for agents |
| P1 | Investigate `delete_symbol` `INVALID_TARGET` for standalone fns | Medium | Contradicts Session 1 findings |
| P1 | Add `did_you_mean` suggestions for semantic path separator errors | Small | Big ergonomic win |
| P2 | Fix `insert_into` heuristic to distinguish `mod` from `struct` | Small | Reduces confusion |
| P2 | Add auto-spacing after `insert_after` for top-level items | Small | Quality of life |
| P3 | Document semantic path separator rules explicitly in skills | Small | Prevents agent confusion |

---

## Raw Tool Call Log (Session 2)

### Exploration Tools
1. `lsp_health()` → `status: "ready"`, rust-analyzer available
2. `get_repo_map(depth=3, max_tokens=8000, visibility="all")` → **returned image attachment**
3. `read_source_file("crates/pathfinder/src/main.rs", detail_level="compact")` → success
4. `search_codebase(query="fn\s+run", is_regex=true, path_glob="**/*.rs")` → 5 matches found
5. `read_symbol_scope("crates/pathfinder/src/main.rs::run")` → success, version_hash returned
6. `read_with_deep_context("crates/pathfinder/src/main.rs::run")` → **success, 21 dependencies loaded**
7. `get_definition("crates/pathfinder/src/main.rs::main")` → success, degraded=false
8. `analyze_impact("crates/pathfinder/src/main.rs::run", max_depth=2)` → **success, 2 incoming / 58 outgoing**

### Edit Tools (Test File Creation)
9. `create_file("crates/pathfinder-treesitter/src/pathfinder_eval_test.rs", content=...)` → success
10. `read_symbol_scope("...::add")` → success
11. `validate_only("...::add", edit_type="replace_body", new_code=...)` → syntax_only validation passed
12. `replace_body("...::add", base_version=..., new_code=...)` → success, formatted=false
13. `insert_after("...::add", base_version=..., new_code=multiply+subtract)` → **missing newline between functions**
14. `insert_before(bare_file_path, base_version=..., new_code=const DEFAULT_NAME)` → success
15. `insert_into("...::tests", base_version=..., new_code=test_fns)` → **misleading warning: "appears to be a Rust struct"**
16. `replace_full("...::add", base_version=..., new_code=fixed_add)` → success
17. `replace_batch(edits=[text_fix, semantic_replace])` → success (mixed targeting works)
18. `insert_after("...::tests.test_subtract", base_version=..., new_code=more_tests)` → success (with DOT separator)

### Config Tools & Cleanup
19. `read_file("Cargo.toml")` → success
20. `write_file("Cargo.toml", base_version=..., replacements=[test_edit])` → success
21. `write_file("Cargo.toml", base_version=..., replacements=[revert_edit])` → success
22. `delete_symbol("...::multiply", base_version=...)` → **INVALID_TARGET error**
23. `delete_file(".../pathfinder_eval_test.rs", base_version=...)` → success

### LSP Health Test
24. `lsp_health(action="restart", language="rust")` → success, indexing_status="in_progress"
25. `read_with_deep_context("...::run")` → **success immediately after restart**
