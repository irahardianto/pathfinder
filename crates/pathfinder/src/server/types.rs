//! Tool parameter and response types for Pathfinder MCP tools.
//!
//! These structs are deserialized by the rmcp framework from MCP tool call
//! payloads. The `dead_code` lint fires for param struct fields that are read
//! by serde (via `Deserialize`) but never accessed by name in production code.
//! A module-level `#![allow]` is used here so that each newly implemented tool
//! can remove its struct's allow without touching unrelated items.
#![allow(dead_code)] // Fields are read by serde deserialization, not by name

use pathfinder_common::types::{ActionableGuidance, DegradedReason, FilterMode};
use rmcp::schemars;
use rmcp::serde::{self, Deserialize, Serialize};

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
    /// Absent (omitted from JSON) when LSP was unavailable — references are unknown.
    /// Present as `[]` when LSP confirmed zero references.
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
    /// Absent (omitted from JSON) when LSP was unavailable — callers are unknown.
    /// Present as `[]` when LSP confirmed zero callers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incoming: Option<Vec<SymbolOverviewImpactEntry>>,
    /// Direct callees.
    /// Absent (omitted from JSON) when LSP was unavailable — callees are unknown.
    /// Present as `[]` when LSP confirmed zero callees.
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

// ── Legacy Response Types ───────────────────────────────────────────
// These response structs are returned by the underlying `_impl` methods
// and serialized into MCP responses by the `#[tool_router]` handlers in `server.rs`.

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
    /// Number of searchable files matching the `path_glob` (excludes binary and gitignored).
    /// When `files_searched < files_in_scope`, some files were skipped
    /// due to permission issues or I/O errors.
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
    /// Short content fingerprint of the file (7-char hex, derived from SHA-256).
    /// Shared by all matches in this group.
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
    /// The detail mode used for this response: `"structure"`, `"files"`, or `"symbols"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Number of directories scanned during repository mapping (structure mode only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dirs_scanned: Option<usize>,
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
    /// Absent when visibility filtering is working correctly.
    /// Present as `true` if the visibility filter could not be applied
    /// (agents should then treat all symbols as public).
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
    /// Optional hint for the agent (e.g. suggesting to increase `max_tokens` if coverage < 100).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Structured recommendation for `max_tokens` to achieve full coverage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_max_tokens: Option<u32>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
