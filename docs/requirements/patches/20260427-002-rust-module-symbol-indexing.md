# PATCH-002: Rust Module Symbol Indexing (`mod_item`)

**Status:** Planned  
**Priority:** P0 — Critical (blocks agent editing of test code in every Rust file)  
**Estimated Effort:** 4–6 hours  
**Prerequisite:** None  
**PR Strategy:** Standalone PR — Tree-sitter extraction and resolution only, no edit pipeline changes  

---

## Problem Statement

Rust `mod` blocks — most commonly `#[cfg(test)] mod tests { ... }` — are **completely invisible** to
Pathfinder's semantic addressing system. The Tree-sitter grammar for Rust names this node type
`mod_item`, but it does not appear in any of the five categories defined in `LanguageNodeTypes`
(`function_kinds`, `class_kinds`, `method_kinds`, `impl_kinds`, `constant_kinds`).

### What happens today

When `SymbolExtractionContext::process_child` encounters a `mod_item` node:

1. It is not an `impl_kind` → `extract_impl_block` is skipped.
2. `determine_symbol_kind` returns `None` (no category matches `mod_item`).
3. Falls through to the recursive call at line 73–80, which traverses the module's **children**
   but uses the **parent scope's path** as `parent_path` — no `tests::` prefix is injected.
4. All functions inside the module are extracted as **flat top-level symbols**.

**Result:** `cache.rs::tests::test_cache_hits_and_misses` → `SYMBOL_NOT_FOUND`.  
**Actual index entry:** `cache.rs::test_cache_hits_and_misses` (flat, no module namespace).

Agents attempting to use the nested path fail. Agents using the flat path can *read* tests but
cannot *insert* into the test module — because `insert_after(file.rs)` appends at EOF (outside
the closing `}`) and `insert_after(file.rs::last_test_fn)` requires knowing which test is last.

### Scope of impact

- **Every Rust file** with `mod tests { }`, `mod types { }`, or any inline module declaration.
- **All five symbol-targeted edit tools**: `replace_body`, `replace_full`, `insert_before`,
  `insert_after`, `delete_symbol` — all use `resolve_symbol_chain` which traverses the extracted
  symbol tree. Without `tests` as a node in that tree, traversal through it is impossible.
- **`read_symbol_scope`**: cannot target test functions by nested path.
- **`get_repo_map`**: test modules collapse invisibly; the tree shows only flat test functions.

### Scope limitation (Rust only for this patch)

This patch targets Rust `mod_item` only. For other languages:

| Language | Inline module node | Status |
|---|---|---|
| Rust | `mod_item` | **This patch** |
| TypeScript | `internal_module` (namespace) | Future patch (PATCH-005) |
| Go | None (package-level only) | Not applicable |
| Python | None inline | Not applicable |
| JavaScript | None standard | Not applicable |
| Vue | Uses TypeScript grammar for script block | Covered by TS patch |

See `20260427-005-future-language-module-support.md` for the deferred language roadmap.

---

## Visibility Scoping

Test modules (`#[cfg(test)] mod tests`) are **private implementation details**.

**Rule:** Module symbols only appear under `visibility: "all"`.  
**Not shown** in `visibility: "public"` repo maps (the default).

This matches the treatment of underscore-prefixed symbols and private functions.
Update `is_symbol_public` in `repo_map.rs` to return `false` for `SymbolKind::Module`
unless the `mod` is declared `pub`.

**Tool description update required:** Add to `get_repo_map` tool description:
> Module scopes (e.g., Rust `mod tests`, `mod types`) appear only with `visibility: "all"`.

---

## Proposed Changes

### 1. `crates/pathfinder-treesitter/src/surgeon.rs`

Add `Module` variant to `SymbolKind`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SymbolKind {
    Function,
    Class,
    Struct,
    Interface,
    Method,
    Constant,
    Variable,
    Impl,
    Module,   // ← NEW: Rust mod blocks, TS namespaces, etc.
}
```

### 2. `crates/pathfinder-treesitter/src/language.rs`

**2a.** Add `module_kinds` field to `LanguageNodeTypes`:

```rust
pub struct LanguageNodeTypes {
    pub function_kinds: &'static [&'static str],
    pub class_kinds:    &'static [&'static str],
    pub method_kinds:   &'static [&'static str],
    pub impl_kinds:     &'static [&'static str],
    pub constant_kinds: &'static [&'static str],
    /// Node kinds that represent scoped module blocks.
    /// Contents are extracted as named children under the module's path segment.
    /// Example: Rust `mod tests { fn foo() {} }` → `tests` (Module) with child `foo`.
    pub module_kinds:   &'static [&'static str],
}
```

**2b.** Populate for each language:

```rust
// Rust
Self::Rust => &LanguageNodeTypes {
    function_kinds: &["function_item"],
    class_kinds:    &["struct_item", "enum_item", "trait_item", "type_item"],
    method_kinds:   &[],
    impl_kinds:     &["impl_item"],
    constant_kinds: &["const_item", "static_item"],
    module_kinds:   &["mod_item"],              // ← NEW
},

