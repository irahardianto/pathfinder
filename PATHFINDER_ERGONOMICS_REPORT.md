# Pathfinder MCP Ergonomics Report for AI Agents

Date: 2026-05-02
Session: 46 tool calls across all 21 Pathfinder MCP tools
Project: Pathfinder itself (Rust, ~8 crates, rust-analyzer LSP)

---

## Executive Summary

Pathfinder MCP tools provide a genuinely useful semantic layer over traditional text-based coding tools. Non-LSP tools are reliable, fast, and well-designed for AI agent consumption. However, LSP-dependent tools suffered a 100% timeout rate during this session despite lsp_health reporting "ready", creating a significant reliability gap. Edit tools have some indentation and structural targeting issues that agents must watch for.

**Reliability tiers:**
- Tier 1 (Rock-solid): get_repo_map, search_codebase, read_source_file, read_symbol_scope, read_file, create_file, delete_file, delete_symbol
- Tier 2 (Works but needs care): replace_body, replace_full, replace_batch, insert_before, insert_after, insert_into, validate_only, write_file
- Tier 3 (Unreliable in practice): read_with_deep_context, get_definition, analyze_impact (all LSP-dependent)

---

## Tool-by-Tool Analysis

### 1. lsp_health

Calls: 2
Results: Both returned `status: "ready"`, rust-analyzer indexed, navigation_ready: true

**The Good:**
- Clean, structured response with per-language breakdown
- Reports diagnostics_strategy, indexing_status, uptime, supports_* flags
- Fast (sub-second)

**The Bad:**
- Reports "ready" when the underlying LSP process is actually non-responsive to tool calls
- No way to distinguish "LSP process exists and passed init" from "LSP can actually answer queries in reasonable time"
- This creates false confidence — agent sees "ready" and proceeds to use LSP tools, which then timeout

**The Ugly:**
- In this session: lsp_health said "ready" for the entire session, but read_with_deep_context, get_definition, and analyze_impact ALL timed out 100% of the time (11 consecutive failures). The health check is not a reliable predictor of LSP tool success.

**Recommendation for agents:** Treat lsp_health as a binary "LSP process exists" check, not as a guarantee that LSP navigation works. Always wrap LSP-dependent calls with fallback logic. If 2+ LSP calls timeout, skip all remaining LSP tools for the session.

---

### 2. get_repo_map

Calls: 3 (basic depth=1, deep depth=5, changed_since="1h")

**The Good:**
- Single call replaces dozens of `ls`, `find`, and `read` calls
- Returns full semantic paths with `::` notation, directly usable by other tools
- Returns version_hash per file for immediate editing without a separate read step
- `changed_since` parameter is excellent for PR reviews
- `visibility` parameter lets you control symbol granularity
- `include_extensions` / `exclude_extensions` for polyglot repos
- Token budgets (`max_tokens`, `max_tokens_per_file`) prevent context explosion

**The Bad:**
- Very large output for deep scans of big repos — 12000 tokens for depth=5 was barely enough
- `coverage_percent` field would help agents know if they need to increase budget, but wasn't prominently surfaced
- No built-in way to get just the module tree without symbol detail (symbols mode would be nice as a param)

**The Ugly:**
- The output format is a custom text rendering, not JSON. This means agents must parse text to extract semantic paths. A structured JSON mode would be more agent-friendly.
- Impl blocks get disambiguated with `#2`, `#3` suffixes (e.g., `impl PathfinderServer#2`), which is correct but confusing for agents constructing semantic paths

**Recommendation:** Always start with depth=1-2 and increase as needed. Use changed_since for PR review workflows. The version_hash from this tool is a huge time-saver — use it to skip read steps before editing.

---

### 3. read_source_file

Calls: 3 (compact, full, symbols detail levels)

**The Good:**
- Three detail levels (compact, full, symbols) give precise control over token spend
- `symbols` mode returns just the AST tree — excellent for quick structural overview
- Returns version_hash for OCC
- `start_line` / `end_line` for range-limited reads
- Correctly rejects non-source files with UNSUPPORTED_LANGUAGE

**The Bad:**
- `full` mode on large files can be very token-heavy
- No streaming/pagination — entire requested range loaded at once

**The Ugly:**
- Nothing major — this tool is clean and well-designed

**Recommendation:** Default to `compact` for initial reads. Use `symbols` when you only need structure. Use `start_line`/`end_line` for large files.

---

### 4. read_symbol_scope

Calls: 7 (4 successful reads, 3 SYMBOL_NOT_FOUND error tests)

