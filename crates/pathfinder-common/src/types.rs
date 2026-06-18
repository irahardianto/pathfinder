//! Shared domain types for Pathfinder.
//!
//! These types are used across all crates to ensure consistent
//! representation of semantic paths, version hashes, and filter modes.

use crate::error::PathfinderError;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

/// Directories that should always be excluded from search and file traversal.
///
/// These are never source code and cause false positives in grep fallback.
/// Used by both the sandbox (file access control) and search (file walking).
/// Includes both Unix (`/`) and Windows (`\`) path separators.
pub const ALWAYS_EXCLUDED_DIRS: &[&str] = &[
    ".git/",
    "node_modules/",
    "vendor/",
    ".idea/",
    ".vscode/",
    "__pycache__/",
    ".qlty/",
];

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
    pub const fn is_bare_file(&self) -> bool {
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

        let dot_count = input.bytes().filter(|&b| b == b'.').count();
        let mut segments = Vec::with_capacity(dot_count + 1);
        for seg in input.split('.') {
            if let Some(s) = Symbol::parse(seg) {
                segments.push(s);
            }
        }

        if segments.is_empty() {
            return None;
        }

        Some(Self { segments })
    }
}

impl fmt::Display for SymbolChain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for seg in &self.segments {
            if !first {
                write!(f, ".")?;
            }
            write!(f, "{seg}")?;
            first = false;
        }
        Ok(())
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

/// A SHA-256 version hash of file content, used as a content fingerprint to detect changes.
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
        let hash_bytes: [u8; 32] = hash.into();
        Self::compute_from_raw(hash_bytes)
    }

    #[must_use]
    pub fn compute_from_raw(hash_bytes: [u8; 32]) -> Self {
        // "sha256:" (7 bytes) + 64 hex chars = 71 bytes total
        let mut buf = String::with_capacity(Self::PREFIX.len() + 64);
        // SAFETY: writing to a `String` via `fmt::Write` is infallible; the `Err` variant is unreachable.
        let _ = std::fmt::write(&mut buf, std::format_args!("{}", Self::PREFIX));
        for b in hash_bytes {
            // SAFETY: writing to a `String` via `fmt::Write` is infallible; the `Err` variant is unreachable.
            let _ = std::fmt::write(&mut buf, std::format_args!("{b:02x}"));
        }
        Self(buf)
    }

    /// Create from a raw hash string (for deserialization from client input).
    #[must_use]
    pub const fn from_raw(hash: String) -> Self {
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
        // Internal layout: "sha256:" (7 bytes) + 64 hex chars (from compute)
        // Return chars [7..14] — the first 7 hex chars, no prefix.
        // Gracefully handle malformed hashes by returning available chars.
        let hex_part = self.0.strip_prefix(Self::PREFIX).unwrap_or(&self.0); // Handle missing prefix
        if hex_part.len() < Self::MIN_HEX_CHARS {
            // Malformed hash — return whatever we have
            hex_part
        } else {
            &hex_part[..Self::MIN_HEX_CHARS]
        }
    }

    /// Check whether an agent-supplied hash token matches this hash.
    ///
    /// This is the single authoritative hash comparison — it replaces all raw
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
    /// The zero-indexed column where the symbol's **name identifier** begins.
    ///
    /// For `pub fn dedent(code: &str)`, this is the column of the `d` in `dedent`
    /// (not the `p` in `pub`). Used by LSP navigation tools (`locate`,
    /// `trace`, `inspect`) to position the cursor on the
    /// symbol name, which is required for rust-analyzer to resolve the symbol.
    pub name_column: usize,
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
    /// # Path traversal protection
    ///
    /// This function returns a joined `PathBuf` even if the input contains
    /// `..` components. The caller's Sandbox is the primary security boundary;
    /// it rejects traversal before any I/O. This method normalizes the path but
    /// does not perform access control.
    ///
    /// # Symlink behavior
    ///
    /// This method does not resolve symlinks. Symlinks are handled at the
    /// Sandbox layer for security enforcement. If you need the canonical path,
    /// use `WorkspaceRoot::path()` and canonicalize manually with appropriate
    /// error handling.
    ///
    /// # Security
    /// This function performs a defense-in-depth traversal check: if the
    /// relative path contains `..` components, a warning is logged.
    /// This guard ensures internal callers that bypass the Sandbox are warned.
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
                // CLONE: self.0 (workspace root PathBuf) is cloned to populate the error struct
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

