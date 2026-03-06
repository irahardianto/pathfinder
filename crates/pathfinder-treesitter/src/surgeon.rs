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

    /// Generate an AST-based skeleton of a directory tree.
    async fn generate_skeleton(
        &self,
        workspace_root: &Path,
        path: &Path,
        max_tokens: u32,
        depth: u32,
        visibility: &str,
    ) -> Result<crate::repo_map::RepoMapResult, SurgeonError>;
}