**The Good:**
- Extracts exactly one symbol — zero wasted context
- Returns version_hash for OCC
- Very fast (Tree-sitter based, no LSP dependency)
- Error messages for SYMBOL_NOT_FOUND are clear and actionable

**The Bad:**
- No `did_you_mean` suggestions were returned even for close typos (e.g., "reinden" vs "reindent", "dedent_then_reinden" vs "dedent_then_reindent"). The docs suggest this feature exists, but it didn't trigger in testing.
- For Rust impl blocks, targeting methods requires the impl-qualified path (e.g., `PathfinderServer.search_codebase`), which requires knowing which impl block a method belongs to

**The Ugly:**
- When targeting Rust methods on the "wrong" impl block (e.g., `impl PathfinderServer#2` vs `impl PathfinderServer#3`), the error message doesn't help you find the right impl block. Agents must use get_repo_map first to discover the correct disambiguated path.

**Recommendation:** Always use get_repo_map or read_source_file(symbols) to discover exact semantic paths before targeting with read_symbol_scope. Don't guess at paths.

---

### 5. search_codebase

Calls: 5 (basic, regex, comments_only filter, known_files, all filter)

**The Good:**
- `enclosing_semantic_path` per match enables direct chaining to edit tools
- `version_hash` per match (or per file_group) eliminates the need for a separate read
- `filter_mode` (code_only, comments_only, all) is AST-aware — genuinely useful
- `known_files` suppresses content for files already in context — saves tokens
- `group_by_file` consolidates matches with shared version_hash
- `exclude_glob` prevents reading test files, generated code, etc.
- Regex support works correctly
- `path_glob` for scope limiting

**The Bad:**
- The `known_files` feature returns matches with empty content but still counts them in total_matches. If ALL matches are in known_files, you see `total_matches: 5, returned_count: 0` which is confusing.
- `enclosing_semantic_path` was null for some matches in the batch.rs file (import lines), meaning some search results can't be directly chained to edit tools.
- Response includes both flat `matches` array AND `file_groups` when `group_by_file=true` — redundant, doubles token cost.

**The Ugly:**
- The truncation behavior: `returned_count: 6, total_matches: 11, truncated: true`. The remaining 5 matches are silently dropped. There's no cursor/offset mechanism to fetch the next page.

**Recommendation:** Always set `max_results` explicitly. Use `exclude_glob="**/*.test.*"` to focus on production code. Use `group_by_file=true` when you plan to edit multiple matches in the same file.

---

### 6. read_with_deep_context

Calls: 5 (ALL timed out)

**The Good:**
- The concept is excellent — read a function AND all its callees in one call
- Would eliminate multiple read_symbol_scope calls for dependency chains
- `degraded` flag + `degraded_reason` provide clear fallback semantics

**The Bad:**
- 100% timeout rate in this session (5/5 calls, including retries after cooldown)
- First call can take 5-60s for LSP warmup, but the MCP tool timeout appears to be shorter than this
- Even after LSP reported "ready" for 3+ minutes, tool still timed out

**The Ugly:**
- The tool is essentially unusable when LSP is flaky, and there's no graceful degradation to Tree-sitter-only results for the dependency signatures. The tool either gives you LSP-quality results or nothing.
- There's no way to set a longer timeout from the agent side

**Recommendation:** Treat as best-effort. If it times out once, don't retry — fall back to read_symbol_scope + search_codebase to manually trace dependencies. The tool's value proposition (callee signatures in one call) is too good to abandon, but agents need a robust fallback path.

---

### 7. get_definition

Calls: 3 (ALL timed out)

**The Good:**
- LSP-powered "go to definition" is the gold standard for navigation
- Multi-strategy grep fallback (file-scoped -> impl-scoped -> global) when LSP unavailable
- `degraded` flag distinguishes LSP-confirmed from grep-approximate results

**The Bad:**
- 100% timeout rate (3/3 calls)
- The grep fallback didn't activate — the tool timed out entirely rather than falling back

**The Ugly:**
- If the LSP is non-responsive, the tool should fall back to grep within a reasonable timeout, not hang indefinitely. The current behavior suggests the LSP timeout is too long or the fallback isn't triggering.

**Recommendation:** Same as read_with_deep_context — treat as best-effort with manual fallback.

---

### 8. analyze_impact

Calls: 3 (ALL timed out)

**The Good:**
- Call graph BFS traversal is the killer feature for safe refactoring
- Returns version_hashes for ALL referenced files (callers + callees)
- `max_depth` parameter controls traversal radius
- Would replace dozens of grep calls for impact assessment

