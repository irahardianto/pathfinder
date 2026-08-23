//! Tool parameter and response types for Pathfinder MCP tools.
//!
//! These structs are deserialized by the rmcp framework from MCP tool call
//! payloads. The `dead_code` lint fires for param struct fields that are read
//! by serde (via `Deserialize`) but never accessed by name in production code.
//! A module-level `#![allow]` is used here so that each newly implemented tool
//! can remove its struct's allow without touching unrelated items.
#![allow(dead_code)] // Fields are read by serde deserialization, not by name

use pathfinder_common::types::{
    ActionableGuidance, Confidence, DegradedReason, Evidence, ToolStatus,
};
use rmcp::schemars;
use rmcp::serde::{self, Deserialize, Serialize};

/// Response for `symbol_overview`.
#[derive(Debug, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SymbolOverviewResponse {
    /// Tool status.
    #[serde(default)]
    pub status: ToolStatus,
    /// Source code and location of the symbol.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<SymbolSource>,
    /// Impact analysis (incoming callers + outgoing callees).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub impact: Option<ImpactSummary>,
    /// Reference locations across the codebase.
    /// `null` when LSP was unavailable -- references are unknown.
    /// `Some(Evidence { confidence: Verified, items: [] })` (`[]`) when LSP confirmed zero references.
    pub references: Option<Evidence<SymbolOverviewReference>>,
    /// Heuristic candidates when status is `NotReady`. Never populated when `Ok`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heuristic_candidates: Option<Vec<SymbolOverviewReference>>,
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
    /// Actionable hint if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Resolution strategy if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_strategy: Option<String>,
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
    /// `null` when LSP was unavailable -- callers are unknown.
    /// `Some(Evidence { ... })` when callers are resolved.
    pub incoming: Option<Evidence<SymbolOverviewImpactEntry>>,
    /// Direct callees.
    /// `null` when LSP was unavailable -- callees are unknown.
    /// `Some(Evidence { ... })` when callees are resolved.
    pub outgoing: Option<Evidence<SymbolOverviewImpactEntry>>,
    /// Whether the impact analysis was degraded.
    pub degraded: bool,
}

/// A single entry in the impact summary.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SymbolOverviewImpactEntry {
    /// Semantic path of the impacted symbol.
    pub semantic_path: String,
    /// File path containing the entry.
    pub file: String,
    /// Line number of the entry.
    pub line: usize,
    /// Code snippet preview.
    pub snippet: String,
    /// Call direction ("caller" or "callee").
    pub direction: String,
    /// Confidence level of this impact entry.
    pub confidence: Confidence,
}

/// A reference location in `symbol_overview`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SymbolOverviewReference {
    /// Relative file path.
    pub file: String,
    /// Line number (1-based).
    pub line: u32,
    /// Column number (1-based).
    pub column: u32,
    /// Code snippet preview.
    pub snippet: String,
    /// Confidence level of this reference.
    pub confidence: Confidence,
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
    /// Confidence level of this dependency.
    pub confidence: Confidence,
}

/// The metadata embedded in `structured_content` for `read_with_deep_context`.
#[derive(Debug, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ReadWithDeepContextMetadata {
    /// Tool status.
    #[serde(default)]
    pub status: ToolStatus,
    /// Start line of the symbol (1-indexed).
    pub start_line: usize,
    /// End line of the symbol (1-indexed).
    pub end_line: usize,
    /// Detected language.
    pub language: String,
    /// Signatures of all symbols called by this one.
    pub dependencies: Option<Evidence<DeepContextDependency>>,
    /// Heuristic candidates when status is `NotReady`. Never populated when `Ok`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heuristic_candidates: Option<Vec<DeepContextDependency>>,
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
    /// Next offset for pagination when truncated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<u32>,
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

