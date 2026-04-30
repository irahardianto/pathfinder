# PATCH-001: Add name_column to ExtractedSymbol and SymbolScope

## Group: A (Critical) — Column-1 Root Cause Fix

## Objective

Add a `name_column` field to `ExtractedSymbol` and `SymbolScope` so that navigation tools
can send the correct cursor position to LSP. Currently all LSP navigation calls hardcode
column 1 (the `pub` keyword), which causes rust-analyzer to return null/empty. This is the
root cause of `get_definition`, `analyze_impact`, and `read_with_deep_context` all failing.

## Severity: CRITICAL — 3 tools non-functional without this

## Background

The Column-1 Bug cascade:

1. `SymbolScope` has `start_line` but no column info
2. Navigation tools send column=1 to LSP
3. Column 1 lands on `pub` keyword, not the symbol name
4. rust-analyzer returns null for `goto_definition` and empty for `call_hierarchy_prepare`
5. `get_definition` falls through to grep fallback (which has its own bug — see PATCH-011)
6. `analyze_impact` returns degraded 0/0
7. `read_with_deep_context` returns 0 dependencies with `degraded: false` (see PATCH-003)

## Scope

| # | File | Action |
|---|------|--------|
| 1 | `crates/pathfinder-treesitter/src/symbols.rs` | Add `name_column` to `ExtractedSymbol` |
| 2 | `crates/pathfinder-treesitter/src/symbols.rs` | Populate `name_column` from tree-sitter AST |
| 3 | `crates/pathfinder-common/src/types.rs` | Add `name_column` to `SymbolScope` |
| 4 | `crates/pathfinder-treesitter/src/treesitter_surgeon.rs` | Propagate `name_column` in `read_symbol_scope_impl` |

## Step 1: Add name_column to ExtractedSymbol

**File:** `crates/pathfinder-treesitter/src/symbols.rs`

The `extract_symbol` method (around line 120) creates `ExtractedSymbol` from a tree-sitter
node. The `resolve_name_node` method (line 108) already finds the `name` child node. We need
to extract the column from this name node.

**Find in `ExtractedSymbol` struct (surgeon.rs line 7):**
```rust
pub struct ExtractedSymbol {
    /// The name of the symbol (e.g., "login").
    pub name: String,
    /// The semantic path to this symbol (e.g., "AuthService.login").
    pub semantic_path: String,
    /// The kind of symbol it is.
    pub kind: SymbolKind,
    /// The byte range in the source file spanning the entire symbol.
    pub byte_range: std::ops::Range<usize>,
    /// The zero-indexed starting line.
    pub start_line: usize,
    /// The zero-indexed ending line.
    pub end_line: usize,
```

**Replace with:**
```rust
pub struct ExtractedSymbol {
    /// The name of the symbol (e.g., "login").
    pub name: String,
    /// The semantic path to this symbol (e.g., "AuthService.login").
    pub semantic_path: String,
    /// The kind of symbol it is.
    pub kind: SymbolKind,
    /// The byte range in the source file spanning the entire symbol.
    pub byte_range: std::ops::Range<usize>,
    /// The zero-indexed starting line.
    pub start_line: usize,
    /// The zero-indexed ending line.
    pub end_line: usize,
    /// The zero-indexed column where the symbol's **name identifier** begins.
    ///
    /// For `pub fn dedent(code: &str)`, this is the column of the `d` in `dedent`,
    /// NOT the `p` in `pub`. Used by LSP navigation tools to position the cursor
    /// on the symbol name rather than the declaration start.
    ///
    /// Falls back to 0 when the name node cannot be resolved (e.g., anonymous symbols).
    pub name_column: usize,
```

**File:** `crates/pathfinder-treesitter/src/surgeon.rs`

## Step 2: Populate name_column during extraction

**File:** `crates/pathfinder-treesitter/src/symbols.rs`

In the `SymbolExtractionContext::extract_symbol` method (around line 120), the name node
is already available via `resolve_name_node`. Add name_column extraction:

**Find:**
```rust
    fn extract_symbol(&mut self, child: Node<'a>, name: String, sk: SymbolKind) {
        let (unique_name, suffix) = make_unique_name(&mut self.name_counts, name);
        let path = self.build_path(&unique_name, &suffix);

        let mut symbol = ExtractedSymbol {
            name: unique_name,
            semantic_path: path.clone(),
            kind: sk,
            byte_range: child.byte_range(),
            start_line: child.start_position().row,
            end_line: child.end_position().row,
            is_public: true,
            children: Vec::new(),
        };
```

