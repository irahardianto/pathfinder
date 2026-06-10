//! Tool parameter and response types for Pathfinder MCP tools.
//!
//! These structs are deserialized by the rmcp framework from MCP tool call
//! payloads. The `dead_code` lint fires for param struct fields that are read
//! by serde (via `Deserialize`) but never accessed by name in production code.
//! A module-level `#![allow]` is used here so that each newly implemented tool
//! can remove its struct's allow without touching unrelated items.
#![allow(dead_code)] // Fields are read by serde deserialization, not by name

use pathfinder_common::types::{ActionableGuidance, DegradedReason};
use rmcp::schemars;
use rmcp::serde::{self, Deserialize, Serialize};

// ── Tool Parameter Types ────────────────────────────────────────────

/// Parameters for `search_codebase`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct SearchCodebaseParams {
    /// Search pattern (literal or regex).
    pub query: String,
    /// Treat query as regex.
    #[serde(default)]
    pub is_regex: bool,
    /// Limit search scope (e.g., `src/**/*.ts`).
    #[serde(default = "default_path_glob")]
    pub path_glob: String,
    /// Filter mode: `code_only`, `comments_only`, or `all`.
    ///
    /// Uses Tree-sitter node classification to filter matches by context.
    /// Defaults to `code_only` (exclude comments and string literals).
    #[serde(default)]
    pub filter_mode: pathfinder_common::types::FilterMode,
    /// Maximum matches returned.
    #[serde(default = "default_max_results")]
    pub max_results: u32,
    /// Lines of context above/below each match.
    #[serde(default = "default_context_lines")]
    pub context_lines: u32,
    /// File paths already in the agent's context.
    ///
    /// For matches in these files, only minimal metadata is returned
    /// (`file`, `line`, `column`, `enclosing_semantic_path`, `version_hash`).
    /// The full `content` and context lines are omitted to save tokens.
    #[serde(default)]
    pub known_files: Vec<String>,
    /// Group matches by file in the response.
    ///
    /// When `true`, the response includes `file_groups` instead of (or in addition to)
    /// the flat `matches` list. Each group contains all matches for one file with a
    /// single `version_hash` at group level.
    #[serde(default)]
    pub group_by_file: bool,
    /// Glob pattern for files to exclude from search (e.g., `**/*.test.*`).
    ///
    /// Applied before search — not as a post-filter — so excluded files are
    /// never read. Can be combined with `path_glob` include patterns.
    #[serde(default)]
    pub exclude_glob: String,
    /// Number of matches to skip before returning results (for pagination).
    /// Use with `max_results` to page through large result sets.
    #[serde(default)]
    pub offset: u32,
}

/// Parameters for `get_repo_map`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct GetRepoMapParams {
    /// Directory to map.
    #[serde(default = "default_repo_map_path")]
    pub path: String,
    /// Total token budget for the entire skeleton output. Default: 16000.
    ///
    /// When `coverage_percent` in the response is low, increase this value
    /// to include more files in the map.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Max directory traversal depth (default: 5).
    ///
    /// Increase this value when `coverage_percent` in the response is low
    /// or when your project has deeply-nested source files (e.g. a depth 6+
    /// monorepo). The walker stops early on shallow repos, so over-provisioning
    /// is safe and nearly free.
    #[serde(default = "default_depth")]
    pub depth: u32,
    /// Visibility filter: `public` or `all`.
    #[serde(default)]
    pub visibility: pathfinder_common::types::Visibility,
    /// Per-file token cap before a file skeleton is collapsed to a summary stub.
    ///
    /// When the rendered skeleton of an individual file exceeds this limit, the
    /// file is replaced with a truncated stub showing only class/struct names and
    /// method counts. Increase this value when files show `[TRUNCATED DUE TO SIZE]`
    /// in the output. Default: 2000.
    #[serde(default = "default_max_tokens_per_file")]
    pub max_tokens_per_file: u32,
    /// Git ref or duration to show only recently modified files (e.g., `HEAD~5`, `3h`, `2024-01-01`).
    #[serde(default)]
    pub changed_since: String,
    /// Only include files with these extensions. Mutually exclusive with `exclude_extensions`.
    #[serde(default)]
    pub include_extensions: Vec<String>,
    /// Exclude files with these extensions. Mutually exclusive with `include_extensions`.
    #[serde(default)]
    pub exclude_extensions: Vec<String>,
    /// Include test functions and test modules regardless of visibility filter.
    ///
    /// When `true` (default), symbols inside `mod tests {}` blocks and functions with
    /// test attributes are always included, even with `visibility="public"`.
    /// When `false`, visibility rules are strictly applied.
    #[serde(default = "default_true")]
    pub include_tests: bool,
}

/// Parameters for `symbol_overview`.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SymbolOverviewParams {
    /// Semantic path (e.g., `src/auth.ts::AuthService.login`).
    /// MUST include file path and '::'.
    pub semantic_path: String,
    /// Filter dependencies to workspace/project files only.
    #[serde(default)]
    pub project_only: Option<bool>,
    /// Maximum number of callers/callees to return per direction.
    #[serde(default = "default_max_references")]
    pub max_callers_callees: u32,
    /// Maximum number of references to return.
    #[serde(default = "default_max_references")]
    pub max_references: u32,
}

