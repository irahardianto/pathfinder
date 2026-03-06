//! Tool parameter and response types for Pathfinder MCP tools.
//!
//! These structs are deserialized by the rmcp framework from MCP tool call
//! payloads. The `dead_code` lint fires for stub tools whose fields aren't
//! accessed yet; the allow will be removed as tools are implemented.

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
    /// Note: `code_only` / `comments_only` require Tree-sitter (Epic 3).
    /// In Epic 2 these modes return unfiltered results with `degraded: true`.
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
    /// Token budget.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Max directory traversal depth.
    #[serde(default = "default_depth")]
    pub depth: u32,
    /// Visibility filter: `public` or `all`.
    #[serde(default = "default_visibility")]
    pub visibility: String,
    /// Import inclusion: `none`, `third_party`, or `all`.
    #[serde(default = "default_include_imports")]
    #[allow(dead_code)]
    pub include_imports: String,
}

/// Parameters for `read_symbol_scope`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
pub struct ReadSymbolScopeParams {
    /// Semantic path (e.g., `src/auth.ts::AuthService.login`).
    pub semantic_path: String,
}

/// Parameters for `read_with_deep_context`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
pub struct ReadWithDeepContextParams {
    /// Semantic path.
    pub semantic_path: String,
}

/// Parameters for `get_definition`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
pub struct GetDefinitionParams {
    /// Semantic path to the reference.
    pub semantic_path: String,
}

/// Parameters for `analyze_impact`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
pub struct AnalyzeImpactParams {
    /// Semantic path to the target.
    pub semantic_path: String,
    /// Traversal depth (max: 5).
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
}

/// Parameters for `replace_body`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
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
#[allow(dead_code)]
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
#[allow(dead_code)]
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
#[allow(dead_code)]
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
#[allow(dead_code)]
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
#[allow(dead_code)]
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
#[allow(dead_code)]
pub struct CreateFileParams {
    /// Relative file path.
    pub filepath: String,
    /// Initial file content.
    pub content: String,
}

/// Parameters for `delete_file`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
pub struct DeleteFileParams {
    /// Relative file path.
    pub filepath: String,
    /// SHA-256 hash from previous read (OCC).
    pub base_version: String,
}

/// Parameters for `read_file`.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
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
#[allow(dead_code)]
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
#[allow(dead_code)]
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

/// A generic response for stubbed tools.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct StubResponse {
    pub error: String,
    pub message: String,
    pub details: std::collections::HashMap<String, String>,
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
    4096
}
pub(crate) fn default_depth() -> u32 {
    3
}
pub(crate) fn default_visibility() -> String {
    "public".to_owned()
}
pub(crate) fn default_include_imports() -> String {
    "third_party".to_owned()
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