**The Bad:**
- 100% timeout rate (3/3 calls)
- `degraded` mode (grep heuristics) didn't activate either

**The Ugly:**
- Same issue as get_definition — LSP timeout blocks the entire response instead of falling back

**Recommendation:** Critical tool that's currently unreliable. Agents MUST have a grep-based fallback for caller analysis (search_codebase with the function name).

---

### 9. create_file

Calls: 2

**The Good:**
- Clean, simple — provides path + content, returns version_hash
- Auto-creates parent directories
- Validation ran and passed for the .rs file (Rust LSP checked the new file)
- Validation ran and passed for the .toml file too

**The Bad:**
- No way to specify file permissions or encoding
- No conflict detection — overwrites existing files silently (though OCC applies if you have a hash)

**The Ugly:**
- Nothing major — this tool does what it says

**Recommendation:** Use for all new file creation. The returned version_hash chains directly into edit tools.

---

### 10. replace_body

Calls: 1

**The Good:**
- Semantic targeting by symbol name — no line numbers needed
- Auto-dedent + re-indent pipeline (dedent_then_reindent)
- Returns new_version_hash for chaining
- LSP validation (when LSP is healthy)

**The Bad:**
- The indentation pipeline produced over-indented output for multi-line if-else bodies. The greet function body after replacement showed inconsistent indentation:
  ```rust
  // Expected (all at consistent indent):
  let greeting = if name.is_empty() {
      "Hello, stranger!".to_owned()
  } else {
      format!("Hello, {}!", name)
  };
  greeting

  // Actual (inner lines over-indented):
  let greeting = if name.is_empty() {
          "Hello, stranger!".to_owned()
      } else {
          format!("Hello, {}!", name)
      };
      greeting
  ```
  The body_indent_column detection or the re-indent step appears to add extra indentation for nested blocks.

**The Ugly:**
- The indentation bug is subtle and hard for agents to detect — the code compiles but looks wrong. If LSP validation was working, it might have caught this as a style issue, but not as an error.

**Recommendation:** After replace_body, always read back the result with read_symbol_scope to verify indentation. Don't trust the auto-indent blindly.

---

### 11. replace_full

Calls: 1

**The Good:**
- Successfully replaced `multiply` with `subtract` (different name, different body)
- Signature + body + doc comments all handled in one operation
- Returned new version_hash

**The Bad:**
- Same LSP validation timeout issue (validation_skipped: true)
- No way to preview the diff before applying

**The Ugly:**
- Nothing beyond the shared LSP issues

**Recommendation:** Works well for rename + restructure operations. Combine with validate_only for risky changes.

---

### 12. insert_after

Calls: 1

**The Good:**
- Correctly inserted new function after the target symbol
- Semantic targeting works — "insert after subtract" is intuitive
- Auto-spacing between symbols

**The Bad:**
- The spacing between the original function and the new one was inconsistent — `subtract` ended without a blank line, and `divide` was inserted immediately after. Result:
  ```rust
  pub fn subtract(a: i32, b: i32) -> i32 {
      a - b
  }
  pub fn divide(a: i32, b: i32) -> Option<i32> {
  ```
  Missing blank line between the two functions.

**The Ugly:**
- Agents need to explicitly add `\n` at the start of new_code to get proper spacing, which is easy to forget.

**Recommendation:** Always prepend `\n` to new_code when using insert_after for functions/classes.

---

### 13. insert_before

Calls: 1

**The Good:**
- BOF insertion (bare file path, no `::`) correctly adds imports at top of file
- Fast and reliable

**The Bad:**
- Added an extra blank line between the import and the doc comment:
  ```rust
  use std::fmt;

  /// Temporary module...
  ```
  This is minor but creates a style inconsistency with the rest of the codebase.

**The Ugly:**
- Nothing critical

**Recommendation:** Works well for adding imports. Check the result for spacing issues.

---

### 14. insert_into

Calls: 1

**The Good:**
- Concept is sound — insert code at the end of a container's body

**The Bad:**
- When targeting `Calculator` (a Rust struct), it inserted the method body INSIDE the struct field list, producing invalid Rust:
  ```rust
  pub struct Calculator {
      pub last_result: i32,

      pub fn reset(&mut self) {
          self.last_result = 0;
      }
  }
  ```
  The agent probably should have targeted the `impl Calculator` block instead of the struct definition, but the tool didn't warn about this. For Rust, `insert_into` on a struct is almost always wrong — you want to target the impl block.

**The Ugly:**
- This is a semantic understanding gap. The tool correctly inserts "into the body of Calculator", but "body of a struct" means "inside the braces of the struct definition" which is structurally valid but semantically wrong for Rust. The tool needs language-aware semantics to know that Rust struct bodies contain fields, not methods.