/// Response for `symbol_overview`.
#[derive(Debug, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SymbolOverviewResponse {
    /// Source code and location of the symbol.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<SymbolSource>,
    /// Impact analysis (incoming callers + outgoing callees).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub impact: Option<ImpactSummary>,
    /// Reference locations across the codebase.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub references: Option<Vec<SymbolOverviewReference>>,
    /// Total number of files containing references.
    pub files_referenced: usize,
    /// Whether any component was degraded.
    pub degraded: bool,
    /// Whether the impact analysis (callers/callees) was degraded.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub impact_degraded: bool,
    /// Whether the references lookup was degraded.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub references_degraded: bool,
    /// Reason for degradation, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<DegradedReason>,
    /// Machine-readable guidance when `degraded` is `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actionable_guidance: Option<ActionableGuidance>,
    /// LSP readiness at the time of the call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lsp_readiness: Option<String>,
    /// Whether warm start is in progress (set on timeout only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warm_start_in_progress: Option<bool>,
}

/// Source code block for `symbol_overview`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SymbolSource {
    /// Content of the symbol (source code).
    pub content: String,
    /// Starting line number (1-indexed).
    pub start_line: usize,
    /// Ending line number (1-indexed).
    pub end_line: usize,
    /// Programming language.
    pub language: String,
}

/// Impact summary for `symbol_overview`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ImpactSummary {
    /// Direct callers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incoming: Option<Vec<SymbolOverviewImpactEntry>>,
    /// Direct callees.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outgoing: Option<Vec<SymbolOverviewImpactEntry>>,
    /// Whether the impact analysis was degraded.
    pub degraded: bool,
}

/// A single entry in the impact summary.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SymbolOverviewImpactEntry {
    pub semantic_path: String,
    pub file: String,
    pub line: usize,
    pub snippet: String,
    pub direction: String,
}

/// A reference location in `symbol_overview`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SymbolOverviewReference {
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub snippet: String,
}

/// Parameters for `read_symbol_scope`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct ReadSymbolScopeParams {
    /// Semantic path (e.g., `src/auth.ts::AuthService.login`).
    pub semantic_path: String,
}

/// Parameters for `read_with_deep_context`.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ReadWithDeepContextParams {
    /// Semantic path (e.g., `src/auth.ts::AuthService.login`). MUST include file path and '::'.
    pub semantic_path: String,
    /// Filter dependencies to workspace/project files only.
    ///
    /// When `true` (default), excludes stdlib and external library dependencies
    /// (e.g., `Vec::push`, `String::clone` from Rust stdlib, or npm packages).
    /// When `false`, includes all references including stdlib/external.
    #[serde(default)]
    pub project_only: Option<bool>,
    /// Maximum number of dependencies (callee signatures) to return.
    /// Prevents context overflow on large functions. Default: 50.
    #[serde(default = "default_max_dependencies")]
    pub max_dependencies: u32,
}

impl Default for ReadWithDeepContextParams {
    fn default() -> Self {
        Self {
            semantic_path: String::default(),
            project_only: Some(true),
            max_dependencies: default_max_dependencies(),
        }
    }
}

/// Parameters for `get_definition`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct GetDefinitionParams {
    /// Semantic path to the reference (e.g., `src/auth.ts::AuthService.login`).
    pub semantic_path: String,
}

/// Parameters for `find_callers_callees` (formerly `analyze_impact`).
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FindCallersCalleesParams {
    /// Semantic path to the target (e.g., `src/mod.rs::func`).
    pub semantic_path: String,
    /// Traversal depth (max: 5).
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
    /// Filter references to workspace/project files only.
    ///
    /// When `true` (default), excludes stdlib and external library references
    /// (e.g., `Vec::push`, `String::clone` from Rust stdlib, or npm packages).
    /// When `false`, includes all references including stdlib/external.
    #[serde(default)]
    pub project_only: Option<bool>,
    /// Maximum total references (incoming + outgoing) to return.
    /// Prevents context overflow on large codebases. Default: 50.
    #[serde(default = "default_max_references")]
    pub max_references: u32,
    /// When `true`, also search for test functions that cover this symbol.
    /// Returns test references in a separate `test_callers` field.
    /// Default: `false`.
    #[serde(default)]
    pub include_test_coverage: bool,
}

impl Default for FindCallersCalleesParams {
    fn default() -> Self {
        Self {
            semantic_path: String::default(),
            max_depth: default_max_depth(),
            project_only: Some(true),
            max_references: default_max_references(),
            include_test_coverage: false,
        }
    }
}

/// Parameters for `read_file`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct ReadFileParams {
    /// Relative file path.
    #[serde(alias = "path")]
    pub filepath: String,
    /// First line to return (1-indexed).
    #[serde(default = "default_start_line")]
    pub start_line: u32,
    /// Maximum lines to return.
    #[serde(default = "default_max_lines")]
    pub max_lines: u32,
}

/// Parameters for `read_source_file`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct ReadSourceFileParams {
    /// Relative file path.
    #[serde(alias = "path")]
    pub filepath: String,
    /// Detail level: `"source_only"`, `"compact"`, `"symbols"`, or `"full"`.
    /// - `"source_only"` — source code only, no symbol metadata (lowest token cost)
    /// - `"compact"` (default) — source + flat symbol list
    /// - `"symbols"` — symbol tree only, no source
    /// - `"full"` — source + nested symbol tree
    #[serde(default = "default_detail_level")]
    pub detail_level: String,
    /// First line to return (1-indexed).
    #[serde(default = "default_start_line")]
    pub start_line: u32,
    /// Last line to return (1-indexed, inclusive).
    #[serde(default)]
    pub end_line: Option<u32>,
}

