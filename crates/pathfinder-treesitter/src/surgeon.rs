use crate::error::SurgeonError;
use pathfinder_common::types::{SemanticPath, SymbolScope};
use std::path::Path;
pub use tree_sitter::Point;

/// The access level of an AST symbol, as determined by language-specific rules.
///
/// > **Note**: Do not confuse with `pathfinder_common::types::Visibility` which is
/// > a repo-map *filter* parameter (`Public | All`). This enum describes the
/// > per-symbol access level extracted from the AST.
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

/// Encoding used for LSP text position offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PositionEncoding {
    /// UTF-8 byte offsets (e.g., clangd, or rust-analyzer when negotiated).
    Utf8,
    /// UTF-16 code units (LSP 3.17 default when unnegotiated).
    #[default]
    Utf16,
}

/// Converts a 0-indexed byte column within a line into an LSP character position (0-indexed).
///
/// When `encoding` is [`PositionEncoding::Utf16`], counts UTF-16 code units in the prefix
/// of `line_bytes` up to `byte_col`. When `encoding` is [`PositionEncoding::Utf8`], returns
/// the clamped byte column directly.
#[must_use]
pub fn to_lsp_character(line_bytes: &[u8], byte_col: usize, encoding: PositionEncoding) -> u32 {
    let clamped_col = byte_col.min(line_bytes.len());
    match encoding {
        PositionEncoding::Utf8 => u32::try_from(clamped_col).unwrap_or(u32::MAX),
        PositionEncoding::Utf16 => {
            let slice = &line_bytes[..clamped_col];
            match std::str::from_utf8(slice) {
                Ok(s) => s
                    .chars()
                    .map(|c| u32::try_from(c.len_utf16()).unwrap_or(0))
                    .sum(),
                Err(e) => {
                    let valid_part = &slice[..e.valid_up_to()];
                    let mut count: u32 = std::str::from_utf8(valid_part)
                        .unwrap_or("")
                        .chars()
                        .map(|c| u32::try_from(c.len_utf16()).unwrap_or(0))
                        .sum();
                    count +=
                        u32::try_from(slice.len().saturating_sub(e.valid_up_to())).unwrap_or(0);
                    count
                }
            }
        }
    }
}

/// Information about a symbol successfully extracted from the AST.
#[derive(Debug, Clone)]
pub struct ExtractedSymbol {
    /// The name of the symbol (e.g., "login").
    pub name: String,
    /// The semantic path to this symbol (e.g., "AuthService.login").
    pub semantic_path: String,
    /// The kind of symbol it is.
    pub kind: SymbolKind,
    /// The byte range in the source file spanning the entire symbol.
    pub byte_range: std::ops::Range<usize>,
    /// The expanded byte range including attached decorators, attributes, doc comments, and export modifiers (TS-C1).
    pub attached_range: std::ops::Range<usize>,
    /// The zero-indexed starting line (for display and range extraction only).
    pub start_line: usize,
    /// The zero-indexed ending line (for display and range extraction only).
    pub end_line: usize,
    /// The zero-indexed column where the symbol's **name identifier** begins.
    ///
    /// Preserved for backward compatibility and display. Derived from `name_point.column`.
    pub name_column: usize,
    /// The exact AST point (0-indexed row, 0-indexed byte column) of the symbol's **name identifier**.
    ///
    /// Used by LSP navigation tools to position queries directly on the identifier token.
    /// Replaces previous loose pairings of declaration `start_line` with `name_column` (TS-A1).
    pub name_point: tree_sitter::Point,
    /// The access level of this symbol as determined by language-specific AST rules.
    ///
    /// - Rust: `pub` → `Public`, `pub(crate)` → `Package`, `pub(super)` → `Protected`, bare → `Private`.
    /// - Go: uppercase name → `Public`, `_`-prefix → `Private`, lowercase → `Package`.
    /// - TypeScript/JS: `export` ancestor → `Public`, `_`-prefix → `Private`, otherwise → `Package`.
    /// - Python: `__`-prefix (non-dunder) → `Private`, `_`-prefix → `Protected`, bare → `Public`.
    pub access_level: AccessLevel,
    /// Nested child symbols (e.g., methods within a class).
    pub children: Vec<Self>,
}

impl Default for ExtractedSymbol {
    fn default() -> Self {
        Self {
            name: String::default(),
            semantic_path: String::default(),
            kind: SymbolKind::Function,
            byte_range: 0..0,
            attached_range: 0..0,
            start_line: 0,
            end_line: 0,
            name_column: 0,
            name_point: tree_sitter::Point { row: 0, column: 0 },
            access_level: AccessLevel::Public,
            children: Vec::default(),
        }
    }
}

