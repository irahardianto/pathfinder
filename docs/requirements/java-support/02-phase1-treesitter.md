# Phase 1: Tree-sitter Java Integration

## Overview

Add Java as a natively supported language in `pathfinder-treesitter`. This enables symbol extraction, repo maps, search enrichment (`filter_mode`), `read_symbol_scope`, and `read_source_file` for `.java` files — all without any external runtime dependency.

## Prerequisites

- Phase 0 (`AccessLevel` enum) must be merged first

## 1. Dependency Addition

**File**: `crates/pathfinder-treesitter/Cargo.toml`

```diff
 # Language grammars (Tier 1)
 tree-sitter-go = "0.23"
 tree-sitter-typescript = "0.23"
 tree-sitter-python = "0.23"
 tree-sitter-javascript = "0.23"
 tree-sitter-rust = "0.23"
+tree-sitter-java = "0.23"
```

Binary size impact: ~500KB compiled (comparable to other grammar crates).

## 2. SupportedLanguage Enum

**File**: `crates/pathfinder-treesitter/src/language.rs`

### Add `Java` variant

```diff
 pub enum SupportedLanguage {
     Go,
     TypeScript,
     Tsx,
     JavaScript,
     Python,
     Rust,
     Vue,
+    Java,
 }
```

### Update `detect()`

```diff
+            "java" => Some(Self::Java),
```

### Update `as_str()`

```diff
+            Self::Java => "java",
```

### Update `grammar()`

```diff
+            Self::Java => tree_sitter_java::LANGUAGE.into(),
```

### Add `node_types()` for Java

```rust
Self::Java => &LanguageNodeTypes {
    function_kinds: &["method_declaration", "constructor_declaration"],
    class_kinds: &[
        "class_declaration",
        "interface_declaration",
        "enum_declaration",
        "annotation_type_declaration",
        "record_declaration",
    ],
    method_kinds: &[],  // Java methods are functions inside classes (like Python)
    impl_kinds: &[],     // Java has no impl blocks
    constant_kinds: &[],     // See §2.1 below — Java fields are too noisy for constant_kinds
    module_kinds: &[],   // Java packages are directory-based, not AST nodes
},
```

### 2.1 Why `constant_kinds` is Empty for Java

Java's `field_declaration` covers ALL fields: `private String name`, `protected int count`, `public static final int MAX = 100`, etc. Including them in `constant_kinds` would extract every field as a "Constant" symbol — a class with 10 private fields would show 10 noisy children. Other languages use `constant_kinds` only for actual constants (Go `const_declaration`, Rust `const_item`/`static_item`). Java fields are excluded for repo map quality. If needed later, a future enhancement can add smart filtering for `static final` fields only.

### Java Node Type Rationale

| tree-sitter-java Node Kind | Maps To | Notes |
|---------------------------|---------|-------|
| `method_declaration` | `function_kinds` | Instance and static methods |
| `constructor_declaration` | `function_kinds` | Treated as functions (have `name` field) |
| `class_declaration` | `class_kinds` | Regular classes |
| `interface_declaration` | `class_kinds` | Refined to `Interface` by `refine_class_kind` |
| `enum_declaration` | `class_kinds` | Refined to `Enum` by `refine_class_kind` |
| `annotation_type_declaration` | `class_kinds` | `@interface` annotations → `Interface` |
| `record_declaration` | `class_kinds` | Java 16+ records → refined to `Struct` |
| `field_declaration` | *(not mapped)* | Excluded — too noisy (see §2.1) |

## 3. Symbol Extraction Changes

**File**: `crates/pathfinder-treesitter/src/symbols.rs`

### 3.1 Update `refine_class_kind()`

Add Java-specific match arms. Note that `"enum_declaration"` is already handled by the existing `"enum_declaration" | "enum_item"` arm — Java and TypeScript use the same tree-sitter node kind name, so this works automatically with no conflict:

```diff
 fn refine_class_kind(node: Node) -> SymbolKind {
     match node.kind() {
         "enum_declaration" | "enum_item" => SymbolKind::Enum,
         "struct_item" => SymbolKind::Struct,
         "trait_item" => SymbolKind::Interface,
+        "interface_declaration" => SymbolKind::Interface,
+        "annotation_type_declaration" => SymbolKind::Interface,
+        "record_declaration" => SymbolKind::Struct,
         _ => {
             // Go type_spec: refine based on the `type` field
             node.child_by_field_name("type")
                 .map_or(SymbolKind::Class, |type_node| match type_node.kind() {
                     "interface_type" => SymbolKind::Interface,
                     "struct_type" => SymbolKind::Struct,
                     _ => SymbolKind::Class,
                 })
         }
     }
 }
```

