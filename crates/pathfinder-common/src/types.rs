//! Shared domain types for Pathfinder.
//!
//! These types are used across all crates to ensure consistent
//! representation of semantic paths, version hashes, and filter modes.

use crate::error::PathfinderError;
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
    /// The segments that make up the symbol chain.
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
    /// The name of the symbol.
    pub name: String,
    /// The optional overload index of the symbol.
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
    /// Internal prefix stored in every hash value.
    const PREFIX: &'static str = "sha256:";
    /// Minimum number of hex chars accepted as a valid short hash.
    const MIN_HEX_CHARS: usize = 7;

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

    /// Get the full internal hash string (`sha256:<64 hex chars>`).
    ///
    /// Use [`short`] for agent-facing responses; use this only for
    /// diagnostic messages and error context where precision is needed.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Compact 7-hex-char hash for agent-facing responses.
    ///
    /// Omits the `sha256:` prefix — the field name `version_hash` already
    /// communicates the purpose. This cuts per-hash token cost from 71 to 7
    /// characters, reducing agent context window pressure across multi-file
    /// editing sessions.
    ///
    /// # Example
    /// ```
    /// # use pathfinder_common::types::VersionHash;
    /// let h = VersionHash::compute(b"hello");
    /// assert_eq!(h.short().len(), 7);
    /// // short() is the first 7 hex chars of the full hash (no prefix)
    /// assert!(h.as_str()[7..].starts_with(h.short()));
    /// ```
    #[must_use]
    pub fn short(&self) -> &str {
        // Internal layout: "sha256:" (7 bytes) + 64 hex chars
        // Return chars [7..14] — the first 7 hex chars, no prefix.
        &self.0[Self::PREFIX.len()..Self::PREFIX.len() + Self::MIN_HEX_CHARS]
    }

    /// Check whether an agent-supplied hash token matches this hash.
    ///
    /// This is the single authoritative OCC comparison — it replaces all raw
    /// `==` / `!=` string comparisons and `check_occ` prefix logic. Accepting
    /// all formats prevents version-mismatch failures when agents supply the
    /// short form produced by [`short`].
    ///
    /// # Accepted formats
    ///
    /// | Format | Example | Notes |
    /// |--------|---------|-------|
    /// | Short (no prefix) | `"e3dc7f9"` | Preferred — what [`short`] emits |
    /// | Short (with prefix) | `"sha256:e3dc7f9"` | Legacy short form |
    /// | Full (with prefix) | `"sha256:<64 hex>"` | Full hash |
    ///
    /// Returns `false` if the input has fewer than 7 hex chars.
    #[must_use]
    pub fn matches(&self, agent_input: &str) -> bool {
        let full_hex = &self.0[Self::PREFIX.len()..]; // 64 hex chars
        let input_hex = agent_input
            .strip_prefix(Self::PREFIX)
            .unwrap_or(agent_input);
        input_hex.len() >= Self::MIN_HEX_CHARS && full_hex.starts_with(input_hex)
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
        let is_absolute = relative.is_absolute();
        let has_traversal = relative
            .components()
            .any(|c| c == std::path::Component::ParentDir);

        // Defense-in-depth: detect path traversal even without sandbox
        if is_absolute || has_traversal {
            tracing::warn!(
                relative = %relative.display(),
                workspace = %self.0.display(),
                "WorkspaceRoot::resolve: absolute path or traversal detected; sandbox will reject"
            );
        }

        let mut normalized = PathBuf::default();
        for comp in relative.components() {
            if matches!(
                comp,
                std::path::Component::Prefix(_) | std::path::Component::RootDir
            ) {
                continue;
            }
            normalized.push(comp);
        }

        self.0.join(normalized)
    }

    /// Strict variant of resolve that rejects path traversal attempts.
    ///
    /// Use this in security-critical paths (file operations, edit tools).
    /// Returns an error if the relative path contains `..` components or is absolute.
    ///
    /// # Errors
    /// Returns `PathfinderError::PathTraversal` if the path contains traversal
    /// components or is absolute.
    pub fn resolve_strict(&self, relative: &Path) -> Result<PathBuf, PathfinderError> {
        let is_absolute = relative.is_absolute();
        let has_traversal = relative
            .components()
            .any(|c| c == std::path::Component::ParentDir);

        if is_absolute || has_traversal {
            return Err(PathfinderError::PathTraversal {
                path: relative.to_path_buf(),
                workspace_root: self.0.clone(),
            });
        }

        // Delegate to resolve for the actual normalization
        // (which still warns but since we've already filtered, it won't trigger)
        Ok(self.resolve(relative))
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
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Only exported/public symbols.
    #[default]
    Public,
    /// All symbols including private/internal.
    All,
}

