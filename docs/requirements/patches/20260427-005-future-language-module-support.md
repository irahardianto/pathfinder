# PATCH-005: Future Language Module Support Roadmap

**Status:** Deferred (post-PATCH-002)  
**Priority:** P3 — Low (Rust is the primary pain point; other languages are secondary)  
**Estimated Effort:** 2–4 hours per language when scheduled  
**Prerequisite:** PATCH-002 (establishes `module_kinds` field and `SymbolKind::Module`)

---

## Overview

PATCH-002 establishes the infrastructure for module symbol indexing in Rust. The `module_kinds`
field in `LanguageNodeTypes` and the `SymbolKind::Module` variant are designed to be extended
to other languages without architectural changes.

This document provides a forward-looking analysis of which languages are affected, what their
equivalent constructs are, and how to implement support when the time comes.

---

## Language Analysis

### TypeScript / JavaScript / Vue (TSX)

**Affected construct:** TypeScript `namespace` declarations

```typescript
// TypeScript only — JavaScript has no standard namespace syntax
namespace Auth {
    export function login() {}
    export function logout() {}
}
```

**Tree-sitter node type:** `internal_module` (TypeScript grammar)

**Impact assessment:**
- **Frequency:** Low to moderate. TypeScript namespaces are a legacy pattern; modern TS uses
  ES modules. Most production codebases encountered in Pathfinder's workspace won't use them.
- **Symptom:** `auth.ts::Auth::login` would return `SYMBOL_NOT_FOUND` today.
- **Priority:** Low. Fix when a user reports it or when namespace usage appears in a target workspace.

**Implementation:**
- Add `"internal_module"` to `module_kinds` in the TypeScript/TSX/JavaScript `LanguageNodeTypes`
- The `module_kinds` field name in Tree-sitter: `name` (same as Rust)
- The body field name in Tree-sitter: `body` (same as Rust)
- No changes needed beyond `language.rs`

**Note for Vue:** Vue SFCs use the TypeScript grammar for the `<script>` block, so TypeScript
namespace support automatically applies to Vue files.

---

### Go

**Affected construct:** None. Go does not have inline module blocks. Code is organized at the
package level (one package per directory), which is already captured by the file-level
organization in Pathfinder's semantic paths.

**Status:** Not applicable. No changes needed.

---

### Python

**Affected construct:** Python has no inline module syntax, but has related patterns:

1. **Nested classes** — Python classes can contain methods AND inner classes:
   ```python
   class TestSuite:
       class TestCase:
           def test_something(self): ...
   ```
   Tree-sitter node: `class_definition` inside `class_definition` body.
   **Current behavior:** Nested classes are already extracted as children of the outer class
   via `extract_nested_symbols`. This works because `class_definition` is in `class_kinds`.
   **Status:** Already handled. No changes needed.

2. **`__init__.py` package modules** — These are file-level, not inline.
   **Status:** Not applicable.

**Verdict:** Python has no meaningful inline module scope gap. No changes needed.

---

### Rust (sub-items, already in PATCH-002)

The Rust `mod_item` is the primary focus of PATCH-002. Additional Rust scoping patterns
that are NOT covered by PATCH-002:

1. **`use` declarations inside modules** — These are not symbols; they're import statements.
   Not extractable, not needed.

2. **`pub(crate) mod`** — PATCH-002's `extract_module_block` detects the `pub` keyword on
   the module node. Visibility detection for `pub mod` (public modules) should make them
   appear in `visibility: "public"` repo maps.

   **Enhancement (add to PATCH-002 or as a follow-up):**
   ```rust
   // In extract_module_block:
   let is_pub = child
       .child_by_field_name("visibility")
       .map(|v| v.kind() == "visibility_modifier")
       .unwrap_or(false);
   // Store in ExtractedSymbol.is_public or use a dedicated SymbolVisibility field
   ```

3. **Inline module files** (`mod foo;` with content in `foo.rs`) — These are bare declarations;
   the content lives in a separate file. Tree-sitter node: `mod_item` without a `body` field.
   PATCH-002's `extract_module_block` handles this correctly: no `body` field → falls back to
   `extract_symbols_recursive` on the node itself (which extracts nothing, since the node has
   no children). The content of `foo.rs` is indexed independently as a separate file.
   **Status:** Correctly handled by PATCH-002's fallback logic.

---

## Implementation Checklist (when scheduling each language)

### TypeScript namespace (`internal_module`)

- [ ] Add `"internal_module"` to `module_kinds` in TypeScript `LanguageNodeTypes`
- [ ] Verify Tree-sitter field names: confirm `name` and `body` fields exist on `internal_module`
      (run `tree-sitter parse` on a sample namespace file and inspect the AST)
- [ ] Add test: `test_extract_typescript_namespace_with_members`
- [ ] Add test: `test_resolve_typescript_namespace_member_via_chain`
- [ ] Update tool description in `get_repo_map`: add TypeScript namespaces to the module note

### Rust `pub mod` visibility

- [ ] Detect `visibility_modifier` child on `mod_item` node
- [ ] Expose public modules in `visibility: "public"` repo maps
- [ ] Add test: `test_pub_mod_appears_in_public_visibility`
- [ ] Add test: `test_private_mod_hidden_in_public_visibility`

---

## Acceptance Criteria for TypeScript Namespace (when implemented)

- [ ] `file.ts::Auth::login` resolves for TypeScript namespace members
- [ ] Namespace members appear in `read_source_file(detail_level="symbols")` as nested children
- [ ] Namespace members only appear in `get_repo_map(visibility="all")` if `namespace` (not `export namespace`)
- [ ] `export namespace` members appear in `visibility: "public"`
- [ ] All new tests pass, no regressions
