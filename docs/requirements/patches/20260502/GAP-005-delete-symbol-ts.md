# GAP-005: Fix delete_symbol for TypeScript Class Methods

## Group: C (Medium) — Language-Specific Correctness
## Depends on: Nothing

## Objective

The fullstack report identified a 100% failure rate for `delete_symbol` on TypeScript
class methods. Three attempts with different semantic path formats all failed:
- `logger.ts::Logger.audit` → `INVALID_TARGET`
- `logger.ts::audit` → `SYMBOL_NOT_FOUND`
- `logger.ts::Logger.audit` (retry) → `INVALID_TARGET`

The `INVALID_TARGET` error in `delete_symbol_impl` comes from the cross-file reference
check (lines 392-435) which runs `rg -l -w <symbol_name>` to find references. If this
search returns false positives (e.g., the symbol name appears in imports, type annotations,
or unrelated code in other files), the delete is blocked.

The `SYMBOL_NOT_FOUND` error for bare `audit` suggests the symbol chain resolution
doesn't find the method when not qualified with the class name.

## Root Cause Analysis (Requires Verification)

There are two possible root causes:

### Hypothesis A: Cross-file reference check is too aggressive

The check uses `rg -l -w <symbol_name>` which finds ALL files containing the word,
including:
- Files that import the class
- Files that have comments mentioning the symbol
- Type definition files

For a method named `audit`, this is likely to match many files. The check counts
all matches outside the target file as "references" that block deletion.

**Fix**: Improve the reference check to use tree-sitter-based symbol search instead
of raw ripgrep, or add a filter for import/type-reference patterns.

### Hypothesis B: Symbol resolution fails for TS class methods

The `resolve_symbol_chain` function may not correctly resolve `Logger.audit` in
TypeScript because TS class methods are structured differently than Rust impl methods.

In TS: `class Logger { audit() {} }` — `audit` is a child of the `Logger` class node.
In Rust: `impl Logger { fn audit() {} }` — `audit` is a child of the impl block.

The symbol extraction code handles both, but the chain resolution may have an edge case
for TS class methods.

## Investigation Steps (Agent Should Follow)

1. **Reproduce the failure** with a minimal TypeScript file containing a class with a method.
   Call `delete_symbol` with `file.ts::ClassName.methodName`.

2. **Check which error path fires**: Add logging to `delete_symbol_impl` to distinguish
   between `INVALID_TARGET` from the reference check vs. `INVALID_TARGET` from
   `resolve_edit_content`.

3. **If INVALID_TARGET from reference check**: The fix is to improve the `rg` command
   to exclude import lines and type annotations. Example:
   ```bash
   rg -l -w --type ts -e 'audit' --but-not 'import.*audit|type.*audit'
   ```
   Or use `search_codebase` with `filter_mode=code_only` instead of raw `rg`.

4. **If SYMBOL_NOT_FOUND from resolve_edit_content**: The fix is in the tree-sitter
   symbol extraction or chain resolution for TS class methods. Check if the symbol
   tree correctly nests methods under classes.

## Scope

| File | Function | Change |
|------|----------|--------|
| `crates/pathfinder/src/server/tools/edit/handlers.rs` | `delete_symbol_impl` (lines 392-435) | Improve reference check accuracy |
| `crates/pathfinder-treesitter/src/symbols.rs` | `resolve_symbol_chain` (if Hypothesis B) | Fix TS class method resolution |

## Current Code (Reference Check)

```rust
// Lines 392-435 in handlers.rs
let mut cmd = tokio::process::Command::new("rg");
cmd.arg("-l")
    .arg("-w")
    .arg("--")
    .arg(symbol_name)
    .arg(&workspace_path)
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::null());

if let Ok(out) = cmd.output().await {
    if out.status.success() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut reference_count = 0u32;
        for line in stdout.lines() {
            let line = line.trim();
            if line.is_empty() { continue; }
            if line != absolute_target {
                reference_count += 1;
            }
        }
        if reference_count > 0 {
            return Err(...INVALID_TARGET...);
        }
    }
}
```

## Target Code

Replace the raw `rg` approach with the existing `search_codebase` infrastructure:

```rust
// Use self.scout.search() with code_only filter to avoid false positives
// from comments, imports, and type annotations.
let search_result = self
    .scout
    .search(&pathfinder_search::SearchParams {
        workspace_root: self.workspace_root.path().to_path_buf(),
        query: symbol_name.clone(),
        is_regex: false,
        max_results: 50,
        path_glob: format!("**/*.{{rs,ts,tsx,js,jsx,go,py,vue}}"),
        exclude_glob: format!("**/{}.test.*", /* strip extension */ ""),
        context_lines: 0,
    })
    .await;

if let Ok(result) = search_result {
    let reference_files: std::collections::HashSet<&str> = result
        .matches
        .iter()
        .filter(|m| {
            let m_path = std::path::Path::new(&m.file);
            m_path != std::path::Path::new(&semantic_path.file_path)
        })
        .map(|m| m.file.as_str())
        .collect();

    if !reference_files.is_empty() {
        return Err(pathfinder_to_error_data(&PathfinderError::InvalidTarget {
            semantic_path: params.semantic_path.clone(),
            reason: format!(
                "Symbol '{symbol_name}' is still referenced in {} other file(s). ...",
                reference_files.len()
            ),
            edit_index: None,
            valid_edit_types: None,
        }));
    }
}
```

Note: This uses the existing `scout` which has better filtering than raw `rg -w`.
If the scout is not available on the server, keep the `rg` fallback but add
`--type-not markdown` and exclude the target file more precisely.

## Exclusions

- Do NOT remove the reference check entirely — it prevents breaking changes.
- Do NOT change the `ignore_validation_failures` bypass mechanism.
- If Hypothesis B is confirmed, do NOT change the Rust impl method resolution —
  it works correctly.

## Verification

```bash
# Create a TS file with a class method, verify delete_symbol works:
cargo test -p pathfinder -- test_delete_symbol_typescript_class_method
```

## Tests

### Test 1: test_delete_symbol_typescript_class_method
```rust
// Create temp workspace with:
// logger.ts containing class Logger with method audit()
// main.ts that imports Logger but does NOT call audit()
// Verify: delete_symbol("logger.ts::Logger.audit") succeeds
```

### Test 2: test_delete_symbol_blocked_by_real_reference
```rust
// Same setup but main.ts DOES call logger.audit()
// Verify: delete_symbol returns INVALID_TARGET with reference count
```