/// Filter mode for `search`.
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

/// Visibility filter for `explore`.
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

/// Error returned when parsing a [`Visibility`] from a string fails.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid visibility: '{0}' (expected 'public' or 'all')")]
pub struct ParseVisibilityError(pub String);

impl std::str::FromStr for Visibility {
    type Err = ParseVisibilityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "public" => Ok(Self::Public),
            "all" => Ok(Self::All),
            other => Err(ParseVisibilityError(other.to_owned())),
        }
    }
}

impl fmt::Display for Visibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Public => "public",
            Self::All => "all",
        };
        write!(f, "{s}")
    }
}

/// Reason for degraded mode in tool responses.
///
/// Standardized enum for all degraded reasons across tools. Provides
/// machine-parsable values for agents to understand and handle degraded responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DegradedReason {
    /// LSP is not available for this language.
    NoLsp,
    /// LSP is warming up and returned empty unverified results.
    LspWarmupEmptyUnverified,
    /// LSP returned no result (likely warming up); fell back to grep.
    LspWarmupGrepFallback,
    /// LSP timed out; fell back to grep.
    LspTimeoutGrepFallback,
    /// LSP returned an error; fell back to grep.
    LspErrorGrepFallback,
    /// LSP unavailable; fell back to grep.
    NoLspGrepFallback,
    /// Grep fallback result from file-scoped search.
    GrepFallbackFileScoped,
    /// Grep fallback result from impl-scoped search.
    GrepFallbackImplScoped,
    /// Grep fallback result from global search.
    GrepFallbackGlobal,
    /// Grep fallback for `inspect` dependencies via heuristic call parsing.
    GrepFallbackDependencies,
    /// Language unsupported; filter was bypassed to return results.
    UnsupportedLanguageFilterBypassed,
    /// Language is not supported.
    UnsupportedLanguage,
    /// Git error (e.g., `explore` `changed_since` filter failed).
    GitError,
}

impl fmt::Display for DegradedReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            DegradedReason::NoLsp => "no_lsp",
            DegradedReason::LspWarmupEmptyUnverified => "lsp_warmup_empty_unverified",
            DegradedReason::LspWarmupGrepFallback => "lsp_warmup_grep_fallback",
            DegradedReason::LspTimeoutGrepFallback => "lsp_timeout_grep_fallback",
            DegradedReason::LspErrorGrepFallback => "lsp_error_grep_fallback",
            DegradedReason::NoLspGrepFallback => "no_lsp_grep_fallback",
            DegradedReason::GrepFallbackFileScoped => "grep_fallback_file_scoped",
            DegradedReason::GrepFallbackImplScoped => "grep_fallback_impl_scoped",
            DegradedReason::GrepFallbackGlobal => "grep_fallback_global",
            DegradedReason::GrepFallbackDependencies => "grep_fallback_dependencies",
            DegradedReason::UnsupportedLanguageFilterBypassed => {
                "unsupported_language_filter_bypassed"
            }
            DegradedReason::UnsupportedLanguage => "unsupported_language",
            DegradedReason::GitError => "git_error",
        };
        write!(f, "{s}")
    }
}

/// Fallback tool to use for authoritative results when degraded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FallbackTool {
    Search,
    Read,
}

impl FallbackTool {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            FallbackTool::Search => "search",
            FallbackTool::Read => "read",
        }
    }
}

impl fmt::Display for FallbackTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Trust level of the degraded results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    Unreliable,
    Heuristic,
    Partial,
    None,
}

impl TrustLevel {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            TrustLevel::Unreliable => "unreliable",
            TrustLevel::Heuristic => "heuristic",
            TrustLevel::Partial => "partial",
            TrustLevel::None => "none",
        }
    }
}

