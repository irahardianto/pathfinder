# PATCH-003: `insert_into` — Module-Scoped Insertion Tool

**Status:** Planned  
**Priority:** P1 — High (required for agents to add test functions after PATCH-002)  
**Estimated Effort:** 4–6 hours  
**Prerequisite:** PATCH-002 (module symbols must be addressable first)  
**PR Strategy:** Standalone PR — new MCP tool, no changes to existing edit semantics

---

## Problem Statement

After PATCH-002 makes Rust module blocks addressable (e.g., `cache.rs::tests`), agents still
cannot insert code **inside** a module's body. The closest existing options both produce
syntactically broken output:

| Tool call | What happens | Result |
|---|---|---|
| `insert_after("cache.rs::tests", code)` | Inserts after `end_byte` of `tests` block | Code lands *outside* the closing `}` → syntax error |
| `insert_after("cache.rs", code)` | Appends at EOF | Code lands after `mod tests {}` → syntax error for test functions |
| `insert_after("cache.rs::test_last_fn", code)` | Works but requires knowing the last test | Fragile, O(1) knowledge requirement |

### Why not change `insert_after` semantics on Module symbols?

**Option A — Change `insert_after(module)` to insert inside:** Inconsistent. Every other symbol
type (`Function`, `Struct`, `Impl`) uses `end_byte` to position after the closing `}`. Changing
Module to be different means agents must remember one symbol type behaves differently. This is
a source of bugs.

**Option B — New `insert_into` tool (this patch):** Semantically explicit. The name states
intent unambiguously: "insert code *into* this scope." No existing semantics change. AI agents
benefit from tools where the name is a precise description of the operation.

**Decision: Option B.**

The `insert_after(module)` behavior remains unchanged (inserts after the module's full declaration).
A new `insert_into` tool provides the "append to scope body" pattern.

---

## Tool Specification

### `insert_into`

**Purpose:** Insert new code at the **end of a scope's body** — just before the closing delimiter.
The target must be a container symbol: `Module`, `Class`, `Struct`, `Impl`, or `Interface`.

**Signature (MCP tool):**

```
insert_into(
    semantic_path: string,   // e.g., "src/lib.rs::tests"
    base_version: string,    // OCC hash (full or 7-char prefix)
    new_code: string,        // Code to insert
    ignore_validation_failures: bool = false
) → EditResponse
```

**Semantic model:**

```
// Before
mod tests {
    fn test_a() {}
}   ← end_byte

// insert_into("file.rs::tests", "fn test_new() { assert!(true); }")

// After
mod tests {
    fn test_a() {}

    fn test_new() { assert!(true); }
}   ← closing brace (unchanged position, shifted by new_code length)
```

**Target validation:**

- Must be a symbol path (bare file paths are rejected with `INVALID_TARGET`)
- Must resolve to a symbol with `SymbolKind` in `{Module, Class, Struct, Interface, Impl}`
- Functions and Methods are rejected — they have no "body" in the module-appending sense
  (use `replace_body` instead)

**Insertion point:** The byte offset just **before** the final `}` (or equivalent closing delimiter),
determined by finding the `body` node's end via the Surgeon.

**Indentation:** The inserted code is re-indented to match the module body's indent level
(one level deeper than the module declaration), using the same `dedent_then_reindent` pipeline
as all other edit tools.

**Separator:** Automatically adds a blank line before the inserted code if the preceding content
does not already end with a blank line.

---

## Proposed Changes

### 1. `crates/pathfinder-treesitter/src/surgeon.rs`

Add `resolve_body_end_range` to the `Surgeon` trait:

```rust
/// Returns the byte offset just before the closing delimiter of a symbol's body.
///
/// Used by `insert_into` to append code at the end of a scope without
/// needing to know which symbol is last inside it.
///
/// Returns `(body_end_byte, indent_column, source, version_hash)`.
/// `indent_column` is the indentation of the scope's body content
/// (one level deeper than the symbol declaration).
async fn resolve_body_end_range(
    &self,
    workspace: &Path,
    semantic_path: &SemanticPath,
) -> Result<(BodyEndRange, Arc<[u8]>, VersionHash), SurgeonError>;

pub struct BodyEndRange {
    /// Byte offset just before the closing `}` (or `end`, etc.) of the body.
    pub insert_byte: usize,
    /// Indentation column for newly inserted content.
    pub body_indent_column: usize,
}
```