### 3.2 Update `extract_symbol()` for Java Nested Classes

Java allows nested classes, enums, and interfaces inside class bodies. The existing `extract_nested_symbols()` already recurses into `body` nodes for `Class`/`Struct`/`Interface` kinds — this handles Java inner classes automatically.

**Edge case — anonymous classes**: `new Runnable() { void run() {} }` creates an `anonymous_class_body` node. These have **no name node**, so `resolve_name_node()` returns `None` and extraction skips them. This is the correct behavior — anonymous classes shouldn't appear in the symbol tree.

**Edge case — local classes**: Classes declared inside method bodies. These are extracted because `extract_nested_symbols` recurses into `body`, but they won't have a useful semantic path. The existing fallback (recursing for unrecognized symbols) handles this gracefully.

### 3.3 Add Java to `detect_access_level()`

(This function is created in Phase 0)

```rust
SupportedLanguage::Java => detect_java_access_level(node),
```

**`detect_java_access_level(node)`**:

Java access modifiers are `modifiers` children containing `public`, `protected`, `private` keywords:

```rust
fn detect_java_access_level(node: Node) -> AccessLevel {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "modifiers" {
            let mut mod_cursor = child.walk();
            for modifier in child.named_children(&mut mod_cursor) {
                match modifier.kind() {
                    "public" => return AccessLevel::Public,
                    "protected" => return AccessLevel::Protected,
                    "private" => return AccessLevel::Private,
                    _ => {}
                }
            }
        }
    }
    // No access modifier → package-private (Java default)
    AccessLevel::Package
}
```

### 3.4 `resolve_name_node()` — No Changes Needed

Java method/class declarations use `name` field in tree-sitter-java grammar, which `child_by_field_name("name")` already resolves. This has been **verified**: `constructor_declaration` in tree-sitter-java has a required `name` field of type `identifier`, so `child_by_field_name("name")` works correctly for constructors too. Generics (`<T>`) are separate `type_parameters` nodes that don't interfere with name resolution.

### 3.5 `extract_symbols_from_tree()` — Add Java impl-merge bypass

```diff
     if matches!(lang, SupportedLanguage::Rust) {
         merge_rust_impl_blocks(&mut symbols);
     }
```

No change needed — Java doesn't have impl blocks, so this is already a no-op.

## 4. Comment/String Node Classification

**File**: `crates/pathfinder-treesitter/src/treesitter_surgeon.rs`

### `is_comment_node()` — Already handles Java

`line_comment` and `block_comment` are already in the match. tree-sitter-java uses these same node kinds. **No change needed.**

### `is_string_node()` — Already handles Java

`string_literal` and `char_literal` are already in the match. For Java 15+ text blocks, tree-sitter-java uses `text_block` node kind:

```diff
+        | "text_block"                  // Java 15+ multi-line text blocks
```

## 5. Server Helper Update

**File**: `crates/pathfinder/src/server/helpers.rs`

Update `language_from_path()`:

```diff
+        Some("java") => "java",
```

## 5b. LSP Idle Timer Extension

**File**: `crates/pathfinder/src/server/tools/source_file.rs`

The `read_source_file` handler has a **second** hardcoded extension-to-language mapping at line ~196 used for `touch_language()` (LT-4 idle timer extension). Without adding Java here, the LSP will not extend its idle timer when reading `.java` files, causing premature jdtls shutdown during active use.

```diff
                     let lang_id = match ext {
                         "rs" => Some("rust"),
                         "go" => Some("go"),
                         "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "vue" => Some("typescript"),
                         "py" | "pyi" => Some("python"),
+                        "java" => Some("java"),
                         _ => None,
                     };
```

## 6. Java Version Compatibility Matrix

tree-sitter is syntax-only — it doesn't compile or type-check. tree-sitter-java 0.23.x supports the full Java syntax through Java 21+:

| Java Version | Feature | tree-sitter-java Support |
|-------------|---------|------------------------|
| 8 | Lambdas, method references, default methods | ✅ |
| 9 | Module declarations (`module-info.java`) | ✅ (parsed but no `module_kinds` extraction) |
| 10 | `var` type inference | ✅ (transparent — `var` is just an identifier) |
| 11-14 | Minor syntax additions | ✅ |
| 15 | Text blocks (`"""..."""`) | ✅ (`text_block` node) |
| 16 | Records | ✅ (`record_declaration` node) |
| 17 | Sealed classes, pattern matching instanceof | ✅ |
| 21 | Pattern matching in switch, record patterns | ✅ |
| 22-25 | Continued evolution | ✅ (grammar tracks JDK releases) |