**Replace with:**
```rust
    fn extract_symbol(&mut self, child: Node<'a>, name: String, sk: SymbolKind) {
        let (unique_name, suffix) = make_unique_name(&mut self.name_counts, name);
        let path = self.build_path(&unique_name, &suffix);

        // Resolve the name node's column for LSP navigation positioning.
        // Falls back to 0 (start of line) when the name node cannot be found
        // (e.g., for anonymous constructs or grammars without a "name" field).
        let name_column = self
            .resolve_name_node(child)
            .map(|n| n.start_position().column)
            .unwrap_or(0);

        let mut symbol = ExtractedSymbol {
            name: unique_name,
            semantic_path: path.clone(),
            kind: sk,
            byte_range: child.byte_range(),
            start_line: child.start_position().row,
            end_line: child.end_position().row,
            name_column,
            is_public: true,
            children: Vec::new(),
        };
```

Also update ALL other `ExtractedSymbol` construction sites in `symbols.rs` (there are several
for specialized symbol types like Vue components, impl blocks, etc.) to include `name_column`.
Each should use the appropriate name node's column or 0 as fallback.

Search for all `ExtractedSymbol {` in the crate and add `name_column: 0` (or computed value)
to each.

## Step 3: Add name_column to SymbolScope

**File:** `crates/pathfinder-common/src/types.rs`

**Find (line ~245):**
```rust
pub struct SymbolScope {
    /// The source code snippet of the symbol block.
    pub content: String,
    /// The zero-indexed starting line.
    pub start_line: usize,
    /// The zero-indexed ending line.
    pub end_line: usize,
    /// The version hash of the *entire file* at the time of extraction.
    pub version_hash: VersionHash,
    /// The language of the file.
    pub language: String,
}
```

**Replace with:**
```rust
pub struct SymbolScope {
    /// The source code snippet of the symbol block.
    pub content: String,
    /// The zero-indexed starting line.
    pub start_line: usize,
    /// The zero-indexed ending line.
    pub end_line: usize,
    /// The zero-indexed column where the symbol's **name identifier** begins.
    ///
    /// For `pub fn dedent(code: &str)`, this is the column of the `d` in `dedent`
    /// (not the `p` in `pub`). Used by LSP navigation tools (`get_definition`,
    /// `analyze_impact`, `read_with_deep_context`) to position the cursor on the
    /// symbol name, which is required for rust-analyzer to resolve the symbol.
    pub name_column: usize,
    /// The version hash of the *entire file* at the time of extraction.
    pub version_hash: VersionHash,
    /// The language of the file.
    pub language: String,
}
```

## Step 4: Propagate name_column in read_symbol_scope_impl

**File:** `crates/pathfinder-treesitter/src/treesitter_surgeon.rs`

Find the `read_symbol_scope` implementation that constructs `SymbolScope`. It currently
uses the `ExtractedSymbol` to build the `SymbolScope`. Add `name_column` from the extracted symbol.

Search for `SymbolScope {` in this file and add:
```rust
name_column: symbol.name_column,
```

Also update the `ReadSymbolScopeMetadata` construction in `symbols.rs` tool handler to
include `name_column` (if it mirrors `SymbolScope` fields).

## EXCLUSIONS — Do NOT Modify These

- `navigation.rs` — that's PATCH-002
- Any test files — we add new tests but don't modify existing ones
- `ReadSymbolScopeMetadata` response type — `name_column` is internal-only for now,
  not surfaced to agents in this patch. Agents don't need it; it's consumed internally
  by navigation tools.

## Verification

```bash
# 1. Confirm name_column exists in both types
grep -n 'name_column' crates/pathfinder-common/src/types.rs
grep -n 'name_column' crates/pathfinder-treesitter/src/surgeon.rs
grep -n 'name_column' crates/pathfinder-treesitter/src/symbols.rs

# Expected: at least 1 result per file

# 2. Confirm all ExtractedSymbol constructions have name_column
grep -A15 'ExtractedSymbol {' crates/pathfinder-treesitter/src/symbols.rs | grep name_column

# Expected: multiple hits (one per construction site)

# 3. Build succeeds
cargo build --all

# 4. Full verification
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```
