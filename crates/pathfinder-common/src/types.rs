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
/// These are never source code and would cause false positives in grep fallback.
/// Used by the search walker (`filter_entry` in `ripgrep.rs`) to skip files
/// whose relative path starts with any of these prefixes.
/// Each entry has a trailing `/` for prefix-based path matching.
pub const ALWAYS_EXCLUDED_DIRS: &[&str] = &[
    ".git/",
    "node_modules/",
    "target/",
    "vendor/",
    ".idea/",
    ".vscode/",
    "__pycache__/",
    ".qlty/",
    // Build output and cache directories — never source code
    "dist/",
    "build/",
    ".next/",
    ".turbo/",
    ".gradle/",
    "coverage/",
    ".nyc_output/",
    ".mypy_cache/",
];

/// Bare directory names (without trailing `/`) for walker-level pruning.
///
/// These correspond exactly to [`ALWAYS_EXCLUDED_DIRS`] but without the
/// trailing separator, for use with `WalkBuilder::filter_entry` which
/// receives individual directory entries by name.
///
/// Kept as a separate constant so the walker can do a fast `entry.file_name()`
/// comparison without stripping suffixes at runtime.
pub const ALWAYS_EXCLUDED_DIR_NAMES: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "vendor",
    ".idea",
    ".vscode",
    "__pycache__",
    ".qlty",
    // Build output and cache directories — matching ALWAYS_EXCLUDED_DIRS
    "dist",
    "build",
    ".next",
    ".turbo",
    ".gradle",
    "coverage",
    ".nyc_output",
    ".mypy_cache",
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
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
    /// The parent symbol's kind (e.g., "interface", "class", "struct", "module").
    ///
    /// Used to detect trait/interface methods so we can call `goto_implementation`
    /// during trace to expand to all concrete implementations.
    ///
    /// `None` for top-level symbols.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub parent_kind: Option<String>,
    /// The parent symbol's name (e.g., the trait/interface name).
    ///
    /// `None` for top-level symbols.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub parent_name: Option<String>,
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
    /// Matches in comments AND string literals (non-code content).
    #[serde(alias = "non_code")]
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
#[path = "types_test.rs"]
mod tests;
