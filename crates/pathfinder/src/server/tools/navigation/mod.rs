//! Navigation tool handlers: `locate`, `trace`, and `inspect`.
//!
//! Internal `_impl` methods: `get_definition_impl`, `find_callers_callees_impl`,
//! `read_with_deep_context_impl`, `find_all_references_impl`, `symbol_overview_impl`.
//!
//! All are LSP-powered but degrade gracefully when no language server is
//! available. The tool responses include `"degraded": true` and
//! `"degraded_reason"` fields to signal the fallback mode to agents.
//!
//! # Degraded Mode
//! When the `Lawyer` returns `LspError::NoLspAvailable`:
//! - `locate` — returns an error response (`LSP_REQUIRED`)
//! - `trace(scope="callers")` — returns `null` caller/callee lists with `degraded: true`
//! - `inspect(include_dependencies=true)` — returns the symbol scope only, no dependencies

use crate::server::helpers::{
    invalid_params_error, millis_to_u64, parse_semantic_path, pathfinder_to_error_data,
    treesitter_error_to_error_data,
};
use crate::server::types::{
    BatchLocateResult, GetDefinitionResponse, GetSemanticPathResult, HealthParams, LocateParams,
    LocateResultEntry, TraceParams, TraceScope,
};
use crate::server::PathfinderServer;
use pathfinder_common::types::DegradedReason;
use rmcp::model::{CallToolResult, ErrorData};
use std::fmt::Write as _;

mod deep_context;
mod definition;
mod health;
mod impact;
mod overview;
mod references;
#[cfg(test)]
mod test_helpers;

/// File extensions considered source code for grep fallback filtering.
///
/// When the LSP is unavailable and we fall back to text search, we only
/// want results from actual source files, not documentation (.md), config
/// (.json, .yaml, .toml), or other non-source files.
const SOURCE_FILE_EXTENSIONS: &[&str] = &[
    "rs",   // Rust
    "go",   // Go
    "ts",   // TypeScript
    "tsx",  // TypeScript + JSX
    "js",   // JavaScript
    "jsx",  // JavaScript + JSX
    "mjs",  // JavaScript (ESM module)
    "cjs",  // JavaScript (CommonJS)
    "py",   // Python
    "pyi",  // Python type stub
    "vue",  // Vue Single-File Component
    "java", // Java
];

/// Returns `true` if the file path has a source code extension.
///
/// Used to filter out non-source files (docs, configs) from grep fallback
/// search results to reduce false positives.
fn is_source_file(file: &str) -> bool {
    let ext = std::path::Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    SOURCE_FILE_EXTENSIONS.contains(&ext)
}

/// Returns `true` if the file looks like it's from the user's workspace (not external/dependencies).
///
/// Uses heuristic string matching — does not perform filesystem I/O — to avoid per-reference
/// latency overhead in BFS traversal. Filters out:
/// - Absolute paths (Unix `/`, Windows `\` or `C:\`)
/// - Paths containing `node_modules/` or `vendor/` (dependency directories)
/// - Known Rust stdlib root paths (`std/`, `core/`, `alloc/`)
/// - Non-source-code files (checked via [`is_source_file`])
fn is_workspace_file(file: &str) -> bool {
    // Filter out absolute paths (stdlib, SDK files)
    // Unix: starts with `/`
    // Windows: starts with `\` or has `:` at position 1 (e.g., `C:\`)
    if file.starts_with('/') || file.starts_with('\\') {
        return false;
    }
    // Check for Windows-style absolute paths like `C:\` or `D:/`
    if file.len() >= 2 {
        let second_char = file.chars().nth(1);
        if second_char == Some(':') {
            return false;
        }
    }
    // Filter out dependency directories
    if file.contains("node_modules/")
        || file.contains("node_modules\\")
        || file.contains("vendor/")
        || file.contains("vendor\\")
    {
        return false;
    }
    // Filter out known Rust stdlib paths
    if file.starts_with("std/")
        || file.starts_with("core/")
        || file.starts_with("alloc/")
        || file.starts_with("library/std/")
        || file.starts_with("library/core/")
        || file.starts_with("library/alloc/")
        || file.starts_with("library\\std\\")
        || file.starts_with("library\\core\\")
        || file.starts_with("library\\alloc\\")
        || file == "std"
        || file == "core"
        || file == "alloc"
    {
        return false;
    }
    // Must be a source code file to be considered a workspace file
    // This filters out docs, configs, and other non-source files
    is_source_file(file)
}