#[derive(Debug, Clone, Serialize, serde::Deserialize, schemars::JsonSchema)]
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
    /// One of: `lsp_call_hierarchy`, `treesitter_direct`, `treesitter_fallback`, `grep_fallback`.
    /// Note: `grep_fallback` corresponds to `GrepFallbackDependencies` reason — the finer-grained
    /// locate/trace values (`grep_file_scoped`, `grep_impl_scoped`, etc.) are NOT emitted here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_strategy: Option<String>,
    /// Spec 5.1: Wall-clock duration of the tool call in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// File-level import/using statements (populated when `include_imports=true`).
    ///
    /// Contains the raw import lines from the file. Useful for Java, C#, Kotlin,
    /// and other OOP languages where imports clarify what types are in scope.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub imports: Vec<String>,
    /// Source code of the symbol. Populated so agents parsing `structured_content`
    /// have access to source alongside dependencies without parsing the text channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
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
    /// `null` when `incoming_verified` is `Some(false)` AND the result is
    /// genuinely unknown (LSP unavailable, no heuristic fallback matched) —
    /// callers are **unknown**, do NOT treat as zero.
    /// An empty array `[]` means callers were verified (either by LSP, or by
    /// heuristic grep fallback) and the list is complete.
    /// Check `incoming_verified` to disambiguate "UNKNOWN (null)" from
    /// "verified empty (Some([]))" — null is a footgun when agents coalesce
    /// it to an empty array.
    pub incoming: Option<Vec<ImpactReference>>,
    /// Symbols the target calls (callee graph).
    /// Same semantics as `incoming` but for the callee list.
    /// Check `outgoing_verified` to disambiguate `null` (UNKNOWN) from `[]`
    /// (verified zero).
    pub outgoing: Option<Vec<ImpactReference>>,
    /// Whether `incoming` was verified by LSP (vs unknown due to degradation).
    ///
    /// - `Some(true)`: callers list is verified (either LSP-confirmed, possibly
    ///   empty, or heuristic grep results that successfully completed).
    ///   `incoming` will be `Some(vec)` — agents may use `incoming.len()` to
    ///   count, but treat the list as a candidate set when the global
    ///   `degraded` flag is `true` (heuristic, may include false positives).
    /// - `Some(false)`: callers are UNKNOWN. `incoming` is `null`.
    ///   Do NOT treat as zero. Use `search` to verify manually.
    /// - `None`: field not applicable (e.g., scope did not request callers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incoming_verified: Option<bool>,
    /// Whether `outgoing` was verified by LSP (vs unknown due to degradation).
    ///
    /// Same semantics as `incoming_verified` but for the callee list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outgoing_verified: Option<bool>,
    /// Number of transitive levels traversed.
    pub depth_reached: u32,
    /// Total files referenced across all incoming and outgoing references.
    pub files_referenced: usize,
    /// Whether the call hierarchy analysis was degraded (LSP unavailable or crashed).
    /// Always present. When `true`, `incoming` and `outgoing` may be `null` (unknown)
    /// or contain heuristic grep-based results. Check `resolution_strategy` and
    /// `confidence` on each reference to assess reliability.
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
    /// One of: `lsp_call_hierarchy`, `lsp_call_hierarchy_with_impl_expansion`,
    /// `grep_file_scoped`, `treesitter_direct`, `treesitter_fallback`.
    /// Note: `lsp_call_hierarchy_with_impl_expansion` indicates the target was a
    /// trait/interface method; callers were merged across all concrete
    /// implementations found via `goto_implementation`.
    /// Note: `grep_file_scoped` covers all LSP-unavailable grep fallback paths; the finer-grained
    /// `grep_impl_scoped`/`grep_global`/`grep_broad` values are NOT emitted for this tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_strategy: Option<String>,
    /// Spec 4.2: Test functions that reference or test this symbol.
    /// Populated when `include_test_coverage=true` was passed.
    /// Absent (omitted from JSON) when unavailable/not requested.
    /// Present as `[]` when requested and confirmed to have zero test callers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_callers: Option<Vec<ImpactReference>>,
    /// Spec 4.2: Status of test coverage search.
    /// One of: `"found"`, `"not_found"`, `"unknown_degraded"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_coverage_status: Option<String>,
    /// P2-7: Actionable hint when zero callers/callees are found and the result
    /// is not degraded. Helps agents distinguish "genuinely unused" from common
    /// false-negative scenarios.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

// ── Get Semantic Path Tool Types ────────────────────────────────────────

/// Result for `get_semantic_path`.
#[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct GetSemanticPathResult {
    /// The full semantic path (`file::symbol`) of the innermost enclosing symbol.
    ///
    /// Absent (omitted from JSON) when the line is not inside any named symbol
    /// (e.g., it is a module-level attribute, blank line, or the file uses an
    /// unsupported language).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_path: Option<String>,
    /// The symbol portion only (without the file prefix).
    ///
    /// Absent (omitted from JSON) when `semantic_path` is absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// The file portion of the semantic path (same as the `file` parameter).
    pub file: String,
    /// The queried line number (1-indexed, echoed back for confirmation).
    pub line: u32,
}