/// The type of an AST symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    /// A test function (e.g., `#[test] fn`, `@Test def`, `test_` prefix).
    Test,
    /// A standalone function.
    Function,
    /// A method associated with a class or impl block.
    Method,
    /// A class declaration.
    Class,
    /// A struct declaration.
    Struct,
    /// An implementation block (`impl`).
    Impl,
    /// A constant value.
    Constant,
    /// An interface declaration.
    Interface,
    /// An enumeration.
    Enum,
    /// A module block (e.g., Rust `mod tests { ... }`, TS `namespace`).
    Module,
    /// A Vue SFC zone container (`template` or `style`).
    ///
    /// Acts as a parent grouping all child symbols extracted from that zone.
    Zone,
    /// A capitalised HTML element used as a Vue component (e.g. `<MyButton>`).
    Component,
    /// A lowercase HTML element (e.g. `<div>`, `<router-view>`).
    HtmlElement,
    /// A CSS selector within a `<style>` block (class `.foo`, id `#bar`, or tag `p`).
    CssSelector,
    /// A CSS at-rule within a `<style>` block (`@media`, `@keyframes`).
    CssAtRule,
}

/// The `Surgeon` trait — testability boundary for AST-aware operations.
///
/// Consumers depend on this trait rather than the concrete `TreeSitterSurgeon`,
/// enabling unit testing without real file parsing dependency.
#[async_trait::async_trait]
pub trait Surgeon: Send + Sync {
    /// Extract the exact source code of a symbol by its semantic path.
    async fn read_symbol_scope(
        &self,
        workspace_root: &Path,
        semantic_path: &SemanticPath,
    ) -> Result<SymbolScope, SurgeonError>;

    /// Extract all identifiable symbols from a file's AST.
    async fn extract_symbols(
        &self,
        workspace_root: &Path,
        file_path: &Path,
    ) -> Result<Vec<ExtractedSymbol>, SurgeonError>;

    /// Extract symbols using pre-loaded file content and mtime.
    ///
    /// Default implementation delegates to [`Self::extract_symbols`]. Override in
    /// implementations that support pre-loaded content to avoid redundant
    /// disk reads.
    async fn extract_symbols_preloaded(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        content: std::sync::Arc<[u8]>,
        mtime: std::time::SystemTime,
    ) -> Result<Vec<ExtractedSymbol>, SurgeonError> {
        let _ = (content, mtime);
        self.extract_symbols(workspace_root, file_path).await
    }

    /// Find the semantic path of the innermost symbol that encloses the
    /// given 1-indexed line.
    async fn enclosing_symbol(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: usize,
    ) -> Result<Option<String>, SurgeonError>;

    /// Find the innermost symbol enclosing the given 1-indexed line,
    /// returning the full [`ExtractedSymbol`] (including `start_line` for
    /// computing `is_definition`).
    async fn enclosing_symbol_detail(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: usize,
    ) -> Result<Option<ExtractedSymbol>, SurgeonError> {
        let _ = (workspace_root, file_path, line);
        Ok(None)
    }

    /// Classify the AST node at a given source position.
    ///
    /// Walks the AST from the leaf node at `(line, column)` upward, returning
    /// the first meaningful category:
    /// - `"comment"` — the position is inside a comment node
    /// - `"string"` — the position is inside a string literal
    /// - `"code"` — all other positions (identifiers, operators, blocks, etc.)
    ///
    /// Used by `search` to implement `filter_mode`:
    /// - `code_only` → keep matches where this returns `"code"`
    /// - `comments_only` → keep matches where this returns `"comment"` or `"string"`
    ///
    /// Falls back to `"code"` for unsupported languages (no Tree-sitter grammar).
    ///
    /// # Arguments
    /// - `line` — 1-indexed line number (matches the `line` field of search results)
    /// - `column` — 0-indexed byte column
    async fn node_type_at_position(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: usize,
        column: usize,
    ) -> Result<String, SurgeonError>;

    /// Generate an AST-based skeleton of a directory tree.
    async fn generate_skeleton(
        &self,
        workspace_root: &Path,
        path: &Path,
        config: &crate::repo_map::SkeletonConfig<'_>,
    ) -> Result<crate::repo_map::RepoMapResult, SurgeonError>;

    /// Read an entire source file and extract its symbols.
    ///
    /// Returns the complete file content, the detected language,
    /// and a hierarchical listing of all extracted symbols (functions, classes, etc).
    async fn read_source_file(
        &self,
        workspace_root: &Path,
        file_path: &Path,
    ) -> Result<(String, String, Vec<ExtractedSymbol>), SurgeonError>;

    /// Invalidate any cached AST or symbols for the given file.
    fn invalidate(&self, workspace_root: &Path, file_path: &Path) {
        let _ = (workspace_root, file_path);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "surgeon_test.rs"]
mod tests;
