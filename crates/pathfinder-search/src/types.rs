//! Types for the search engine — `SearchParams`, `SearchMatch`, `SearchResult`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Parameters for a `search_codebase` call.
#[derive(Debug, Clone)]
pub struct SearchParams {
    /// Absolute path to the workspace root.
    pub workspace_root: PathBuf,
    /// Search pattern (literal text or regex).
    pub query: String,
    /// When `true`, treat `query` as a regex; otherwise literal text.
    pub is_regex: bool,
    /// Glob pattern restricting which files are searched (e.g. `src/**/*.ts`).
    /// Matches against paths relative to the workspace root.
    pub path_glob: String,
    /// Glob pattern for files to *exclude* from search (e.g. `**/*.test.*`).
    /// Applied before search — not as a post-filter. Empty string = no exclusion.
    pub exclude_glob: String,
    /// Maximum number of matches to return.
    pub max_results: usize,
    /// Lines of surrounding context to include above and below each match.
    pub context_lines: usize,
}

impl Default for SearchParams {
    fn default() -> Self {
        Self {
            workspace_root: PathBuf::from("."),
            query: String::new(),
            is_regex: false,
            path_glob: "**/*".to_owned(),
            exclude_glob: String::new(),
            max_results: 50,
            context_lines: 2,
        }
    }
}

/// A single match returned by `search_codebase`.
///
/// Matches the JSON schema described in PRD §3.1.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct SearchMatch {
    /// Path to the matching file, relative to the workspace root.
    pub file: String,
    /// 1-indexed line number of the match.
    pub line: u64,
    /// 1-indexed column number of the match start.
    pub column: u64,
    /// The full content of the matching line (without newline).
    ///
    /// Empty string when the file is listed in `known_files` (content suppressed).
    pub content: String,
    /// Lines immediately before the match (up to `context_lines`).
    ///
    /// Empty when the file is listed in `known_files`.
    pub context_before: Vec<String>,
    /// Lines immediately after the match (up to `context_lines`).
    ///
    /// Empty when the file is listed in `known_files`.
    pub context_after: Vec<String>,
    /// AST-derived semantic path enclosing this match.
    ///
    /// Always `null` in Epic 2; populated by Tree-sitter in Epic 3.
    pub enclosing_semantic_path: Option<String>,
    /// SHA-256 hash of the matched file, for OCC chaining.
    pub version_hash: String,
    /// `true` when this file was listed in `known_files`.
    ///
    /// When set, `content`, `context_before`, and `context_after` are empty.
    /// Omitted from the serialised output for normal (unknown) matches.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub known: Option<bool>,
}

/// The result of a `search_codebase` call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// All matches found, up to `max_results`.
    pub matches: Vec<SearchMatch>,
    /// Total number of matches found before the `max_results` cap.
    pub total_matches: usize,
    /// `true` if results were capped at `max_results`.
    pub truncated: bool,
}