// ── Find All References Tool Types ─────────────────────────────────────

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
    /// P2-7: Actionable hint when zero references are found and the result
    /// is not degraded. Helps agents distinguish "genuinely unused" from common
    /// false-negative scenarios like reflection or dynamic dispatch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
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
    /// Tool name (e.g., `"trace"`, `"locate"`, `"inspect"`).
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
    /// P2-6: Whether ALL detected languages have finished background indexing.
    ///
    /// `true` when every language reports `indexing_status == "complete"` or has
    /// no indexing information (language unavailable). `false` when any language
    /// is still indexing. Agents can poll this single field instead of iterating
    /// the `languages` array.
    pub indexing_complete: bool,
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
    /// Status of the language server process.
    ///
    /// Lifecycle: `unavailable` → `starting` → `warming_up` → `ready`
    /// A `ready` LSP may be downgraded to `degraded` if a live probe fails.
    ///
    /// - `"unavailable"`: No LSP process running or detected
    /// - `"starting"`: Process exists, no capability info yet (lazy start)
    /// - `"warming_up"`: Process running, `navigation_ready` not yet confirmed
    /// - `"ready"`: Initialize handshake complete, `navigation_ready=true`,
    ///   and live probe succeeded (`navigation_verified=Some(true)`)
    /// - `"degraded"`: Initialize handshake completed (`navigation_ready=true`)
    ///   but live probe failed (`navigation_verified=Some(false)`).
    ///   Navigation MAY still work — retry or use with caution.
    pub status: String,
    /// Time since LSP process started, formatted as a human-readable string (e.g., "45s").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime: Option<String>,
    /// How diagnostics work for this language.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics_strategy: Option<String>,
    /// Whether call hierarchy is supported (affects `trace`, `inspect` with dependencies).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_call_hierarchy: Option<bool>,
    /// Whether diagnostics are supported (affects LSP health quality).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_diagnostics: Option<bool>,
    /// Whether definition is supported (affects `locate`).
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
    /// Whether the LSP advertised navigation capabilities during initialize.
    ///
    /// This reflects capability negotiation only — it does NOT guarantee
    /// navigation actually works. For live verification, check `navigation_verified`.
    ///
    /// `true` once the LSP initialize handshake completes with `definitionProvider: true`.
    /// Independent of `indexing_status` — navigation works during indexing but
    /// results may be partial until indexing completes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigation_ready: Option<bool>,
    /// Whether navigation was verified by a live probe (not just capability
    /// advertisement). Distinct from `navigation_ready` which reflects
    /// capability negotiation only.
    ///
    /// - `Some(true)`: live probe succeeded — navigation is operational
    /// - `Some(false)`: live probe failed — navigation may be broken despite
    ///   `navigation_ready: true`
    /// - `None`: probe not yet run (freshness unknown)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigation_verified: Option<bool>,
    /// Whether the status was verified by a live probe (rather than just progress notifications).
    /// When true, the agent can trust the status.
    #[serde(skip_serializing_if = "crate::server::types::is_false", default)]
    pub probe_verified: bool,
    /// Whether navigation (`locate`, `trace`) was confirmed by a live probe.
    ///
    /// `true` only when a live `goto_definition` probe request succeeded — meaning the LSP
    /// returned a real location, not just that it advertised the capability in the initialize
    /// handshake. Stronger signal than `navigation_ready` alone.
    ///
    /// Note: This is an independent signal from `status: "ready"`. An LSP can be in the `"ready"` status
    /// (handshake completed) even if a live probe has not yet been executed or has failed (in which case
    /// `navigation_tested` will be `None` or `Some(false)` respectively).
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
    /// The LSP server identity (e.g., "rust-analyzer", "Pyright", "gopls").
    /// Useful for distinguishing which Python LSP is running.
    /// `None` when the process is not running or server omitted serverInfo.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub server_name: Option<String>,
    /// Number of dynamic capability registrations received from the LSP server.
    /// Useful for diagnosing dynamic registration delays (e.g., jdtls).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub registrations_received: Option<u32>,
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
    /// Seconds since last liveness probe for this language.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_probe_age_secs: Option<u32>,
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
    /// Suggested alternative symbol paths when `symbols` is empty.
    ///
    /// Populated using fuzzy matching and cross-file search when the
    /// query matches no exact symbol. Each entry is a full semantic path
    /// (`file::symbol`) that can be used directly with `inspect` or
    /// `trace`.
    ///
    /// Absent when `symbols` is non-empty (exact match found).
    /// Absent when no suggestions could be found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub did_you_mean: Option<Vec<String>>,
    /// Human-readable hint when no exact match found but suggestions exist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
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
    /// Total number of lines in the file (before truncation).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_lines: Option<u32>,
    /// Short content fingerprint of the file (7-char hex, derived from SHA-256).
    ///
    /// Use for change detection: if the hash differs from a previous call, the file
    /// has been modified. Not a full SHA-256 digest — compare as opaque strings only.
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

