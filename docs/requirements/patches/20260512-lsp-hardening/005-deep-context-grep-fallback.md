# 005: Grep Fallback for `read_with_deep_context`

**Epic**: 2 — LSP Hardening
**Status**: ☐ Pending
**Severity**: Medium
**Risk**: Medium — adds heuristic dependency discovery; may produce false positives

---

## Problem

`read_with_deep_context` calls `resolve_lsp_dependencies()` to discover the outgoing callees of a symbol (i.e., what functions does this function call?). When LSP is unavailable (warming up, timed out, or not supported), the tool returns `degraded: true` with **zero dependencies** — giving the agent no information about the symbol's call graph.

This contrasts with `get_definition` and `analyze_impact`, which both have grep-based fallbacks that provide heuristic results when LSP is unavailable. `read_with_deep_context` is the only navigation tool with no fallback strategy.

### Impact

Agents reading a function to understand its dependencies get the source code but no context about what it calls. They must then manually search for each function call — losing the key value proposition of `read_with_deep_context`.

---

## Proposed Solution

When `resolve_lsp_dependencies` returns `degraded: true` with empty dependencies, parse the symbol body for function call patterns and resolve each called name via grep:

### Algorithm

1. Extract the symbol source code (already available from tree-sitter)
2. Parse for call-like patterns using a language-aware regex:
   - Rust/Go: `\b(\w+)\s*\(` (function calls)
   - TypeScript/JS: `\b(\w+)\s*\(` + `\b(\w+)\s*\.(\w+)\s*\(` (method calls)
   - Python: `\b(\w+)\s*\(` + `\b(\w+)\s*\.(\w+)\s*\(` (method calls)
3. Deduplicate candidate names
4. Filter out language keywords (`if`, `for`, `while`, `match`, `return`, etc.)
5. For each candidate, run `search_codebase` with language-aware definition patterns to find the definition
6. Return as heuristic dependencies with `degraded_reason: GrepFallbackDependencies`

### Constraints

- Cap at 20 candidate names to prevent excessive search calls
- Cap at 50 total dependencies (matching `default_max_dependencies`)
- Each search is bounded to `max_results: 5` for performance
- Results labeled with `"heuristic"` in the dependency metadata

### Files to Modify

| File | Change |
|------|--------|
| `crates/pathfinder/src/server/tools/navigation.rs` | Add grep dependency fallback in `read_with_deep_context_impl` after LSP failure branches |
| `crates/pathfinder-common/src/types.rs` | Add `GrepFallbackDependencies` variant to `DegradedReason` |

---

## Acceptance Criteria

- [ ] When LSP returns degraded with empty deps, grep fallback runs automatically
- [ ] Fallback extracts function call patterns from symbol body
- [ ] Language keywords are filtered out (no `if`, `for`, `while`, etc. as "dependencies")
- [ ] Each candidate is resolved to a definition file+line via grep search
- [ ] Results capped at 50 dependencies
- [ ] `degraded_reason` set to `GrepFallbackDependencies`
- [ ] `degraded: true` remains set (results are heuristic, not authoritative)
- [ ] Source code is always returned even when dependency discovery fails

---

## Test Plan

| Test | Description |
|------|-------------|
| `test_deep_context_grep_fallback_finds_deps` | Symbol calling `foo()` and `bar()` → grep finds definitions for both |
| `test_deep_context_grep_fallback_filters_keywords` | Symbol with `if (x)` and `for (i)` → `if` and `for` not in deps |
| `test_deep_context_grep_fallback_caps_at_limit` | Symbol with 30 calls → only first 20 candidates searched |
| `test_deep_context_grep_fallback_returns_source_on_failure` | Grep search fails → source code still returned, empty deps |

---

## Verification

```bash
cargo test -p pathfinder-mcp -- deep_context_grep_fallback
cargo clippy -p pathfinder-mcp -- -D warnings
```

---

## Example Output

```json
{
  "source": "async fn process_order(db: &Database, order: Order) -> Result<()> {\n    let user = fetch_user(db, order.user_id).await?;\n    validate_inventory(&order).await?;\n    charge_payment(&user, order.total).await?;\n    send_confirmation(user.email, &order).await\n}",
  "dependencies": [
    { "name": "fetch_user", "file": "src/users.rs", "line": 42, "signature": "async fn fetch_user(db: &Database, id: UserId) -> Result<User>", "heuristic": true },
    { "name": "validate_inventory", "file": "src/inventory.rs", "line": 18, "signature": "async fn validate_inventory(order: &Order) -> Result<()>", "heuristic": true },
    { "name": "charge_payment", "file": "src/payments.rs", "line": 95, "signature": "async fn charge_payment(user: &User, amount: Decimal) -> Result<()>", "heuristic": true }
  ],
  "degraded": true,
  "degraded_reason": "grep_fallback_dependencies"
}
```
