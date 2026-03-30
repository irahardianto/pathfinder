//! Tool parameter and response types for Pathfinder MCP tools.
//!
//! These structs are deserialized by the rmcp framework from MCP tool call
//! payloads. The `dead_code` lint fires for param struct fields that are read
//! by serde (via `Deserialize`) but never accessed by name in production code.
//! A module-level `#![allow]` is used here so that each newly implemented tool
//! can remove its struct's allow without touching unrelated items.
#![allow(dead_code)] // Fields are read by serde deserialization, not by name

use rmcp::schemars;
use rmcp::serde::{self, Serialize};

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
    /// Max directory traversal depth.
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
    /// Semantic path.
    pub semantic_path: String,
}

/// Parameters for `get_definition`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct GetDefinitionParams {
    /// Semantic path to the reference.
    pub semantic_path: String,
}

/// Parameters for `analyze_impact`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct AnalyzeImpactParams {
    /// Semantic path to the target.
    pub semantic_path: String,
    /// Traversal depth (max: 5).
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
}

/// Parameters for `replace_body`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct ReplaceBodyParams {
    /// Full semantic path to the target.
    pub semantic_path: String,
    /// SHA-256 hash from previous read (OCC).
    pub base_version: String,
    /// Replacement body content (without outer braces).
    pub new_code: String,
    /// Write to disk even if validation fails.
    #[serde(default)]
    pub ignore_validation_failures: bool,
}

/// Parameters for `replace_full`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct ReplaceFullParams {
    /// Full semantic path to the target.
    pub semantic_path: String,
    /// SHA-256 hash from previous read (OCC).
    pub base_version: String,
    /// Complete replacement declaration.
    pub new_code: String,
    /// Write to disk even if validation fails.
    #[serde(default)]
    pub ignore_validation_failures: bool,
}

/// Parameters for `insert_before`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct InsertBeforeParams {
    /// Full semantic path or bare file path (for BOF).
    pub semantic_path: String,
    /// SHA-256 hash from previous read (OCC).
    pub base_version: String,
    /// Code block to insert.
    pub new_code: String,
    /// Write to disk even if validation fails.
    #[serde(default)]
    pub ignore_validation_failures: bool,
}

/// Parameters for `insert_after`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct InsertAfterParams {
    /// Full semantic path or bare file path (for EOF).
    pub semantic_path: String,
    /// SHA-256 hash from previous read (OCC).
    pub base_version: String,
    /// Code block to insert.
    pub new_code: String,
    /// Write to disk even if validation fails.
    #[serde(default)]
    pub ignore_validation_failures: bool,
}

/// Parameters for `delete_symbol`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct DeleteSymbolParams {
    /// Full semantic path to the target.
    pub semantic_path: String,
    /// SHA-256 hash from previous read (OCC).
    pub base_version: String,
    /// Write to disk even if validation fails.
    #[serde(default)]
    pub ignore_validation_failures: bool,
}

/// Parameters for `validate_only`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct ValidateOnlyParams {
    /// Full semantic path to the target.
    pub semantic_path: String,
    /// Edit type: `replace_body`, `replace_full`, `insert_before`, `insert_after`, or `delete`.
    pub edit_type: String,
    /// Replacement code (required for all types except `delete`).
    pub new_code: Option<String>,
    /// SHA-256 hash from previous read (OCC).
    pub base_version: String,
}

/// Parameters for `create_file`.
#[derive(Debug, Default, Clone, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateFileParams {
    /// Relative file path.
    pub filepath: String,
    /// Initial file content.
    pub content: String,
}

/// Parameters for `delete_file`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct DeleteFileParams {
    /// Relative file path.
    pub filepath: String,
    /// SHA-256 hash from previous read (OCC).
    pub base_version: String,
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

/// Parameters for `write_file`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct WriteFileParams {
    /// Relative file path.
    pub filepath: String,
    /// SHA-256 hash from previous read (OCC).
    pub base_version: String,
    /// Full replacement content. Mutually exclusive with `replacements`.
    pub content: Option<String>,
    /// Search-and-replace operations. Mutually exclusive with `content`.
    pub replacements: Option<Vec<Replacement>>,
}

/// A search-and-replace operation for `write_file`.
#[derive(Debug, Default, Clone, serde::Deserialize, schemars::JsonSchema)]
pub struct Replacement {
    /// Exact text to find.
    pub old_text: String,
    /// Replacement text.
    pub new_text: String,
}

// ── Response Types ──────────────────────────────────────────────────

/// The response for `search_codebase`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SearchCodebaseResponse {
    pub matches: Vec<pathfinder_search::SearchMatch>,
    pub total_matches: usize,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
}

/// The response for `get_repo_map`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct GetRepoMapResponse {
    pub skeleton: String,
    pub tech_stack: Vec<String>,
    pub files_scanned: usize,
    pub files_truncated: usize,
    pub files_in_scope: usize,
    pub coverage_percent: u8,
    pub version_hashes: std::collections::HashMap<String, String>,
    /// Always `true` while visibility filtering is not yet implemented.
    /// Agents should treat all symbols as public regardless of `visibility` param.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility_degraded: Option<bool>,
}

