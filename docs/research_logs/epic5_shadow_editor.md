# Epic 5: The Shadow Editor — Research Log

## Date: 2026-03-06

## Context
Epic 5 implements the AST-aware edit pipeline — the core value proposition of Pathfinder.
Starting with `replace_body` as the foundational tool because all other edit tools
(`replace_full`, `insert_before`, `insert_after`, `delete_symbol`, `validate_only`)
reuse 80%+ of the same pipeline.

## Key Findings

### Existing Infrastructure (Reusable)

1. **OCC / Version Hashing** — `VersionHash::compute()` in `pathfinder-common/src/types.rs`
   already used by `create_file`, `delete_file`, `read_file`, `write_file`.
   Same SHA-256 pattern applies to edit tools.

2. **Sandbox Checks** — `self.sandbox.check()` already wired in all existing tools.

3. **Error Taxonomy** — `PathfinderError` in `pathfinder-common/src/error.rs` has:
   - `FileNotFound`, `VersionMismatch`, `SymbolNotFound`, `AccessDenied`
   - `InvalidTarget` — needed for `replace_body` on non-block targets
   - All map to `ErrorData` via `pathfinder_to_error_data()`

4. **Semantic Path Resolution** — `SemanticPath::parse()` + `resolve_symbol_chain()`
   already works. Returns `ExtractedSymbol` with `byte_range`.

5. **AST Cache** — `TreeSitterSurgeon.cached_parse()` returns
   `(SupportedLanguage, Vec<u8>, VersionHash, Vec<ExtractedSymbol>)`.

### What's Missing (Must Build)

1. **Body Range Resolution** — `ExtractedSymbol.byte_range` covers the *entire*
   declaration (signature + body). `replace_body` needs *only* the body content
   inside braces. Must walk AST child nodes to find the body/block node.

2. **Input Normalization** — PRD step 0:
   - Markdown fence stripping (`\`\`\`lang ... \`\`\``)
   - Brace-leniency (`{ content }` → `content` for block-bodied)
   - CRLF → LF normalization

3. **Indentation Pre-pass** — PRD step 5:
   - Dedent: strip common leading whitespace to column 0
   - Re-indent: pad every line with target AST node's column offset

4. **TOCTOU Late-check** — PRD step 10: re-read + re-hash before write.
   Already done in `write_file_impl` — pattern is extractable.

5. **Edit Response Type** — New `EditResponse` struct matching PRD §3.4
   (success, new_version_hash, formatted, validation).

### Body Node Detection Strategy

Different languages use different AST node kinds for "body":

| Language   | Function Body Kind | Class Body Kind    |
| ---------- | ------------------ | ------------------ |
| Go         | `block`            | N/A (no classes)   |
| TypeScript | `statement_block`  | `class_body`       |
| JavaScript | `statement_block`  | `class_body`       |
| Python     | `block`            | `block`            |
| Rust       | `block`            | `declaration_list` |

**Approach**: Walk child nodes of the resolved symbol looking for body field
(`child_by_field_name("body")`). Most tree-sitter grammars use "body" as the
field name. Fall back to searching named children for block-type nodes.

### Indentation Algorithm

```
Algorithm: dedent_then_reindent(new_code, target_column)

1. Find min_indent = minimum leading whitespace across all non-empty lines
2. Strip min_indent from every line (dedent to column 0)  
3. Pad every line with ' '.repeat(target_column) spaces
4. Return indented code
```

### LSP Validation (Deferred)

Stories 5.9-5.11 (Pull Diagnostics, multiset diffing, range formatting)
require Epic 4 (LSP). For now, all edit responses include:
- `validation.status: "skipped"`
- `validation_skipped: true`
- `validation_skipped_reason: "no_lsp"`

### Sources
- PRD v4.6 §3.4 (Edit Execution Flow, steps 0-13)
- Existing `symbols.rs` — `resolve_symbol_chain()`, `ExtractedSymbol`
- Existing `treesitter_surgeon.rs` — `cached_parse()`, `read_symbol_scope()`
- Existing `file_ops.rs` — OCC pattern, TOCTOU pattern
- Relying on training data for tree-sitter body node field names