// ── Response Types ──────────────────────────────────────────────────

/// The response for `search_codebase`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SearchCodebaseResponse {
    /// List of search matches.
    pub matches: Vec<pathfinder_search::SearchMatch>,
    /// Raw match count from ripgrep **before** `filter_mode` filtering, **after** ripgrep truncation.
    ///
    /// When `truncated = true`, this equals `max_results` and ripgrep stopped searching early.
    /// When `filter_mode` is `"comments_only"` or `"code_only"`, matches that do not
    /// pass the filter are excluded from `matches` but still counted here.
    /// Compare with `total_matches` to see how many matches were removed by filtering.
    pub raw_match_count: usize,
    /// Total matches in this response (after `filter_mode` filtering).
    ///
    /// This always equals `matches.len()` and `returned_count`. Provided for consistency
    /// with agent expectations: "total" means what you actually get, not ripgrep's pre-filter count.
    /// Use `raw_match_count` to see ripgrep's count before filtering.
    /// Use `filtered_count` to see how many matches were removed by `filter_mode`.
    pub total_matches: usize,
    /// Number of matches actually returned in this response (after `filter_mode` filtering).
    ///
    /// `returned_count == total_matches == matches.len()`. Provided as a convenience field
    /// and for backward compatibility.
    pub returned_count: usize,
    /// Number of matches removed by `filter_mode` filtering.
    ///
    /// `filtered_count = raw_match_count - total_matches`.
    /// When `filter_mode = "All"`, this is always 0.
    pub filtered_count: usize,
    /// Number of files that were actually searched.
    pub files_searched: usize,
    /// Number of files matching the `path_glob` that were in scope for search.
    /// When `files_searched < files_in_scope`, some files were skipped
    /// (binary, .gitignored, or permission-denied).
    pub files_in_scope: usize,
    /// Percentage of in-scope files that were actually searched.
    /// 100% means exhaustive search; lower values indicate skipped files.
    pub coverage_percent: u8,
    /// Indicates if the match list was truncated by `max_results`.
    pub truncated: bool,
    /// Grouped output — populated when `group_by_file: true`.
    ///
    /// Each group represents one file and contains either full matches (for
    /// unknown files) or minimal matches (for files in `known_files`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_groups: Option<Vec<SearchResultGroup>>,
    /// Whether the search response is degraded.
    #[serde(default)]
    pub degraded: bool,
    /// Reason for degradation, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<DegradedReason>,
    /// When results are truncated, this field provides the `offset` value
    /// to use for the next page of results. Absent when not truncated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<u32>,
    /// Actionable hint when `filter_mode` removes all results.
    /// Suggests retrying with `filter_mode=all` when matches exist but were filtered out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Machine-readable guidance when `degraded` is `true`.
    /// Tells the agent whether to retry, what fallback tool to use, and whether
    /// results are trustworthy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actionable_guidance: Option<ActionableGuidance>,
    /// Wall-clock time in milliseconds that this tool call took to complete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Files skipped because they matched known binary extensions.
    pub binary_skipped: usize,
    /// Files skipped because they were excluded by `.gitignore` rules.
    pub gitignored_skipped: usize,
    /// Files skipped for other reasons (permission denied, I/O error, etc.).
    pub other_skipped: usize,
}

/// A minimal match entry for files already in the agent's context (`known_files`)
/// when grouped by file.
///
/// Omits `file`, `version_hash` (deduplicated at group level), and `content`,
/// `context_before`, and `context_after` to save tokens.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct GroupedKnownMatch {
    /// 1-indexed line number.
    pub line: u64,
    /// 1-indexed column number.
    pub column: u64,
    /// AST symbol enclosing this match (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enclosing_semantic_path: Option<String>,
    /// Whether this match is at a definition position (fn, struct, class, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_definition: Option<bool>,
    /// Always `true` — signals this match was suppressed because the file is known.
    pub known: bool,
}

/// A group of matches belonging to one file, returned when `group_by_file: true`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SearchResultGroup {
    /// File path relative to workspace root.
    pub file: String,
    /// SHA-256 hash of the file (shared by all matches in this group).
    pub version_hash: String,
    /// Total number of matches in this group (both full and known).
    ///
    /// Provided so agents can quickly assess match density without counting sub-arrays.
    /// Always present regardless of whether `matches` or `known_matches` are serialized.
    pub total_matches: usize,
    /// Full matches (for files NOT in `known_files`).
    ///
    /// Per-match objects contain only `{ line, column, content, context_before,
    /// context_after, enclosing_semantic_path }` — `file` and `version_hash` are
    /// deduplicated at group level to avoid repeating them for every match.
    ///
    /// Absent (not just empty) when all matches in this group are for known files.
    /// Check `total_matches` for the match count regardless of which array is populated.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[schemars(skip)]
    pub matches: Vec<GroupedMatch>,
    /// Minimal matches (for files in `known_files`).
    ///
    /// Absent when no matches in this group are for known files.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[schemars(skip)]
    pub known_matches: Vec<GroupedKnownMatch>,
}
/// A single match within a `SearchResultGroup`.
///
/// Omits `file` and `version_hash` (deduplicated at group level) to reduce
/// token usage when many matches belong to the same file.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct GroupedMatch {
    /// 1-indexed line number of the match.
    pub line: u64,
    /// 1-indexed column number of the match start.
    pub column: u64,
    /// The full content of the matching line.
    pub content: String,
    /// Lines immediately before the match.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub context_before: Vec<String>,
    /// Lines immediately after the match.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub context_after: Vec<String>,
    /// AST symbol enclosing this match (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enclosing_semantic_path: Option<String>,
    /// Whether this match is at a definition position (fn, struct, class, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_definition: Option<bool>,
}

