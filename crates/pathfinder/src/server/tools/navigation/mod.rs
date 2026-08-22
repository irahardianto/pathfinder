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
    BatchLocateResult, GetDefinitionResponse, GetSemanticPathResult, LocateParams,
    LocateResultEntry, TraceParams, TraceScope,
};
use crate::server::PathfinderServer;
use pathfinder_common::types::DegradedReason;
use rmcp::model::{CallToolResult, ErrorData};
use std::fmt::Write as _;

mod deep_context;
mod definition;
pub(crate) mod grep_support;
mod impact;
mod overview;
mod references;
#[cfg(test)]
mod test_helpers;

/// Shared budget for grep fallback execution in navigation tools (RSP-A10, RSP-D3).
///
/// Enforces:
/// 1. Shared wall-clock deadline (5.0 seconds).
/// 2. Hard limit of at most 8 searches across all strategies and candidates.
#[derive(Debug, Clone)]
pub(crate) struct GrepFallbackBudget {
    deadline: std::time::Instant,
    max_searches: usize,
    searches_done: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl GrepFallbackBudget {
    /// Create a budget with standard 5-second deadline and 8-search limit.
    pub(crate) fn new() -> Self {
        Self::with_budget(std::time::Duration::from_secs(5), 8)
    }

    pub(crate) fn with_budget(timeout: std::time::Duration, max_searches: usize) -> Self {
        Self {
            deadline: std::time::Instant::now() + timeout,
            max_searches,
            searches_done: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Checks if a search may proceed under the budget.
    /// If yes, returns `Some(deadline)` and increments the counter.
    /// If budget is exhausted (time expired or max searches reached), returns `None`.
    pub(crate) fn acquire_search(&self) -> Option<std::time::Instant> {
        if std::time::Instant::now() >= self.deadline {
            return None;
        }
        let prev = self
            .searches_done
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if prev >= self.max_searches {
            return None;
        }
        Some(self.deadline)
    }

    /// Returns the wall-clock deadline.
    #[cfg(test)]
    pub(crate) fn deadline(&self) -> std::time::Instant {
        self.deadline
    }

    /// Returns the number of searches performed so far.
    #[cfg(test)]
    pub(crate) fn searches_performed(&self) -> usize {
        self.searches_done.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Default for GrepFallbackBudget {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns `true` if the file path has a source code extension.
///
/// Driven from the Tree-sitter grammar and LSP plugin registries (WS-8).
/// Excludes non-code markup/styling grammars (e.g. html, css).
pub(crate) fn is_source_file(file: &str) -> bool {
    let ext = std::path::Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    if ext.is_empty() {
        return false;
    }
    if pathfinder_lsp::plugin_for_extension(ext).is_some() {
        return true;
    }
    if let Some(grammar) = pathfinder_treesitter::language::LanguageGrammar::find_by_extension(ext)
    {
        return grammar.id != "html" && grammar.id != "css";
    }
    false
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
/// Driven from grammar and LSP plugin registry with normalized fallback.
#[expect(
    clippy::too_many_lines,
    reason = "keyword lookup table; large match is natural"
)]
fn keywords_for_language(language: &str) -> &'static [&'static str] {
    let lang = pathfinder_treesitter::language::LanguageGrammar::find_by_id(language).map_or_else(
        || pathfinder_lsp::plugin_for_language(language).map_or(language, |p| p.language_id()),
        |g| g.lsp_language_id(),
    );
    match lang {
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

/// Map language ID to file glob pattern, driven from plugin and grammar registries.
///
/// Used by candidate resolution and grep fallbacks to search for definition files.
fn language_to_file_glob(language: &str) -> String {
    match language {
        "rust" => "**/*.rs".to_string(),
        "typescript" | "tsx" => "**/*.{ts,tsx}".to_string(),
        "javascript" => "**/*.{js,jsx}".to_string(),
        "python" => "**/*.py".to_string(),
        "go" => "**/*.go".to_string(),
        "vue" => "**/*.{vue,ts,tsx,js,jsx,mjs,cjs}".to_string(),
        "java" => "**/*.java".to_string(),
        _ => {
            if let Some(plugin) = pathfinder_lsp::plugin_for_language(language) {
                let exts = plugin.file_extensions();
                if exts.len() == 1 {
                    format!("**/*.{}", exts[0])
                } else if !exts.is_empty() {
                    format!("**/*.{{{}}}", exts.join(","))
                } else {
                    "**/*".to_string()
                }
            } else if let Some(grammar) =
                pathfinder_treesitter::language::LanguageGrammar::find_by_id(language)
            {
                let exts = grammar.extensions;
                if exts.len() == 1 {
                    format!("**/*.{}", exts[0])
                } else if !exts.is_empty() {
                    format!("**/*.{{{}}}", exts.join(","))
                } else {
                    "**/*".to_string()
                }
            } else {
                "**/*".to_string()
            }
        }
    }
}

/// Java-specific regex pattern for resolving candidate definitions.
///
/// Matches Java method definitions, constructors, records, and class/interface declarations.
/// Used by grep fallback for outgoing dependency discovery.
fn java_resolve_pattern(candidate: &str) -> String {
    let escaped = if candidate.contains('|') {
        candidate.to_string()
    } else {
        regex::escape(candidate)
    };
    format!(
        r"(?:^[ \t]*(?:(?:public|private|protected)\s+)?(?:<[^>]*?(?:<[^>]*?>[^>]*?)*>\s+)?(?:{escaped})\s*\(|^[ \t]*(?:@\w+(?:\([^)]*\))?\s+)*(?:(?:public|private|protected|static|final|abstract|synchronized|native|default|strictfp)\s+)*(?:<[^>]*?(?:<[^>]*?>[^>]*?)*>\s+)?(?:(?:void|boolean|int|long|double|float|short|byte|char)|[A-Z][a-zA-Z0-9_]*(?:<[^>]*?(?:<[^>]*?>[^>]*?)*>)?)(?:\[\])*\s+(?:{escaped})\s*\(|^[ \t]*(?:(?:public|private|protected|static|final)\s+)*record\s+(?:{escaped})\s*[<(]|(?:public\s+|private\s+|protected\s+|static\s+|final\s+|abstract\s+|sealed\s+|non-sealed\s+|strictfp\s+)*(?:class|interface|enum|@interface)\s+(?:{escaped})\b)"
    )
}

/// Build a regex pattern to find a candidate function's definition.
///
/// Supports single candidate identifiers or alternation patterns (`candidate1|candidate2`).
/// Used by grep fallback for outgoing dependency discovery in both
/// `inspect` (deep context) and `trace` (callers/callees).
fn candidate_definition_pattern(language: &str, candidate: &str) -> String {
    let escaped = if candidate.contains('|') {
        candidate.to_string()
    } else {
        regex::escape(candidate)
    };
    let lang = pathfinder_treesitter::language::LanguageGrammar::find_by_id(language).map_or_else(
        || pathfinder_lsp::plugin_for_language(language).map_or(language, |p| p.language_id()),
        |g| g.lsp_language_id(),
    );
    match lang {
        // The outer non-capturing group is intentionally closed at the end so
        // regex::Regex::new() succeeds and optional pub/async qualifiers are grouped.
        "rust" => format!(r"(?:(?:pub\s*(?:\([^)]*\)\s*)?(?:async\s*)?)?fn\s+(?:{escaped})\b)"),
        "go" => format!(r"func\s+(?:{escaped})\b"),
        "typescript" | "javascript" | "vue" => {
            format!(
                r"(?:(?:export\s+(?:default\s*)?)?function\s+(?:{escaped})\b|(?:export\s+)?(?:const|let|var)\s+(?:{escaped})\s*[=:]|(?:(?:{escaped})\s*:\s*)[^{{]*\([^)]*\)\s*=>)"
            )
        }
        "python" => format!(r"(?:async\s+)?def\s+(?:{escaped})\b"),
        "java" => java_resolve_pattern(&escaped),
        _ => format!(r"\b(?:fn|def|function|class|struct|type|interface)\s+(?:{escaped})\b"),
    }
}

/// Language-aware regex patterns for definition search.
///
/// Returns language-specific regex patterns for finding symbol definitions,
/// driven from the Tree-sitter and LSP plugin registries (WS-8).
/// Used by `find_symbol` and `fallback_definition_grep` for grep-based fallbacks.
pub(crate) fn definition_patterns(ext: &str, symbol_name: &str) -> Vec<String> {
    if ext == "invalid_regex" {
        return vec!["[invalid".to_string()];
    }
    let name = regex::escape(symbol_name);
    let lang = pathfinder_treesitter::language::LanguageGrammar::find_by_extension(ext)
        .map_or_else(
            || pathfinder_lsp::plugin_for_extension(ext).map_or(ext, |p| p.language_id()),
            |g| g.lsp_language_id(),
        );
    match lang {
        "rust" => vec![
            format!(
                r"(?:pub\s*(?:\([^)]*\)\s*)?)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?fn\s+{name}\b"
            ),
            format!(r"(?:pub\s*(?:\([^)]*\)\s*)?)?(?:struct|enum|trait|type|mod)\s+{name}\b"),
            format!(r"(?:pub\s*(?:\([^)]*\)\s*)?)?(?:const|static)\s+{name}\b"),
            format!(r"macro_rules!\s+{name}\b"),
        ],
        "typescript" | "javascript" => vec![
            format!(r"(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s+{name}\b"),
            format!(
                r"(?:export\s+)?(?:default\s+)?(?:abstract\s+)?(?:class|interface|type|enum)\s+{name}\b"
            ),
            format!(r"(?:export\s+)?(?:const|let|var)\s+{name}\s*[=:]"),
            format!(
                r"(?:export\s+)?(?:const|let|var)\s+{name}\s*=\s*(?:async\s+)?\([^)]*\)\s*(?::\s*[^=]+)?\s*=>"
            ),
        ],
        "python" => vec![
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
                format!(
                    r"^[ \t]*(?:(?:public|private|protected)\s+)?(?:<[^>]*?(?:<[^>]*?>[^>]*?)*>\s+)?{parent}\s*\("
                ),
                // P2: Record type — keyword `record` prevents false positives.
                format!(
                    r"^[ \t]*(?:(?:public|private|protected|static|final)\s+)*record\s+{name}\s*[<(]"
                ),
                // P3: Method — return type must be primitive keyword or start uppercase.
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
    pub(crate) async fn enrich_did_you_mean(
        &self,
        semantic_path_str: &str,
        original_suggestions: Vec<String>,
    ) -> Vec<String> {
        let mut suggestions = original_suggestions;

        // Spec 2.2: Separator confusion correction
        // If multiple '::' used, suggest the '.' variant
        if let Some((file_part, symbol_part)) = semantic_path_str.split_once("::") {
            if symbol_part.contains("::") {
                let corrected_symbol = symbol_part.replace("::", ".");
                let corrected_path = format!("{file_part}::{corrected_symbol}");
                if !suggestions.contains(&corrected_path) {
                    suggestions.insert(0, corrected_path);
                }
            }
        }

        // Spec 2.3: PATCH-002-C: Cross-file search
        // Trigger cross-file search if:
        //   a) Same-file suggestions are completely empty
        //   b) Same-file suggestions contain ONLY the parent symbol (no '.' in the
        //      symbol portion, i.e. the part after '::'), which usually means the
        //      trait/interface itself matched but not its methods.
        //      In this case, impl methods in other files might be the real target.
        let only_parent_matches = !suggestions.is_empty()
            && suggestions.iter().all(|s| {
                // Extract symbol portion (after '::'); a parent-only match has no '.'
                s.split_once("::")
                    .is_some_and(|(_, sym)| !sym.contains('.'))
            });
        let needs_cross_file = suggestions.is_empty() || only_parent_matches;

        if needs_cross_file {
            if let Ok(semantic_path) = parse_semantic_path(semantic_path_str) {
                if let Some(chain) = &semantic_path.symbol_chain {
                    if let Some(base_name) = chain.segments.last() {
                        // Use find_symbol to search across files
                        let find_params = grep_support::FindSymbolParams {
                            query: base_name.name.clone(),
                            // PATCH-002-C: filter to function kind to narrow cross-file results
                            // (avoids matching unrelated structs/enums named "search")
                            kind: Some("function".to_string()),
                            path_glob: "**/*".to_owned(),
                            max_results: 20,
                        };
                        match self.find_symbol_impl_inner(find_params).await {
                            Ok(response) => {
                                for symbol in response.symbols {
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
    pub(crate) async fn read_symbol_scope_enriched(
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
    #[allow(clippy::missing_errors_doc)]
    pub async fn locate_impl(&self, params: LocateParams) -> Result<CallToolResult, ErrorData> {
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
            retry_attempt: 0,
        };
        let is_semantic_path = entry.semantic_path.is_some();
        let res = self.locate_impl_single(single_params).await;
        match res {
            Ok(call_res) => {
                let val = call_res
                    .structured_content
                    .unwrap_or(serde_json::Value::Null);
                if is_semantic_path {
                    Self::parse_locate_def_response(entry, val)
                } else {
                    Self::parse_locate_path_response(entry, val)
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
                confidence: pathfinder_common::types::Confidence::Heuristic,
                readiness: None,
                degraded_reason: None,
                error: Some(err.message.to_string()),
            },
        }
    }

    fn parse_locate_def_response(
        entry: crate::server::types::LocateEntry,
        val: serde_json::Value,
    ) -> LocateResultEntry {
        match serde_json::from_value::<GetDefinitionResponse>(val) {
            Ok(meta) => {
                let confidence = if meta.degraded {
                    pathfinder_common::types::Confidence::Heuristic
                } else {
                    pathfinder_common::types::Confidence::Verified
                };
                LocateResultEntry {
                    input: entry,
                    status: "ok".to_string(),
                    file: Some(meta.file),
                    line: Some(meta.line),
                    column: Some(meta.column),
                    semantic_path: None,
                    preview: Some(meta.preview),
                    resolution_strategy: meta.resolution_strategy,
                    confidence,
                    readiness: meta.lsp_readiness,
                    degraded_reason: meta.degraded_reason,
                    error: None,
                }
            }
            Err(e) => LocateResultEntry {
                input: entry,
                status: "error".to_string(),
                file: None,
                line: None,
                column: None,
                semantic_path: None,
                preview: None,
                resolution_strategy: None,
                confidence: pathfinder_common::types::Confidence::Heuristic,
                readiness: None,
                degraded_reason: None,
                error: Some(format!("failed to deserialize metadata: {e}")),
            },
        }
    }

    fn parse_locate_path_response(
        entry: crate::server::types::LocateEntry,
        val: serde_json::Value,
    ) -> LocateResultEntry {
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
                confidence: pathfinder_common::types::Confidence::Verified,
                readiness: Some("ready".to_string()),
                degraded_reason: None,
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
                confidence: pathfinder_common::types::Confidence::Heuristic,
                readiness: None,
                degraded_reason: None,
                error: Some(format!("failed to deserialize metadata: {e}")),
            },
        }
    }

    fn render_batch_locate_text(response: &BatchLocateResult) -> String {
        let mut text_parts = Vec::new();
        if let Some(header) = crate::server::helpers::format_tool_status_header(&response.status) {
            text_parts.push(header);
        }

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
                let heur_tag =
                    if entry.confidence == pathfinder_common::types::Confidence::Heuristic {
                        " [HEURISTIC]"
                    } else {
                        ""
                    };
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
                    let _ = write!(resolved, "{heur_tag}");
                    text_parts.push(resolved);
                } else {
                    text_parts.push(format!(
                        "{} -> {}{heur_tag}",
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
            response.total_duration_ms,
            response.succeeded,
            response.succeeded + response.failed
        ));

        text_parts.join("\n")
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

        let mut results = Vec::with_capacity(futures.len());
        for fut in futures {
            results.push(fut.await);
        }

        let mut succeeded = 0;
        let mut failed = 0;
        let mut heuristic_count = 0;
        for r in &results {
            if r.status == "ok" {
                succeeded += 1;
                if r.confidence == pathfinder_common::types::Confidence::Heuristic {
                    heuristic_count += 1;
                }
            } else {
                failed += 1;
            }
        }

        let status = if failed > 0 || heuristic_count > 0 {
            pathfinder_common::types::ToolStatus::Degraded {
                reason: if heuristic_count > 0 {
                    "heuristic_results_present".to_string()
                } else {
                    "errors_present".to_string()
                },
                retry_after_secs: None,
            }
        } else {
            pathfinder_common::types::ToolStatus::Ok
        };

        let total_duration_ms = millis_to_u64(start.elapsed().as_millis());
        let response = BatchLocateResult {
            status,
            results,
            succeeded,
            failed,
            heuristic_count,
            total_duration_ms,
        };

        let rendered_text = Self::render_batch_locate_text(&response);
        let mut call_result =
            CallToolResult::success(vec![rmcp::model::Content::text(rendered_text)]);
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
    #[allow(clippy::missing_errors_doc)]
    pub async fn trace_impl(&self, params: TraceParams) -> Result<CallToolResult, ErrorData> {
        match params.scope {
            TraceScope::Callers => self.find_callers_callees_impl(params).await,
            TraceScope::References => self.find_all_references_impl(params).await,
            TraceScope::Overview => self.symbol_overview_impl(params).await,
        }
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
