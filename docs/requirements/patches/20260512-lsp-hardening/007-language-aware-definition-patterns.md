# 007: Language-Aware Definition Patterns

**Epic**: 3 — Richer Grep Fallbacks
**Status**: ✅ Complete (2026-05-12)
**Severity**: Medium
**Risk**: Medium — regex changes affect false-positive/negative rates

---

## Problem

The current grep fallback for `get_definition` uses a single mega-regex pattern to match symbol definitions across all languages:

```rust
r"(?:pub\s*(?:\([^)]*\)\s*)?)?(?:async\s+)?fn\s+{symbol}\b|..."
```

This pattern:
1. **Over-matches**: Captures variable declarations like `var symbol_name = ...` in Go when looking for a function
2. **Under-matches**: Misses language-specific syntax like Python decorators (`@property def symbol`), Go receiver methods (`func (s *Service) symbol(...)`), or Rust associated functions in `impl` blocks
3. **Produces noisy results**: Comments and strings containing the symbol name are returned alongside real definitions

### Impact

When LSP is unavailable and the agent relies on grep-based `get_definition`, it receives a list of candidates with low signal-to-noise ratio. Agents waste time parsing irrelevant matches.

---

## Proposed Solution

Replace the single mega-regex with a `definition_patterns(ext, symbol_name)` function that returns language-specific regex patterns:

```rust
fn definition_patterns(ext: &str, symbol_name: &str) -> Vec<String> {
    match ext {
        "rs" => vec![
            // Functions (pub, async, const, unsafe variants)
            format!(r"(?:pub\s*(?:\([^)]*\)\s*)?)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?fn\s+{symbol_name}\b"),
            // Types (struct, enum, trait, type alias, mod)
            format!(r"(?:pub\s*(?:\([^)]*\)\s*)?)?(?:struct|enum|trait|type|mod)\s+{symbol_name}\b"),
            // Constants and statics
            format!(r"(?:pub\s*(?:\([^)]*\)\s*)?)?(?:const|static)\s+{symbol_name}\b"),
        ],
        "ts" | "tsx" | "js" | "jsx" => vec![
            // Functions (export, async, default variants)
            format!(r"(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s+{symbol_name}\b"),
            // Classes, interfaces, type aliases, enums
            format!(r"(?:export\s+)?(?:default\s+)?(?:abstract\s+)?(?:class|interface|type|enum)\s+{symbol_name}\b"),
            // Const/let/var exports
            format!(r"(?:export\s+)?(?:const|let|var)\s+{symbol_name}\s*[=:]"),
        ],
        "py" => vec![
            // Functions and async functions
            format!(r"(?:async\s+)?def\s+{symbol_name}\b"),
            // Classes
            format!(r"class\s+{symbol_name}\b"),
            // Module-level assignments (constants)
            format!(r"^{symbol_name}\s*[=:]"),
        ],
        "go" => vec![
            // Functions and methods (with receiver)
            format!(r"func\s+(?:\([^)]+\)\s+)?{symbol_name}\b"),
            // Type declarations
            format!(r"type\s+{symbol_name}\s+"),
            // Constants and variables
            format!(r"(?:const|var)\s+{symbol_name}\b"),
        ],
        _ => vec![format!(r"\b{symbol_name}\b")],
    }
}
```

### Integration Points

This function is used in:
1. `fallback_definition_grep` in `navigation.rs` (existing, replace current patterns)
2. `grep_reference_fallback` in `navigation.rs` (optional enhancement — use definition patterns to distinguish definition from usage)
3. `find_symbol` tool (Epic 4.1 — uses these patterns for discovery)

### Files to Modify

| File | Change |
|------|--------|
| `crates/pathfinder/src/server/tools/navigation.rs` | Add `definition_patterns()` function; wire into `fallback_definition_grep` |

---

## Acceptance Criteria

- [ ] `definition_patterns` returns language-specific regex vectors for rs, ts/tsx/js/jsx, py, go
- [ ] Rust patterns handle `pub(crate)`, `async`, `unsafe`, `const fn` variants
- [ ] TypeScript patterns handle `export default`, `abstract class`
- [ ] Python patterns handle `async def`, module-level assignments
- [ ] Go patterns handle receiver methods `func (s *Service) Method(...)`
- [ ] Unknown extensions fall back to bare `\b{symbol}\b`
- [ ] All patterns are valid regex (compiled without error)
- [ ] Integration with `fallback_definition_grep` replaces the current mega-regex

---

## Test Plan

| Test | Description |
|------|-------------|
| `test_definition_patterns_rust_fn` | `pub async fn foo` matched; `let foo =` not matched |
| `test_definition_patterns_rust_struct` | `pub(crate) struct Foo` matched |
| `test_definition_patterns_ts_export_class` | `export default class Foo` matched |
| `test_definition_patterns_py_async_def` | `async def process_order` matched |
| `test_definition_patterns_go_receiver_method` | `func (s *Service) HandleRequest(...)` matched |
| `test_definition_patterns_unknown_extension` | `.java` → bare word boundary pattern |

---

## Verification

```bash
cargo test -p pathfinder-mcp -- definition_patterns
cargo clippy -p pathfinder-mcp -- -D warnings
```