/// The response for `read_symbol_scope`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ReadSymbolScopeResponse {
    pub content: String,
    pub start_line: usize,
    pub end_line: usize,
    pub version_hash: String,
    pub language: String,
}

/// The response for all AST-aware edit tools:
/// `replace_body`, `replace_full`, `insert_before`, `insert_after`,
/// `delete_symbol`, and `validate_only`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct EditResponse {
    /// Whether the edit succeeded (always `true` for non-`validate_only` tools).
    pub success: bool,
    /// SHA-256 hash of the file after the edit. `None` for `validate_only`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_version_hash: Option<String>,
    /// Whether the code was reformatted (always `false` until LSP formatting is wired).
    pub formatted: bool,
    /// LSP validation result.
    pub validation: EditValidation,
    /// `true` when LSP validation was skipped (no language server available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation_skipped: Option<bool>,
    /// Machine-readable reason why validation was skipped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation_skipped_reason: Option<String>,
}

/// LSP validation result embedded in `EditResponse`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct EditValidation {
    /// `"passed"`, `"failed"`, or `"skipped"`.
    pub status: String,
    /// Errors introduced by the edit.
    pub introduced_errors: Vec<pathfinder_common::error::DiagnosticError>,
    /// Errors resolved by the edit.
    pub resolved_errors: Vec<pathfinder_common::error::DiagnosticError>,
}

impl EditValidation {
    /// Return a skipped validation result (no LSP available).
    #[must_use]
    pub fn skipped() -> Self {
        Self {
            status: "skipped".to_owned(),
            introduced_errors: vec![],
            resolved_errors: vec![],
        }
    }
}

/// The response for `create_file`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct CreateFileResponse {
    pub success: bool,
    pub version_hash: String,
    pub validation: ValidationResult,
}

/// The response for `delete_file`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct DeleteFileResponse {
    pub success: bool,
}

/// The response for `read_file`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ReadFileResponse {
    pub content: String,
    pub start_line: u32,
    pub lines_returned: u32,
    pub total_lines: u32,
    pub truncated: bool,
    pub version_hash: String,
    pub language: String,
}

/// The response for `write_file`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct WriteFileResponse {
    pub success: bool,
    pub new_version_hash: String,
}

/// Validation result for edits.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ValidationResult {
    pub status: String,
    pub introduced_errors: Vec<pathfinder_common::error::DiagnosticError>,
}

// ── Navigation Tool Response Types ─────────────────────────────────

/// A dependency signature extracted for `read_with_deep_context`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
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

/// The response for `read_with_deep_context`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ReadWithDeepContextResponse {
    /// The source code of the requested symbol (same as `read_symbol_scope`).
    pub content: String,
    /// Start line of the symbol (1-indexed).
    pub start_line: usize,
    /// End line of the symbol (1-indexed).
    pub end_line: usize,
    /// OCC version hash for the symbol's file.
    pub version_hash: String,
    /// Detected language.
    pub language: String,
    /// Signatures of all symbols called by this one.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<DeepContextDependency>,
    /// `true` when LSP dependency resolution was unavailable (Tree-sitter only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded: Option<bool>,
    /// Reason for degradation (e.g., `"no_lsp"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded: Option<bool>,
    /// Reason for degradation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
}

/// A single reference in an impact analysis.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ImpactReference {
    /// Semantic path of the referencing/referenced symbol.
    pub semantic_path: String,
    /// File path relative to workspace root.
    pub file: String,
    /// 1-indexed line of the call site or definition.
    pub line: usize,
    /// A short code snippet showing the call site or declaration.
    pub snippet: String,
    /// OCC version hash for this file.
    pub version_hash: String,
}

/// The response for `analyze_impact`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct AnalyzeImpactResponse {
    /// Symbols that call the target (caller graph).
    pub incoming: Vec<ImpactReference>,
    /// Symbols the target calls (callee graph).
    pub outgoing: Vec<ImpactReference>,
    /// Number of transitive levels traversed.
    pub depth_reached: u32,
    /// Total files referenced (for `version_hashes`).
    pub files_referenced: usize,
    /// `true` when LSP call hierarchy was unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded: Option<bool>,
    /// Reason for degradation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
}

// ── Default Value Functions ─────────────────────────────────────────

pub(crate) fn default_path_glob() -> String {
    "**/*".to_owned()
}
pub(crate) fn default_max_results() -> u32 {
    50
}
pub(crate) fn default_context_lines() -> u32 {
    2
}
pub(crate) fn default_repo_map_path() -> String {
    ".".to_owned()
}
pub(crate) fn default_max_tokens() -> u32 {
    16_000
}
pub(crate) fn default_max_tokens_per_file() -> u32 {
    2_000
}
pub(crate) fn default_depth() -> u32 {
    3
}
pub(crate) fn default_max_depth() -> u32 {
    2
}
pub(crate) fn default_start_line() -> u32 {
    1
}
pub(crate) fn default_max_lines() -> u32 {
    500
}