/// The metadata embedded in `structured_content` for `get_repo_map`.
#[derive(Debug, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct GetRepoMapMetadata {
    /// Technology stack of the repository.
    pub tech_stack: Vec<String>,
    /// Number of files scanned.
    pub files_scanned: usize,
    /// Number of files truncated.
    pub files_truncated: usize,
    /// File paths that were truncated due to token budget.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub truncated_paths: Vec<String>,
    /// Number of files within the configured scope.
    pub files_in_scope: usize,
    /// Percentage of files covered by the search.
    pub coverage_percent: u8,
    /// Map of file paths to their version hashes.
    pub version_hashes: std::collections::HashMap<String, String>,
    /// Always `true` while visibility filtering is not yet implemented.
    /// Agents should treat all symbols as public regardless of `visibility` param.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility_degraded: Option<bool>,
    /// `true` when filtering by `changed_since` fails (e.g., git is unavailable).
    #[serde(default)]
    pub degraded: bool,
    /// Reason for degradation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<DegradedReason>,
    /// Machine-readable guidance when `degraded` is `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actionable_guidance: Option<ActionableGuidance>,
    /// System capabilities available for this repository.
    pub capabilities: RepoCapabilities,
    /// Actual `max_tokens` used (may differ from requested due to auto-scaling).
    pub max_tokens_used: u32,
    /// Flat map of language ID to status string (`"ready"`, `"warming_up"`, `"unavailable"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lsp_status: Option<std::collections::HashMap<String, String>>,
    /// Wall-clock time in milliseconds that this tool call took to complete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// The overall capabilities of the Pathfinder system.
#[derive(Debug, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct RepoCapabilities {
    /// Whether the search engine is supported.
    pub search: bool,
    /// LSP-specific capabilities and status.
    pub lsp: LspCapabilities,
}

/// LSP status and capabilities.
#[derive(Debug, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct LspCapabilities {
    /// `true` if LSP is generally supported by the system.
    pub supported: bool,
    /// Map of language ID to its specific LSP process status.
    pub per_language: std::collections::HashMap<String, pathfinder_lsp::types::LspLanguageStatus>,
}

/// The metadata embedded in `structured_content` for `read_symbol_scope`.
#[derive(Debug, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ReadSymbolScopeMetadata {
    /// The extracted symbol source code.
    ///
    /// Mirrors `content[0].text` in the MCP response. Provided here so that
    /// agents consuming `structured_content` directly have the full source
    /// without needing to inspect the main content array.
    pub content: String,
    /// Starting line number of the symbol in the source.
    pub start_line: usize,
    /// Ending line number of the symbol in the source.
    pub end_line: usize,
    /// Programming language of the source symbol.
    pub language: String,
    /// Wall-clock time in milliseconds that this tool call took to complete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// A symbol output for `read_source_file`.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SourceSymbol {
    /// Name of the symbol.
    pub name: String,
    /// Semantic path of the symbol.
    pub semantic_path: String,
    /// Kind of the symbol (e.g., function, struct).
    pub kind: String,
    /// Starting line number of the symbol in the source.
    pub start_line: usize,
    /// Ending line number of the symbol in the source.
    pub end_line: usize,
    /// Child symbols nested within this symbol.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Self>,
}

/// The metadata embedded in `structured_content` for `read_source_file`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ReadSourceFileMetadata {
    /// Programming language of the source file.
    pub language: String,
    /// Clean source content without timing metadata appended.
    /// Provided so consumers like `read_files` get uncontaminated content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Symbols extracted from the source file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<SourceSymbol>,
    /// Wall-clock time in milliseconds that this tool call took to complete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Whether this file's language is not supported for AST parsing.
    /// When true, content is raw file content and symbols is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsupported_language: Option<bool>,
}

/// The metadata embedded in `structured_content` for `read_file`.
#[derive(Debug, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ReadFileMetadata {
    /// First line number returned from the file.
    pub start_line: u32,
    /// Number of lines returned.
    pub lines_returned: u32,
    /// Total number of lines in the file.
    pub total_lines: u32,
    /// Total size of the file in bytes.
    pub file_size_bytes: u64,
    /// Whether the output was truncated.
    pub truncated: bool,
    /// Detected language of the file.
    pub language: String,
    /// Wall-clock time in milliseconds that this tool call took to complete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

// ── Navigation Tool Response Types ─────────────────────────────────

/// A dependency signature extracted for `read_with_deep_context`.
#[derive(Debug, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct DeepContextDependency {
    /// Semantic path of the called symbol.
    pub semantic_path: String,
    /// Extracted signature (declaration line only, no body).
    pub signature: String,
    /// File path relative to workspace root.
    pub file: String,
    /// 1-indexed line of the definition.
    pub line: usize,
}

