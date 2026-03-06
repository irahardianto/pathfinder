//! Shared domain types for Pathfinder.
//!
//! These types are used across all crates to ensure consistent
//! representation of semantic paths, version hashes, and filter modes.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

/// A parsed semantic path in the format `file_path[::symbol_chain]`.
///
/// The semantic path is the unified addressing scheme used by all
/// symbol-level tools. See PRD §1.3 for the full grammar:
///
/// ```ebnf
/// semantic_path   = file_path ["::"] symbol_chain]
/// file_path       = relative_path
/// symbol_chain    = symbol ("." symbol)*
/// symbol          = identifier [overload_suffix]
/// overload_suffix = "#" digit+
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticPath {
    /// Relative file path within the workspace.
    pub file_path: PathBuf,
    /// Optional symbol chain (e.g., `AuthService.login`).
    pub symbol_chain: Option<SymbolChain>,
}

impl SemanticPath {
    /// Parse a semantic path string.
    ///
    /// # Examples
    /// - `"src/auth.ts::AuthService.login"` → file + symbol chain
    /// - `"src/utils.ts"` → bare file path (no symbol)
    /// - `"src/auth.ts::AuthService.login#2"` → overloaded method
    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        if input.is_empty() {
            return None;
        }

        if let Some((file_part, symbol_part)) = input.split_once("::") {
            if file_part.is_empty() {
                return None;
            }
            let symbol_chain = SymbolChain::parse(symbol_part)?;
            Some(Self {
                file_path: PathBuf::from(file_part),
                symbol_chain: Some(symbol_chain),
            })
        } else {
            Some(Self {
                file_path: PathBuf::from(input),
                symbol_chain: None,
            })
        }
    }

    /// Returns `true` if this is a bare file path (no `::` symbol chain).
    #[must_use]
    pub fn is_bare_file(&self) -> bool {
        self.symbol_chain.is_none()
    }
}

impl fmt::Display for SemanticPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.file_path.display())?;
        if let Some(chain) = &self.symbol_chain {
            write!(f, "::{chain}")?;
        }
        Ok(())
    }
}

/// A chain of symbols separated by dots.
///
/// Example: `AuthService.login` → `["AuthService", "login"]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolChain {
    pub segments: Vec<Symbol>,
}

impl SymbolChain {
    /// Parse a symbol chain from the part after `::`.
    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        if input.is_empty() {
            return None;
        }

        let segments: Vec<Symbol> = input.split('.').filter_map(Symbol::parse).collect();

        if segments.is_empty() {
            return None;
        }

        Some(Self { segments })
    }
}

impl fmt::Display for SymbolChain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = self.segments.iter().map(ToString::to_string).collect();
        write!(f, "{}", parts.join("."))
    }
}

/// A single symbol, optionally with an overload suffix.
///
/// Example: `login` or `login#2`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub overload_index: Option<u32>,
}

impl Symbol {
    /// Parse a single symbol segment.
    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        if input.is_empty() {
            return None;
        }

        if let Some((name, suffix)) = input.split_once('#') {
            let index = suffix.parse::<u32>().ok()?;
            Some(Self {
                name: name.to_owned(),
                overload_index: Some(index),
            })
        } else {
            Some(Self {
                name: input.to_owned(),
                overload_index: None,
            })
        }
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)?;
        if let Some(idx) = self.overload_index {
            write!(f, "#{idx}")?;
        }
        Ok(())
    }
}

/// A SHA-256 version hash of file content, used for OCC (Optimistic Concurrency Control).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VersionHash(String);

impl VersionHash {
    /// Compute the SHA-256 hash of file content.
    #[must_use]
    pub fn compute(content: &[u8]) -> Self {
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(content);
        Self(format!("sha256:{hash:x}"))
    }

    /// Create from a raw hash string (for deserialization from client input).
    #[must_use]
    pub fn from_raw(hash: String) -> Self {
        Self(hash)
    }

    /// Get the hash string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for VersionHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The exact source code and metadata for an AST symbol.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// The workspace root path. All file operations are relative to this.
#[derive(Debug, Clone)]
pub struct WorkspaceRoot(PathBuf);

impl WorkspaceRoot {
    /// Create a new workspace root, verifying the directory exists.
    ///
    /// # Errors
    /// Returns `std::io::Error` if the path does not exist or cannot be canonicalized.
    pub fn new(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        let canonical = path.canonicalize()?;
        Ok(Self(canonical))
    }