### 2. `crates/pathfinder-treesitter/src/treesitter_surgeon.rs`

Implement `resolve_body_end_range`:

```rust
async fn resolve_body_end_range(
    &self,
    workspace: &Path,
    semantic_path: &SemanticPath,
) -> Result<(BodyEndRange, Arc<[u8]>, VersionHash), SurgeonError> {
    let (tree, source, hash) = self.cached_parse(workspace, &semantic_path.file_path).await?;
    let symbols = extract_symbols_from_tree(&tree, &source, lang)?;

    let symbol = resolve_symbol_chain(&symbols, chain)
        .ok_or(SurgeonError::SymbolNotFound { ... })?;

    // Only container symbols are valid targets
    match symbol.kind {
        SymbolKind::Module | SymbolKind::Class | SymbolKind::Struct
        | SymbolKind::Interface | SymbolKind::Impl => {}
        other => return Err(SurgeonError::InvalidTarget {
            reason: format!("insert_into requires a container symbol (Module, Class, Struct, \
                Impl, Interface), but got {:?}. Use replace_body for functions.", other),
        }),
    }

    // Find the body node from the AST
    let node = self.find_node_for_symbol(symbol)?;
    let body = node.child_by_field_name("body")
        .ok_or(SurgeonError::SymbolNotFound { ... })?;

    // The insert point is just before the closing `}` of the body
    // body.end_byte() points to the byte AFTER `}`, so body.end_byte() - 1 gives `}`
    // We want to insert BEFORE `}`, so we find the last non-whitespace byte before `}`
    let raw_end = body.end_byte().saturating_sub(1); // points at `}`
    let insert_byte = find_last_newline_before(&source, raw_end);

    // Body indent: detect from existing content, or use symbol indent + 4
    let body_indent_column = detect_body_indent(&source, body.start_byte(), body.end_byte())
        .unwrap_or(symbol_indent_column + 4);

    Ok((BodyEndRange { insert_byte, body_indent_column }, source, hash))
}
```

### 3. `crates/pathfinder/src/server/tools/edit/handlers.rs`

Add `insert_into_impl` method on `PathfinderServer`:

```rust
pub(crate) async fn insert_into_impl(
    &self,
    params: InsertIntoParams,
) -> Result<Json<EditResponse>, ErrorData> {
    let start = std::time::Instant::now();
    tracing::info!(
        tool = "insert_into",
        semantic_path = %params.semantic_path,
        "insert_into: start"
    );

    let semantic_path = parse_semantic_path(&params.semantic_path)?;
    require_symbol_target(&semantic_path, &params.semantic_path)?;

    check_sandbox_access(
        &self.sandbox, &semantic_path.file_path,
        "insert_into", &params.semantic_path,
    )?;

    let (body_end, source, current_hash) = self
        .surgeon
        .resolve_body_end_range(self.workspace_root.path(), &semantic_path)
        .await
        .map_err(treesitter_error_to_error_data)?;

    check_occ(&params.base_version, &current_hash, semantic_path.file_path.clone())?;

    let new_code = &params.new_code;
    let normalized = normalize_for_full_replace(new_code);
    let indented = dedent_then_reindent(&normalized, body_end.body_indent_column);

    let before = &source[..body_end.insert_byte];
    let after  = &source[body_end.insert_byte..];

    // Add blank line separator before inserted code if needed
    let sep = if before.ends_with(b"\n\n") { "" } else { "\n" };
    let trailing = if indented.ends_with('\n') { "" } else { "\n" };

    let mut new_bytes = Vec::with_capacity(
        before.len() + sep.len() + indented.len() + trailing.len() + after.len(),
    );
    new_bytes.extend_from_slice(before);
    new_bytes.extend_from_slice(sep.as_bytes());
    new_bytes.extend_from_slice(indented.as_bytes());
    new_bytes.extend_from_slice(trailing.as_bytes());
    new_bytes.extend_from_slice(after);

    let resolve_ms = start.elapsed().as_millis();
    self.finalize_edit(FinalizeEditParams {
        tool_name: "insert_into",
        semantic_path: &semantic_path,
        raw_semantic_path_str: &params.semantic_path,
        source: &source,
        original_hash: &current_hash,
        new_content: new_bytes,
        ignore_validation_failures: params.ignore_validation_failures,
        start_time: start,
        resolve_ms,
    })
    .await
}
```