/// Import inclusion mode for `get_repo_map`.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
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

    // ── VersionHash::short() tests ────────────────────────────────────────────

    /// `short()` must return exactly 7 hex characters with no prefix.
    #[test]
    fn test_version_hash_short_is_7_hex_chars() {
        let hash = VersionHash::compute(b"hello world");
        let s = hash.short();
        assert_eq!(s.len(), 7, "short() must be exactly 7 chars");
        assert!(
            s.chars().all(|c| c.is_ascii_hexdigit()),
            "short() must be hex chars only, got: {s}"
        );
    }

    /// `short()` must NOT contain the 'sha256:' prefix.
    #[test]
    fn test_version_hash_short_has_no_prefix() {
        let hash = VersionHash::compute(b"test content");
        assert!(
            !hash.short().starts_with("sha256:"),
            "short() must not start with 'sha256:'"
        );
    }

    /// `short()` must be the start of the hex portion of `as_str()`.
    #[test]
    fn test_version_hash_short_is_prefix_of_full_hex() {
        let hash = VersionHash::compute(b"hello world");
        let full = hash.as_str(); // "sha256:<64 hex>"
        assert!(
            full["sha256:".len()..].starts_with(hash.short()),
            "full hex must start with short()"
        );
    }

    // ── VersionHash::matches() tests ──────────────────────────────────────────

    /// The preferred format: 7 hex chars, no prefix — what `short()` emits.
    #[test]
    fn test_matches_short_no_prefix() {
        let hash = VersionHash::compute(b"hello world");
        assert!(
            hash.matches(hash.short()),
            "hash.matches(hash.short()) must be true — roundtrip test"
        );
    }

    /// Short hash with the legacy sha256: prefix.
    #[test]
    fn test_matches_short_with_legacy_prefix() {
        let hash = VersionHash::compute(b"hello world");
        let with_prefix = format!("sha256:{}", hash.short());
        assert!(
            hash.matches(&with_prefix),
            "7-char hash with sha256: prefix must match"
        );
    }

    /// Full 71-char hash with prefix (backward compatibility).
    #[test]
    fn test_matches_full_hash_with_prefix() {
        let hash = VersionHash::compute(b"hello world");
        assert!(
            hash.matches(hash.as_str()),
            "full hash as_str() must match itself"
        );
    }

    /// 8-char prefix should also be accepted (> minimum).
    #[test]
    fn test_matches_8_char_prefix_accepted() {
        let hash = VersionHash::compute(b"hello world");
        let eight = &hash.as_str()["sha256:".len().."sha256:".len() + 8];
        assert!(hash.matches(eight), "8-char prefix must be accepted");
    }

    /// Inputs shorter than 7 hex chars must be rejected.
    #[test]
    fn test_matches_too_short_rejected() {
        let hash = VersionHash::compute(b"hello world");
        assert!(!hash.matches("e3dc7f"), "6 hex chars must be rejected");
        assert!(
            !hash.matches("sha256:abc"),
            "3 hex chars with prefix rejected"
        );
        assert!(!hash.matches(""), "empty string must be rejected");
    }

    /// Wrong prefix must not match.
    #[test]
    fn test_matches_wrong_hex_fails() {
        let hash = VersionHash::compute(b"hello world");
        assert!(!hash.matches("0000000"), "wrong 7-char hex must not match");
        assert!(
            !hash.matches("sha256:0000000"),
            "wrong prefixed hex must not match"
        );
    }

    /// Hashes of different content must not match each other.
    #[test]
    fn test_matches_different_content_fails() {
        let hash_a = VersionHash::compute(b"content A");
        let hash_b = VersionHash::compute(b"content B");
        assert!(
            !hash_a.matches(hash_b.short()),
            "short hash from different content must not match"
        );
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

    #[test]
    fn test_resolve_strict_rejects_traversal() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let root = WorkspaceRoot::new(dir.path()).expect("create workspace root");

        let traversal = std::path::Path::new("../../etc/passwd");
        let result = root.resolve_strict(traversal);

        assert!(result.is_err());
        assert!(matches!(result, Err(PathfinderError::PathTraversal { .. })));
    }

    #[test]
    fn test_resolve_strict_rejects_absolute_path() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let root = WorkspaceRoot::new(dir.path()).expect("create workspace root");

        let absolute = std::path::Path::new("/etc/passwd");
        let result = root.resolve_strict(absolute);

        assert!(result.is_err());
        assert!(matches!(result, Err(PathfinderError::PathTraversal { .. })));
    }

    #[test]
    fn test_resolve_strict_accepts_relative_path() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let root = WorkspaceRoot::new(dir.path()).expect("create workspace root");

        let relative = std::path::Path::new("src/main.rs");
        let result = root.resolve_strict(relative);

        assert!(result.is_ok());
        let resolved = result.expect("should be Ok");
        assert!(resolved.to_string_lossy().contains("src/main.rs"));
    }
}