impl fmt::Display for TrustLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Actionable guidance for handling degraded tool responses.
///
/// Gives agents machine-readable next steps: whether to retry, which fallback
/// tool to use, how trustworthy the results are, and whether the degradation
/// is permanent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ActionableGuidance {
    /// Whether retrying after a delay is recommended.
    pub retry_recommended: bool,
    /// How many seconds to wait before retrying (if `retry_recommended` is true).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_seconds: Option<u32>,
    /// Fallback tool to use for authoritative results (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_tool: Option<FallbackTool>,
    /// Trust level of the degraded results.
    ///
    /// - `Unreliable`: Results should not be trusted, especially empty counts.
    /// - `Heuristic`: Best-effort grep-based results. Verify manually.
    /// - `Partial`: Some features unavailable, but available results are correct.
    /// - `None`: Results are unavailable for this language.
    pub trust_level: TrustLevel,
    /// Whether this degradation is permanent (won't improve on retry).
    pub permanent: bool,
}

impl DegradedReason {
    /// Returns actionable guidance for handling this degraded reason.
    ///
    /// Maps each degradation scenario to recommended next steps for agents:
    /// - Retry timing (transient vs permanent)
    /// - Fallback tool to use
    /// - Trust level of the current results
    #[must_use]
    pub fn guidance(&self) -> ActionableGuidance {
        match self {
            DegradedReason::NoLsp => ActionableGuidance {
                retry_recommended: false,
                retry_after_seconds: None,
                fallback_tool: Some(FallbackTool::Search),
                trust_level: TrustLevel::Partial,
                permanent: true,
            },
            DegradedReason::LspWarmupEmptyUnverified => ActionableGuidance {
                retry_recommended: true,
                retry_after_seconds: Some(15),
                fallback_tool: None,
                trust_level: TrustLevel::Unreliable,
                permanent: false,
            },
            DegradedReason::LspWarmupGrepFallback | DegradedReason::LspTimeoutGrepFallback => {
                ActionableGuidance {
                    retry_recommended: true,
                    retry_after_seconds: Some(30),
                    fallback_tool: Some(FallbackTool::Search),
                    trust_level: TrustLevel::Heuristic,
                    permanent: false,
                }
            }
            DegradedReason::LspErrorGrepFallback
            | DegradedReason::NoLspGrepFallback
            | DegradedReason::GrepFallbackFileScoped
            | DegradedReason::GrepFallbackImplScoped
            | DegradedReason::GrepFallbackGlobal
            | DegradedReason::GrepFallbackDependencies => ActionableGuidance {
                retry_recommended: false,
                retry_after_seconds: None,
                fallback_tool: Some(FallbackTool::Search),
                trust_level: TrustLevel::Heuristic,
                permanent: true,
            },
            DegradedReason::UnsupportedLanguageFilterBypassed => ActionableGuidance {
                retry_recommended: false,
                retry_after_seconds: None,
                fallback_tool: Some(FallbackTool::Read),
                trust_level: TrustLevel::Partial,
                permanent: true,
            },
            DegradedReason::UnsupportedLanguage => ActionableGuidance {
                retry_recommended: false,
                retry_after_seconds: None,
                fallback_tool: Some(FallbackTool::Read),
                trust_level: TrustLevel::None,
                permanent: true,
            },
            DegradedReason::GitError => ActionableGuidance {
                retry_recommended: true,
                retry_after_seconds: Some(5),
                fallback_tool: None,
                trust_level: TrustLevel::Partial,
                permanent: false,
            },
        }
    }
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

    /// `short()` must gracefully handle malformed hashes (too short).
    #[test]
    fn test_version_hash_short_handles_malformed_short() {
        let hash = VersionHash::from_raw("sha256:a1b2".to_string());
        assert_eq!(hash.short(), "a1b2", "short() returns all available chars");
    }

