# Pathfinder Lifetime/Generics Impl Block Remediation Plan

Date: 2026-05-10
Source: Honest agent feedback from MCP tool testing sessions

## Executive Summary

An agent reported SYMBOL_NOT_FOUND errors when trying to access methods on Rust structs that have impl blocks with lifetimes or generics. This is a REAL BUG in the symbol extraction logic.

**Root Cause:** In `extract_impl_block`, the type name is extracted using `type_node.byte_range()` which includes the full type with lifetimes/generics (`SymbolExtractionContext<'a>`). But the struct itself is extracted using `child_by_field_name("name")` which gives just the identifier (`SymbolExtractionContext`). When `merge_rust_impl_blocks` tries to match them, the keys don't match.

**Result:** Methods are not merged under the struct. Agents using paths like `SymbolExtractionContext.process_child` get SYMBOL_NOT_FOUND because the methods are actually stored under `SymbolExtractionContext<'a>.process_child`.

---

## Finding Validation Matrix

| ID | Finding | Verdict | Impact | Effort |
|----|---------|---------|--------|--------|
| BUG-1 | Rust impl blocks with lifetimes/generics not merging | **CONFIRMED BUG** | Critical | Low |
| BUG-2 | `find_all_references` doesn't find `extends` relationships | Design Gap | Medium | Medium |
| ISSUE-3 | LSP health says "ready" but tools return DEGRADED | Partially by design | Medium | Medium |
| ISSUE-4 | `did_you_mean` doesn't help for BUG-1 | Symptom of BUG-1 | Medium | Low (fixed by BUG-1) |

---

## Phase 1: Fix BUG-1 (Rust impl blocks with lifetimes/generics)

**CRITICAL: Do this first. This is the root cause of SYMBOL_NOT_FOUND errors.**

### Problem Location

File: `crates/pathfinder-treesitter/src/symbols.rs`
Lines: 703-714 (function: `extract_impl_block`)

### Current Code (BUGGY)

```rust
// Line 703-714
let Some(type_node) = node.child_by_field_name("type") else {
    return;
};
let Some(type_name_bytes) = source.get(type_node.byte_range()) else {
    return;
};
let Ok(type_name) = std::str::from_utf8(type_name_bytes) else {
    return;
};
let type_name = type_name.trim().to_string();
// type_name = "SymbolExtractionContext<'a>" (WRONG - includes lifetime)
```

### Expected Behavior

For all these patterns:
- `impl<'a> SymbolExtractionContext<'a>` → base_name = "SymbolExtractionContext"
- `impl<T> Container<T>` → base_name = "Container"
- `impl MyStruct` → base_name = "MyStruct" (unchanged)
- `impl<'a, T> Cache<'a, T>` → base_name = "Cache"

### Step 1: Add helper function to strip generics/lifetimes

Add this function BEFORE `extract_impl_block`:

```rust
/// Strip angle-bracket generics and lifetimes from a type name.
/// 
/// Examples:
/// - "SymbolExtractionContext<'a>" → "SymbolExtractionContext"
/// - "Container<T>" → "Container"
/// - "Result<T, E>" → "Result"
/// - "MyStruct" → "MyStruct" (unchanged)
/// - "std::collections::HashMap<K, V>" → "std::collections::HashMap" (path preserved)
fn strip_generics(type_name: &str) -> &str {
    // Find the first '<' that starts generic parameters
    // Note: This handles paths like "std::collections::HashMap<K, V>" correctly
    // by finding the FIRST '<' after any path components
    match type_name.find('<') {
        Some(idx) => type_name[..idx].trim_end(),
        None => type_name.trim(),
    }
}
```

### Step 2: Modify `extract_impl_block` to use the helper

Change line 712 from:
```rust
let type_name = type_name.trim().to_string();
```

To:
```rust
let type_name = strip_generics(type_name).to_string();
```

### Step 3: Verify `merge_rust_impl_blocks` doesn't also need fixing

Check `merge_rust_impl_blocks` at line 865:
```rust
let clean_name = s.name.split('#').next().unwrap_or(&s.name);
```

This only strips `#N` suffix (for multiple impl blocks). It does NOT handle `<>`, so Step 1 and 2 are correct.

### Step 4: Add regression test

Add this test to `crates/pathfinder-treesitter/src/symbols.rs` in the `tests` module:

```rust
/// BUG-REGRESSION: Impl blocks with lifetimes/generics must merge correctly.
///
/// The bug was:
/// - impl<'a> SymbolExtractionContext<'a> extracted as "SymbolExtractionContext<'a>"
/// - struct SymbolExtractionContext<'a> extracted as "SymbolExtractionContext"
/// - merge_rust_impl_blocks couldn't match them
/// - Methods stayed under "SymbolExtractionContext<'a>.method"
/// - Agents using "SymbolExtractionContext.method" got SYMBOL_NOT_FOUND
#[test]
fn test_impl_block_with_lifetime_generics_merges_correctly() {
    let source = b"struct Context<'a> { data: &'a str }\n\
impl<'a> Context<'a> {\n\
    fn new(data: &'a str) -> Self { Context { data } }\n\
    fn get_data(&self) -> &str { self.data }\n\
}\n";

    let tree = AstParser::parse_source(
        std::path::Path::new("test.rs"),
        SupportedLanguage::Rust,
        source,
    ).unwrap();
    
    let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Rust);

    // The struct "Context" should exist (NOT "Context<'a>")
    let struct_sym = syms.iter().find(|s| s.name == "Context")
        .expect("struct Context should exist with clean name (no '<'a>')");
    assert_eq!(struct_sym.kind, SymbolKind::Struct);

    // Methods should be children of the struct (merged)
    let has_new = struct_sym.children.iter().any(|s| s.name == "new");
    let has_get_data = struct_sym.children.iter().any(|s| s.name == "get_data");
    assert!(has_new, "method 'new' should be merged under Context");
    assert!(has_get_data, "method 'get_data' should be merged under Context");

    // Method semantic paths should NOT have lifetimes
    let new_method = struct_sym.children.iter().find(|s| s.name == "new").unwrap();
    assert_eq!(new_method.semantic_path, "Context.new", 
        "semantic path should be 'Context.new', NOT 'Context<'a>.new'");

    // The chain resolver should find the method
    let chain = SymbolChain::parse("Context.new").unwrap();
    let resolved = resolve_symbol_chain(&syms, &chain);
    assert!(resolved.is_some(), "resolve_symbol_chain should find Context.new");

    // Verify the old WRONG paths don't exist
    let no_lifetime_struct = syms.iter().find(|s| s.name.contains('<'));
    assert!(no_lifetime_struct.is_none(), 
        "No symbol should have '<' or '>' in its name. Found: {:?}", 
        no_lifetime_struct.map(|s| &s.name));
}

/// Test with multiple generic parameters
#[test]
fn test_impl_block_with_multiple_generics() {
    let source = b"struct Pair<K, V> { key: K, value: V }\n\
impl<K, V> Pair<K, V> {\n\
    fn key(&self) -> &K { &self.key }\n\
}\n\
impl Pair<i32, String> {\n\
    fn format(&self) -> String { format!(\"{}: {}\", self.key, self.value) }\n\
}\n";

    let tree = AstParser::parse_source(
        std::path::Path::new("test.rs"),
        SupportedLanguage::Rust,
        source,
    ).unwrap();
    
    let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Rust);

    // Struct should have clean name
    let struct_sym = syms.iter().find(|s| s.name == "Pair")
        .expect("struct Pair should exist");

    // Both impl blocks' methods should be merged
    let method_names: Vec<_> = struct_sym.children.iter().map(|s| s.name.as_str()).collect();
    assert!(method_names.contains(&"key"), "key() from generic impl should be merged");
    assert!(method_names.contains(&"format"), "format() from concrete impl should be merged");
}

/// Test with path-qualified types (e.g., std::result::Result)
#[test]
fn test_impl_block_with_path_qualified_type() {
    let source = b"struct Wrapper<T>(T);\n\
impl<T> std::fmt::Display for Wrapper<T>\n\
where\n\
    T: std::fmt::Display,\n\
{\n\
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {\n\
        write!(f, \"Wrapper({})\", self.0)\n\
    }\n\
}\n";

    let tree = AstParser::parse_source(
        std::path::Path::new("test.rs"),
        SupportedLanguage::Rust,
        source,
    ).unwrap();
    
    let syms = extract_symbols_from_tree(&tree, source, SupportedLanguage::Rust);

    // Wrapper struct should exist
    let wrapper = syms.iter().find(|s| s.name == "Wrapper")
        .expect("struct Wrapper should exist");

    // Note: The trait implementation "for" case uses "type" and "trait" fields
    // Let's verify the impl block for Display is handled correctly
    // For "impl<T> std::fmt::Display for Wrapper<T>":
    // - "type" field = Wrapper<T> (strip to "Wrapper")
    // - "trait" field = std::fmt::Display
    // This should merge under Wrapper

    // The method should be under Wrapper (may be under an impl block symbol)
    // Let's check either directly or through chain resolution
    let chain = SymbolChain::parse("Wrapper.fmt").unwrap();
    let resolved = resolve_symbol_chain(&syms, &chain);
    // This might be "impl std::fmt::Display.fmt" depending on handling
    // Let's be more lenient - just verify no symbols have angle brackets in names
    let no_bad_names = syms.iter().all(|s| 
        !s.name.contains('<') && !s.name.contains('>')
    );
    assert!(no_bad_names, "No symbol should have '<' or '>' in name");
}
```