/// The metadata embedded in `structured_content` for `read_with_deep_context`.
#[derive(Debug, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ReadWithDeepContextMetadata {
    /// Start line of the symbol (1-indexed).
    pub start_line: usize,
    /// End line of the symbol (1-indexed).
    pub end_line: usize,
    /// Detected language.
    pub language: String,
    /// Signatures of all symbols called by this one.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub dependencies: Vec<DeepContextDependency>,
    /// `true` when LSP dependency resolution was unavailable (Tree-sitter only).
    #[serde(default)]
    pub degraded: bool,
    /// Reason for degradation (e.g., `"no_lsp"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<DegradedReason>,
    /// Machine-readable guidance when `degraded` is `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actionable_guidance: Option<ActionableGuidance>,
    /// IW-2: LSP readiness signal at the time of the call.
    ///
    /// - `"ready"`: LSP is fully operational — results are authoritative.
    /// - `"warming_up"`: LSP is still indexing — results may be partial.
    /// - `"unavailable"`: No LSP; Tree-sitter fallback used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lsp_readiness: Option<String>,
    /// Whether the LSP warm-start is still in progress at the time of the call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warm_start_in_progress: Option<bool>,
    /// `true` when the `max_dependencies` limit was reached and results were truncated.
    pub dependencies_truncated: bool,
    /// Spec 5.2: How the deep context was resolved.
    /// One of: `lsp_call_hierarchy`, `grep_file_scoped`, `grep_impl_scoped`, `grep_global`, `grep_broad`, `treesitter_direct`, `treesitter_fallback`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_strategy: Option<String>,
    /// Spec 5.1: Wall-clock duration of the tool call in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// The response for `get_definition`.
#[derive(Debug, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct GetDefinitionResponse {
    /// Relative file path of the definition site.
    pub file: String,
    /// 1-indexed line number of the definition.
    pub line: u32,
    /// 1-indexed column number.
    pub column: u32,
    /// First line of the definition (code preview).
    pub preview: String,
    /// `true` when LSP was unavailable and no fallback was possible.
    #[serde(default)]
    pub degraded: bool,
    /// Reason for degradation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<DegradedReason>,
    /// Machine-readable guidance when `degraded` is `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actionable_guidance: Option<ActionableGuidance>,
    /// IW-2: LSP readiness at the time of the call.
    ///
    /// - `"ready"`: LSP operational — definition is authoritative.
    /// - `"warming_up"`: LSP still indexing — result may be from Tree-sitter fallback.
    /// - `"unavailable"`: No LSP; result is from ripgrep heuristics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lsp_readiness: Option<String>,
    /// Whether the LSP warm-start is still in progress at the time of the call.
    /// When `true`, retrying after 15-30s may yield better results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warm_start_in_progress: Option<bool>,
    /// Spec 5.1: Wall-clock duration of the tool call in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Spec 5.2: How the definition was resolved.
    /// One of: `lsp`, `lsp_retry`, `grep_file`, `grep_impl`, `grep_global`, `grep_broad`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_strategy: Option<String>,
}

/// A single reference in an impact analysis.
#[derive(Debug, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ImpactReference {
    /// Semantic path of the referencing/referenced symbol.
    pub semantic_path: String,
    /// File path relative to workspace root.
    pub file: String,
    /// 1-indexed line of the call site or definition.
    pub line: usize,
    /// A short code snippet showing the call site or declaration.
    pub snippet: String,
    /// Direction of the reference relative to the target symbol.
    ///
    /// - `"incoming"` — this symbol calls or references the target (a caller).
    /// - `"outgoing"` — the target calls or references this symbol (a callee).
    /// - `"incoming_heuristic"` — inferred by grep fallback when LSP is unavailable;
    ///   treat as a candidate, not a confirmed call.
    pub direction: String,
    /// BFS traversal depth (0 = direct caller/callee, 1 = one hop away, etc.).
    pub depth: usize,
    /// Confidence level of this reference.
    ///
    /// - `"lsp"` — confirmed by LSP call hierarchy (authoritative).
    /// - `"heuristic"` — inferred by grep or AST fallback when LSP is unavailable or degraded.
    ///   Treat as a candidate; may include false positives from dynamic dispatch or
    ///   same-named symbols in different scopes.
    ///
    /// `null` (absent) when the confidence is unknown or the caller pre-dates this field.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub confidence: Option<String>,
}

/// The metadata embedded in `structured_content` for `find_callers_callees`.
#[derive(Debug, Default, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct FindCallersCalleesMetadata {
    /// Symbols that call the target (caller graph).
    /// `null` when `degraded` is `true` — LSP was unavailable so callers are **unknown**.
    /// An empty array `[]` means LSP confirmed zero callers.
    pub incoming: Option<Vec<ImpactReference>>,
    /// Symbols the target calls (callee graph).
    /// `null` when `degraded` is `true` — LSP was unavailable so callees are **unknown**.
    /// An empty array `[]` means LSP confirmed zero callees.
    pub outgoing: Option<Vec<ImpactReference>>,
    /// Number of transitive levels traversed.
    pub depth_reached: u32,
    /// Total files referenced across all incoming and outgoing references.
    pub files_referenced: usize,
    /// Whether the call hierarchy analysis was degraded (LSP unavailable or crashed).
    /// Always present. When `true`, `incoming` and `outgoing` are `null` (not empty arrays).
    pub degraded: bool,
    /// Machine-readable reason for degradation (e.g., `no_lsp`, `lsp_crash`, `lsp_timeout`).
    /// Absent when `degraded` is `false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<DegradedReason>,
    /// Machine-readable guidance when `degraded` is `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actionable_guidance: Option<ActionableGuidance>,
    /// LSP readiness at the time of the call.
    ///
    /// - `"ready"`: LSP is fully operational — results are authoritative.
    /// - `"warming_up"`: LSP is still indexing — results may be partial.
    /// - `"unavailable"`: No LSP; results degraded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lsp_readiness: Option<String>,
    /// Whether the LSP warm-start is still in progress at the time of the call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warm_start_in_progress: Option<bool>,
    /// `true` when the `max_references` limit was reached and results were truncated.
    pub references_truncated: bool,
    /// Spec 5.1: Wall-clock duration of the tool call in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Spec 5.2: How the call hierarchy was resolved.
    /// One of: `lsp_call_hierarchy`, `grep_file_scoped`, `grep_impl_scoped`, `grep_global`, `grep_broad`, `treesitter_fallback`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_strategy: Option<String>,
    /// Spec 4.2: Test functions that reference or test this symbol.
    /// Populated when `include_test_coverage=true` was passed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_callers: Option<Vec<ImpactReference>>,
    /// Spec 4.2: Status of test coverage search.
    /// One of: `"found"`, `"not_found"`, `"unknown_degraded"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_coverage_status: Option<String>,
}