    /// `short()` must gracefully handle malformed hashes (no prefix).
    #[test]
    fn test_version_hash_short_handles_malformed_no_prefix() {
        let hash = VersionHash::from_raw("abcdef".to_string());
        assert_eq!(hash.short(), "abcdef", "short() returns entire string");
    }

    /// `short()` must gracefully handle malformed hashes (just prefix).
    #[test]
    fn test_version_hash_short_handles_malformed_only_prefix() {
        let hash = VersionHash::from_raw("sha256:".to_string());
        assert_eq!(hash.short(), "", "short() returns empty string");
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

    // ── DegradedReason tests ────────────────────────────────────────────────

    #[test]
    fn test_degraded_reason_serde_snake_case() {
        // Verify serde serialization produces snake_case strings (backward compatible)
        use super::DegradedReason;

        assert_eq!(
            serde_json::to_string(&DegradedReason::NoLsp).expect("NoLsp should serialize to JSON"),
            "\"no_lsp\""
        );
        assert_eq!(
            serde_json::to_string(&DegradedReason::LspWarmupGrepFallback)
                .expect("LspWarmupGrepFallback should serialize to JSON"),
            "\"lsp_warmup_grep_fallback\""
        );
        assert_eq!(
            serde_json::to_string(&DegradedReason::GitError)
                .expect("GitError should serialize to JSON"),
            "\"git_error\""
        );
    }

    #[test]
    fn test_degraded_reason_display() {
        use super::DegradedReason;

        assert_eq!(DegradedReason::NoLsp.to_string(), "no_lsp");
        assert_eq!(
            DegradedReason::LspWarmupEmptyUnverified.to_string(),
            "lsp_warmup_empty_unverified"
        );
        assert_eq!(
            DegradedReason::GrepFallbackGlobal.to_string(),
            "grep_fallback_global"
        );
    }

    #[test]
    fn test_degraded_reason_guidance_no_lsp() {
        use super::DegradedReason;
        let g = DegradedReason::NoLsp.guidance();
        assert!(!g.retry_recommended);
        assert!(g.permanent);
        assert_eq!(g.fallback_tool, Some(FallbackTool::Search));
        assert_eq!(g.trust_level, TrustLevel::Partial);
    }

    #[test]
    fn test_degraded_reason_guidance_warmup_retry() {
        use super::DegradedReason;
        let g = DegradedReason::LspWarmupEmptyUnverified.guidance();
        assert!(g.retry_recommended);
        assert_eq!(g.retry_after_seconds, Some(15));
        assert!(!g.permanent);
    }

    #[test]
    fn test_degraded_reason_guidance_grep_fallback_permanent() {
        use super::DegradedReason;
        let g = DegradedReason::LspErrorGrepFallback.guidance();
        assert!(!g.retry_recommended);
        assert!(g.permanent);
        assert_eq!(g.fallback_tool, Some(FallbackTool::Search));
        assert_eq!(g.trust_level, TrustLevel::Heuristic);
    }

    #[test]
    fn test_visibility_display_and_from_str() {
        let v_pub = Visibility::Public;
        assert_eq!(v_pub.to_string(), "public");
        assert_eq!(
            "public".parse::<Visibility>().expect("valid"),
            Visibility::Public
        );

        let v_all = Visibility::All;
        assert_eq!(v_all.to_string(), "all");
        assert_eq!("all".parse::<Visibility>().expect("valid"), Visibility::All);

        let err = "invalid".parse::<Visibility>();
        assert!(err.is_err());
        assert_eq!(
            err.expect_err("invalid visibility").to_string(),
            "invalid visibility: 'invalid' (expected 'public' or 'all')"
        );
    }

    #[test]
    fn test_semantic_path_parse_edge_cases() {
        // Line 106: symbol_part is empty
        assert!(SemanticPath::parse("src/auth.ts::").is_none());

        // Line 118: dot-only symbol part (segments is empty)
        assert!(SemanticPath::parse("src/auth.ts::.").is_none());

        // Line 155: empty segment in symbol chain is skipped, but chain can still parse if other segments exist
        let sp = SemanticPath::parse("src/auth.ts::a..b")
            .expect("should parse a..b by skipping empty segment");
        let chain = sp.symbol_chain.expect("should have symbol chain");
        assert_eq!(chain.segments.len(), 2);
        assert_eq!(chain.segments[0].name, "a");
        assert_eq!(chain.segments[1].name, "b");
    }

    #[test]
    fn test_version_hash_display() {
        let h = VersionHash::compute(b"hello");
        assert_eq!(h.to_string(), h.as_str());
    }

    #[test]
    fn test_resolve_absolute_path_unstrict() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let root = WorkspaceRoot::new(dir.path()).expect("create workspace root");
        let resolved = root.resolve(std::path::Path::new("/etc/passwd"));
        assert!(resolved.to_string_lossy().contains("etc/passwd"));
        assert!(!resolved.to_string_lossy().starts_with("/etc/passwd"));
    }

