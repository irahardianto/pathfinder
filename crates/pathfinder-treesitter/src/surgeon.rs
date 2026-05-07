use crate::error::SurgeonError;
use pathfinder_common::types::{SemanticPath, SymbolScope};
use std::path::Path;

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
    /// Whether this symbol is publicly visible.
    ///
    /// - For Rust modules: `true` when declared `pub mod`, `false` for bare `mod`.
    /// - For all other symbols: defaults to `true` (visibility determined by
    ///   name-convention heuristics in `repo_map::is_symbol_public`).
    pub is_public: bool,
    /// Nested child symbols (e.g., methods within a class).
    pub children: Vec<Self>,
}

/// The type of an AST symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
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

    /// Find the semantic path of the innermost symbol that encloses the
    /// given 1-indexed line.
    async fn enclosing_symbol(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: usize,
    ) -> Result<Option<String>, SurgeonError>;

    /// Classify the AST node at a given source position.
    ///
    /// Walks the AST from the leaf node at `(line, column)` upward, returning
    /// the first meaningful category:
    /// - `"comment"` — the position is inside a comment node
    /// - `"string"` — the position is inside a string literal
    /// - `"code"` — all other positions (identifiers, operators, blocks, etc.)
    ///
    /// Used by `search_codebase` to implement `filter_mode`:
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
}