    /// Resolve a relative path against the workspace root.
    ///
    /// # Security
    /// This function performs a defense-in-depth traversal check: if the
    /// relative path contains `..` components, a warning is logged.
    /// The caller's Sandbox is the primary security boundary; this guard
    /// ensures internal callers that bypass the Sandbox are also warned.
    #[must_use]
    pub fn resolve(&self, relative: &Path) -> PathBuf {
        // Defense-in-depth: detect path traversal even without sandbox
        let has_traversal = relative
            .components()
            .any(|c| c == std::path::Component::ParentDir);
        if has_traversal {
            tracing::warn!(
                relative = %relative.display(),
                workspace = %self.0.display(),
                "WorkspaceRoot::resolve: path traversal detected; sandbox will reject"
            );
        }
        self.0.join(relative)
    }

    /// Get the workspace root path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }
}

/// Filter mode for `search_codebase`.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FilterMode {
    /// Only matches in code (exclude comments and string literals).
    #[default]
    CodeOnly,
    /// Only matches in comments.
    CommentsOnly,
    /// All matches (no filtering).
    All,
}

/// Visibility filter for `get_repo_map`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Only exported/public symbols.
    #[default]
    Public,
    /// All symbols including private/internal.
    All,
}

/// Import inclusion mode for `get_repo_map`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncludeImports {
    /// Omit all imports.
    None,
    /// Include only external/package imports.
    #[default]
    ThirdParty,
    /// Include all import statements.
    All,
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_semantic_path_with_symbol() {
        let sp = SemanticPath::parse("src/auth.ts::AuthService.login").expect("should parse");
        assert_eq!(sp.file_path, PathBuf::from("src/auth.ts"));
        assert!(!sp.is_bare_file());

        let chain = sp.symbol_chain.as_ref().expect("should have symbol chain");
        assert_eq!(chain.segments.len(), 2);
        assert_eq!(chain.segments[0].name, "AuthService");
        assert_eq!(chain.segments[1].name, "login");
    }

    #[test]
    fn test_semantic_path_bare_file() {
        let sp = SemanticPath::parse("src/utils.ts").expect("should parse");
        assert_eq!(sp.file_path, PathBuf::from("src/utils.ts"));
        assert!(sp.is_bare_file());
    }

    #[test]
    fn test_semantic_path_with_overload() {
        let sp =
            SemanticPath::parse("src/auth.ts::AuthService.refreshToken#2").expect("should parse");
        let chain = sp.symbol_chain.as_ref().expect("should have symbol chain");
        let last = chain.segments.last().expect("should have segments");
        assert_eq!(last.name, "refreshToken");
        assert_eq!(last.overload_index, Some(2));
    }

    #[test]
    fn test_semantic_path_display_roundtrip() {
        let input = "src/auth.ts::AuthService.login#2";
        let sp = SemanticPath::parse(input).expect("should parse");
        assert_eq!(sp.to_string(), input);
    }

    #[test]
    fn test_semantic_path_empty_input() {
        assert!(SemanticPath::parse("").is_none());
    }

    #[test]
    fn test_semantic_path_empty_file_part() {
        assert!(SemanticPath::parse("::AuthService").is_none());
    }

    #[test]
    fn test_semantic_path_default_export() {
        let sp = SemanticPath::parse("src/auth.ts::default").expect("should parse");
        let chain = sp.symbol_chain.as_ref().expect("should have chain");
        assert_eq!(chain.segments.len(), 1);
        assert_eq!(chain.segments[0].name, "default");
    }

    #[test]
    fn test_version_hash_compute() {
        let hash = VersionHash::compute(b"hello world");
        assert!(hash.as_str().starts_with("sha256:"));
        // SHA-256 of "hello world" is well-known
        assert!(hash.as_str().contains("b94d27b9934d3e08a52e52d7"));
    }

    #[test]
    fn test_version_hash_equality() {
        let h1 = VersionHash::compute(b"same content");
        let h2 = VersionHash::compute(b"same content");
        assert_eq!(h1, h2);

        let h3 = VersionHash::compute(b"different content");
        assert_ne!(h1, h3);
    }

    #[test]
    fn test_filter_mode_default() {
        assert_eq!(FilterMode::default(), FilterMode::CodeOnly);
    }

    #[test]
    fn test_resolve_path_traversal_is_detected() {
        // WorkspaceRoot::resolve must still return the joined path (so the
        // Sandbox can do its job), but the traversal-detection branch must
        // fire without panicking.
        let dir = tempfile::tempdir().expect("create tempdir");
        let root = WorkspaceRoot::new(dir.path()).expect("create workspace root");

        let traversal = std::path::Path::new("../../etc/passwd");
        // Should not panic; the sandbox is the primary enforcement layer.
        let resolved = root.resolve(traversal);
        // The resolved path escapes the workspace — that is expected here.
        // The Sandbox (not resolve) is responsible for rejection.
        assert!(resolved.to_string_lossy().contains("etc/passwd"));
    }
}