    #[test]
    fn test_resolve_warning_logs() {
        let _ = tracing_subscriber::fmt::try_init();
        let dir = tempfile::tempdir().expect("create tempdir");
        let root = WorkspaceRoot::new(dir.path()).expect("create workspace root");
        let _resolved = root.resolve(std::path::Path::new("../../etc/passwd"));
    }

    #[test]
    fn test_degraded_reason_all_variants() {
        use super::DegradedReason;

        let variants = [
            DegradedReason::NoLsp,
            DegradedReason::LspWarmupEmptyUnverified,
            DegradedReason::LspWarmupGrepFallback,
            DegradedReason::LspTimeoutGrepFallback,
            DegradedReason::LspErrorGrepFallback,
            DegradedReason::NoLspGrepFallback,
            DegradedReason::GrepFallbackFileScoped,
            DegradedReason::GrepFallbackImplScoped,
            DegradedReason::GrepFallbackGlobal,
            DegradedReason::GrepFallbackDependencies,
            DegradedReason::UnsupportedLanguageFilterBypassed,
            DegradedReason::UnsupportedLanguage,
            DegradedReason::GitError,
        ];

        for &variant in &variants {
            // Verify Display impl (line 494)
            let s = variant.to_string();
            assert!(!s.is_empty());

            // Verify guidance logic
            let guidance = variant.guidance();
            assert_eq!(
                guidance.retry_recommended,
                guidance.retry_after_seconds.is_some(),
                "retry_recommended must match the presence of retry_after_seconds for variant {variant:?}"
            );
        }
    }

    // ── FallbackTool serde roundtrip ───────────────────────────────────────

    #[test]
    fn test_fallback_tool_serde_roundtrip() {
        let variants = [
            (FallbackTool::Search, "\"search\""),
            (FallbackTool::Read, "\"read\""),
        ];
        for (variant, expected_json) in variants {
            let serialized =
                serde_json::to_string(&variant).expect("FallbackTool should serialize");
            assert_eq!(serialized, expected_json);
            let deserialized: FallbackTool =
                serde_json::from_str(&serialized).expect("FallbackTool should deserialize");
            assert_eq!(deserialized, variant);
        }
    }

    // ── TrustLevel serde roundtrip ────────────────────────────────────────

    #[test]
    fn test_trust_level_serde_roundtrip() {
        let variants = [
            (TrustLevel::Unreliable, "\"unreliable\""),
            (TrustLevel::Heuristic, "\"heuristic\""),
            (TrustLevel::Partial, "\"partial\""),
            (TrustLevel::None, "\"none\""),
        ];
        for (variant, expected_json) in variants {
            let serialized = serde_json::to_string(&variant).expect("TrustLevel should serialize");
            assert_eq!(serialized, expected_json);
            let deserialized: TrustLevel =
                serde_json::from_str(&serialized).expect("TrustLevel should deserialize");
            assert_eq!(deserialized, variant);
        }
    }

    // ── ActionableGuidance serde roundtrip ─────────────────────────────────

