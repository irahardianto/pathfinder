use crate::error::SurgeonError;
use pathfinder_common::types::{SemanticPath, SymbolScope, VersionHash};
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
    /// Nested child symbols (e.g., methods within a class).
    pub children: Vec<ExtractedSymbol>,
}

/// The type of an AST symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Impl,
    Constant,
    Interface,
    Enum,
    /// A Vue SFC zone container (`template` or `style`).
    ///
    /// Acts as a parent grouping all child symbols extracted from that zone.
    /// Read-only — not a target for edit operations.
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

/// The byte range and context needed to splice a new body into a symbol.
///
/// Used by `replace_body` to locate the content region inside a function's
/// braces (or equivalent block delimiters in the target language).
#[derive(Debug, Clone)]
pub struct BodyRange {
    /// Byte offset of the start of the body block in the file.
    pub start_byte: usize,
    /// Byte offset of the end of the body block in the file (exclusive).
    pub end_byte: usize,
    /// Column (0-indexed) of the symbol's starting line, used for re-indentation.
    pub indent_column: usize,
    /// Column (0-indexed) of the first non-empty line inside the body.
    pub body_indent_column: usize,
}

/// The byte range and context spanning an entire declaration (including decorators, doc comments, etc).
///
/// Used by `replace_full` and `delete_symbol` to replace or remove the entire symbol.
#[derive(Debug, Clone)]
pub struct FullRange {
    /// Byte offset of the start of the entire declaration (including preceding doc comments/decorators).
    pub start_byte: usize,
    /// Byte offset of the end of the declaration (exclusive).
    pub end_byte: usize,
    /// Column (0-indexed) of the symbol's start (excluding comments), used for indentation.
    pub indent_column: usize,
}

/// The byte range and context used to position new code around an existing symbol.
///
/// Functionally identical to `FullRange` data, but semantically distinct. Used
/// by `insert_before` and `insert_after`.
#[derive(Debug, Clone)]
pub struct SymbolRange {
    /// Byte offset of the start of the entire declaration.
    pub start_byte: usize,
    /// Byte offset of the end of the declaration.
    pub end_byte: usize,
    /// Column (0-indexed) of the symbol's start, used as the baseline indentation for inserted code.
    pub indent_column: usize,
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
    #[allow(clippy::too_many_arguments)]
    async fn generate_skeleton(
        &self,
        workspace_root: &Path,
        path: &Path,
        max_tokens: u32,
        depth: u32,
        visibility: &str,
        max_tokens_per_file: u32,
        changed_files: Option<std::collections::HashSet<std::path::PathBuf>>,
        include_extensions: Vec<String>,
        exclude_extensions: Vec<String>,
    ) -> Result<crate::repo_map::RepoMapResult, SurgeonError>;

    /// Resolve the body byte range and indent column for a symbol.
    ///
    /// Returns the `BodyRange` (brace positions + indent column) along with
    /// the raw file source bytes and the current `VersionHash` for OCC.
    ///
    /// # Errors
    /// - `SurgeonError::SymbolNotFound` — semantic path does not resolve
    /// - `SurgeonError::InvalidTarget` — target symbol has no body (e.g., constant)
    /// - `SurgeonError::UnsupportedLanguage` — file language not supported
    /// - `SurgeonError::Io` — file cannot be read
    async fn resolve_body_range(
        &self,
        workspace_root: &Path,
        semantic_path: &SemanticPath,
    ) -> Result<(BodyRange, Vec<u8>, VersionHash), SurgeonError>;

    /// Read an entire source file and extract its symbols.
    ///
    /// Returns the complete file content, its OCC version hash, the detected language,
    /// and a hierarchical listing of all extracted symbols (functions, classes, etc).
    async fn read_source_file(
        &self,
        workspace_root: &Path,
        file_path: &Path,
    ) -> Result<(String, VersionHash, String, Vec<ExtractedSymbol>), SurgeonError>;

    /// Resolve the full byte range for a symbol, including decorators and doc comments.
    ///
    /// Used by `replace_full` and `delete_symbol`.
    async fn resolve_full_range(
        &self,
        workspace_root: &Path,
        semantic_path: &SemanticPath,
    ) -> Result<(FullRange, Vec<u8>, VersionHash), SurgeonError>;

    /// Resolve the symbol byte range for insertion operations.
    ///
    /// Used by `insert_before` and `insert_after`.
    async fn resolve_symbol_range(
        &self,
        workspace_root: &Path,
        semantic_path: &SemanticPath,
    ) -> Result<(SymbolRange, Vec<u8>, VersionHash), SurgeonError>;
}
