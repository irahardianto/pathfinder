# GAP-008: Improve Error Responses with Remediation Hints

## Group: D (Low) — Agent Quality of Life
## Depends on: Nothing

## Objective

Both reports identified that error responses for LSP failures are unhelpful:
- `LspError::Timeout` → "LSP timed out on 'textDocument/definition' after 10000ms"
- `LspError::LspError` → "LSP error: [raw message]"
- `SYMBOL_NOT_FOUND` → "Symbol 'X' not found" (no hint on what to do)

Agents cannot self-correct from these messages. Adding remediation hints
("Fall back to search_codebase", "Use read_source_file to discover symbols",
"Check lsp_health for current status") would reduce retry loops.

## Scope

| File | Function | Change |
|------|----------|--------|
| `crates/pathfinder-common/src/error.rs` | `PathfinderError::LspError` | Add remediation hint |
| `crates/pathfinder-common/src/error.rs` | `PathfinderError::hint()` | Extend with LSP-specific hints |
| `crates/pathfinder/src/server/tools/navigation.rs` | error returns | Include degraded_reason in LspError messages |

## Current Code

```rust
// In error.rs
PathfinderError::LspError { message } => {
    Some(format!("LSP error: {message}"))
}
```

## Target Code

```rust
// In error.rs, improve LspError hint
PathfinderError::LspError { message } => {
    let hint = if message.contains("timed out") {
        format!(
            "LSP timed out. The language server is running but not responding to queries. \
             Possible causes: still indexing, memory pressure, or internal deadlock. \
             Workaround: use search_codebase + read_symbol_scope (tree-sitter) instead of \
             LSP-dependent tools (get_definition, analyze_impact, read_with_deep_context). \
             Original error: {message}"
        )
    } else if message.contains("connection lost") {
        format!(
            "LSP process crashed or disconnected. Pathfinder will attempt to restart it. \
             Workaround: use tree-sitter-based tools (search_codebase, read_symbol_scope). \
             Original error: {message}"
        )
    } else {
        format!("LSP error: {message}. Workaround: use search_codebase for text-based navigation.")
    };
    Some(hint)
}
```

Also improve the `NoLspAvailable` hint:

```rust
PathfinderError::NoLspAvailable { language } => {
    Some(format!(
        "No LSP available for {language}. Install a language server to enable LSP-dependent features. \
         Tree-sitter tools (read_symbol_scope, search_codebase, read_source_file) still work without LSP."
    ))
}
```

## Exclusions

- Do NOT change error codes or error structure — only improve hint text.
- Do NOT add error codes for specific LSP timeout types — the existing `LspError`
  variants already distinguish these.

## Verification

```bash
cargo test -p pathfinder-common -- test_lsp_error_hint_includes_workaround
cargo test -p pathfinder-common -- test_no_lsp_hint_mentions_tree_sitter
```

## Tests

### Test 1: test_lsp_error_hint_includes_workaround
```rust
let err = PathfinderError::LspError {
    message: "LSP timed out on 'textDocument/definition' after 10000ms".to_owned(),
};
let hint = err.hint().unwrap();
assert!(hint.contains("search_codebase"));
assert!(hint.contains("tree-sitter"));
```

### Test 2: test_no_lsp_hint_mentions_tree_sitter
```rust
let err = PathfinderError::NoLspAvailable {
    language: "go".to_owned(),
};
let hint = err.hint().unwrap();
assert!(hint.contains("go"));
assert!(hint.contains("tree-sitter"));
```