    #[test]
    fn test_actionable_guidance_serde_roundtrip() {
        // Case 1: all Option fields populated
        let guidance = ActionableGuidance {
            retry_recommended: true,
            retry_after_seconds: Some(30),
            fallback_tool: Some(FallbackTool::Search),
            trust_level: TrustLevel::Heuristic,
            permanent: false,
        };
        let serialized =
            serde_json::to_string(&guidance).expect("ActionableGuidance should serialize");
        let deserialized: ActionableGuidance =
            serde_json::from_str(&serialized).expect("ActionableGuidance should deserialize");
        assert_eq!(deserialized, guidance);

        // Case 2: fallback_tool=None — must NOT appear as "null" in JSON
        // (validates skip_serializing_if = "Option::is_none" is present)
        let no_fallback = DegradedReason::GitError.guidance(); // fallback_tool: None, retry_after_seconds: Some(5)
        let serialized_no_fallback =
            serde_json::to_string(&no_fallback).expect("ActionableGuidance should serialize");
        assert!(
            !serialized_no_fallback.contains("\"fallback_tool\":null"),
            "fallback_tool=None should be omitted, not serialized as null. Got: {serialized_no_fallback}"
        );
        let deserialized_no_fallback: ActionableGuidance =
            serde_json::from_str(&serialized_no_fallback)
                .expect("ActionableGuidance should deserialize");
        assert_eq!(deserialized_no_fallback, no_fallback);

        // Case 3: retry_after_seconds=None — must NOT appear as "null" in JSON
        let no_retry = DegradedReason::NoLsp.guidance(); // retry_after_seconds: None, fallback_tool: None
        let serialized_no_retry =
            serde_json::to_string(&no_retry).expect("ActionableGuidance should serialize");
        assert!(
            !serialized_no_retry.contains("\"retry_after_seconds\":null"),
            "retry_after_seconds=None should be omitted, not serialized as null. Got: {serialized_no_retry}"
        );
        assert!(
            !serialized_no_retry.contains("\"fallback_tool\":null"),
            "fallback_tool=None should be omitted, not serialized as null. Got: {serialized_no_retry}"
        );
        let deserialized_no_retry: ActionableGuidance = serde_json::from_str(&serialized_no_retry)
            .expect("ActionableGuidance should deserialize");
        assert_eq!(deserialized_no_retry, no_retry);
    }

    // ── guidance() field-level tests for uncovered arms ────────────────────

    #[test]
    fn test_guidance_lsp_warmup_grep_fallback() {
        let g = DegradedReason::LspWarmupGrepFallback.guidance();
        assert_eq!(g.fallback_tool, Some(FallbackTool::Search));
        assert_eq!(g.trust_level, TrustLevel::Heuristic);
        assert!(!g.permanent);
        assert!(g.retry_recommended);
    }

    #[test]
    fn test_guidance_lsp_timeout_grep_fallback() {
        let g = DegradedReason::LspTimeoutGrepFallback.guidance();
        assert_eq!(g.fallback_tool, Some(FallbackTool::Search));
        assert_eq!(g.trust_level, TrustLevel::Heuristic);
        assert!(!g.permanent);
        assert!(g.retry_recommended);
    }

    #[test]
    fn test_guidance_unsupported_language_filter_bypassed() {
        let g = DegradedReason::UnsupportedLanguageFilterBypassed.guidance();
        assert_eq!(g.fallback_tool, Some(FallbackTool::Read));
        assert_eq!(g.trust_level, TrustLevel::Partial);
        assert!(g.permanent);
    }

    #[test]
    fn test_guidance_unsupported_language() {
        let g = DegradedReason::UnsupportedLanguage.guidance();
        assert_eq!(g.fallback_tool, Some(FallbackTool::Read));
        assert_eq!(g.trust_level, TrustLevel::None);
        assert!(g.permanent);
    }

    #[test]
    fn test_guidance_git_error() {
        let g = DegradedReason::GitError.guidance();
        assert!(g.retry_recommended);
        assert_eq!(g.retry_after_seconds, Some(5));
        assert_eq!(g.fallback_tool, None);
        assert!(!g.permanent);
    }
}