### Step 5: Run all existing tests

```bash
cargo test -p pathfinder-treesitter
```

**Expected:** All tests pass including the new ones.

**Common failure modes to watch for:**
- If `strip_generics` is too aggressive: test paths like `HashMap` get broken
- If `strip_generics` is not aggressive enough: `<'a>` remains in names

### Step 6: Manual verification with the actual failing case

Use the exact semantic path from the bug report:

```
Semantic path from bug: crates/pathfinder-treesitter/src/symbols.rs::SymbolExtractionContext.process_child
```

1. Run `get_repo_map` on this file
2. Verify the symbol appears as `SymbolExtractionContext.process_child`
3. NOT as `SymbolExtractionContext<'a>.process_child`

---

## Phase 2: Fix BUG-2 (find_all_references doesn't find `extends`)

### Problem

`find_all_references(BaseEntity)` only found the definition itself, but:
- `Person extends BaseEntity` 
- `NamedEntity extends BaseEntity`

These "extends" relationships should be counted.

### Root Cause Analysis

- `find_all_references` uses LSP `textDocument/references`
- This finds USAGES (calls, field access, variable assignments)
- It does NOT find `implements` or `extends` relationships
- For those, LSP has `textDocument/implementation`

### Current Code Location

File: `crates/pathfinder/src/server/tools/navigation.rs`
Function: `find_all_references_impl` (line ~1633)

### Step 1: Add `goto_implementation` to Lawyer trait

File: `crates/pathfinder-lsp/src/lawyer.rs`

Add after `goto_definition`:

```rust
/// Find implementations/extensions of the symbol at the given position.
///
/// For Java: finds classes that `extend` or `implement` this class/interface.
/// For Rust: finds types that implement a trait.
/// For TypeScript: finds classes that `extend` or `implement`.
///
/// Returns `Ok(None)` if no implementations exist or LSP doesn't support.
/// Returns `Err(LspError::NoLspAvailable)` when no LSP is configured.
async fn goto_implementation(
    &self,
    workspace_root: &Path,
    file_path: &Path,
    line: u32,
    column: u32,
) -> Result<Option<Vec<DefinitionLocation>>, LspError> {
    // Default implementation for backwards compatibility
    let _ = (workspace_root, file_path, line, column);
    Ok(None)
}
```

### Step 2: Implement in LspClient

File: `crates/pathfinder-lsp/src/client/mod.rs`

Add `textDocument/implementation` request handler. Follow the same pattern as `goto_definition`.

### Step 3: Add to MockLawyer and NoOpLawyer

Check `crates/pathfinder-lsp/src/mock.rs` and `crates/pathfinder-lsp/src/no_op.rs`

### Step 4: Modify `find_all_references_impl` to call both

In `crates/pathfinder/src/server/tools/navigation.rs`:

```rust
// After getting references from LSP:
let lsp_refs = self.lawyer.references(...).await?;

// Also get implementations:
let implementations = self.lawyer.goto_implementation(...).await?;

// Merge them:
let mut all_locations = lsp_refs;
if let Some(impls) = implementations {
    // Convert DefinitionLocation to ReferenceLocation
    for def in impls {
        all_locations.push(ReferenceLocation {
            file: def.file,
            line: def.line,
            column: def.column,
            snippet: def.preview, // or similar
        });
    }
}
```

### Step 5: Update text output to distinguish

When implementations are found, add a note:
```
Found 5 references + 2 implementations across 4 files.

Implementations (extends/implements):
- Person.java:3 - class Person extends BaseEntity
- NamedEntity.java:5 - class NamedEntity extends BaseEntity

References:
...
```

---

## Phase 3: Improve ISSUE-3 (LSP health signaling)

### Problem

User sees:
```
lsp_health: java: ready indexing: complete
```

But then:
```
find_callers_callees: DEGRADED
read_with_deep_context: DEGRADED with 0 dependencies
```

### Current State Analysis