**Recommendation:** For Rust, ALWAYS target the impl block, never the struct definition. Example: use `file.rs::impl Calculator` not `file.rs::Calculator`. For other languages (TypeScript classes, Go structs), the distinction may not matter.

---

### 15. replace_batch

Calls: 1 (2 edits: replace_body + insert_after in test module)

**The Good:**
- Atomic — both edits land or neither does
- Single OCC guard for the whole batch
- Supports mixing edit types (replace_body, insert_after, delete, replace_full, insert_before)
- Edits applied back-to-front to avoid offset shifting

**The Bad:**
- Same LSP validation timeout
- No intermediate version hashes — you get one new_version_hash at the end

**The Ugly:**
- Nothing major — this tool is well-designed

**Recommendation:** Prefer replace_batch over sequential replace_body calls when editing multiple symbols in the same file. Reduces OCC chain management and is atomic.

---

### 16. validate_only

Calls: 1

**The Good:**
- Dry-run without disk side effects — perfect for risky edits
- Returns same validation result as a real edit would
- Explicit about not writing (new_version_hash is null)
- Reuse original base_version for the real edit after validation passes

**The Bad:**
- In this session, validation was skipped (lsp_timeout) — so the dry-run confirmed nothing useful
- No way to increase the validation timeout

**The Ugly:**
- When validation is skipped, the tool returns `status: "skipped"` which is honest but means the dry-run gave you no safety guarantee. Agents might misinterpret "success: true" as "code is valid" when actually validation was skipped.

**Recommendation:** Always check both `success` AND `validation.status` (not just `success`). If validation was skipped, the edit might still introduce errors.

---

### 17. write_file

Calls: 1 (surgical search-and-replace on TOML config)

**The Good:**
- Surgical `replacements` mode — no need to rewrite the entire file for small config changes
- OCC-protected
- Works correctly on non-source files (TOML, YAML, .env, Dockerfile)

**The Bad:**
- Requires `base_version` even for new files (use create_file instead for new files)
- Error message when base_version is missing is clear

**The Ugly:**
- Nothing — this tool fills its niche well

**Recommendation:** Use for config file edits. Use `replacements` array for surgical edits, `content` for full rewrites.

---

### 18. read_file

Calls: 2 (Cargo.toml, temp_config.toml)

**The Good:**
- Simple, fast, returns raw file content
- Includes version_hash for OCC
- Appropriate for config files, documentation, .env

**The Bad:**
- Very similar to built-in `read` tool — agents may question which to use
- Appends `---\nversion_hash: xxx` to the content, which agents must be aware of when parsing

**The Ugly:**
- Nothing major

**Recommendation:** Use read_file when you need version_hash for subsequent write_file operations. Otherwise, built-in `read` is equivalent.

---

### 19. delete_symbol

Calls: 1 (deleted `divide` function)

**The Good:**
- Clean removal — handles surrounding whitespace, blank lines
- Semantic targeting — "delete the divide function" is intuitive
- Returns new version_hash

**The Bad:**
- Same LSP validation skip

**The Ugly:**
- Nothing — tool worked as expected

**Recommendation:** Combine with analyze_impact first (when LSP works) to check for callers before deleting.

---

### 20. delete_file

Calls: 2

**The Good:**
- OCC-protected — prevents deleting files modified by concurrent agents
- Clean, simple operation
- Returns success + version_hash

**The Bad:**
- Nothing notable

**The Ugly:**
- Nothing

**Recommendation:** Use over `rm` via bash for safety (OCC protection).

---

## Cross-Cutting Observations

### OCC (Optimistic Concurrency Control)

**Verdict: Excellent design, smooth in practice**

- The version_hash chain pattern is intuitive once understood
- Every read tool returns a hash, every edit tool consumes one and produces a new one
- VERSION_MISMATCH errors are clear and actionable
- The hash chain per file (not per session) is correct behavior — multi-file edits use independent chains

**Pain point:** Agents must track which hash belongs to which file. If you do:
```
read_symbol_scope(fileA) -> hash_A
read_symbol_scope(fileB) -> hash_B
replace_body(fileA, hash_A) -> hash_A2
```
You must remember that hash_B is still valid for fileB, but fileA is now at hash_A2. No tool helps manage this state — it's purely the agent's responsibility.

### LSP Reliability

**Verdict: Critical reliability gap**