// ── Get Semantic Path Tool Types ────────────────────────────────────────

/// Parameters for `get_semantic_path`.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GetSemanticPathParams {
    /// Relative path to the file (e.g., `src/auth.ts`).
    #[serde(alias = "path")]
    pub file: String,
    /// 1-indexed line number to resolve.
    pub line: u32,
}

/// Result for `get_semantic_path`.
#[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct GetSemanticPathResult {
    /// The full semantic path (`file::symbol`) of the innermost enclosing symbol.
    ///
    /// `null` when the line is not inside any named symbol (e.g., it is a module-level
    /// attribute, blank line, or the file uses an unsupported language).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_path: Option<String>,
    /// The symbol portion only (without the file prefix).
    ///
    /// `null` when `semantic_path` is null.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// The file portion of the semantic path (same as the `file` parameter).
    pub file: String,
    /// The queried line number (1-indexed, echoed back for confirmation).
    pub line: u32,
}

// ── Find All References Tool Types ─────────────────────────────────────

/// Parameters for `find_all_references`.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FindAllReferencesParams {
    /// Semantic path to the target symbol (e.g., `src/mod.rs::func`).
    pub semantic_path: String,
    /// Maximum number of references to return. Default: 50.
    #[serde(default = "default_max_references")]
    pub max_results: u32,
    /// Number of results to skip (for pagination).
    #[serde(default)]
    pub offset: u32,
}

/// The metadata embedded in `structured_content` for `find_all_references`.
#[derive(Debug, Default, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct FindAllReferencesMetadata {
    /// All reference locations for the target symbol.
    /// Empty array `[]` means LSP confirmed zero references.
    /// `null` when `degraded` is `true` — LSP was unavailable.
    pub references: Option<Vec<ReferenceLocation>>,
    /// Total number of references found (before pagination).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_references: Option<usize>,
    /// Whether the results were truncated due to `max_results`.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub truncated: bool,
    /// Total number of files containing references.
    pub files_referenced: usize,
    /// Whether the reference lookup was degraded (LSP unavailable or crashed).
    pub degraded: bool,
    /// Machine-readable reason for degradation (e.g., `no_lsp`, `lsp_crash`, `lsp_timeout`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<DegradedReason>,
    /// Machine-readable guidance when `degraded` is `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actionable_guidance: Option<ActionableGuidance>,
    /// LSP readiness at the time of the call.
    ///
    /// - `"ready"`: LSP is fully operational — results are authoritative.
    /// - `"warming_up"`: LSP is still indexing — results may be partial.
    /// - `"unavailable"`: No LSP; results degraded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lsp_readiness: Option<String>,
    /// Whether the LSP warm-start is still in progress at the time of the call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warm_start_in_progress: Option<bool>,
    /// Wall-clock time in milliseconds that this tool call took to complete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Spec 5.2: How the references were resolved.
    /// One of: `lsp_references`, `grep_file_scoped`, `grep_impl_scoped`, `grep_global`, `grep_broad`, `treesitter_fallback`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_strategy: Option<String>,
}

/// A single reference location for `find_all_references`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReferenceLocation {
    /// File path relative to workspace root.
    pub file: String,
    /// 1-indexed line number where the reference occurs.
    pub line: u32,
    /// 1-indexed column number where the reference occurs.
    pub column: u32,
    /// A short code snippet showing the reference (e.g., function call or variable access).
    pub snippet: String,
}

// ── LSP Health Tool Types ────────────────────────────────────────────

/// Structured information about a degraded tool.
///
/// Provides detailed information about which tools are degraded and what
/// fallback behavior agents can expect. Replaces the old `Vec<String>`
/// format with machine-readable severity and human-readable descriptions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DegradedToolInfo {
    /// Tool name (e.g., `"find_callers_callees"`, `"get_definition"`, `"read_with_deep_context"`).
    pub tool: String,
    /// Severity of degradation:
    ///
    /// - `"unavailable"` — tool will error, use alternatives
    /// - `"grep_fallback"` — tool returns heuristic results, verify manually
    /// - `"warmup_pending"` — retry after indexing completes
    /// - `"partial"` — some features work (e.g., definition works but not call hierarchy)
    pub severity: String,
    /// Human-readable description of the fallback behavior and limitations.
    pub description: String,
}

