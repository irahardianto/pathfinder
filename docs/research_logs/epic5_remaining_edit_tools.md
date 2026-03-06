# Epic 5: Remaining Edit Tools — Research Log

## Date: 2026-03-07

## Context
Implementing `replace_full` (5.4), `insert_before`/`insert_after` (5.5), and `delete_symbol` (5.6)
as continuations of the shared edit pipeline established by `replace_body` (5.3).

## Key Findings

### Existing Infrastructure (Reusable from replace_body)
1. **Edit pipeline** — `edit.rs` implements the full 10-step flow:
   parse → sandbox → resolve → OCC → normalize → indent → splice → TOCTOU → write → hash
2. **Input normalization** — `normalize_for_body_replace()` and `normalize_for_full_replace()` already exist
3. **Indentation** — `dedent_then_reindent()` in `pathfinder-common/src/indent.rs`
4. **Types** — `EditResponse`, `EditValidation`, `ReplaceFullParams`, `InsertBeforeParams`,
   `InsertAfterParams`, `DeleteSymbolParams` already defined in `types.rs`
5. **Stub registrations** — All 4 tools registered in `server.rs` with correct descriptions

### New Surgeon Methods Needed

#### `resolve_full_range()` — for replace_full
Returns the **full declaration range** including decorators, doc comments, and export keywords.

| Language   | What to include above declaration                     |
| ---------- | ----------------------------------------------------- |
| Go         | Doc comment block (`//` lines directly above)         |
| TypeScript | `@decorator()` lines + `/** */` doc comments + export |
| Python     | `@decorator` lines                                    |
| Rust       | `#[attr]` lines + `///` doc comments + `pub`          |

Strategy: From the resolved AST node, walk backwards byte-by-byte to capture:
1. Comments/doc comments directly above (no blank line gap)
2. Decorators/attributes
3. `export`/`pub` keywords (typically part of the AST node itself)

Return type: `FullRange { start_byte, end_byte, indent_column }`

#### `resolve_symbol_range()` — for insert_before / insert_after / delete_symbol
Returns the **full symbol range** (same as full range) plus insertion point info.

Return type: `SymbolRange { start_byte, end_byte, indent_column }`
Same data as `FullRange` but semantically distinct — used for positioning.

### Bare File Path Handling (insert_before / insert_after)
When `semantic_path.is_bare_file()`:
- `insert_before`: insertion point = byte 0 (BOF)
- `insert_after`: insertion point = file end (EOF)
No need for Surgeon — just read file, OCC check, splice at boundary.

### Whitespace Rules (PRD §3.4)
- **insert_before/after**: One blank line separator between inserted code and existing code
- **delete_symbol**: Collapse consecutive blank lines to max one after deletion
- **replace_full**: Direct replacement, no extra whitespace handling needed

### delete_symbol Cleanup Algorithm
```
1. Resolve full range (decorators + declaration)
2. Expand range upward to absorb blank lines between this and previous symbol
3. Remove the byte range
4. Collapse consecutive blank lines (>1) to exactly one
```

### Sources
- PRD v4.6 §3.4 (replace_full, insert_before, insert_after, delete_symbol specs)
- Existing `edit.rs` — `replace_body_impl()` pipeline
- Existing `normalize.rs` — `normalize_for_full_replace()`
- Existing `symbols.rs` — `resolve_symbol_chain()`, `ExtractedSymbol`
- Relying on training data for tree-sitter decorator/attribute node traversal