/// Result of LSP call-hierarchy resolution for `read_with_deep_context`.
struct LspResolution {
    dependencies: Vec<crate::server::types::DeepContextDependency>,
    degraded: bool,
    degraded_reason: Option<DegradedReason>,
    engines: Vec<&'static str>,
    dependencies_truncated: bool,
}

static CALL_PATTERN_FULL: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();

#[allow(clippy::expect_used)]
fn call_pattern_full() -> &'static regex::Regex {
    CALL_PATTERN_FULL.get_or_init(|| {
        regex::Regex::new(r"\b([a-zA-Z_][a-zA-Z0-9_]*)\s*\(|\.([a-zA-Z_][a-zA-Z0-9_]*)\s*\(")
            .expect("call pattern full is valid regex")
    })
}

/// PATCH-005: Extract function call patterns from symbol body using language-aware regex.
///
/// Returns candidate function names that might be called by this symbol.
/// Filters out language keywords and caps at 20 candidates.
///
/// GAP 3 FIX: Uses `call_pattern_full()` for ALL languages to capture method calls
/// like `self.validate()`, `s.HandleRequest()`, `service.process()` in Rust/Go/Java.
/// Previously only TS/JS/Python/Vue used the full pattern.
fn extract_call_candidates(symbol_content: &str, language: &str) -> Vec<String> {
    let re = call_pattern_full();

    let keywords = keywords_for_language(language);

    let mut candidates = std::collections::HashSet::new();

    for caps in re.captures_iter(symbol_content) {
        let name = caps.get(1).or_else(|| caps.get(2));
        if let Some(m) = name {
            let name = m.as_str();
            if !keywords.contains(&name) {
                candidates.insert(name.to_owned());
            }
        }
    }

    let mut result: Vec<String> = candidates.into_iter().collect();
    result.truncate(20);
    result
}

/// Extract the last segment name from a semantic path's symbol chain.
///
/// Returns `None` if the semantic path has no symbol chain or the chain is empty.
/// Used by `find_callers_callees` and `read_with_deep_context` to get the symbol name
/// for grep-based fallback searches.
pub(crate) fn last_symbol_name(
    semantic_path: &pathfinder_common::types::SemanticPath,
) -> Option<String> {
    semantic_path
        .symbol_chain
        .as_ref()
        .and_then(|c| c.segments.last())
        .map(|s| s.name.clone())
}