/// Metadata returned by single symbol inspect (`read_symbol_scope`) when `include_dependencies=false`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ReadSymbolScopeMetadata {
    /// Tool status.
    #[serde(default)]
    pub status: ToolStatus,
    /// The source code content of the symbol.
    pub content: String,
    /// 1-indexed line where the symbol definition begins.
    pub start_line: usize,
    /// 1-indexed line where the symbol definition ends.
    pub end_line: usize,
    /// Language ID of the file.
    pub language: String,
    /// Wall-clock duration of the tool call in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// The response for `get_definition`.
#[derive(Debug, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct GetDefinitionResponse {
    /// Tool status.
    #[serde(default)]
    pub status: ToolStatus,
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
#[derive(Debug, Clone, Serialize, serde::Deserialize, schemars::JsonSchema)]
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
    pub confidence: Confidence,
}

/// The metadata embedded in `structured_content` for `find_callers_callees`.
#[derive(Debug, Default, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct FindCallersCalleesMetadata {
    /// Tool status.
    #[serde(default)]
    pub status: ToolStatus,
    /// Symbols that call the target (caller graph).
    /// `null` when `degraded` (LSP was unavailable and no heuristic fallback matched) -- callers are UNKNOWN.
    /// `Some(Evidence { confidence: Verified, items: [] })` (`[]`) means LSP confirmed zero callers.
    pub incoming: Option<Evidence<ImpactReference>>,
    /// Symbols the target calls (callee graph).
    /// `null` when `degraded` (LSP was unavailable and no heuristic fallback matched) -- callees are UNKNOWN.
    /// `Some(Evidence { confidence: Verified, items: [] })` (`[]`) means LSP confirmed zero callees.
    pub outgoing: Option<Evidence<ImpactReference>>,
    /// Heuristic candidates when status is `NotReady`. Never populated when `Ok`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heuristic_candidates: Option<Vec<ImpactReference>>,
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
    /// Next offset for pagination when truncated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<u32>,
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
    /// Tool status.
    #[serde(default)]
    pub status: ToolStatus,
    /// All reference locations for the target symbol.
    /// `null` when `degraded` is `true` — LSP was unavailable.
    /// `Some(Evidence { confidence: Verified, items: [] })` (`[]`) means LSP confirmed zero references.
    pub references: Option<Evidence<ReferenceLocation>>,
    /// Heuristic candidates when status is `NotReady`. Never populated when `Ok`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heuristic_candidates: Option<Vec<ReferenceLocation>>,
    /// Total number of references found (before pagination).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_references: Option<usize>,
    /// Whether the results were truncated due to `max_results`.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub truncated: bool,
    /// Next offset for pagination when truncated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<u32>,
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
    /// Confidence level of this reference.
    pub confidence: Confidence,
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

/// Helper to skip serializing false values for boolean fields.
#[allow(clippy::trivially_copy_pass_by_ref)] // Required by serde's skip_serializing_if signature
pub(crate) fn is_false(b: &bool) -> bool {
    !b
}

// ── Consolidated Tool Parameter Types ───────────────────────────────
//
// Parameter structs for the 3 surviving tools (inspect, locate, trace).

/// Parameters for inspect
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[allow(missing_docs)]
pub struct InspectParams {
    #[serde(default)]
    pub semantic_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_paths: Option<Vec<String>>,
    #[serde(default)]
    pub include_dependencies: bool,
    #[serde(default = "default_max_dependencies")]
    pub max_dependencies: u32,
    #[serde(default)]
    pub include_imports: bool,
    #[serde(default)]
    pub retry_attempt: u32,
}

impl Default for InspectParams {
    fn default() -> Self {
        Self {
            semantic_path: None,
            semantic_paths: None,
            include_dependencies: false,
            max_dependencies: default_max_dependencies(),
            include_imports: false,
            retry_attempt: 0,
        }
    }
}

/// A single entry in a batch locate request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[allow(missing_docs)]
pub struct LocateEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