### 4. `crates/pathfinder/src/server/types.rs`

Add `InsertIntoParams`:

```rust
#[derive(Debug, Deserialize)]
pub struct InsertIntoParams {
    pub semantic_path: String,
    pub base_version: String,
    pub new_code: String,
    #[serde(default)]
    pub ignore_validation_failures: bool,
}
```

### 5. `crates/pathfinder/src/server.rs`

Register the new tool:

```rust
#[tool(
    name = "insert_into",
    description = "Insert new code at the END of a container symbol's body \
        (Module, Class, Struct, Impl, Interface). This is the correct tool \
        for adding new functions to a test module, new methods to a struct, \
        or new items to any scope. IMPORTANT: semantic_path must target a \
        container symbol (e.g. 'src/lib.rs::tests'), NOT a bare file path. \
        For inserting before/after a specific sibling symbol, use insert_before \
        or insert_after instead."
)]
async fn insert_into(
    &self,
    Parameters(params): Parameters<InsertIntoParams>,
) -> Result<Json<EditResponse>, ErrorData> {
    self.insert_into_impl(params).await
}
```

### 6. `crates/pathfinder-treesitter/src/mock.rs`

Add `resolve_body_end_range` to `MockSurgeon`.

---

## Implementation Steps

1. **Add `BodyEndRange` struct** to `surgeon.rs`
2. **Add `resolve_body_end_range` to `Surgeon` trait** in `surgeon.rs`
3. **Implement in `TreeSitterSurgeon`** — body node lookup + end byte calculation
4. **Implement in `MockSurgeon`** — add `body_end_range_results` queue
5. **Add `InsertIntoParams` type** to `types.rs`
6. **Add `insert_into_impl`** to `handlers.rs`
7. **Register `insert_into` tool** in `server.rs`
8. **Add tests** (see below)
9. **Verify:** `cargo test --workspace`, `cargo clippy`, `cargo fmt --check`

---

## Test Plan

### Surgeon-level tests (`treesitter_surgeon.rs`)

```rust
/// PATCH-003-T1: resolve_body_end_range returns byte before closing `}` of mod
#[tokio::test]
async fn test_resolve_body_end_range_mod_block() {
    // Verify insert_byte lands inside the module, not after it
}

/// PATCH-003-T2: resolve_body_end_range rejects Function symbols
#[tokio::test]
async fn test_resolve_body_end_range_rejects_function() {
    // Should return Err(InvalidTarget)
}
```

### Server-level tests (`server.rs` or `edit/handlers.rs`)

```rust
/// PATCH-003-T3: insert_into appends function inside test module
#[tokio::test]
async fn test_insert_into_mod_tests_appends_inside() {
    // Write file with mod tests { fn test_a() {} }
    // Call insert_into("file.rs::tests", "fn test_b() {}")
    // Verify result compiles (or at minimum: `}` is the last char)
    // Verify test_a still present, test_b is before the closing `}`
}

/// PATCH-003-T4: insert_into on bare file returns INVALID_TARGET
#[tokio::test]
async fn test_insert_into_bare_file_is_rejected() {}

/// PATCH-003-T5: insert_into on Function symbol returns INVALID_TARGET
#[tokio::test]
async fn test_insert_into_function_symbol_is_rejected() {}

/// PATCH-003-T6: OCC mismatch returns VERSION_MISMATCH
#[tokio::test]
async fn test_insert_into_occ_mismatch() {}
```

---

## Acceptance Criteria

- [ ] `insert_into("cache.rs::tests", code)` inserts code **inside** the `mod tests {}` block
- [ ] Inserted code is correctly indented (one level deeper than `mod tests`)
- [ ] Existing symbols inside the module are preserved
- [ ] Bare file path → `INVALID_TARGET` error
- [ ] Function/Method target → `INVALID_TARGET` error with helpful message
- [ ] OCC guard works (VERSION_MISMATCH on stale hash)
- [ ] All new tests pass
- [ ] `cargo test --workspace` passes
- [ ] Tool description clearly differentiates `insert_into` vs `insert_after`