/// Returns language keywords to filter out from call-candidate extraction.
#[expect(
    clippy::too_many_lines,
    reason = "keyword lookup table; large match is natural"
)]
fn keywords_for_language(language: &str) -> &'static [&'static str] {
    match language {
        "rust" => &[
            "if", "else", "for", "while", "loop", "match", "return", "break", "continue", "let",
            "mut", "const", "static", "struct", "enum", "trait", "impl", "fn", "type", "where",
            "use", "mod", "pub", "crate", "super", "self", "Self", "move", "ref", "unsafe",
            "async", "await",
        ],
        "go" => &[
            "if",
            "else",
            "for",
            "range",
            "switch",
            "case",
            "default",
            "return",
            "break",
            "continue",
            "go",
            "defer",
            "goto",
            "fallthrough",
            "select",
            "chan",
            "make",
            "new",
            "func",
            "type",
            "var",
            "const",
            "struct",
            "interface",
            "import",
            "package",
        ],
        "typescript" | "javascript" => &[
            "if",
            "else",
            "for",
            "while",
            "switch",
            "case",
            "break",
            "continue",
            "return",
            "function",
            "class",
            "interface",
            "type",
            "const",
            "let",
            "var",
            "new",
            "this",
            "super",
            "static",
            "async",
            "await",
            "import",
            "export",
            "from",
            "as",
            "try",
            "catch",
            "finally",
            "throw",
            "yield",
            "typeof",
            "instanceof",
            "in",
        ],
        "python" => &[
            "if", "elif", "else", "for", "while", "def", "class", "return", "break", "continue",
            "yield", "async", "await", "import", "from", "as", "try", "except", "finally", "raise",
            "with", "lambda", "global", "nonlocal", "assert", "pass",
        ],
        "java" => &[
            "if",
            "else",
            "for",
            "while",
            "switch",
            "case",
            "break",
            "continue",
            "return",
            "try",
            "catch",
            "finally",
            "throw",
            "new",
            "class",
            "interface",
            "extends",
            "implements",
            "instanceof",
            "import",
            "package",
            "void",
            "int",
            "long",
            "float",
            "double",
            "boolean",
            "char",
            "byte",
            "short",
            "final",
            "static",
            "synchronized",
            "native",
            "this",
            "super",
            "assert",
        ],
        "vue" => &[
            "if",
            "else",
            "for",
            "while",
            "switch",
            "case",
            "break",
            "continue",
            "return",
            "function",
            "class",
            "interface",
            "type",
            "const",
            "let",
            "var",
            "new",
            "this",
            "super",
            "static",
            "async",
            "await",
            "import",
            "export",
            "from",
            "as",
            "try",
            "catch",
            "finally",
            "throw",
            "yield",
            "typeof",
            "instanceof",
            "in",
            "defineProps",
            "defineEmits",
            "defineExpose",
            "defineModel",
            "withDefaults",
            "ref",
            "reactive",
            "computed",
            "watch",
            "watchEffect",
            "onMounted",
            "onUnmounted",
            "provide",
            "inject",
            "toRef",
            "toRefs",
            "useSlots",
            "useAttrs",
            "useTemplateRef",
            "template",
            "script",
            "style",
            "setup",
        ],
        _ => &["if", "else", "for", "while", "return", "break", "continue"],
    }
}

/// PATCH-005: Map tree-sitter language ID to file glob pattern.
///
/// Used by `resolve_candidate_via_grep` to search for definition files.
fn language_to_file_glob(language: &str) -> &str {
    match language {
        "rust" => "**/*.rs",
        "typescript" | "tsx" => "**/*.{ts,tsx}",
        "javascript" => "**/*.{js,jsx}",
        "python" => "**/*.py",
        "go" => "**/*.go",
        "vue" => "**/*.{vue,ts,tsx,js,jsx,mjs,cjs}",
        "java" => "**/*.java",
        _ => "**/*",
    }
}

/// DELIVERABLE F: Java-specific regex pattern for resolving candidate definitions.
///
/// Matches Java method definitions, constructors, records, and class/interface declarations.
/// Used by grep fallback for outgoing dependency discovery.
fn java_resolve_pattern(candidate: &str) -> String {
    let escaped = regex::escape(candidate);
    format!(
        r"(?:^[ \t]*(?:(?:public|private|protected)\s+)?(?:<[^>]*?(?:<[^>]*?>[^>]*?)*>\s+)?{escaped}\s*\(|^[ \t]*(?:@\w+(?:\([^)]*\))?\s+)*(?:(?:public|private|protected|static|final|abstract|synchronized|native|default|strictfp)\s+)*(?:<[^>]*?(?:<[^>]*?>[^>]*?)*>\s+)?(?:(?:void|boolean|int|long|double|float|short|byte|char)|[A-Z][a-zA-Z0-9_]*(?:<[^>]*?(?:<[^>]*?>[^>]*?)*>)?)(?:\[\])*\s+{escaped}\s*\(|^[ \t]*(?:(?:public|private|protected|static|final)\s+)*record\s+{escaped}\s*[<(]|(?:public\s+|private\s+|protected\s+|static\s+|final\s+|abstract\s+|sealed\s+|non-sealed\s+|strictfp\s+)*(?:class|interface|enum|@interface)\s+{escaped}\b)"
    )
}