// ── Consolidated Tool Parameter Types ───────────────────────────────
//
// These param structs are used by the 7 consolidated tool handlers
// registered in the `#[tool_router]` impl block in `server.rs`.

/// Detail level for the `explore` tool.
#[derive(Debug, Clone, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Detail {
    /// Directory tree + package manager files only.
    Structure,
    /// Directory tree + all filenames (no symbols).
    Files,
    /// Full AST symbol hierarchy (current behavior).
    #[default]
    Symbols,
}

/// Parameters for the `explore` tool.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct ExploreParams {
    /// Directory to map.
    #[serde(default = "default_repo_map_path")]
    pub path: String,
    /// Level of detail to include in the output.
    #[serde(default)]
    pub detail: Detail,
    /// Total token budget for the entire skeleton output. Default: 16000.
    ///
    /// When `coverage_percent` in the response is low, increase this value
    /// to include more files in the map.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Max directory traversal depth (default: 3).
    ///
    /// Increase when `coverage_percent` is low or the project has deeply-nested
    /// source files. The walker stops early on shallow repos, so over-provisioning
    /// is safe and nearly free.
    #[serde(default = "default_explore_depth")]
    pub depth: u32,
    /// Visibility filter: `public` or `all`.
    #[serde(default)]
    pub visibility: pathfinder_common::types::Visibility,
    /// Per-file token cap before a file skeleton is collapsed to a summary stub.
    ///
    /// When the rendered skeleton of an individual file exceeds this limit, the
    /// file is replaced with a truncated stub showing only class/struct names and
    /// method counts. Default: 2000.
    #[serde(default = "default_max_tokens_per_file")]
    pub max_tokens_per_file: u32,
    /// Git ref or duration to show only recently modified files (e.g., `HEAD~5`, `3h`, `2024-01-01`).
    ///
    /// Defaults to an empty string, which disables the git filter and maps the entire repository.
    #[serde(default)]
    pub changed_since: String,
    /// Only include files with these extensions. Mutually exclusive with `exclude_extensions`.
    ///
    /// Format: raw extension string without a leading dot or wildcard (e.g. `"rs"` or `"ts"`,
    /// NOT `".rs"` or `"*.rs"`).
    ///
    /// If both `include_extensions` and `exclude_extensions` are non-empty, the tool returns
    /// an invalid params error.
    #[serde(default)]
    pub include_extensions: Vec<String>,
    /// Exclude files with these extensions. Mutually exclusive with `include_extensions`.
    ///
    /// Format: raw extension string without a leading dot or wildcard (e.g. `"rs"` or `"ts"`,
    /// NOT `".rs"` or `"*.rs"`).
    ///
    /// If both `include_extensions` and `exclude_extensions` are non-empty, the tool returns
    /// an invalid params error.
    #[serde(default)]
    pub exclude_extensions: Vec<String>,
}

/// Search mode for the `search` tool.
#[derive(Debug, Clone, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    /// Text search (default).
    #[default]
    Text,
    /// Symbol name resolution.
    Symbol,
    /// Regex search.
    Regex,
}

/// Glob pattern representation that accepts either a single string or an array of strings.
#[derive(
    Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema, PartialEq, Eq,
)]
#[serde(untagged)]
pub enum ExcludeGlob {
    /// A single glob pattern.
    Single(String),
    /// Multiple glob patterns.
    Multiple(Vec<String>),
}

impl Default for ExcludeGlob {
    fn default() -> Self {
        Self::Single(String::new())
    }
}