## Acceptance Criteria

- [ ] AC-1.1: `SupportedLanguage::Java` variant exists with `detect()`, `grammar()`, `node_types()`
- [ ] AC-1.2: `tree-sitter-java = "0.23"` added to `Cargo.toml`
- [ ] AC-1.3: Java classes, interfaces, enums, records, methods, constructors, fields extracted correctly
- [ ] AC-1.4: `refine_class_kind` maps Java nodes to correct `SymbolKind` variants
- [ ] AC-1.5: `detect_java_access_level` maps all 4 Java visibility levels correctly
- [ ] AC-1.6: Nested/inner classes produce correct hierarchical symbol trees
- [ ] AC-1.7: Anonymous classes are silently skipped (no panic, no garbage symbols)
- [ ] AC-1.8: `text_block` added to `is_string_node()`
- [ ] AC-1.9: `language_from_path()` returns `"java"` for `.java` files
- [ ] AC-1.10: `cargo test --workspace` passes
- [ ] AC-1.11: Repo map correctly shows Java symbols with visibility filtering
- [ ] AC-1.12: `search_codebase` with `filter_mode=code_only` correctly excludes Java comments/strings

## Test Fixtures Required

Create `crates/pathfinder-treesitter/tests/fixtures/` Java test files:

### `BasicClass.java`
```java
package com.example;

public class BasicClass {
    private String name;
    protected int count;

    public BasicClass(String name) {
        this.name = name;
    }

    public String getName() { return name; }
    private void helper() {}
    void packageMethod() {}  // package-private
}
```

Expected symbols (with `constant_kinds: &[]`):
- `BasicClass` (Class, Public) with children:
  - `BasicClass` (Function, Public) — constructor
  - `getName` (Function, Public)
  - `helper` (Function, Private)
  - `packageMethod` (Function, Package)

> **Note**: `name` and `count` fields are NOT extracted because `constant_kinds` is empty for Java. This is intentional — see §2.1.

### `InterfaceWithDefaults.java` (Java 8+)
```java
public interface Sortable {
    void sort();
    default void printSorted() { sort(); }
}
```

### `RecordExample.java` (Java 16+)
```java
public record Point(int x, int y) {
    public double distance() {
        return Math.sqrt(x * x + y * y);
    }
}
```

Expected: `Point` (Struct, Public) with child `distance` (Function, Public)

### `SealedHierarchy.java` (Java 17+)
```java
public sealed class Shape permits Circle, Rectangle {
    public record Circle(double radius) implements Shape {}
    public record Rectangle(double w, double h) implements Shape {}
}
```

### `EnumWithMethods.java`
```java
public enum Status {
    ACTIVE, INACTIVE;

    public boolean isActive() { return this == ACTIVE; }
}
```

### `AnnotationType.java`
```java
public @interface MyAnnotation {
    String value();
    int priority() default 0;
}
```

### `InnerClasses.java`
```java
public class Outer {
    public class Inner { void innerMethod() {} }
    public static class StaticNested { void nestedMethod() {} }
    // Anonymous class — should be skipped
    Runnable r = new Runnable() { public void run() {} };
}
```

### `GenericClass.java`
```java
public class Container<T extends Comparable<T>> {
    private T value;
    public <R> R transform(java.util.function.Function<T, R> fn) {
        return fn.apply(value);
    }
}
```

### `LambdaExpressions.java` (Java 8+)
```java
public class Lambdas {
    public void example() {
        Runnable r = () -> {};
        java.util.function.Function<String, Integer> f = String::length;
    }
}
```

### `ModuleInfo.java` (Java 9+ — edge case)
```java
// module-info.java
module com.example.app {
    requires java.base;
    exports com.example.api;
}
```

Expected: No symbols extracted (module declarations are not in `module_kinds` for Java). The parser should not crash.

## Test Convention

> **IMPORTANT**: Existing tree-sitter tests use **inline source bytes** (`b"fn compute() {}"`) rather than fixture files. For consistency, Java tests should follow the same convention — use inline `b"public class Foo { ... }"` constants in test functions. The fixture files above serve as **reference specifications** for what the inline test content should contain, not as files to load at runtime.
