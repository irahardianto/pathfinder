# GAP-006: Warn When insert_into Targets a Rust Struct

## Group: C (Medium) — Language-Specific Correctness
## Depends on: Nothing

## Objective

When `insert_into` targets a Rust struct, it inserts code inside the struct's field list
(`{ ... }`), which is structurally valid but semantically wrong — methods belong in impl
blocks, not struct bodies. The Rust report showed this producing invalid Rust:

```rust
pub struct Calculator {
    pub last_result: i32,

    pub fn reset(&mut self) {  // ← INVALID: method in struct body
        self.last_result = 0;
    }
}
```

The tree-sitter surgeon's `resolve_body_end_range` correctly identifies structs as
valid container symbols (the match arm includes `SymbolKind::Struct`). For Rust,
this is technically wrong — you almost never want to insert into a struct body.

## Scope

| File | Function | Change |
|------|----------|--------|
| `crates/pathfinder/src/server/tools/edit/handlers.rs` | `resolve_insert_into` | Add Rust struct warning |
| `crates/pathfinder/src/server/tools/edit/handlers.rs` | tests | Add test for warning |

## Current Code

```rust
// In resolve_body_end_range (treesitter_surgeon.rs):
match symbol.kind {
    crate::surgeon::SymbolKind::Module
    | crate::surgeon::SymbolKind::Class
    | crate::surgeon::SymbolKind::Struct
    | crate::surgeon::SymbolKind::Interface
    | crate::surgeon::SymbolKind::Impl => {}
    other => {
        return Err(SurgeonError::InvalidTarget { ... });
    }
}
```

No language-specific distinction — Struct is treated the same as Class/Impl.

## Target Code

### Option A (Preferred): Add a warning in the response

In `resolve_insert_into` (handlers.rs), after resolving the body end range, check if
the target is a Rust struct and add a warning to the response text:

```rust
// In resolve_insert_into, after the surgeon call:
let is_rust_struct = semantic_path.file_path.ends_with(".rs")
    && /* check if the resolved symbol kind is Struct */;

let warning = if is_rust_struct {
    Some(
        "WARNING: Target is a Rust struct. Methods should be inserted into \
         an impl block, not the struct body. Consider targeting \
         'file.rs::impl MyStruct' instead."
            .to_owned(),
    )
} else {
    None
};
```

This requires passing the symbol kind through from `resolve_body_end_range`.
Alternatively, do a simpler check:

```rust
// Check if the file is Rust and the semantic path doesn't contain "impl"
let is_rust_struct_target = semantic_path.file_path.ends_with(".rs")
    && !semantic_path.to_string().contains("impl ");

if is_rust_struct_target {
    // Log a warning
    tracing::warn!(
        tool = "insert_into",
        "insert_into targeting a Rust struct — methods should go in an impl block"
    );
    // Include warning in response text
}
```

### Option B (Alternative): Reject at tree-sitter level

In `resolve_body_end_range`, for Rust files specifically, reject `SymbolKind::Struct`
with a helpful error:

```rust
// In resolve_body_end_range, after the kind check:
if symbol.kind == crate::surgeon::SymbolKind::Struct {
    // Check file extension
    if /* file is .rs */ {
        return Err(SurgeonError::InvalidTarget {
            path: semantic_path.to_string(),
            reason: "Rust structs contain fields, not methods. \
                     Use insert_into with 'impl StructName' instead."
                .to_owned(),
        });
    }
}
```

**Recommendation**: Use Option A (warning, not rejection). Agents may legitimately
want to insert struct fields. Rejecting would break that use case. A warning lets
the agent self-correct.

## Exclusions

- Do NOT change the tree-sitter layer's container symbol check — `Struct` is a valid
  container in general (TypeScript, Go, etc.).
- Do NOT auto-redirect to the impl block — that's too magical and may not always be
  what the agent wants.

## Verification

```bash
cargo test -p pathfinder --lib -- test_insert_into_rust_struct_warning
```

## Tests

```rust
#[tokio::test]
async fn test_insert_into_rust_struct_warning() {
    // Create a Rust file with a struct and an impl block
    // Call insert_into targeting the struct
    // Verify: response includes warning about targeting impl block instead
}
```