impl std::fmt::Display for ExcludeGlob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Single(s) => write!(f, "{s}"),
            Self::Multiple(v) => write!(f, "[{}]", v.join(", ")),
        }
    }
}

impl ExcludeGlob {
    /// Convert the glob pattern(s) to a list of string patterns.
    #[must_use]
    pub fn to_vec(&self) -> Vec<String> {
        match self {
            Self::Single(s) => {
                if s.is_empty() {
                    Vec::new()
                } else {
                    vec![s.clone()]
                }
            }
            Self::Multiple(v) => v.clone(),
        }
    }
}

/// Parameters for the `search` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SearchParams {
    /// Search pattern (literal text, regex, or symbol name depending on `mode`).
    pub query: String,
    /// Search mode: `text` (default), `symbol`, or `regex`.
    #[serde(default)]
    pub mode: SearchMode,
    /// Limit search scope (e.g., `src/**/*.ts`).
    ///
    /// Supports brace expansion to search multiple patterns in one call:
    /// `{src/**/*.ts,tests/**/*.ts}` matches files in both directories.
    ///
    /// Brace expansion is handled by the `globset` crate and works out of the
    /// box — no extra configuration needed.
    #[serde(default = "default_path_glob")]
    pub path_glob: String,
    /// Maximum matches returned. Default: 50.
    ///
    /// Applies to all modes including `symbol`. When `mode="symbol"`, this caps
    /// the number of resolved symbol matches returned.
    #[serde(default = "default_max_results")]
    pub max_results: u32,
    /// Lines of context above/below each match (text/regex modes only).
    #[serde(default = "default_context_lines")]
    pub context_lines: u32,
    /// File paths already in the agent's context.
    ///
    /// For matches in these files, only minimal metadata is returned.
    /// Full content and context lines are omitted to save tokens.
    #[serde(default)]
    pub known_files: Vec<String>,
    /// Glob patterns for files to exclude from search. Accepts either a single
    /// string or an array of strings.
    ///
    /// Supports brace expansion to exclude multiple patterns in one call:
    /// `{**/*.test.*,**/*.spec.*}` excludes both test and spec files.
    /// `{**/*.test.ts,**/*.spec.ts,**/*.e2e.ts}` — three patterns, one string.
    ///
    /// Brace expansion is handled by the `globset` crate and works out of the
    /// box — no extra configuration needed.
    #[serde(default)]
    pub exclude_glob: ExcludeGlob,
    /// Number of matches to skip before returning results (for pagination).
    /// Use with `max_results` to page through large result sets.
    #[serde(default)]
    pub offset: u32,
    /// Optional filter by symbol kind. Only used when `mode` is `symbol`.
    ///
    /// **Canonical values:** `function`, `class`, `struct`, `interface`, `enum`,
    /// `constant`, `module`, `impl`, `type`.
    ///
    /// **Accepted aliases (all case-insensitive):**
    /// - `method`, `fn` → treated as `function`
    /// - `trait` → treated as `interface` (Rust traits, Go interfaces)
    /// - `const`, `static`, `let` → treated as `constant`
    /// - `mod`, `namespace` → treated as `module`
    /// - `type` matches class, struct, interface, trait, and enum (broadest type-level search).
    /// - `class` matches class, struct, and interface (broad OOP-style search, but NOT enums).
    /// - `struct` matches only struct.
    ///
    /// Example: `kind="trait"` finds Rust traits and Go interfaces.
    #[serde(default)]
    pub kind: Option<String>,
    /// Group matches by file path. Default: true.
    #[serde(default = "default_group_by_file")]
    pub group_by_file: bool,
    /// Filter results: `all`, `code_only`, `comments_only`, or `non_code` (matches in comments AND string literals (non-code content)). Default: `code_only`.
    #[serde(default = "default_filter_mode")]
    pub filter_mode: FilterMode,
}