/// Parameters for `lsp_health`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct LspHealthParams {
    /// Optional language to check (e.g., "rust", "typescript").
    /// If omitted, checks all available languages.
    #[serde(default)]
    pub language: Option<String>,
    /// IW-4: Optional action to perform.
    ///
    /// - `"restart"`: Force-restart the LSP process for the specified language.
    ///   `language` must be set when using `"restart"`.
    ///   Returns updated health status after the restart attempt.
    #[serde(default)]
    pub action: Option<String>,
}

/// The response for `lsp_health`.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LspHealthResponse {
    /// Overall LSP readiness: `"ready"`, `"warming_up"`, or `"unavailable"`.
    pub status: String,
    /// Per-language status details.
    pub languages: Vec<LspLanguageHealth>,
    /// PATCH-004: Whether `warm_start` has completed.
    ///
    /// When `true`, all `warm_start` background tasks have finished (even if
    /// some languages failed). When `false`, `warm_start` is still in progress.
    /// This allows distinguishing "still warming up" from "`warm_start` finished
    /// but LSP didn't report readiness".
    pub warm_start_complete: bool,
    /// Spec 1.3: Known limitations of the current LSP setup.
    ///
    /// Populated with actionable limitations that agents should be aware of,
    /// such as missing capabilities or languages that require manual setup.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub known_limitations: Vec<String>,
}

/// Per-language LSP health status.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LspLanguageHealth {
    /// Language ID (e.g., "rust", "typescript").
    pub language: String,
    /// Status: `"ready"`, `"warming_up"`, `"starting"`, or `"unavailable"`.
    pub status: String,
    /// Time since LSP process started, formatted as a human-readable string (e.g., "45s").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime: Option<String>,
    /// How diagnostics work for this language.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics_strategy: Option<String>,
    /// Whether call hierarchy is supported (affects `find_callers_callees`, `read_with_deep_context`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_call_hierarchy: Option<bool>,
    /// Whether diagnostics are supported (affects LSP health quality).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_diagnostics: Option<bool>,
    /// Whether definition is supported (affects `get_definition`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_definition: Option<bool>,
    /// Background indexing status: `"complete"`, `"in_progress"`, or None.
    ///
    /// Independent of overall status — an LSP can be "ready" for navigation
    /// while still indexing in the background.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexing_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexing_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexing_duration_secs: Option<u64>,
    /// Whether navigation (`get_definition`, `find_callers_callees`) is functional.
    ///
    /// `true` once the LSP initialize handshake completes with `definitionProvider: true`.
    /// Independent of `indexing_status` — navigation works during indexing but
    /// results may be partial until indexing completes.
    ///
    /// Agents should use this signal to decide:
    /// - `navigation_ready = true` + `indexing_status = "complete"` → full confidence
    /// - `navigation_ready = true` + `indexing_status = "in_progress"` → results may be partial
    /// - `navigation_ready = false` or `None` → fall back to Tree-sitter
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigation_ready: Option<bool>,
    /// Whether the status was verified by a live probe (rather than just progress notifications).
    /// When true, the agent can trust the status.
    #[serde(skip_serializing_if = "crate::server::types::is_false", default)]
    pub probe_verified: bool,
    /// Whether navigation (`get_definition`, `find_all_references`) was confirmed by a live probe.
    ///
    /// `true` only when a live `goto_definition` probe request succeeded — meaning the LSP
    /// returned a real location, not just that it advertised the capability in the initialize
    /// handshake. Stronger signal than `navigation_ready` alone.
    ///
    /// Agents should prefer this over `probe_verified` — it has the same meaning but
    /// communicates intent more clearly.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub navigation_tested: Option<bool>,
    /// Whether the call hierarchy capability was verified by a live probe.
    #[serde(skip_serializing_if = "crate::server::types::is_false", default)]
    pub call_hierarchy_verified: bool,
    /// Install guidance when LSP is unavailable.
    ///
    /// Provides actionable commands users can run to install their LSP servers.
    /// `None` when LSP is running or language not detected at all.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub install_hint: Option<String>,
    /// Indexing progress percentage (0-100) if the LSP reports it via workDoneProgress.
    /// `None` when the LSP does not report progress or indexing is complete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexing_progress_percent: Option<u8>,
    /// Tools that are degraded (using fallback) for this language.
    ///
    /// Empty when LSP is fully operational. Lists which tools lose LSP support
    /// with detailed severity and description for each.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub degraded_tools: Vec<DegradedToolInfo>,
}

// ── find_symbol tool types ─────────────────────────────────────────

/// Parameters for `find_symbol`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct FindSymbolParams {
    /// Bare symbol name to search for (e.g., [`AuthService`]).
    pub name: String,
    /// Optional filter by symbol kind (e.g., `class`, `function`, `struct`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Optional glob pattern to limit search scope (e.g., `src/**/*.ts`).
    #[serde(default = "default_path_glob")]
    pub path_glob: String,
    /// Maximum results to return (default 10).
    #[serde(default = "find_symbol_default_max_results")]
    pub max_results: u32,
}

#[must_use]
pub const fn find_symbol_default_max_results() -> u32 {
    10
}

/// A single symbol found by `find_symbol`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct FoundSymbol {
    /// Full semantic path (<file::symbol>).
    pub semantic_path: String,
    /// Symbol kind (e.g., "class", "function", "struct", "interface", "enum").
    pub kind: String,
    /// File path relative to workspace root.
    pub file: String,
    /// 1-indexed line where the symbol is defined.
    pub line: u64,
    /// First 100 characters of the definition line.
    pub preview: String,
}