/// DELIVERABLE F: Build a regex pattern to find a candidate function's definition.
///
/// Used by grep fallback for outgoing dependency discovery in both
/// `inspect` (deep context) and `trace` (callers/callees).
fn candidate_definition_pattern(language: &str, candidate: &str) -> String {
    let escaped = regex::escape(candidate);
    match language {
        // The outer non-capturing group is intentionally closed at the end so
        // regex::Regex::new() succeeds and optional pub/async qualifiers are grouped.
        "rust" => format!(r"(?:(?:pub\s*(?:\([^)]*\)\s*)?(?:async\s*)?)?fn\s+{escaped}\b)"),
        "go" => format!(r"func\s+{escaped}\b"),
        "typescript" | "tsx" | "javascript" | "vue" => {
            format!(
                r"(?:(?:export\s+(?:default\s*)?)?function\s+{escaped}\b|(?:export\s+)?(?:const|let|var)\s+{escaped}\s*[=:]|(?:{escaped}\s*:\s*)[^{{]*\([^)]*\)\s*=>)"
            )
        }
        "python" => format!(r"(?:async\s+)?def\s+{escaped}\b"),
        "java" => java_resolve_pattern(candidate),
        _ => format!(r"\b(?:fn|def|function|class|struct|type|interface)\s+{escaped}\b"),
    }
}

/// SPEC 007: Language-aware regex patterns for definition search.
///
/// Returns language-specific regex patterns for finding symbol definitions.
/// Used by `find_symbol` and `fallback_definition_grep` for grep-based fallbacks.
///
/// # Patterns
///
/// | Extension | Patterns |
/// |-----------|----------|
/// | `rs` | Functions (`fn`), types (`struct`, `enum`, `trait`, `type`, `mod`), constants (`const`, `static`) |
/// | `ts`/`tsx`/`js`/`jsx` | Functions (`function`), classes (`class`, `interface`, `type`, `enum`), variable declarations |
/// | `py` | Functions (`def`), classes (`class`), module-level assignments |
/// | `go` | Functions (`func`), types (`type`), constants (`const`, `var`) |
/// | Other | Bare word boundary fallback |
///
pub(crate) fn definition_patterns(ext: &str, symbol_name: &str) -> Vec<String> {
    let name = regex::escape(symbol_name);
    match ext {
        "invalid_regex" => vec!["[invalid".to_string()],
        "rs" => vec![
            format!(
                r"(?:pub\s*(?:\([^)]*\)\s*)?)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?fn\s+{name}\b"
            ),
            format!(r"(?:pub\s*(?:\([^)]*\)\s*)?)?(?:struct|enum|trait|type|mod)\s+{name}\b"),
            format!(r"(?:pub\s*(?:\([^)]*\)\s*)?)?(?:const|static)\s+{name}\b"),
            format!(r"macro_rules!\s+{name}\b"),
        ],
        "ts" | "tsx" | "js" | "jsx" => vec![
            format!(r"(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s+{name}\b"),
            format!(
                r"(?:export\s+)?(?:default\s+)?(?:abstract\s+)?(?:class|interface|type|enum)\s+{name}\b"
            ),
            format!(r"(?:export\s+)?(?:const|let|var)\s+{name}\s*[=:]"),
            format!(
                r"(?:export\s+)?(?:const|let|var)\s+{name}\s*=\s*(?:async\s+)?\([^)]*\)\s*(?::\s*[^=]+)?\s*=>"
            ),
        ],
        "py" => vec![
            format!(r"(?:async\s+)?def\s+{name}\b"),
            format!(r"class\s+{name}\b"),
            format!(r"^{name}\s*[=:]"),
        ],
        "go" => vec![
            format!(r"func\s+(?:\([^)]+\)\s+)?{name}\b"),
            format!(r"func\s+(?:\([^)]*\[[^\]]*\][^)]*\)\s+)?{name}\b"),
            format!(r"type\s+{name}\s+"),
            format!(r"type\s+{name}\s*\["),
            format!(r"(?:const|var)\s+{name}\b"),
        ],
        "java" => {
            let parent = name.clone();
            vec![
                // P0: Class/interface/enum definitions (sealed, non-sealed, strictfp handled via modifier)
                format!(
                    r"(?:public\s+|private\s+|protected\s+|static\s+|final\s+|abstract\s+|sealed\s+|non-sealed\s+|strictfp\s+)*(?:class|interface|enum|@interface)\s+{name}\b"
                ),
                // P1: Constructor — no return type, only modifiers + optional type-params before name.
                // Line-anchored (^) prevents matching `throw new MyClass(` or `return new MyClass(`.
                format!(
                    r"^[ \t]*(?:(?:public|private|protected)\s+)?(?:<[^>]*?(?:<[^>]*?>[^>]*?)*>\s+)?{parent}\s*\("
                ),
                // P2: Record type — keyword `record` prevents false positives.
                format!(
                    r"^[ \t]*(?:(?:public|private|protected|static|final)\s+)*record\s+{name}\s*[<(]"
                ),
                // P3: Method — return type must be primitive keyword or start uppercase.
                // `new`/`throw` are lowercase non-primitives → rejected. No \s in type token.
                format!(
                    r"^[ \t]*(?:@\w+(?:\([^)]*\))?\s+)*(?:(?:public|private|protected|static|final|abstract|synchronized|native|default|strictfp)\s+)*(?:<[^>]*?(?:<[^>]*?>[^>]*?)*>\s+)?(?:(?:void|boolean|int|long|double|float|short|byte|char)|[A-Z][a-zA-Z0-9_]*(?:<[^>]*?(?:<[^>]*?>[^>]*?)*>)?)(?:\[\])*\s+{name}\s*\("
                ),
            ]
        }
        "vue" => vec![
            format!(r"(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s+{name}\b"),
            format!(
                r"(?:export\s+)?(?:default\s+)?(?:abstract\s+)?(?:class|interface|type|enum)\s+{name}\b"
            ),
            format!(r"(?:export\s+)?(?:const|let|var)\s+{name}\s*[=:]"),
            format!(
                r"(?:export\s+)?(?:const|let|var)\s+{name}\s*=\s*(?:async\s+)?\([^)]*\)\s*(?::\s*[^=]+)?\s*=>"
            ),
            format!(
                r"(?:const|let)\s+{name}\s*=\s*(?:defineProps|defineEmits|defineExpose|defineModel|withDefaults)[(<]"
            ),
        ],
        _ => vec![format!(r"\b{name}\b")],
    }
}

