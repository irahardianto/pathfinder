# Phase 0: AccessLevel Enum Refactoring

## Rationale

The current `ExtractedSymbol.is_public: bool` cannot express Java's 4-level visibility (`public`, `protected`, package-private, `private`). This phase replaces it with a proper enum **before** any Java code is added, keeping the refactoring isolated and reviewable.

> **CRITICAL**: `pathfinder-common` already has a `Visibility` enum used as a repo-map filter parameter (`Public | All`). The new per-symbol enum MUST use a different name: `AccessLevel`.

## New Type Definition

**File**: `crates/pathfinder-treesitter/src/surgeon.rs`

```rust
/// The access level of an AST symbol, as determined by language-specific rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccessLevel {
    /// Visible to all consumers (Rust `pub`, Java `public`, Go uppercase, TS `export`).
    #[default]
    Public,
    /// Visible to subclasses/submodules (Java `protected`, Rust `pub(super)`, Python `_prefix`).
    Protected,
    /// Visible within the same package/crate/module (Java default, Go lowercase, Rust `pub(crate)`).
    Package,
    /// Visible only within the declaring scope (Java `private`, Rust no modifier, Python `__prefix`).
    Private,
}
```

**Field change in `ExtractedSymbol`**:
```diff
-    pub is_public: bool,
+    pub access_level: AccessLevel,
```

## New Function: `detect_access_level`

**File**: `crates/pathfinder-treesitter/src/symbols.rs`

Replace `has_visibility_modifier()` with `detect_access_level()`. The new function must preserve the existing **parent-walk logic** for TypeScript/JS `export_statement` detection.

```rust
fn detect_access_level(node: Node, lang: SupportedLanguage, source: &[u8]) -> AccessLevel {
    match lang {
        SupportedLanguage::Rust => detect_rust_access_level(node),
        SupportedLanguage::Go => detect_go_access_level(node, source),
        SupportedLanguage::TypeScript | SupportedLanguage::Tsx
        | SupportedLanguage::JavaScript | SupportedLanguage::Vue => {
            detect_ts_access_level(node)
        }
        SupportedLanguage::Python => detect_python_access_level(node, source),
    }
}
```

### Per-Language Detection Rules

**Rust** — `detect_rust_access_level(node)`:
| AST Signal | AccessLevel |
|-----------|-------------|
| `visibility_modifier` child with text containing `pub(crate)` | `Package` |
| `visibility_modifier` child with text containing `pub(super)` | `Protected` |
| `visibility_modifier` child present (any other `pub`) | `Public` |
| No `visibility_modifier` child | `Private` |

**Go** — `detect_go_access_level(node, source)`:
| AST Signal | AccessLevel |
|-----------|-------------|
| Name starts with uppercase ASCII letter | `Public` |
| Name starts with `_` | `Private` |
| Name starts with lowercase | `Package` |

**TypeScript/JavaScript/Vue** — `detect_ts_access_level(node)`:
Must walk **up the parent chain** (existing behavior from `has_visibility_modifier`):
| AST Signal | AccessLevel |
|-----------|-------------|
| Ancestor is `export_statement` (stop at `program`/`statement_block`) | `Public` |
| Name starts with `_` | `Private` |
| No export ancestor | `Package` |

**Python** — `detect_python_access_level(node, source)`:
| AST Signal | AccessLevel |
|-----------|-------------|
| Name starts with `__` but does NOT end with `__` (not dunder) | `Private` |
| Name starts with `_` (single) | `Protected` |
| No prefix | `Public` |

## Changes to `repo_map.rs`

### Delete `is_symbol_public()` function entirely

### Simplify `filter_by_visibility()`

```diff
-fn filter_by_visibility(
-    symbols: Vec<ExtractedSymbol>,
-    visibility: &str,
-    lang_is_go: bool,
-) -> Vec<ExtractedSymbol> {
+fn filter_by_visibility(
+    symbols: Vec<ExtractedSymbol>,
+    visibility: &str,
+) -> Vec<ExtractedSymbol> {
     if visibility != "public" {
         return symbols;
     }
     symbols
         .into_iter()
-        .filter(|sym| is_symbol_public(sym, lang_is_go))
+        .filter(|sym| matches!(sym.access_level, AccessLevel::Public | AccessLevel::Protected))
         .map(|mut sym| {
-            sym.children = filter_by_visibility(sym.children, visibility, lang_is_go);
+            sym.children = filter_by_visibility(sym.children, visibility);
             sym
         })
         .collect()
 }
```

### Update `generate_skeleton_text()`

```diff
-        let lang_is_go = matches!(lang, crate::language::SupportedLanguage::Go);
-        let symbols = filter_by_visibility(raw_symbols, config.visibility, lang_is_go);
+        let symbols = filter_by_visibility(raw_symbols, config.visibility);
```