// All others: add empty slice
Self::Go | Self::TypeScript | Self::Tsx |
Self::JavaScript | Self::Python | Self::Vue => &LanguageNodeTypes {
    // ... existing fields unchanged ...
    module_kinds: &[],                          // ← NEW (empty)
},
```

### 3. `crates/pathfinder-treesitter/src/symbols.rs`

**3a.** Update `process_child` to handle `module_kinds` before the fallback-recurse:

```rust
fn process_child(&mut self, child: Node<'a>) {
    let kind = child.kind();

    // Existing: impl blocks
    if self.types.impl_kinds.contains(&kind) {
        extract_impl_block(
            child, self.source, self.types,
            self.parent_path, self.out, &mut self.name_counts,
        );
        return;
    }

    // NEW: module blocks (e.g., `mod tests { ... }`)
    if self.types.module_kinds.contains(&kind) {
        self.extract_module_block(child);
        return;
    }

    // ... rest of process_child unchanged ...
}
```

**3b.** Add `extract_module_block` to `SymbolExtractionContext`:

```rust
/// Extract a module block as a named scope symbol with its children.
///
/// The module's `name` field becomes the scope prefix for all nested symbols.
/// Example: `mod tests { fn test_foo() {} }` becomes:
///   - `tests` (Module, with children)
///     - `test_foo` (Function, path = "tests.test_foo")
fn extract_module_block(&mut self, child: Node<'a>) {
    // Get the module name node (Tree-sitter field: "name")
    let Some(name_node) = child.child_by_field_name("name") else {
        // Unnamed module — recurse with current parent path (safe fallback)
        extract_symbols_recursive(
            child, self.source, self.types, self.lang,
            self.parent_path, self.out,
        );
        return;
    };

    let Some(name) = self.extract_name(name_node) else {
        return;
    };

    let (unique_name, suffix) = make_unique_name(&mut self.name_counts, name);
    let module_path = self.build_path(&unique_name, &suffix);

    let mut children = Vec::new();

    // Extract body contents as children scoped under `module_path`
    if let Some(body) = child.child_by_field_name("body") {
        extract_symbols_recursive(
            body, self.source, self.types, self.lang,
            &module_path, &mut children,
        );
    }

    // Determine visibility: `pub mod` → public, `mod` → private
    // (for use by is_symbol_public in repo_map.rs)
    self.out.push(ExtractedSymbol {
        name: unique_name,
        semantic_path: module_path,
        kind: SymbolKind::Module,
        byte_range: child.byte_range(),
        start_line: child.start_position().row,
        end_line:   child.end_position().row,
        children,
    });
}
```

### 4. `crates/pathfinder-treesitter/src/repo_map.rs`

**4a.** Update `is_symbol_public` to treat `Module` as private by default (like `Impl`):

```rust
fn is_symbol_public(symbol: &ExtractedSymbol, lang: SupportedLanguage) -> bool {
    match symbol.kind {
        // Impl blocks are always considered internal scaffolding
        SymbolKind::Impl => false,
        // Module blocks are private unless explicitly `pub mod`
        // For now, conservatively treat all modules as private (visibility: "all" only)
        SymbolKind::Module => false,
        // ... existing logic for other kinds ...
    }
}
```

**4b.** Update `render_symbols_recursive` to render `Module` like `Class` (with indented children
in the skeleton output):

```
├── tests [Module] L354-L607 (tests)
│   ├── test_cache_hits_and_misses [Function] L362-L408
│   └── test_cache_eviction_lru [Function] L411-L460
```

### 5. Tool description updates

In the MCP tool descriptor for `get_repo_map`:

```
// Add to description:
"Module scopes (e.g., Rust `mod tests`, `mod types`) are only shown when
`visibility` is set to `\"all\"`. They are hidden in public-only maps."
```

---

## Implementation Steps

Each step must produce a green build:

1. **Add `Module` to `SymbolKind`** — add variant, update any exhaustive `match` arms  
2. **Add `module_kinds` to `LanguageNodeTypes`** — add field, populate all language arms  
3. **Add `extract_module_block`** — new method on `SymbolExtractionContext`  
4. **Update `process_child`** — add `module_kinds` branch before fallback recurse  
5. **Update `is_symbol_public`** in `repo_map.rs` — `Module` → private  
6. **Update `render_symbols_recursive`** — render Module with children (indented)  
7. **Update tool descriptions** — `get_repo_map` description updated  
8. **Add tests** (see below)  
9. **Verify:** `cargo test --workspace`, `cargo clippy`, `cargo fmt --check`

---

## Test Plan

All new tests go in `crates/pathfinder-treesitter/src/symbols.rs` (inline `#[cfg(test)]`):