/// Parameters for locate
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[allow(missing_docs)]
pub struct LocateParams {
    #[serde(default)]
    pub semantic_path: Option<String>,
    #[serde(default, alias = "path")]
    pub file: Option<String>,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locations: Option<Vec<LocateEntry>>,
    #[serde(default)]
    pub retry_attempt: u32,
}

/// Result entry for a single inspect operation in a batch.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct InspectResultEntry {
    /// Semantic path inspected.
    pub semantic_path: String,
    /// Operation status ("ok" or "error").
    pub status: String,
    /// Confidence of this inspect result.
    pub confidence: Confidence,
    /// Source code content if successful.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Start line of symbol definition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_line: Option<usize>,
    /// End line of symbol definition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
    /// Detected language.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Symbol dependencies if requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<Vec<DeepContextDependency>>,
    /// LSP readiness signal at the time of the call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readiness: Option<String>,
    /// Degraded reason if degraded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<DegradedReason>,
    /// Error message if status is "error".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Response from a batch `inspect` tool call.
#[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct BatchInspectResult {
    /// Tool status.
    #[serde(default)]
    pub status: ToolStatus,
    /// Batch inspect result entries.
    pub results: Vec<InspectResultEntry>,
    /// Number of succeeded inspections.
    pub succeeded: usize,
    /// Number of failed inspections.
    pub failed: usize,
    /// Number of heuristic inspections.
    pub heuristic_count: usize,
    /// Total duration of batch inspection in milliseconds.
    pub total_duration_ms: u64,
}

/// Result entry for a single locate operation in a batch.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct LocateResultEntry {
    /// Echo back input for correlation.
    pub input: LocateEntry,
    /// Operation status ("ok" or "error").
    pub status: String,
    /// Confidence of this locate result.
    pub confidence: Confidence,
    /// Resolved file path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Resolved line number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Resolved column number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    /// Resolved semantic path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_path: Option<String>,
    /// Preview snippet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    /// Strategy used for resolution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_strategy: Option<String>,
    /// LSP readiness signal at the time of the call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readiness: Option<String>,
    /// Degraded reason if degraded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<DegradedReason>,
    /// Error message if status is "error".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Response from a batch `locate` tool call.
#[derive(Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct BatchLocateResult {
    /// Tool status.
    #[serde(default)]
    pub status: ToolStatus,
    /// Batch locate result entries.
    pub results: Vec<LocateResultEntry>,
    /// Number of succeeded locations.
    pub succeeded: usize,
    /// Number of failed locations.
    pub failed: usize,
    /// Number of heuristic locations.
    pub heuristic_count: usize,
    /// Total duration of batch location in milliseconds.
    pub total_duration_ms: u64,
}

/// Scope for trace
#[derive(Debug, Clone, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum TraceScope {
    #[default]
    Callers,
    References,
    Overview,
}

/// Parameters for trace
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[allow(missing_docs)]
pub struct TraceParams {
    pub semantic_path: String,
    #[serde(default)]
    pub scope: TraceScope,
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
    #[serde(default = "default_max_references")]
    pub max_references: u32,
    #[serde(default)]
    pub offset: u32,
    #[serde(default)]
    pub retry_attempt: u32,
}

impl Default for TraceParams {
    fn default() -> Self {
        Self {
            semantic_path: String::default(),
            scope: TraceScope::default(),
            max_depth: default_max_depth(),
            max_references: default_max_references(),
            offset: 0,
            retry_attempt: 0,
        }
    }
}

// ── Default Value Functions ─────────────────────────────────────────

/// Default max depth (1).
#[must_use]
pub const fn default_max_depth() -> u32 {
    1
}
/// Default max references (20).
#[must_use]
pub const fn default_max_references() -> u32 {
    20
}
/// Default max dependencies (50).
#[must_use]
pub const fn default_max_dependencies() -> u32 {
    50
}

#[cfg(test)]
#[path = "types_test.rs"]
mod tests;