impl Default for SearchParams {
    fn default() -> Self {
        Self {
            query: String::new(),
            mode: SearchMode::Text,
            path_glob: default_path_glob(),
            max_results: default_max_results(),
            context_lines: default_context_lines(),
            known_files: Vec::new(),
            exclude_glob: ExcludeGlob::default(),
            offset: 0,
            kind: None,
            group_by_file: default_group_by_file(),
            filter_mode: default_filter_mode(),
        }
    }
}

/// Parameters for the `read` tool.
///
/// Accepts either a single file via `filepath` or multiple files via `paths`.
/// Exactly one of the two must be provided; the handler validates this at runtime.
/// File type (source vs config) is auto-detected from the extension.
#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
pub struct ReadParams {
    /// Single file path. Use this for reading one file.
    /// Mutually exclusive with `paths`.
    #[serde(default, alias = "path")]
    pub filepath: Option<String>,
    /// Multiple file paths (max 10). Use this for batch reading.
    /// Mutually exclusive with `filepath`.
    #[serde(default)]
    pub paths: Option<Vec<String>>,
    /// Detail level for source files: `source_only`, `compact`, `symbols`, or `full`.
    /// Ignored for config/non-source files (always raw content).
    #[serde(default = "default_detail_level")]
    pub detail_level: String,
    /// First line to return (1-indexed). Single-file mode only.
    #[serde(default = "default_start_line")]
    pub start_line: u32,
    /// Last line to return (1-indexed, inclusive). Single-file mode only.
    #[serde(default)]
    pub end_line: Option<u32>,
    /// Maximum lines returned per file (defaults to 500). Applies to batch mode and config/raw files.
    #[serde(default = "default_max_lines")]
    pub max_lines_per_file: u32,
}

impl Default for ReadParams {
    fn default() -> Self {
        Self {
            filepath: None,
            paths: None,
            detail_level: default_detail_level(),
            start_line: default_start_line(),
            end_line: None,
            max_lines_per_file: default_max_lines(),
        }
    }
}

/// Parameters for the `inspect` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct InspectParams {
    /// Semantic path (e.g., `src/auth.ts::AuthService.login`). MUST include file path and '::'.
    #[serde(default)]
    pub semantic_path: Option<String>,
    /// Multiple semantic paths (max 10) for batch inspection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_paths: Option<Vec<String>>,
    /// When `true`, also fetch callee signatures (dependency context).
    /// Default: `false` (equivalent to old `read_symbol_scope` behavior).
    #[serde(default)]
    pub include_dependencies: bool,
    /// Maximum number of dependencies (callee signatures) to return.
    /// Only used when `include_dependencies` is `true`. Default: 50.
    #[serde(default = "default_max_dependencies")]
    pub max_dependencies: u32,
    /// Include file-level import statements in the response.
    ///
    /// Useful for languages with verbose package paths (Java, C#, Kotlin)
    /// where imports clarify what types are in scope.
    /// Only used when `include_dependencies` is `true`. Default: `false`.
    #[serde(default)]
    pub include_imports: bool,
}

impl Default for InspectParams {
    fn default() -> Self {
        Self {
            semantic_path: None,
            semantic_paths: None,
            include_dependencies: false,
            max_dependencies: default_max_dependencies(),
            include_imports: false,
        }
    }
}

/// A single entry in a batch locate request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct LocateEntry {
    /// Semantic path to resolve (e.g., `src/auth.ts::AuthService.login`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_path: Option<String>,
    /// File path (e.g., `src/auth.ts`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// 1-indexed line number to resolve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

/// Parameters for the `locate` tool.
///
/// Auto-detects mode from input:
/// - If `semantic_path` is provided → jump to definition.
/// - If `file` + `line` are provided → resolve to semantic path.
///
/// Exactly one mode must be specified; the handler validates this at runtime.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct LocateParams {
    /// Semantic path to the reference (e.g., `src/auth.ts::AuthService.login`).
    /// Provide this to jump to a symbol's definition.
    #[serde(default)]
    pub semantic_path: Option<String>,
    /// Relative path to the file (e.g., `src/auth.ts`).
    /// Provide together with `line` to resolve a location to its semantic path.
    #[serde(default, alias = "path")]
    pub file: Option<String>,
    /// 1-indexed line number to resolve.
    /// Provide together with `file` to resolve a location to its semantic path.
    #[serde(default)]
    pub line: Option<u32>,
    /// Multiple locations (max 10) for batch location/resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locations: Option<Vec<LocateEntry>>,
}

