//! Tool parameter and response types for Pathfinder MCP tools.
//!
//! These structs are deserialized by the rmcp framework from MCP tool call
//! payloads. The `dead_code` lint fires for param struct fields that are read
//! by serde (via `Deserialize`) but never accessed by name in production code.
//! A module-level `#![allow]` is used here so that each newly implemented tool
//! can remove its struct's allow without touching unrelated items.
#![allow(dead_code)] // Fields are read by serde deserialization, not by name

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
    /// Import inclusion: `none`, `third_party`, or `all`.
    #[serde(default)]
    pub include_imports: pathfinder_common::types::IncludeImports,
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
}

/// Parameters for `read_symbol_scope`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct ReadSymbolScopeParams {
    /// Semantic path (e.g., `src/auth.ts::AuthService.login`).
    pub semantic_path: String,
}

/// Parameters for `read_with_deep_context`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct ReadWithDeepContextParams {
    /// Semantic path (e.g., `src/auth.ts::AuthService.login`). MUST include file path and '::'.
    pub semantic_path: String,
}

/// Parameters for `get_definition`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct GetDefinitionParams {
    /// Semantic path to the reference (e.g., `src/auth.ts::AuthService.login`).
    pub semantic_path: String,
}

/// Parameters for `analyze_impact`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct AnalyzeImpactParams {
    /// Semantic path to the target (e.g., `src/mod.rs::func`).
    pub semantic_path: String,
    /// Traversal depth (max: 5).
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
}

/// Parameters for `read_file`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct ReadFileParams {
    /// Relative file path.
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
    pub filepath: String,
    /// Detail level: "compact", "symbols", "full".
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
    /// Compare with `returned_count` to understand how many matches were filtered.
    pub total_matches: usize,
    /// Number of matches actually returned in this response (after `filter_mode` filtering).
    ///
    /// `returned_count == matches.len()`. Provided as a convenience field so agents
    /// do not need to count `matches` themselves.
    pub returned_count: usize,
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
    pub degraded_reason: Option<String>,
    /// When results are truncated, this field provides the `offset` value
    /// to use for the next page of results. Absent when not truncated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<u32>,
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
    pub degraded_reason: Option<String>,
    /// System capabilities available for this repository.
    pub capabilities: RepoCapabilities,
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
}

/// The response for `read_symbol_scope`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ReadSymbolScopeResponse {
    /// The extracted symbol source code.
    pub content: String,
    /// Starting line number of the symbol in the source.
    pub start_line: usize,
    /// Ending line number of the symbol in the source.
    pub end_line: usize,
    /// Version hash of the file containing the symbol.
    pub version_hash: String,
    /// Programming language of the source symbol.
    pub language: String,
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
    /// Symbols extracted from the source file.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub symbols: Vec<SourceSymbol>,
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
    /// Whether the output was truncated.
    pub truncated: bool,
    /// Detected language of the file.
    pub language: String,
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
    pub degraded_reason: Option<String>,
    /// IW-2: LSP readiness signal at the time of the call.
    ///
    /// - `"ready"`: LSP is fully operational — results are authoritative.
    /// - `"warming_up"`: LSP is still indexing — results may be partial.
    /// - `"unavailable"`: No LSP; Tree-sitter fallback used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lsp_readiness: Option<String>,
}

/// The response for `get_definition`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
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
    pub degraded_reason: Option<String>,
    /// IW-2: LSP readiness at the time of the call.
    ///
    /// - `"ready"`: LSP operational — definition is authoritative.
    /// - `"warming_up"`: LSP still indexing — result may be from Tree-sitter fallback.
    /// - `"unavailable"`: No LSP; result is from ripgrep heuristics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lsp_readiness: Option<String>,
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
    /// SHA-256 content fingerprint for this file.
    pub version_hash: String,
    /// Direction of the reference relative to the target symbol.
    ///
    /// - `"incoming"` — this symbol calls or references the target (a caller).
    /// - `"outgoing"` — the target calls or references this symbol (a callee).
    /// - `"incoming_heuristic"` — inferred by grep fallback when LSP is unavailable;
    ///   treat as a candidate, not a confirmed call.
    pub direction: String,
    /// BFS traversal depth (0 = direct caller/callee, 1 = one hop away, etc.).
    pub depth: usize,
}

/// The metadata embedded in `structured_content` for `analyze_impact`.
#[derive(Debug, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct AnalyzeImpactMetadata {
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
    pub degraded_reason: Option<String>,
    /// SHA-256 version hashes for all referenced files (including the target file itself),
    /// keyed by relative file path. Agents can use these as content fingerprints to detect
    /// file changes without a separate read call.
    #[serde(skip_serializing_if = "std::collections::HashMap::is_empty", default)]
    pub version_hashes: std::collections::HashMap<String, String>,
}

// ── LSP Health Tool Types ────────────────────────────────────────────

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
    /// Whether call hierarchy is supported (affects `analyze_impact`, `read_with_deep_context`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_call_hierarchy: Option<bool>,
    /// Whether diagnostics are supported (affects LSP health quality).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_diagnostics: Option<bool>,
    /// Whether definition is supported (affects `get_definition`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_definition: Option<bool>,
    /// Whether formatting is supported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_formatting: Option<bool>,
    /// Background indexing status: `"complete"`, `"in_progress"`, or None.
    ///
    /// Independent of overall status — an LSP can be "ready" for navigation
    /// while still indexing in the background.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexing_status: Option<String>,
    /// Whether navigation (`get_definition`, `analyze_impact`) is functional.
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
    #[serde(skip_serializing_if = "crate::server::types::is_false")]
    pub probe_verified: bool,
    /// Install guidance when LSP is unavailable.
    ///
    /// Provides actionable commands users can run to install their LSP servers.
    /// `None` when LSP is running or language not detected at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_hint: Option<String>,
    /// Tools that are degraded (using fallback) for this language.
    ///
    /// Empty when LSP is fully operational. Lists which tools lose LSP support.
    /// Example: `["analyze_impact", "read_with_deep_context"]` when call hierarchy unsupported.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub degraded_tools: Vec<String>,
    /// Approximate validation latency in milliseconds for this language.
    ///
    /// `None` when unknown or not applicable. Helps agents decide whether to validate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation_latency_ms: Option<u64>,
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
    2
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