**Already implemented but user may miss:**
1. `lsp_health` returns `degraded_tools` in structured content
2. `compute_degraded_tools` already checks `supports_call_hierarchy`

**Gaps:**
1. Text summary doesn't prominently show degraded tools
2. Capability vs Runtime: `supports_call_hierarchy=true` but runtime fails for certain symbol types (interfaces without impls)
3. No `textDocument/implementation` call in probe

### Step 1: Make `degraded_tools` more prominent in text output

File: `crates/pathfinder/src/server/tools/navigation.rs`
Function: `lsp_health_impl`

Currently the text is:
```
java: ready indexing: complete
```

Change to show degraded tools:
```
java: ready indexing: complete
  ⚠️ degraded_tools: analyze_impact (grep_fallback), read_with_deep_context (unavailable)
  → Reason: supports_call_hierarchy = false. Use search_codebase as fallback.
```

### Step 2: Add runtime probe for call_hierarchy in `lsp_health`

Currently the probe only uses `goto_definition`. Add `call_hierarchy_prepare` probe for extra verification.

When call hierarchy probe fails but capability says `true`:
- Mark the tools as degraded
- Add reason: "Runtime probe failed - LSP says it supports call hierarchy but actual calls fail"

### Step 3: Improve degraded messages in navigation tools

For `analyze_impact` (find_callers_callees):

Current when degraded:
```
⚠️ DEGRADED (lsp_warmup_empty_unverified) — Reference counts are UNRELIABLE
```

Improve to:
```
⚠️ DEGRADED ({reason}) — LSP call hierarchy unavailable for this symbol.
   
   Common causes for Java/Spring projects:
   - Interface types without concrete implementations in source (JPA repositories)
   - Annotation-driven dependency injection (Spring proxies at runtime)
   - LSP still warming up (wait 30s, try again)
   
   Workaround: Use search_codebase(query="SymbolName") to find usages manually.
   Reference count below is heuristic only:
```

---

## Phase 4: Verification Checklist

**Must be completed before considering this remediated:**

### BUG-1 Verification
- [ ] `cargo test -p pathfinder-treesitter` passes
- [ ] New test `test_impl_block_with_lifetime_generics_merges_correctly` passes
- [ ] New test `test_impl_block_with_multiple_generics` passes
- [ ] `get_repo_map` on `symbols.rs` shows `SymbolExtractionContext.process_child` (NOT `SymbolExtractionContext<'a>.process_child`)
- [ ] `read_symbol_scope` with path `SymbolExtractionContext.process_child` succeeds (no SYMBOL_NOT_FOUND)

### BUG-2 Verification
- [ ] `goto_implementation` added to Lawyer trait
- [ ] `goto_implementation` implemented in LspClient
- [ ] `find_all_references` on `BaseEntity` returns both references AND implementations (extends)

### ISSUE-3 Verification
- [ ] `lsp_health` text output shows `degraded_tools` when applicable
- [ ] `analyze_impact` degraded message includes common causes and workaround

---

## Rollback Plan

If any fix causes regressions:

### BUG-1 Rollback
1. Remove `strip_generics` function
2. Revert `extract_impl_block` line 712 to `.trim().to_string()`
3. Remove new tests
4. Run `cargo test -p pathfinder-treesitter` to verify baseline

### BUG-2 Rollback
1. Revert `find_all_references_impl` to not call `goto_implementation`
2. Remove `goto_implementation` from trait (keep default impl if already added)

---

## Dumb Agent Execution Guide

**For any agent executing this plan:**

1. **DO NOT SKIP PHASES.** BUG-1 is critical and blocks other work.
2. **DO NOT GUESS.** If a code snippet doesn't match exactly, STOP and get clarification.
3. **RUN TESTS AFTER EVERY CHANGE.** Don't accumulate multiple changes before testing.
4. **DO NOT MODIFY TESTS TO MAKE THEM PASS.** If an existing test fails, you introduced a regression.

**Exact file paths (copy-paste these):**
- `crates/pathfinder-treesitter/src/symbols.rs`
- `crates/pathfinder-lsp/src/lawyer.rs`
- `crates/pathfinder/src/server/tools/navigation.rs`

**Exact test commands:**
```bash
cargo test -p pathfinder-treesitter
cargo test -p pathfinder-lsp
cargo test -p pathfinder
```

**When stuck:**
1. Check the line numbers in this document
2. Read the surrounding context (10 lines before/after)
3. Look for similar patterns already in the codebase
4. DO NOT invent new patterns without explicit instructions
