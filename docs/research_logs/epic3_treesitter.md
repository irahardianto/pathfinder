# Research Log: Epic 3 — The Surgeon (Tree-sitter)

## Technologies Researched

### 1. tree-sitter (Rust crate) — v0.24
- **Source:** https://docs.rs/tree-sitter/0.24 + https://crates.io/crates/tree-sitter
- **Key patterns:**
  - `Parser` struct — create, set language, parse source → `Tree`
  - `Tree` → `root_node()` → `TreeCursor` for traversal
  - `Query::new(language, scm_source)` → compile `.scm` patterns
  - `QueryCursor::matches(query, node, source)` → iterate matches
  - `Node` — `kind()`, `start_byte()`, `end_byte()`, `start_position()`, `end_position()`, `child_by_field_name()`, `named_children()`
  - v0.24 has fully safe public API (internal unsafe for C FFI is encapsulated)
  - Compatible with workspace `unsafe_code = "deny"` lint (lint only applies to our crate code, not deps)

### 2. Language Grammar Crates (Tier 1)
- `tree-sitter-go = "0.23"` — exports `LANGUAGE` constant, `language()` fn available
- `tree-sitter-typescript = "0.23"` — exports `LANGUAGE_TYPESCRIPT` and `LANGUAGE_TSX` (two grammars)
- `tree-sitter-python = "0.23"` — exports `LANGUAGE` constant
- `tree-sitter-javascript = "0.23"` — exports `LANGUAGE` constant  
- `tree-sitter-rust = "0.24"` — exports `LANGUAGE` constant (self-hosting support)
- All grammars use `Language` type from tree-sitter 0.24: `tree_sitter_go::LANGUAGE.into()`

### 3. AST Node Type Names (Per Language)
- **Go:** `function_declaration`, `method_declaration`, `type_declaration`, `const_declaration`, `var_declaration`
- **TypeScript/JavaScript:** `function_declaration`, `class_declaration`, `method_definition`, `lexical_declaration`, `variable_declaration`, `interface_declaration`, `type_alias_declaration`
- **Python:** `function_definition`, `class_definition`, `decorated_definition`
- **Rust:** `function_item`, `impl_item`, `struct_item`, `enum_item`, `const_item`, `trait_item`, `type_item`
- Symbol names extracted from `name` or `identifier` child fields

### 4. Levenshtein Distance (`strsim`)
- **Source:** https://crates.io/crates/strsim
- Lightweight string similarity library, includes `levenshtein()` function
- Used for `did_you_mean` suggestions in `SYMBOL_NOT_FOUND` errors (PRD §1.3)

### 5. AST Caching Strategy
- **Source:** PRD §4.4 + Epic 3 Story 3.6
- Parse-on-demand — parse only when a tool call requests it
- Hash-compare invalidation — compare stored content hash vs file on disk
- `Tree` and source bytes stored together in cache entry
- Eviction: LRU by last-access time when cache exceeds max entries
- Synchronous update after edits (no file watcher race conditions)

## Key Gotchas
- TypeScript crate exports TWO grammars — `LANGUAGE_TYPESCRIPT` and `LANGUAGE_TSX` — must detect `.tsx` vs `.ts`
- Tree-sitter node `kind()` names vary per language even for similar constructs (e.g., `function_declaration` in Go vs `function_definition` in Python)
- `Node::child_by_field_name("name")` is how you get the identifier name from a declaration node
- tree-sitter `Tree` is `!Send` in older versions — in 0.24 it's `Send + Sync` (safe to cache across threads)
- `Parser` is `!Send` — must create new parser instances per parse call (or per thread)
- `.scm` query syntax: `(function_declaration name: (identifier) @name)` captures the function name node

## Architecture Decision
Using AST `TreeCursor` traversal for symbol extraction rather than `.scm` queries for v1 because:
- Direct cursor traversal is simpler and doesn't require maintaining `.scm` files per language
- Per-language node type names are well-known and stable
- `.scm` queries can be added later for advanced filtering (Epic 2's `filter_mode` AST integration)
- Keeps the initial implementation compact and testable