## Production Code Changes — Complete List (16 total sites)

| File | Line | Current | Replacement |
|------|------|---------|-------------|
| `surgeon.rs:33` | Field definition | `is_public: bool` | `access_level: AccessLevel` |
| `symbols.rs:140` | `extract_symbol()` | `is_public: true` | `access_level: AccessLevel::Public` |
| `symbols.rs:209` | `extract_module_block()` | `let is_public = has_visibility_modifier(child)` | `let access_level = detect_access_level(child, self.lang, self.source)` |
| `symbols.rs:218` | `extract_module_block()` push | `is_public,` | `access_level,` |
| `symbols.rs:453` | `extract_impl_block()` method | `is_public: true` | `access_level: AccessLevel::Public` |
| `symbols.rs:469` | `extract_impl_block()` impl | `is_public: false` | `access_level: AccessLevel::Private` |
| `symbols.rs:605` | `merge_rust_impl_blocks()` | `is_public: false` | `access_level: AccessLevel::Private` |
| `symbols.rs:733` | `push_zone_symbol()` | `is_public: true` | `access_level: AccessLevel::Public` |
| `symbols.rs:852` | `walk_html_elements_flat()` | `is_public: true` | `access_level: AccessLevel::Public` |
| `symbols.rs:1063` | `emit_jsx_symbol()` | `is_public: true` | `access_level: AccessLevel::Public` |
| `symbols.rs:1170` | `walk_css_rules()` | `is_public: true` | `access_level: AccessLevel::Public` |
| `symbols.rs:1271` | `extract_css_rule_set()` | `is_public: true` | `access_level: AccessLevel::Public` |
| `repo_map.rs:127-149` | `is_symbol_public()` | Entire function | **DELETE** |
| `repo_map.rs:164-175` | `filter_by_visibility()` | 3-param function | 2-param function using `access_level` |
| `repo_map.rs:374-375` | `generate_skeleton_text()` | `lang_is_go` variable + call | Remove variable, update call |
| `source_file.rs:260` | test helper | `is_public: true` | `access_level: AccessLevel::Public` |

## Test Code Changes (26 sites)

All test `ExtractedSymbol` constructors: `is_public: true` → `access_level: AccessLevel::Public`, `is_public: false` → `access_level: AccessLevel::Private`.

Key test assertion changes:
```diff
- assert!(module.is_public, "pub mod should have is_public = true");
+ assert_eq!(module.access_level, AccessLevel::Public);

- assert!(!module.is_public, "bare mod should have is_public = false");
+ assert_eq!(module.access_level, AccessLevel::Private);

- assert!(module.is_public, "pub(crate) mod should have is_public = true");
+ assert_eq!(module.access_level, AccessLevel::Package);  // NEW: finer granularity
```

> **NOTE**: The `pub(crate)` test changes from asserting `true` to asserting `Package`. This is a **semantic improvement**, not a regression.

## Acceptance Criteria

- [ ] AC-0.1: `AccessLevel` enum exists in `surgeon.rs` with `Public`, `Protected`, `Package`, `Private` variants
- [ ] AC-0.2: `ExtractedSymbol` uses `access_level: AccessLevel` field (no `is_public`)
- [ ] AC-0.3: `has_visibility_modifier()` is replaced by `detect_access_level()`
- [ ] AC-0.4: `is_symbol_public()` function is deleted from `repo_map.rs`
- [ ] AC-0.5: `filter_by_visibility()` uses `access_level` enum match (no `lang_is_go` param)
- [ ] AC-0.6: All existing tests pass with updated assertions
- [ ] AC-0.7: `cargo test --workspace` passes
- [ ] AC-0.8: `cargo clippy --workspace` passes with zero warnings
- [ ] AC-0.9: New unit tests for `detect_access_level()` covering all 4 current languages (Rust, Go, TypeScript, Python) × all visibility levels. Java tests are added in Phase 1.

### Test Cases for `detect_access_level()`

```
test_detect_rust_pub_fn → Public
test_detect_rust_pub_crate_mod → Package
test_detect_rust_pub_super_fn → Protected
test_detect_rust_private_fn → Private
test_detect_go_uppercase_function → Public
test_detect_go_lowercase_function → Package
test_detect_go_underscore_function → Private
test_detect_ts_exported_function → Public
test_detect_ts_non_exported_function → Package
test_detect_ts_underscore_function → Private
test_detect_python_public_function → Public
test_detect_python_single_underscore → Protected
test_detect_python_double_underscore → Private
test_detect_python_dunder_method → Public (not Private — __init__ is public)
```