impl PathfinderServer {
    /// Enrich a flat LSP-derived name into a fully-qualified treesitter semantic path.
    ///
    /// LSP call-hierarchy returns bare symbol names (e.g. `"handle_request"`), but
    /// treesitter semantic paths require the qualified dot-chain for nested symbols
    /// (e.g. `"Server.handle_request"`). This method calls `Surgeon::enclosing_symbol_detail`
    /// at the given file+line to derive the authoritative qualified chain.
    ///
    /// Falls back silently to `file::flat_name` when:
    /// - Surgeon returns `Ok(None)` (top-level symbol with no enclosing context), OR
    /// - Surgeon returns an error (parse failure, unsupported language, etc.)
    ///
    /// The fallback ensures BFS traversal is never blocked by treesitter failures.
    pub(crate) async fn enrich_semantic_path(
        &self,
        file: &str,
        line: u32,
        flat_name: &str,
    ) -> String {
        let file_path = std::path::Path::new(file);
        // LSP lines are 1-indexed; Surgeon's enclosing_symbol_detail also takes 1-indexed.
        let line_1indexed = usize::try_from(line).unwrap_or(0);
        match self
            .surgeon
            .enclosing_symbol_detail(self.workspace_root.path(), file_path, line_1indexed)
            .await
        {
            Ok(Some(sym)) if !sym.semantic_path.is_empty() => {
                format!("{}::{}", file, sym.semantic_path)
            }
            Ok(_) | Err(_) => format!("{file}::{flat_name}"),
        }
    }