/// Result entry for a single inspect operation in a batch.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct InspectResultEntry {
    pub semantic_path: String,
    pub status: String, // "ok" or "error"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<Vec<DeepContextDependency>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Response from a batch `inspect` tool call.
#[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct BatchInspectResult {
    pub results: Vec<InspectResultEntry>,
    pub succeeded: usize,
    pub failed: usize,
    pub total_duration_ms: u64,
}

/// Result entry for a single locate operation in a batch.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct LocateResultEntry {
    pub input: LocateEntry, // echo back for correlation
    pub status: String,     // "ok" or "error"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_strategy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Response from a batch `locate` tool call.
#[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct BatchLocateResult {
    pub results: Vec<LocateResultEntry>,
    pub succeeded: usize,
    pub failed: usize,
    pub total_duration_ms: u64,
}

/// Scope for the `trace` tool.
#[derive(Debug, Clone, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TraceScope {
    /// Call hierarchy — callers and callees (default).
    #[default]
    Callers,
    /// All references (calls, imports, type annotations, etc.).
    References,
    /// Combined overview: source + callers + callees + references.
    Overview,
}

/// Parameters for the `trace` tool.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TraceParams {
    /// Semantic path to the target symbol (e.g., `src/mod.rs::func`). MUST include file path and '::'.
    pub semantic_path: String,
    /// What to trace: `callers` (default), `references`, or `overview`.
    #[serde(default)]
    pub scope: TraceScope,
    /// Traversal depth for call hierarchy (max: 5). Only used with `scope=callers`.
    ///
    /// Each depth level requires additional LSP round-trips, so deeper traversal is slower.
    /// Use `max_depth=1` for a fast, shallow result (direct callers/callees only).
    /// Use `max_depth=2` or `max_depth=3` for moderate transitive analysis.
    /// Reserve `max_depth=4` or `max_depth=5` for large-scale architectural analysis.
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
    /// Maximum total references to return (clamped to 1..=500).
    /// Prevents context overflow on large codebases. Default: 50.
    ///
    /// In `overview` scope, this single parameter controls both the
    /// callers/callees cap and the references cap (both set to this value).
    #[serde(default = "default_max_references")]
    pub max_references: u32,
    /// Number of results to skip before returning (for pagination).
    #[serde(default)]
    pub offset: u32,
}

impl Default for TraceParams {
    fn default() -> Self {
        Self {
            semantic_path: String::default(),
            scope: TraceScope::default(),
            max_depth: default_max_depth(),
            max_references: default_max_references(),
            offset: 0,
        }
    }
}

/// Parameters for the `health` tool.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct HealthParams {
    /// Optional language to check (e.g., "rust", "typescript").
    /// If omitted, checks all available languages.
    #[serde(default)]
    pub language: Option<String>,
    /// Optional action to perform.
    ///
    /// - `"restart"`: Force-restart the LSP process for the specified language.
    ///   `language` must be set when using `"restart"`.
    ///   Returns updated health status after the restart attempt.
    #[serde(default)]
    pub action: Option<String>,
    /// Optional parameter to force a live liveness probe regardless of cache age.
    #[serde(default)]
    pub force_probe: Option<bool>,
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
pub const fn default_max_depth() -> u32 {
    3
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
pub const fn default_explore_depth() -> u32 {
    3
}
#[must_use]
pub const fn default_group_by_file() -> bool {
    true
}
#[must_use]
pub const fn default_filter_mode() -> FilterMode {
    FilterMode::CodeOnly
}

#[cfg(test)]
#[path = "types_test.rs"]
mod tests;