### New tests

```rust
/// PATCH-002-T1: Basic mod block creates Module symbol with children
#[test]
fn test_extract_rust_mod_block_with_children() {
    let source = r#"
fn outer() {}

mod helpers {
    fn inner_one() {}
    fn inner_two() {}
}
"#;
    let symbols = parse_and_extract(source, SupportedLanguage::Rust);
    let module = symbols.iter().find(|s| s.name == "helpers").expect("helpers module not found");
    assert_eq!(module.kind, SymbolKind::Module);
    assert_eq!(module.children.len(), 2);
    assert!(module.children.iter().any(|c| c.name == "inner_one"));
    assert!(module.children.iter().any(|c| c.name == "inner_two"));
    // Module path
    assert_eq!(module.semantic_path, "helpers");
    // Child paths include module prefix
    let child = module.children.iter().find(|c| c.name == "inner_one").unwrap();
    assert_eq!(child.semantic_path, "helpers.inner_one");
}

/// PATCH-002-T2: cfg(test) mod tests is extracted
#[test]
fn test_extract_rust_cfg_test_mod_block() {
    let source = r#"
fn production_code() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic() { assert!(true); }

    #[test]
    fn test_advanced() { assert!(true); }
}
"#;
    let symbols = parse_and_extract(source, SupportedLanguage::Rust);
    let module = symbols.iter().find(|s| s.name == "tests").expect("tests module not found");
    assert_eq!(module.kind, SymbolKind::Module);
    // Should contain the two test functions, but NOT the `use` statement
    assert_eq!(module.children.len(), 2);
    assert!(module.children.iter().any(|c| c.name == "test_basic"));
    assert!(module.children.iter().any(|c| c.name == "test_advanced"));
}

/// PATCH-002-T3: resolve_symbol_chain traverses through module
#[test]
fn test_resolve_symbol_chain_through_module() {
    let source = r#"
mod tests {
    fn test_foo() {}
}
"#;
    let symbols = parse_and_extract(source, SupportedLanguage::Rust);
    let chain = SymbolChain::parse("tests.test_foo").unwrap();
    let resolved = resolve_symbol_chain(&symbols, &chain);
    assert!(resolved.is_some(), "tests.test_foo should resolve");
    assert_eq!(resolved.unwrap().name, "test_foo");
}

/// PATCH-002-T4: Nested mod (mod inside mod) works
#[test]
fn test_extract_rust_nested_mod_blocks() {
    let source = r#"
mod outer {
    mod inner {
        fn deep() {}
    }
}
"#;
    let symbols = parse_and_extract(source, SupportedLanguage::Rust);
    let outer = symbols.iter().find(|s| s.name == "outer").unwrap();
    assert_eq!(outer.kind, SymbolKind::Module);
    let inner = outer.children.iter().find(|c| c.name == "inner").unwrap();
    assert_eq!(inner.kind, SymbolKind::Module);
    let deep = inner.children.iter().find(|c| c.name == "deep").unwrap();
    assert_eq!(deep.name, "deep");
    assert_eq!(deep.semantic_path, "outer.inner.deep");
}

/// PATCH-002-T5: Top-level functions are NOT affected (regression)
#[test]
fn test_extract_rust_top_level_unchanged_with_module_kinds() {
    let source = r#"
fn top_level_a() {}
fn top_level_b() {}

mod helpers {
    fn helper() {}
}
"#;
    let symbols = parse_and_extract(source, SupportedLanguage::Rust);
    // Top-level functions still at root
    assert!(symbols.iter().any(|s| s.name == "top_level_a" && s.kind == SymbolKind::Function));
    assert!(symbols.iter().any(|s| s.name == "top_level_b" && s.kind == SymbolKind::Function));
    // Module present
    assert!(symbols.iter().any(|s| s.name == "helpers" && s.kind == SymbolKind::Module));
    // helper is NOT at root level anymore
    assert!(!symbols.iter().any(|s| s.name == "helper" && s.semantic_path == "helper"));
}
```

---

## Acceptance Criteria

- [ ] `mod_item` nodes in Rust files produce `ExtractedSymbol { kind: Module }` with children
- [ ] `file.rs::tests::test_foo` resolves via `read_symbol_scope` (no `SYMBOL_NOT_FOUND`)
- [ ] Test functions appear nested under `tests` in `read_source_file(detail_level="symbols")`
- [ ] Test functions do **not** appear in `get_repo_map(visibility="public")` output
- [ ] Test functions **do** appear in `get_repo_map(visibility="all")` output, nested under `tests`
- [ ] All 5 new tests pass
- [ ] `cargo test --workspace` passes with zero regressions
- [ ] `cargo clippy --all-targets` produces 0 warnings
- [ ] `cargo fmt --check` passes
