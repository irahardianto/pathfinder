# Research Log: Semantic Path Overloads

## Context
We are continuing the development of `pathfinder-prd-v4.6.md`. After reviewing the PRD and the codebase, Epic 7 and others are largely complete. However, Epic 3 (The Surgeon) and Section 1.3 defined the syntax for semantic path overloads (`symbol#2`):

```ebnf
semantic_path   = file_path ["::" symbol_chain]
file_path       = relative_path
symbol_chain    = symbol ("." symbol)*
symbol          = identifier [overload_suffix]
overload_suffix = "#" digit+
```

## Findings
The `SymbolChain` parser in `crates/pathfinder-common/src/types.rs` already correctly parses `#2` into `overload_index: Some(2)`.

However, `crates/pathfinder-treesitter/src/symbols.rs` explicitly skips overload handling:
1. Extraction does not add `#2` to the generated `semantic_path`.
2. `resolve_symbol_chain` has a `TODO: handle overloads properly.` and just takes the first match regardless of the `overload_index`.

## Plan
We will update `pathfinder-treesitter/src/symbols.rs`:
- Append `#count` to `semantic_path` during extraction.
- Select the `N - 1` element matched by name during resolution when `overload_index` `N` is provided. 