/// Response from `find_symbol`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct FindSymbolResponse {
    /// Matching symbols found.
    pub symbols: Vec<FoundSymbol>,
    /// Total number of matches found (before truncation).
    pub total_found: u32,
    /// Search strategy used: "ripgrep+treesitter", "ripgrep+fallback".
    pub search_strategy: String,
    /// Time taken in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

// ── read_files tool types ─────────────────────────────────────────

/// Parameters for `read_files`.
#[derive(Debug, Clone, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct ReadFilesParams {
    /// File paths to read (max 10 per call).
    pub paths: Vec<String>,
    /// Detail level for source files: `source_only`, `compact`, `full`.
    #[serde(default = "read_files_default_detail_level")]
    pub detail_level: String,
    /// Maximum lines per file (default 500).
    #[serde(default = "default_max_lines")]
    pub max_lines_per_file: u32,
}

#[must_use]
pub fn read_files_default_detail_level() -> String {
    "source_only".to_string()
}

/// Result for a single file in `read_files`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct FileResult {
    /// File path.
    pub path: String,
    /// File content (None if error).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Language of the file (e.g., "rust", "typescript", "toml").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Total lines in the file (or lines returned if truncated).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_lines: Option<u32>,
    /// SHA-256 hash of the file content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_hash: Option<String>,
    /// Error message if file could not be read (e.g., "file not found", "sandbox denied").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Response from `read_files`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ReadFilesResponse {
    /// Results for each requested file.
    pub files: Vec<FileResult>,
    /// Number of files successfully read.
    pub succeeded: u32,
    /// Number of files that failed to read.
    pub failed: u32,
    /// Time taken in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// Helper to skip serializing false values for `probe_verified`.
#[allow(clippy::trivially_copy_pass_by_ref)] // Required by serde's skip_serializing_if signature
pub(crate) fn is_false(b: &bool) -> bool {
    !b
}

// ── Default Value Functions ─────────────────────────────────────────

#[must_use]
pub fn default_path_glob() -> String {
    "**/*".to_owned()
}
#[must_use]
pub const fn default_max_results() -> u32 {
    50
}
#[must_use]
pub const fn default_context_lines() -> u32 {
    2
}
#[must_use]
pub fn default_repo_map_path() -> String {
    ".".to_owned()
}
#[must_use]
pub const fn default_max_tokens() -> u32 {
    16_000
}
#[must_use]
pub const fn default_max_tokens_per_file() -> u32 {
    2_000
}
#[must_use]
pub const fn default_depth() -> u32 {
    5
}
#[must_use]
pub const fn default_max_depth() -> u32 {
    3
}
#[must_use]
pub const fn default_max_callers_callees() -> u32 {
    20
}
#[must_use]
pub const fn default_max_references() -> u32 {
    50
}
#[must_use]
pub const fn default_max_dependencies() -> u32 {
    50
}
#[must_use]
pub const fn default_start_line() -> u32 {
    1
}
#[must_use]
pub const fn default_max_lines() -> u32 {
    500
}
#[must_use]
pub fn default_detail_level() -> String {
    "compact".to_string()
}
#[must_use]
pub const fn default_true() -> bool {
    true
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_read_with_deep_context_params_default() {
        let params = ReadWithDeepContextParams::default();
        assert_eq!(params.semantic_path, "");
        assert_eq!(params.project_only, Some(true));
        assert_eq!(params.max_dependencies, 50);
    }

    #[test]
    fn test_find_callers_callees_params_default() {
        let params = FindCallersCalleesParams::default();
        assert_eq!(params.semantic_path, "");
        assert_eq!(params.max_depth, 3);
        assert_eq!(params.project_only, Some(true));
        assert_eq!(params.max_references, 50);
        assert!(!params.include_test_coverage);
    }

    #[test]
    fn test_default_value_helpers() {
        assert_eq!(default_path_glob(), "**/*");
        assert_eq!(default_max_results(), 50);
        assert_eq!(default_context_lines(), 2);
        assert_eq!(default_repo_map_path(), ".");
        assert_eq!(default_max_tokens(), 16_000);
        assert_eq!(default_max_tokens_per_file(), 2_000);
        assert_eq!(default_depth(), 5);
        assert_eq!(default_max_depth(), 3);
        assert_eq!(default_max_callers_callees(), 20);
        assert_eq!(default_max_references(), 50);
        assert_eq!(default_max_dependencies(), 50);
        assert_eq!(default_start_line(), 1);
        assert_eq!(default_max_lines(), 500);
        assert_eq!(default_detail_level(), "compact");
        assert!(default_true());
    }

    #[test]
    fn test_filepath_alias_deserialization() {
        let json_data = serde_json::json!({
            "path": "src/lib.rs",
            "start_line": 10,
            "max_lines": 20
        });
        let read_file_params: ReadFileParams = serde_json::from_value(json_data).unwrap();
        assert_eq!(read_file_params.filepath, "src/lib.rs");

        let json_data_sf = serde_json::json!({
            "path": "src/main.rs"
        });
        let read_sf_params: ReadSourceFileParams = serde_json::from_value(json_data_sf).unwrap();
        assert_eq!(read_sf_params.filepath, "src/main.rs");
    }
}