    /// Spec 2.2 + 2.3: Enrich `did_you_mean` suggestions with:
    /// 1. Separator correction (:: → . within symbol chain)
    /// 2. Cross-file search via `find_symbol` when same-file suggestions empty
    async fn enrich_did_you_mean(
        &self,
        semantic_path_str: &str,
        original_suggestions: Vec<String>,
    ) -> Vec<String> {
        let mut suggestions = original_suggestions;

        // Spec 2.2: Separator confusion correction
        // If multiple '::' used, suggest the '.' variant
        let parts: Vec<&str> = semantic_path_str.splitn(2, "::").collect();
        if parts.len() == 2 {
            let file_part = parts[0];
            let symbol_part = parts[1];
            if symbol_part.contains("::") {
                let corrected_symbol = symbol_part.replace("::", ".");
                let corrected_path = format!("{file_part}::{corrected_symbol}");
                if !suggestions.contains(&corrected_path) {
                    suggestions.insert(0, corrected_path);
                }
            }
        }

        // Spec 2.3: Cross-file search when same-file suggestions empty
        if suggestions.is_empty() {
            if let Ok(semantic_path) = parse_semantic_path(semantic_path_str) {
                if let Some(chain) = &semantic_path.symbol_chain {
                    if let Some(base_name) = chain.segments.last() {
                        // Use find_symbol to search across files
                        let find_params = crate::server::types::SearchParams {
                            query: base_name.name.clone(),
                            mode: crate::server::types::SearchMode::Symbol,
                            kind: None,
                            path_glob: "**/*".to_owned(),
                            max_results: 3,
                            ..Default::default()
                        };
                        match self.find_symbol_impl(find_params).await {
                            Ok(response) => {
                                for symbol in response.0.symbols {
                                    if !suggestions.contains(&symbol.semantic_path) {
                                        suggestions.push(symbol.semantic_path);
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    symbol = %base_name.name,
                                    error = %e,
                                    "enrich_did_you_mean: cross-file search failed — \
                                     agent will receive empty suggestions"
                                );
                            }
                        }
                    }
                }
            }
        }

        suggestions
    }

    /// Wrapper around `surgeon.read_symbol_scope` that enriches `SymbolNotFound` errors
    /// with separator correction (Spec 2.2) and cross-file search (Spec 2.3).
    async fn read_symbol_scope_enriched(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        semantic_path_str: &str,
    ) -> Result<pathfinder_common::types::SymbolScope, ErrorData> {
        match self
            .surgeon
            .read_symbol_scope(self.workspace_root.path(), semantic_path)
            .await
        {
            Ok(scope) => Ok(scope),
            Err(pathfinder_treesitter::SurgeonError::SymbolNotFound { path, did_you_mean }) => {
                let enriched = self
                    .enrich_did_you_mean(semantic_path_str, did_you_mean)
                    .await;

                // Auto-retry: if the symbol part contains '::' (Rust impl method
                // convention uses '.' not '::'), try the corrected path before
                // returning the error. This eliminates the 3-step retry cycle
                // agents currently experience.
                if let Some(corrected) = Self::try_separator_correction(semantic_path_str) {
                    if let Some(corrected_path) =
                        pathfinder_common::types::SemanticPath::parse(&corrected)
                    {
                        if let Ok(scope) = self
                            .surgeon
                            .read_symbol_scope(self.workspace_root.path(), &corrected_path)
                            .await
                        {
                            tracing::info!(
                                original = %semantic_path_str,
                                corrected = %corrected,
                                "read_symbol_scope: auto-corrected '::' to '.' in symbol path"
                            );
                            return Ok(scope);
                        }
                    }
                }

                Err(pathfinder_to_error_data(
                    &pathfinder_common::error::PathfinderError::SymbolNotFound {
                        semantic_path: path,
                        did_you_mean: enriched,
                        retry_after_seconds: None,
                    },
                ))
            }
            Err(e) => Err(treesitter_error_to_error_data(e)),
        }
    }

    /// Consolidated `locate` handler.
    pub(crate) async fn locate_impl(
        &self,
        params: LocateParams,
    ) -> Result<CallToolResult, ErrorData> {
        if let Some(ref locations) = params.locations {
            // Mutual exclusion: batch mode cannot be combined with single-mode params
            if params.semantic_path.is_some() || params.file.is_some() || params.line.is_some() {
                return Err(invalid_params_error(
                    "provide either `locations` (batch) or single-mode params (`semantic_path` / `file`+`line`), not both",
                ));
            }
            self.locate_impl_batch(locations).await
        } else {
            self.locate_impl_single(params).await
        }
    }

    async fn locate_entry(&self, entry: crate::server::types::LocateEntry) -> LocateResultEntry {
        let single_params = LocateParams {
            semantic_path: entry.semantic_path.clone(),
            file: entry.file.clone(),
            line: entry.line,
            locations: None,
        };
        let is_semantic_path = entry.semantic_path.is_some();
        let res = self.locate_impl_single(single_params).await;
        match res {
            Ok(call_res) => {
                let val = call_res
                    .structured_content
                    .unwrap_or(serde_json::Value::Null);
                if is_semantic_path {
                    match serde_json::from_value::<GetDefinitionResponse>(val) {
                        Ok(meta) => LocateResultEntry {
                            input: entry,
                            status: "ok".to_string(),
                            file: Some(meta.file),
                            line: Some(meta.line),
                            column: Some(meta.column),
                            semantic_path: None,
                            preview: Some(meta.preview),
                            resolution_strategy: meta.resolution_strategy,
                            error: None,
                        },
                        Err(e) => LocateResultEntry {
                            input: entry,
                            status: "error".to_string(),
                            file: None,
                            line: None,
                            column: None,
                            semantic_path: None,
                            preview: None,
                            resolution_strategy: None,
                            error: Some(format!("failed to deserialize metadata: {e}")),
                        },
                    }
                } else {
                    match serde_json::from_value::<GetSemanticPathResult>(val) {
                        Ok(meta) => LocateResultEntry {
                            input: entry,
                            status: "ok".to_string(),
                            file: Some(meta.file),
                            line: Some(meta.line),
                            column: None,
                            semantic_path: meta.semantic_path,
                            preview: None,
                            resolution_strategy: None,
                            error: None,
                        },
                        Err(e) => LocateResultEntry {
                            input: entry,
                            status: "error".to_string(),
                            file: None,
                            line: None,
                            column: None,
                            semantic_path: None,
                            preview: None,
                            resolution_strategy: None,
                            error: Some(format!("failed to deserialize metadata: {e}")),
                        },
                    }
                }
            }
            Err(err) => LocateResultEntry {
                input: entry,
                status: "error".to_string(),
                file: None,
                line: None,
                column: None,
                semantic_path: None,
                preview: None,
                resolution_strategy: None,
                error: Some(err.message.to_string()),
            },
        }
    }

    async fn locate_impl_batch(
        &self,
        locations: &[crate::server::types::LocateEntry],
    ) -> Result<CallToolResult, ErrorData> {
        if locations.is_empty() {
            return Err(invalid_params_error("`locations` must not be empty"));
        }
        if locations.len() > 10 {
            return Err(invalid_params_error(
                "`locations` must contain at most 10 entries",
            ));
        }

        let start = std::time::Instant::now();
        let mut futures = Vec::new();
        for entry in locations {
            let server = self.clone();
            let entry = entry.clone();
            futures.push(async move { server.locate_entry(entry).await });
        }

        // Process entries sequentially to guarantee deterministic ordering.
        // Each entry performs heavy I/O (LSP + tree-sitter), and the max batch
        // size is 10, so sequential execution has negligible latency impact.
        let mut results = Vec::with_capacity(futures.len());
        for fut in futures {
            results.push(fut.await);
        }

        let mut succeeded = 0;
        let mut failed = 0;
        for r in &results {
            if r.status == "ok" {
                succeeded += 1;
            } else {
                failed += 1;
            }
        }

        let total_duration_ms = millis_to_u64(start.elapsed().as_millis());
        let response = BatchLocateResult {
            results,
            succeeded,
            failed,
            total_duration_ms,
        };

        let mut text_parts = Vec::new();
        for entry in &response.results {
            let input_str = if let Some(ref sp) = entry.input.semantic_path {
                format!("semantic_path \"{sp}\"")
            } else {
                format!(
                    "file \"{}\" line {}",
                    entry.input.file.as_deref().unwrap_or(""),
                    entry.input.line.unwrap_or(0)
                )
            };

            if entry.status == "ok" {
                if entry.input.semantic_path.is_some() {
                    let mut resolved = format!(
                        "{} -> {}:L{}",
                        input_str,
                        entry.file.as_deref().unwrap_or(""),
                        entry.line.unwrap_or(0)
                    );
                    if let Some(col) = entry.column {
                        let _ = write!(resolved, " col:{col}");
                    }
                    if let Some(ref prev) = entry.preview {
                        if !prev.is_empty() {
                            let _ = write!(resolved, " — {prev}");
                        }
                    }
                    text_parts.push(resolved);
                } else {
                    text_parts.push(format!(
                        "{} -> {}",
                        input_str,
                        entry
                            .semantic_path
                            .as_deref()
                            .unwrap_or("(no enclosing symbol)")
                    ));
                }
            } else {
                text_parts.push(format!(
                    "{} -> error: {}",
                    input_str,
                    entry.error.as_deref().unwrap_or("unknown error")
                ));
            }
        }

        text_parts.push(format!(
            "[completed in {}ms, {}/{} locations resolved]",
            total_duration_ms,
            succeeded,
            succeeded + failed
        ));

        let mut call_result =
            CallToolResult::success(vec![rmcp::model::Content::text(text_parts.join("\n"))]);
        call_result.structured_content = crate::server::helpers::serialize_metadata(&response);
        Ok(call_result)
    }

    /// Single `locate` implementation helper.
    pub(crate) async fn locate_impl_single(
        &self,
        params: LocateParams,
    ) -> Result<CallToolResult, ErrorData> {
        match (params.semantic_path.as_ref(), params.file.as_ref(), params.line) {
            // Definition lookup
            (Some(_), None, None) => {
                self.get_definition_impl(params).await
            }
            // Semantic path resolution
            (None, Some(_), Some(_)) => {
                self.get_semantic_path_impl(params).await
            }
            // Ambiguous: both modes specified
            (Some(_), Some(_), _) | (Some(_), _, Some(_)) => Err(invalid_params_error(
                "provide either `semantic_path` (definition lookup) or `file`+`line` (semantic path resolution), not both",
            )),
            // Missing required fields for semantic path mode
            (None, Some(_), None) => Err(invalid_params_error(
                "`line` is required when using `file` for semantic path resolution",
            )),
            (None, None, Some(_)) => Err(invalid_params_error(
                "`file` is required when using `line` for semantic path resolution",
            )),
            // Nothing provided
            (None, None, None) => Err(invalid_params_error(
                "provide either `semantic_path` or `file`+`line`",
            )),
        }
    }

    /// Consolidated `trace` handler.
    pub(crate) async fn trace_impl(
        &self,
        params: TraceParams,
    ) -> Result<CallToolResult, ErrorData> {
        match params.scope {
            TraceScope::Callers => self.find_callers_callees_impl(params).await,
            TraceScope::References => self.find_all_references_impl(params).await,
            TraceScope::Overview => self.symbol_overview_impl(params).await,
        }
    }

    /// Consolidated `health` handler.
    pub(crate) async fn health_impl(
        &self,
        params: HealthParams,
    ) -> Result<CallToolResult, ErrorData> {
        self.lsp_health_impl(params).await
    }

    fn try_separator_correction(semantic_path_str: &str) -> Option<String> {
        let (file_part, symbol_part) = semantic_path_str.split_once("::")?;
        if !symbol_part.contains("::") {
            return None;
        }
        let corrected_symbol = symbol_part.replace("::", ".");
        Some(format!("{file_part}::{corrected_symbol}"))
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