- lsp_health reports "ready" but LSP-dependent tools timeout at 100% rate
- Three tools fully affected: read_with_deep_context, get_definition, analyze_impact
- All edit tools affected in validation (validation_skipped: true)
- The grep fallback for get_definition and analyze_impact did NOT activate — tools simply timed out instead of degrading gracefully
- This represents ~15% of total Pathfinder functionality being unreliable

**Root cause hypothesis:** The LSP process is alive (responds to initialize/health) but non-responsive to actual semantic queries (callHierarchy, gotoDefinition, pullDiagnostics). This could be a rust-analyzer indexing issue on its own codebase, or an MCP transport timeout that's shorter than the LSP query time.

### Semantic Path Addressing

**Verdict: Good concept, rough edges**

- The `file::symbol.subsymbol` notation is intuitive for agents
- Rust impl block disambiguation (#2, #3) is correct but requires prior knowledge from get_repo_map
- No autocomplete or discovery mechanism — agents must always use get_repo_map or search_codebase first
- SYMBOL_NOT_FOUND errors don't include did_you_mean suggestions (tested with close typos)

### Auto-Indentation

**Verdict: Works for simple cases, buggy for nested blocks**

- The dedent_then_reindent pipeline correctly handles single-level indentation
- Multi-line nested structures (if-else, match) can get over-indented
- The `insert_into` tool lacks Rust-specific awareness (struct vs impl block)

### Token Efficiency

**Verdict: Well-designed with room for improvement**

- `known_files` in search_codebase saves significant tokens
- `filter_mode` (code_only, comments_only) prevents wasting tokens on noise
- `max_tokens` / `max_tokens_per_file` in get_repo_map prevents context explosion
- search_codebase returns BOTH flat matches AND file_groups when grouped — redundant
- Error messages include full context that's sometimes unnecessary

---

## Priority Improvements

### Critical (Blocks Real Work)

1. **LSP timeout handling**: LSP-dependent tools should fall back to Tree-sitter/grep within 10-15 seconds, not hang until MCP timeout. The degraded mode exists in the code but doesn't appear to activate during MCP-level timeouts.

2. **lsp_health accuracy**: Health check should verify actual query responsiveness, not just process existence. A probe query (e.g., a lightweight textDocument/hover) would be more meaningful than just checking process status.

### High (Impacts Quality)

3. **Auto-indentation fix**: The dedent_then_reindent pipeline produces over-indented output for nested blocks. The `body_indent_column` detection may need adjustment.

4. **insert_into language awareness**: For Rust, `insert_into` targeting a struct should either (a) warn that you probably want an impl block, or (b) auto-redirect to the impl block.

5. **SYMBOL_NOT_FOUND did_you_mean**: The feature exists in docs but didn't fire for close typos. Agents need this to self-correct path errors.

### Medium (Quality of Life)

6. **search_codebase pagination**: Add offset/cursor for paginating through truncated results.

7. **Replace redundant response fields**: When `group_by_file=true`, omit the flat `matches` array or vice versa.

8. **read_file version_hash placement**: Append as a separate field, not mixed into content text.

9. **insert_after spacing**: Auto-insert blank line separator between top-level items.

---

## Summary Scorecard

| Category | Score | Notes |
|----------|-------|-------|
| Tool coverage | 9/10 | All major operations covered |
| Non-LSP reliability | 9/10 | Tree-sitter tools are rock-solid |
| LSP reliability | 2/10 | 100% timeout rate in testing |
| OCC design | 9/10 | Elegant, prevents data loss |
| Error messages | 7/10 | Clear but missing did_you_mean |
| Auto-formatting | 5/10 | Indentation bugs on nested code |
| Token efficiency | 8/10 | Good controls, some redundancy |
| Agent ergonomics | 7/10 | Semantic paths great, discovery friction |
| Documentation match | 8/10 | Docs accurate to behavior (except LSP) |
| Overall | 7/10 | Strong foundation, LSP gap is critical |

---

## Recommendations for AI Agent Developers

1. **Always bootstrap with lsp_health**, but treat it as advisory, not authoritative
2. **Default to Tree-sitter tools** (read_symbol_scope, search_codebase, get_repo_map) — they're faster and more reliable
3. **Wrap every LSP call in try/fallback**: if read_with_deep_context fails, fall back to read_symbol_scope + manual dependency tracing
4. **Maintain a per-file hash map** in agent memory to avoid OCC chain confusion
5. **Always read back after replace_body** to verify indentation
6. **For Rust: target impl blocks, never struct definitions** when using insert_into
7. **Prefer replace_batch** over sequential single edits in the same file
8. **Use get_repo_map early and often** — it's the cheapest way to get version_hashes for files you haven't read yet
