# PATCH-011: Fix get_definition Grep Fallback for Same-File Symbols

## Group: C (Medium) — Validation & Fallback Improvements

## Objective

Fix the secondary mystery from the agent report: even when `goto_definition` returns null,
the grep fallback in `get_definition` fails to find the symbol. The grep fallback searches
for the definition of a symbol in its own file. When the semantic path points to
`indent.rs::dedent`, Strategy 1 (`grep_definition_in_file`) searches `indent.rs` for
`fn dedent`. It should find it — but the report says it returns SYMBOL_NOT_FOUND.

Two possible causes:
1. The regex pattern doesn't match Rust's `pub fn dedent` (the `pub` keyword isn't in the pattern)
2. The search returns the match but it's filtered out because it's the "same symbol"

## Severity: HIGH — grep fallback is the safety net when LSP fails

## Background

The `grep_definition_in_file` regex pattern is:
```rust
r"(?:fn|def|func|class|struct|type|interface|const|let|var|enum|trait|mod)\s+{symbol_name}\b"
```

For `pub fn dedent`, the text is `pub fn dedent`. The regex matches `fn dedent` because
`(?:fn|...)` doesn't require the pattern to start at the beginning of the line. The `fn`
in `pub fn dedent` should match.

Let me re-examine. The pattern uses `\\b` (escaped backslash-b) in a Rust raw string.
In the actual regex, this becomes `\b` (word boundary). For `fn dedent`, the match would
be `fn dedent` with `\b` after `dedent`. This should work.

Wait — the actual code shows `\\b` in a `format!` macro:
```rust
format!(r"(?:fn|def|func|class|struct|type|interface|const|let|var|enum|trait|mod)\s+{symbol_name}\\b")
```

In a `format!` macro, `\\b` becomes `\b` in the resulting string, which is the regex word
boundary. This is correct.

So the grep should find `fn dedent` in `indent.rs`. Why does it fail?

Hypothesis: The search_codebase `grep_definition_in_file` calls `self.scout.search()`
which uses ripgrep. But ripgrep might not find the pattern if:
- The file is in a different path format (absolute vs relative)
- The `path_glob` filtering excludes it
- The file hasn't been saved to disk yet (in-memory only)

The most likely cause: `path_glob: file_path.to_string_lossy().to_string()` — if
`file_path` is a relative path like `crates/pathfinder-common/src/normalize.rs`, the
ripgrep search with this exact glob might not match because ripgrep interprets the
glob differently than a literal file path.

## Scope

| # | File | Action |
|---|------|--------|
| 1 | `crates/pathfinder/src/server/tools/navigation.rs` | Improve `grep_definition_in_file` path handling and add `pub` to pattern |

## Step 1: Improve the grep pattern and path handling

**File:** `crates/pathfinder/src/server/tools/navigation.rs`

**Find:**
```rust
    async fn grep_definition_in_file(
        &self,
        symbol_name: String,
        file_path: std::path::PathBuf,
    ) -> Option<GetDefinitionResponse> {
        let pattern = format!(
            r"(?:fn|def|func|class|struct|type|interface|const|let|var|enum|trait|mod)\s+{symbol_name}\\b"
        );

        let search_result = self
            .scout
            .search(&pathfinder_search::SearchParams {
                workspace_root: self.workspace_root.path().to_path_buf(),
                query: pattern,
                is_regex: true,
                max_results: 5,
                path_glob: file_path.to_string_lossy().to_string(),
                exclude_glob: String::default(),
                context_lines: 0,
            })
            .await;
```

**Replace with:**
```rust
    async fn grep_definition_in_file(
        &self,
        symbol_name: String,
        file_path: std::path::PathBuf,
    ) -> Option<GetDefinitionResponse> {
        // Match definition patterns with optional preceding visibility modifier.
        // Rust: `pub fn`, `pub(crate) fn`, `pub async fn`, bare `fn`
        // TypeScript: `export function`, `export default function`, bare `function`
        // Python: `def`, `async def`
        let pattern = format!(
            r"(?:(?:pub|export|public|private|protected|internal|open)\s*(?:\([^)]*\)\s*)?(?:async\s*)?)?(?:fn|def|func|function|class|struct|type|interface|const|let|var|enum|trait|mod)\s+{symbol_name}\\b"
        );

        // Use the file as a specific path glob. Convert to forward-slash
        // format for ripgrep compatibility across platforms.
        let glob = file_path.to_string_lossy().replace('\\', "/");

        let search_result = self
            .scout
            .search(&pathfinder_search::SearchParams {
                workspace_root: self.workspace_root.path().to_path_buf(),
                query: pattern,
                is_regex: true,
                max_results: 5,
                path_glob: glob,
                exclude_glob: String::default(),
                context_lines: 0,
            })
            .await;
```

Also update the `grep_impl_method` and `grep_definition_global` methods to use the
same improved pattern format.

## Step 2: Add fallback to search_codebase if file-scoped grep fails

If `grep_definition_in_file` returns None (possibly due to path issues), fall back
to a broader search that doesn't restrict by file:

After the Strategy 1 call in `fallback_definition_grep`, add a log and ensure Strategy 3
(global search) can find it:

The existing code already has Strategy 3 (`grep_definition_global`). Verify that it
searches the entire workspace without file restrictions.

Additionally, in `grep_definition_global`, ensure the same improved pattern is used.

## EXCLUSIONS — Do NOT Modify These

- The `SemanticPath` parsing — the symbol name extraction is correct
- The LSP `goto_definition` path — that's fixed by PATCH-002
- The `fallback_definition_grep` Strategy 2 (impl method search) — it already works

## Verification

```bash
# 1. Confirm improved pattern
grep -n 'pub.*fn\|export.*function' crates/pathfinder/src/server/tools/navigation.rs | head -5

# 2. Run navigation tests
cargo test -p pathfinder navigation

# 3. Full verification
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```
