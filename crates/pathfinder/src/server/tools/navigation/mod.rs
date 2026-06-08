//! Navigation tool handlers: `get_definition`, `find_callers_callees`, and
//! `read_with_deep_context`.
//!
//! All three tools are LSP-powered but degrade gracefully when no language
//! server is available. The tool responses include `"degraded": true` and
//! `"degraded_reason"` fields to signal the fallback mode to agents.
//!
//! # Degraded Mode
//! When the `Lawyer` returns `LspError::NoLspAvailable`:
//! - `get_definition` — returns an error response (`LSP_REQUIRED`)
//! - `find_callers_callees` — returns `null` caller/callee lists with `degraded: true`
//! - `read_with_deep_context` — returns the symbol scope only, no dependencies

use crate::server::helpers::{
    parse_semantic_path, pathfinder_to_error_data, treesitter_error_to_error_data,
};
use crate::server::PathfinderServer;
use pathfinder_common::types::DegradedReason;
use rmcp::model::ErrorData;

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

/// Returns `true` if the file path looks like a test file.
///
/// Uses language-specific heuristics:
/// - Rust: files ending in `_test.rs` or containing `mod tests`
/// - Go: files ending in `_test.go`
/// - Python: files starting with `test_` or containing test functions
/// - TypeScript/JavaScript: files ending in `.test.ts`, `.spec.ts`, `.test.js`, `.spec.js`
fn is_test_file(file: &str) -> bool {
    let path = std::path::Path::new(file);
    let filename = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let file_str = file.replace('\\', "/");

    // Directory-based detection: files inside test/spec/__tests__ directories
    if file_str.contains("/tests/")
        || file_str.contains("/test/")
        || file_str.contains("/spec/")
        || file_str.contains("/specs/")
        || file_str.contains("/__tests__/")
        || file_str.contains("/__test__/")
    {
        return true;
    }

    match ext {
        "rs" => filename.ends_with("_test.rs") || filename == "test.rs",
        "go" => filename.ends_with("_test.go"),
        "py" => filename.starts_with("test_") || filename == "conftest.py",
        "java" => filename.ends_with("Test.java") || filename.ends_with("Tests.java"),
        "kt" | "kts" => filename.ends_with("Test.kt") || filename.ends_with("Spec.kt"),
        "cs" => filename.ends_with("Test.cs") || filename.ends_with("Tests.cs"),
        "rb" => filename.ends_with("_test.rb") || filename.ends_with("_spec.rb"),
        "dart" => filename.ends_with("_test.dart"),
        "ts" | "tsx" | "js" | "jsx" => {
            filename.ends_with(".test.ts")
                || filename.ends_with(".spec.ts")
                || filename.ends_with(".test.tsx")
                || filename.ends_with(".spec.tsx")
                || filename.ends_with(".test.js")
                || filename.ends_with(".spec.js")
                || filename.ends_with(".test.jsx")
                || filename.ends_with(".spec.jsx")
        }
        _ => false,
    }
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
        "typescript" => "**/*.{ts,tsx}",
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
/// `read_with_deep_context` and `find_callers_callees`.
fn candidate_definition_pattern(language: &str, candidate: &str) -> String {
    let escaped = regex::escape(candidate);
    match language {
        "rust" => format!(r"(?:(?:pub\s*(?:\([^)]*\)\s*)?(?:async\s*)?)?fn\s+{escaped}\b"),
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
                        let find_params = crate::server::types::FindSymbolParams {
                            name: base_name.name.clone(),
                            kind: None,
                            path_glob: "**/*".to_owned(),
                            max_results: 3,
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
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {

    // ── language_to_file_glob tests ─────────────────────────────────────────

    #[test]
    fn test_language_to_file_glob_rust() {
        assert_eq!(super::language_to_file_glob("rust"), "**/*.rs");
    }

    #[test]
    fn test_language_to_file_glob_typescript() {
        assert_eq!(super::language_to_file_glob("typescript"), "**/*.{ts,tsx}");
    }

    #[test]
    fn test_language_to_file_glob_javascript() {
        assert_eq!(super::language_to_file_glob("javascript"), "**/*.{js,jsx}");
    }

    #[test]
    fn test_language_to_file_glob_python() {
        assert_eq!(super::language_to_file_glob("python"), "**/*.py");
    }

    #[test]
    fn test_language_to_file_glob_go() {
        assert_eq!(super::language_to_file_glob("go"), "**/*.go");
    }

    #[test]
    fn test_language_to_file_glob_vue() {
        assert_eq!(
            super::language_to_file_glob("vue"),
            "**/*.{vue,ts,tsx,js,jsx,mjs,cjs}"
        );
    }

    #[test]
    fn test_language_to_file_glob_java() {
        assert_eq!(super::language_to_file_glob("java"), "**/*.java");
    }

    #[test]
    fn test_language_to_file_glob_unknown_defaults_to_catch_all() {
        assert_eq!(super::language_to_file_glob("haskell"), "**/*");
        assert_eq!(super::language_to_file_glob(""), "**/*");
    }

    // ── definition_patterns tests ──────────────────────────────────────────

    #[test]
    fn test_definition_patterns_rust_fn() {
        let patterns = super::definition_patterns("rs", "my_function");
        assert!(!patterns.is_empty(), "must return at least one pattern");
        // First pattern should match function definitions
        let re = regex::Regex::new(&patterns[0]).expect("valid regex");
        assert!(
            re.is_match("pub async fn my_function("),
            "must match 'pub async fn my_function('"
        );
        assert!(
            re.is_match("fn my_function("),
            "must match bare 'fn my_function('"
        );
        assert!(
            !re.is_match("let my_function = 42"),
            "must not match variable assignment"
        );
    }

    #[test]
    fn test_definition_patterns_rust_struct() {
        let patterns = super::definition_patterns("rs", "MyStruct");
        assert!(patterns.len() >= 2, "must return patterns for types too");
        let re = regex::Regex::new(&patterns[1]).expect("valid regex");
        assert!(
            re.is_match("pub(crate) struct MyStruct {"),
            "must match 'pub(crate) struct MyStruct {{'"
        );
        assert!(
            re.is_match("enum MyStruct {"),
            "must match 'enum MyStruct {{'"
        );
    }

    #[test]
    fn test_definition_patterns_ts_export_class() {
        let patterns = super::definition_patterns("ts", "AuthService");
        assert!(!patterns.is_empty());
        // Second pattern matches classes/interfaces
        let re = regex::Regex::new(&patterns[1]).expect("valid regex");
        assert!(
            re.is_match("export default class AuthService {"),
            "must match 'export default class AuthService {{'"
        );
        assert!(
            re.is_match("export interface AuthService {"),
            "must match 'export interface AuthService {{'"
        );
    }

    // ── Vue definition_patterns tests (DELIVERABLE C) ─────────────────────

    #[test]
    fn test_definition_patterns_vue_function() {
        let patterns = super::definition_patterns("vue", "handleClick");
        assert!(!patterns.is_empty(), "vue must have definition patterns");
        let re = regex::Regex::new(&patterns[0]).expect("valid regex");
        assert!(
            re.is_match("export async function handleClick("),
            "must match 'export async function handleClick('"
        );
        assert!(
            re.is_match("function handleClick("),
            "must match bare 'function handleClick('"
        );
    }

    #[test]
    fn test_definition_patterns_vue_const_assignment() {
        let patterns = super::definition_patterns("vue", "handleClick");
        assert!(patterns.len() >= 3);
        let re = regex::Regex::new(&patterns[2]).expect("valid regex");
        assert!(
            re.is_match("const handleClick = () => {}"),
            "must match 'const handleClick = () => {{}}'"
        );
        assert!(
            re.is_match("export const handleClick = () => {}"),
            "must match 'export const handleClick = () => {{}}'"
        );
        assert!(
            re.is_match("let handleClick: Handler = () => {}"),
            "must match typed assignment 'let handleClick: Handler ='"
        );
    }

    #[test]
    fn test_definition_patterns_vue_ref() {
        let patterns = super::definition_patterns("vue", "count");
        assert!(patterns.len() >= 3);
        let re = regex::Regex::new(&patterns[2]).expect("valid regex");
        assert!(
            re.is_match("const count = ref(0)"),
            "must match 'const count = ref(0)'"
        );
        assert!(
            re.is_match("const count = reactive({ value: 0 })"),
            "must match 'const count = reactive(...)'"
        );
        assert!(
            re.is_match("const count = computed(() => 0)"),
            "must match 'const count = computed(...)'"
        );
    }

    #[test]
    fn test_definition_patterns_vue_define_macros() {
        let patterns_props = super::definition_patterns("vue", "props");
        let patterns_emit = super::definition_patterns("vue", "emit");
        assert!(patterns_props.len() >= 5);
        let re_props = regex::Regex::new(&patterns_props[4]).expect("valid regex");
        let re_emit = regex::Regex::new(&patterns_emit[4]).expect("valid regex");
        assert!(
            re_props.is_match("const props = defineProps<{ id: string }>()"),
            "must match 'const props = defineProps(...)'"
        );
        assert!(
            re_emit.is_match("const emit = defineEmits<{ (e: 'save'): void }>()"),
            "must match 'const emit = defineEmits(...)'"
        );
        assert!(
            re_props.is_match("const props = withDefaults(defineProps<{ }>(), {})"),
            "must match 'const props = withDefaults(...)'"
        );
    }

    #[test]
    fn test_definition_patterns_py_async_def() {
        let patterns = super::definition_patterns("py", "process_order");
        assert!(!patterns.is_empty());
        let re = regex::Regex::new(&patterns[0]).expect("valid regex");
        assert!(
            re.is_match("async def process_order("),
            "must match 'async def process_order('"
        );
        assert!(
            re.is_match("def process_order("),
            "must match 'def process_order('"
        );
    }

    #[test]
    fn test_definition_patterns_py_class() {
        let patterns = super::definition_patterns("py", "MyClass");
        assert!(patterns.len() >= 2);
        let re = regex::Regex::new(&patterns[1]).expect("valid regex");
        assert!(re.is_match("class MyClass:"), "must match 'class MyClass:'");
    }

    #[test]
    fn test_definition_patterns_go_receiver_method() {
        let patterns = super::definition_patterns("go", "HandleRequest");
        assert!(!patterns.is_empty());
        let re = regex::Regex::new(&patterns[0]).expect("valid regex");
        assert!(
            re.is_match("func (s *Service) HandleRequest("),
            "must match receiver method"
        );
        assert!(
            re.is_match("func HandleRequest("),
            "must match bare function"
        );
    }

    #[test]
    fn test_definition_patterns_go_type() {
        let patterns = super::definition_patterns("go", "UserService");
        assert!(patterns.len() >= 3, "go must have func + type + const/var");
        let re = regex::Regex::new(&patterns[2]).expect("valid regex");
        assert!(
            re.is_match("type UserService struct {"),
            "must match 'type UserService struct {{'"
        );
    }

    #[test]
    fn test_definition_patterns_unknown_extension_uses_fallback() {
        let patterns = super::definition_patterns("java", "MyClass");
        assert!(!patterns.is_empty());
        // Java has its own patterns
        let re = regex::Regex::new(&patterns[0]).expect("valid regex");
        assert!(
            re.is_match("public class MyClass {"),
            "must match Java class declaration"
        );
    }

    #[test]
    fn test_definition_patterns_catch_all_extension() {
        let patterns = super::definition_patterns("unknown_ext", "foo");
        assert_eq!(
            patterns.len(),
            1,
            "catch-all must return exactly one pattern"
        );
        let re = regex::Regex::new(&patterns[0]).expect("valid regex");
        assert!(re.is_match("foo"), "must match bare word");
        assert!(!re.is_match("foobar"), "must not match partial word");
    }

    #[test]
    fn test_definition_patterns_special_chars_escaped() {
        // Symbol name with regex special characters must be escaped
        let patterns = super::definition_patterns("rs", "my+function");
        assert!(!patterns.is_empty());
        let re = regex::Regex::new(&patterns[0]).expect("valid regex");
        // Must match literal "my+function", not "myXfunction"
        assert!(re.is_match("fn my+function("));
        assert!(!re.is_match("fn myXfunction("));
    }

    #[test]
    fn test_definition_patterns_all_languages_compile() {
        // Verify every extension returns valid regex patterns
        let extensions = [
            "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "vue", "xyz",
        ];
        for ext in &extensions {
            let patterns = super::definition_patterns(ext, "TestSymbol");
            for (i, pattern) in patterns.iter().enumerate() {
                assert!(
                    regex::Regex::new(pattern).is_ok(),
                    "pattern {i} for ext '{ext}' must be valid regex: {pattern}"
                );
            }
        }
    }

    // ── Java definition_patterns tests (DELIVERABLE E) ───────────────────

    #[test]
    fn test_definition_patterns_java_class() {
        let patterns = super::definition_patterns("java", "MyClass");
        assert!(!patterns.is_empty(), "java must have definition patterns");
        let re = regex::Regex::new(&patterns[0]).expect("valid regex");
        assert!(
            re.is_match("public class MyClass {"),
            "must match 'public class MyClass {{'"
        );
        assert!(
            re.is_match("private static final class MyClass {"),
            "must match 'private static final class MyClass {{'"
        );
    }

    #[test]
    fn test_definition_patterns_java_constructor() {
        let patterns = super::definition_patterns("java", "MyClass");
        assert!(!patterns.is_empty(), "java must have definition patterns");
        // Look for a pattern that matches constructors
        let constructor_pattern = patterns.iter().find(|p| p.contains("MyClass\\s*\\("));
        assert!(
            constructor_pattern.is_some(),
            "java must have a constructor pattern"
        );
        let re = regex::Regex::new(constructor_pattern.unwrap()).expect("valid regex");
        assert!(
            re.is_match("public MyClass(String name) {"),
            "must match 'public MyClass(String name) {{'"
        );
        assert!(
            re.is_match("MyClass(String name, int age) {"),
            "must match bare 'MyClass(String name, int age) {{'"
        );
        assert!(
            re.is_match("private MyClass() {"),
            "must match 'private MyClass() {{'"
        );
    }

    #[test]
    fn test_definition_patterns_java_record() {
        let patterns = super::definition_patterns("java", "Person");
        assert!(!patterns.is_empty(), "java must have definition patterns");
        // Look for a pattern that matches records
        let record_pattern = patterns.iter().find(|p| p.contains("record"));
        assert!(record_pattern.is_some(), "java must have a record pattern");
        let re = regex::Regex::new(record_pattern.unwrap()).expect("valid regex");
        assert!(
            re.is_match("public record Person(String name) {"),
            "must match 'public record Person(String name) {{'"
        );
        assert!(
            re.is_match("record Person(String name, int age) {"),
            "must match bare 'record Person(String name, int age) {{'"
        );
        assert!(
            re.is_match("private final record Person(String name) {"),
            "must match 'private final record Person(String name) {{'"
        );
    }

    #[test]
    fn test_definition_patterns_java_sealed_class() {
        let patterns = super::definition_patterns("java", "Shape");
        assert!(!patterns.is_empty(), "java must have definition patterns");
        // Look for a pattern that matches sealed classes
        let sealed_pattern = patterns.iter().find(|p| p.contains("sealed"));
        assert!(
            sealed_pattern.is_some(),
            "java must have a sealed class/interface pattern"
        );
        let re = regex::Regex::new(sealed_pattern.unwrap()).expect("valid regex");
        assert!(
            re.is_match("public sealed class Shape permits Circle, Square {"),
            "must match 'public sealed class Shape permits Circle, Square {{'"
        );
        assert!(
            re.is_match("sealed interface Shape permits Circle {"),
            "must match 'sealed interface Shape permits Circle {{'"
        );
        assert!(
            re.is_match("private sealed abstract class Shape {"),
            "must match 'private sealed abstract class Shape {{'"
        );
    }

    #[test]
    fn test_definition_patterns_java_annotated_method() {
        let patterns = super::definition_patterns("java", "myService");
        // The last pattern is for methods with annotations
        let method_pattern = patterns.last().expect("java should have patterns");
        let re = regex::Regex::new(method_pattern).expect("valid regex");
        assert!(
            re.is_match("@Bean public MyService myService()"),
            "must match '@Bean public MyService myService()'"
        );
        assert!(
            re.is_match("@Override public void myService()"),
            "must match '@Override public void myService()'"
        );
        assert!(
            re.is_match("@GetMapping public Response myService()"),
            "must match '@GetMapping public Response myService()'"
        );
    }

    #[test]
    fn test_definition_patterns_java_primitive_return() {
        let patterns = super::definition_patterns("java", "process");
        assert!(!patterns.is_empty(), "java must have definition patterns");
        // The last pattern matches methods with any return type
        let method_pattern = patterns.last().expect("java should have patterns");
        let re = regex::Regex::new(method_pattern).expect("valid regex");
        assert!(re.is_match("void process()"), "must match 'void process()'");
        assert!(
            re.is_match("public boolean process()"),
            "must match 'public boolean process()'"
        );
        assert!(
            re.is_match("private int process()"),
            "must match 'private int process()'"
        );
        assert!(
            re.is_match("protected String process()"),
            "must match 'protected String process()'"
        );
        assert!(
            re.is_match("static final double process()"),
            "must match 'static final double process()'"
        );
    }

    #[test]
    fn test_definition_patterns_java_generic_return() {
        let patterns = super::definition_patterns("java", "process");
        assert!(!patterns.is_empty(), "java must have definition patterns");
        // The last pattern matches methods with generic return types
        let method_pattern = patterns.last().expect("java should have patterns");
        let re = regex::Regex::new(method_pattern).expect("valid regex");
        assert!(
            re.is_match("public List<String> process()"),
            "must match 'public List<String> process()'"
        );
        assert!(
            re.is_match("Map<String, Integer> process()"),
            "must match 'Map<String, Integer> process()'"
        );
    }

    #[test]
    fn test_definition_patterns_java_array_return() {
        let patterns = super::definition_patterns("java", "process");
        assert!(!patterns.is_empty(), "java must have definition patterns");
        let method_pattern = patterns.last().expect("java should have patterns");
        let re = regex::Regex::new(method_pattern).expect("valid regex");
        assert!(
            re.is_match("public String[] process()"),
            "must match 'public String[] process()'"
        );
        assert!(
            re.is_match("int[] process()"),
            "must match 'int[] process()'"
        );
        assert!(
            re.is_match("public int[][] process()"),
            "must match 'public int[][] process()' — multi-dimensional array"
        );
        assert!(
            re.is_match("String[][][] process()"),
            "must match 'String[][][] process()' — 3D array"
        );
    }

    #[test]
    fn test_definition_patterns_java_method_with_type_params() {
        let patterns = super::definition_patterns("java", "process");
        assert!(!patterns.is_empty(), "java must have definition patterns");
        // The last pattern matches methods with type parameters
        let method_pattern = patterns.last().expect("java should have patterns");
        let re = regex::Regex::new(method_pattern).expect("valid regex");
        assert!(
            re.is_match("public <T> T process()"),
            "must match 'public <T> T process()'"
        );
        assert!(
            re.is_match("<T, U> Map<T, U> process()"),
            "must match '<T, U> Map<T, U> process()'"
        );
    }

    // ── Java negative test cases (Deliverable E fixes) ─────────────────────

    #[test]
    fn test_definition_patterns_java_constructor_rejects_return_types() {
        // CRITICAL-2: Pattern 1 (constructor) must not match methods with return types
        let patterns = super::definition_patterns("java", "MyClass");
        let constructor_pattern = patterns
            .get(1)
            .expect("java should have constructor pattern");
        let re = regex::Regex::new(constructor_pattern).expect("valid regex");
        assert!(
            !re.is_match("public void MyClass()"),
            "must NOT match 'public void MyClass()' - this is a method, not a constructor"
        );
        assert!(
            !re.is_match("private String MyClass()"),
            "must NOT match 'private String MyClass()' - this is a method, not a constructor"
        );
        assert!(
            !re.is_match("protected int MyClass()"),
            "must NOT match 'protected int MyClass()' - this is a method, not a constructor"
        );
    }

    #[test]
    fn test_definition_patterns_java_method_pattern_rejects_new_and_throw() {
        // CRITICAL-1: Pattern 4 must NOT match new ClassName() or throw new MyError()
        let patterns = super::definition_patterns("java", "MyError");
        let method_pattern = patterns.last().expect("java should have method pattern");
        let re = regex::Regex::new(method_pattern).expect("valid regex");
        assert!(
            !re.is_match("throw new MyError(msg)"),
            "must NOT match 'throw new MyError(msg)' - false positive"
        );
        assert!(
            !re.is_match("return new MyError()"),
            "must NOT match 'return new MyError()' - false positive"
        );
        assert!(
            !re.is_match("new MyError().getMessage()"),
            "must NOT match 'new MyError().getMessage()' - false positive"
        );
    }

    #[test]
    fn test_definition_patterns_java_constructor_rejects_new_keyword() {
        let patterns = super::definition_patterns("java", "MyClass");
        let constructor_pattern = patterns
            .get(1)
            .expect("java should have constructor pattern");
        let re = regex::Regex::new(constructor_pattern).expect("valid regex");
        assert!(
            !re.is_match("new MyClass()"),
            "must NOT match 'new MyClass()' - this is a call, not a definition"
        );
        assert!(
            !re.is_match("return new MyClass()"),
            "must NOT match 'return new MyClass()' - this is a call, not a definition"
        );
    }

    #[test]
    fn test_definition_patterns_java_generic_constructor() {
        // MEDIUM-4: Support generic constructors like public <E> MyClass(E item)
        let patterns = super::definition_patterns("java", "MyClass");
        let constructor_pattern = patterns
            .get(1)
            .expect("java should have constructor pattern");
        let re = regex::Regex::new(constructor_pattern).expect("valid regex");
        assert!(
            re.is_match("public <E> MyClass(E item)"),
            "must match 'public <E> MyClass(E item)'"
        );
        assert!(
            re.is_match("<T, U> MyClass(T a, U b)"),
            "must match '<T, U> MyClass(T a, U b)'"
        );
    }

    #[test]
    fn test_definition_patterns_java_nested_generics() {
        // MEDIUM-2: Support nested generics like Map<String, List<Integer>>
        let patterns = super::definition_patterns("java", "process");
        let method_pattern = patterns.last().expect("java should have method pattern");
        let re = regex::Regex::new(method_pattern).expect("valid regex");
        assert!(
            re.is_match("public Map<String, List<Integer>> process()"),
            "must match 'public Map<String, List<Integer>> process()'"
        );
        assert!(
            re.is_match("Map<String, Map<String, Integer>> process()"),
            "must match 'Map<String, Map<String, Integer>> process()'"
        );
    }

    #[test]
    fn test_definition_patterns_java_sealed_no_trailing_whitespace() {
        // MAJOR-2: Pattern should match sealed class at end-of-line (no trailing whitespace)
        let patterns = super::definition_patterns("java", "Shape");
        let class_pattern = patterns.first().expect("java should have class pattern");
        let re = regex::Regex::new(class_pattern).expect("valid regex");
        assert!(
            re.is_match("public sealed class Shape"),
            "must match 'public sealed class Shape' at end-of-line"
        );
        assert!(
            re.is_match("sealed class Shape{"),
            "must match 'sealed class Shape{{' without space before brace"
        );
    }

    #[test]
    fn test_definition_patterns_java_strictfp_method() {
        let patterns = super::definition_patterns("java", "calculate");
        let method_pattern = patterns.last().expect("java should have method pattern");
        let re = regex::Regex::new(method_pattern).expect("valid regex");
        assert!(
            re.is_match("public strictfp void calculate()"),
            "must match 'public strictfp void calculate()'"
        );
        assert!(
            re.is_match("strictfp double calculate(int x)"),
            "must match 'strictfp double calculate(int x)'"
        );
    }

    #[test]
    fn test_definition_patterns_java_strictfp_class() {
        let patterns = super::definition_patterns("java", "MathUtils");
        let class_pattern = patterns.first().expect("java should have class pattern");
        let re = regex::Regex::new(class_pattern).expect("valid regex");
        assert!(
            re.is_match("strictfp class MathUtils"),
            "must match 'strictfp class MathUtils'"
        );
        assert!(
            re.is_match("public strictfp class MathUtils"),
            "must match 'public strictfp class MathUtils'"
        );
    }

    #[test]
    fn test_definition_patterns_java_non_sealed_class() {
        let patterns = super::definition_patterns("java", "Shape");
        let class_pattern = patterns.first().expect("java should have class pattern");
        let re = regex::Regex::new(class_pattern).expect("valid regex");
        assert!(
            re.is_match("non-sealed class Shape"),
            "must match 'non-sealed class Shape'"
        );
        assert!(
            re.is_match("public non-sealed class Shape"),
            "must match 'public non-sealed class Shape'"
        );
    }

    #[test]
    fn test_definition_patterns_java_multi_dimensional_array_return() {
        let patterns = super::definition_patterns("java", "getData");
        let method_pattern = patterns.last().expect("java should have method pattern");
        let re = regex::Regex::new(method_pattern).expect("valid regex");
        assert!(
            re.is_match("public int[][] getData()"),
            "must match 'public int[][] getData()' — 2D array"
        );
        assert!(
            re.is_match("String[][][] getData()"),
            "must match 'String[][][] getData()' — 3D array"
        );
        assert!(
            re.is_match("Map<String, Integer>[][] getData()"),
            "must match 'Map<String, Integer>[][] getData()' — generic 2D array"
        );
    }

    #[test]
    fn test_definition_patterns_java_bounded_generics() {
        let patterns = super::definition_patterns("java", "sort");
        let method_pattern = patterns.last().expect("java should have method pattern");
        let re = regex::Regex::new(method_pattern).expect("valid regex");
        assert!(
            re.is_match("public <T extends Comparable<T>> void sort(List<T> list)"),
            "must match 'public <T extends Comparable<T>> void sort(List<T> list)' — bounded generics"
        );
        let patterns_get = super::definition_patterns("java", "get");
        let method_pattern_get = patterns_get
            .last()
            .expect("java should have method pattern");
        let re_get = regex::Regex::new(method_pattern_get).expect("valid regex");
        assert!(
            re_get.is_match("<K, V extends Serializable> V get(K key)"),
            "must match '<K, V extends Serializable> V get(K key)' — multiple bounded params"
        );
        let patterns2 = super::definition_patterns("java", "MyClass");
        let constructor_pattern = patterns2
            .get(1)
            .expect("java should have constructor pattern");
        let re2 = regex::Regex::new(constructor_pattern).expect("valid regex");
        assert!(
            re2.is_match("public <T extends Comparable<T>> MyClass(T item)"),
            "must match 'public <T extends Comparable<T>> MyClass(T item)' — generic constructor with bounds"
        );
    }

    #[test]
    fn test_definition_patterns_java_static_record() {
        let patterns = super::definition_patterns("java", "Inner");
        let record_pattern = patterns.get(2).expect("java should have record pattern");
        let re = regex::Regex::new(record_pattern).expect("valid regex");
        assert!(
            re.is_match("static record Inner(String name, int value)"),
            "must match 'static record Inner(String name, int value)' — nested static record"
        );
        assert!(
            re.is_match("public static final record Inner(String name)"),
            "must match 'public static final record Inner(String name)' — full modifiers"
        );
    }

    // ── extract_call_candidates tests ──────────────────────────────────────

    #[test]
    fn test_extract_call_candidates_rust_basic() {
        let code = r"
            fn process() {
                fetch_user(id);
                validate_order(&order);
                charge_payment(amount);
            }
        ";
        let candidates = super::extract_call_candidates(code, "rust");
        assert!(candidates.contains(&"fetch_user".to_string()));
        assert!(candidates.contains(&"validate_order".to_string()));
        assert!(candidates.contains(&"charge_payment".to_string()));
    }

    #[test]
    fn test_extract_call_candidates_filters_keywords() {
        let code = r"
            fn process() {
                if condition { return; }
                for item in items { do_something(item); }
                while running { check(); }
                match value { _ => {} }
            }
        ";
        let candidates = super::extract_call_candidates(code, "rust");
        assert!(
            !candidates.contains(&"if".to_string()),
            "must filter 'if' keyword"
        );
        assert!(
            !candidates.contains(&"for".to_string()),
            "must filter 'for' keyword"
        );
        assert!(
            !candidates.contains(&"while".to_string()),
            "must filter 'while' keyword"
        );
        assert!(
            !candidates.contains(&"match".to_string()),
            "must filter 'match' keyword"
        );
        assert!(
            !candidates.contains(&"return".to_string()),
            "must filter 'return' keyword"
        );
        assert!(
            candidates.contains(&"do_something".to_string()),
            "must keep real function call"
        );
        assert!(
            candidates.contains(&"check".to_string()),
            "must keep real function call"
        );
    }

    #[test]
    fn test_extract_call_candidates_go_keywords() {
        let code = r"
            func process() {
                if err != nil { return err }
                for _, v := range items { handle(v) }
                select { case <-ch: }
            }
        ";
        let candidates = super::extract_call_candidates(code, "go");
        assert!(!candidates.contains(&"if".to_string()));
        assert!(!candidates.contains(&"for".to_string()));
        assert!(!candidates.contains(&"range".to_string()));
        assert!(!candidates.contains(&"select".to_string()));
        assert!(candidates.contains(&"handle".to_string()));
    }

    #[test]
    fn test_extract_call_candidates_python_keywords() {
        let code = r#"
def process():
    if condition:
        return result
    for item in items:
        handle(item)
    raise ValueError("bad")
        "#;
        let candidates = super::extract_call_candidates(code, "python");
        assert!(!candidates.contains(&"if".to_string()));
        assert!(!candidates.contains(&"for".to_string()));
        assert!(!candidates.contains(&"return".to_string()));
        assert!(!candidates.contains(&"raise".to_string()));
        assert!(candidates.contains(&"handle".to_string()));
    }

    #[test]
    fn test_extract_call_candidates_deduplicates() {
        let code = r"
            fn process() {
                fetch(id);
                fetch(id);
                fetch(id);
            }
        ";
        let candidates = super::extract_call_candidates(code, "rust");
        let count = candidates.iter().filter(|c| *c == "fetch").count();
        assert_eq!(count, 1, "must deduplicate call candidates");
    }

    #[test]
    #[allow(clippy::format_push_string)]
    fn test_extract_call_candidates_caps_at_20() {
        // Generate 25 unique function calls
        let mut code = String::from("fn process() {\n");
        for i in 0..25 {
            code.push_str(&format!("    func_{i}(x);\n"));
        }
        code.push('}');

        let candidates = super::extract_call_candidates(&code, "rust");
        assert!(
            candidates.len() <= 20,
            "must cap at 20 candidates, got {}",
            candidates.len()
        );
    }

    #[test]
    fn test_extract_call_candidates_typescript_method_calls() {
        let code = r"
            function process() {
                user.getName();
                order.calculateTotal();
                service.validate(data);
            }
        ";
        let candidates = super::extract_call_candidates(code, "typescript");
        // Method calls (obj.method()) should also be extracted for TS/JS
        assert!(candidates.contains(&"getName".to_string()));
        assert!(candidates.contains(&"calculateTotal".to_string()));
        assert!(candidates.contains(&"validate".to_string()));
    }

    // ── Vue extract_call_candidates test (DELIVERABLE C) ──────────────────

    #[test]
    fn test_extract_call_candidates_vue_method_calls() {
        // Vue <script setup> uses same patterns as TypeScript
        let code = r"
            const handleSubmit = () => {
                userService.login(credentials);
                router.push('/dashboard');
                toast.showSuccess();
            }
        ";
        let candidates = super::extract_call_candidates(code, "vue");
        // Method calls (obj.method()) should also be extracted for Vue
        assert!(
            candidates.contains(&"login".to_string()),
            "expected 'login' in {candidates:?}"
        );
        assert!(
            candidates.contains(&"push".to_string()),
            "expected 'push' in {candidates:?}"
        );
        assert!(
            candidates.contains(&"showSuccess".to_string()),
            "expected 'showSuccess' in {candidates:?}"
        );
    }

    #[test]
    fn test_extract_call_candidates_empty_input() {
        let candidates = super::extract_call_candidates("", "rust");
        assert!(candidates.is_empty(), "empty input must return empty vec");
    }

    #[test]
    fn test_extract_call_candidates_no_calls() {
        let code = "let x = 42;\nlet y = x + 1;";
        let candidates = super::extract_call_candidates(code, "rust");
        assert!(
            candidates.is_empty(),
            "no function calls must return empty vec"
        );
    }

    // ── keywords_for_language tests ────────────────────────────────────────

    #[test]
    fn test_keywords_for_language_rust() {
        let kw = super::keywords_for_language("rust");
        assert!(kw.contains(&"fn"), "must contain 'fn'");
        assert!(kw.contains(&"struct"), "must contain 'struct'");
        assert!(kw.contains(&"impl"), "must contain 'impl'");
        assert!(kw.contains(&"async"), "must contain 'async'");
        assert!(kw.contains(&"await"), "must contain 'await'");
        assert!(kw.len() > 20, "rust keywords must be comprehensive");
    }

    #[test]
    fn test_keywords_for_language_go() {
        let kw = super::keywords_for_language("go");
        assert!(kw.contains(&"func"), "must contain 'func'");
        assert!(kw.contains(&"defer"), "must contain 'defer'");
        assert!(kw.contains(&"select"), "must contain 'select'");
        assert!(kw.contains(&"chan"), "must contain 'chan'");
    }

    #[test]
    fn test_keywords_for_language_typescript() {
        let kw = super::keywords_for_language("typescript");
        assert!(kw.contains(&"function"), "must contain 'function'");
        assert!(kw.contains(&"class"), "must contain 'class'");
        assert!(kw.contains(&"const"), "must contain 'const'");
    }

    #[test]
    fn test_keywords_for_language_python() {
        let kw = super::keywords_for_language("python");
        assert!(kw.contains(&"def"), "must contain 'def'");
        assert!(kw.contains(&"class"), "must contain 'class'");
        assert!(kw.contains(&"raise"), "must contain 'raise'");
    }

    #[test]
    fn test_keywords_for_language_java() {
        let kw = super::keywords_for_language("java");
        assert!(kw.contains(&"class"), "must contain 'class'");
        assert!(kw.contains(&"interface"), "must contain 'interface'");
        assert!(kw.contains(&"extends"), "must contain 'extends'");
    }

    // ── Vue keywords_for_language test (DELIVERABLE C) ────────────────────

    #[test]
    fn test_keywords_for_language_vue() {
        let kw = super::keywords_for_language("vue");
        // TS/JS base keywords
        assert!(kw.contains(&"function"), "must contain 'function'");
        assert!(kw.contains(&"const"), "must contain 'const'");
        // Vue-specific composables
        assert!(kw.contains(&"ref"), "must contain 'ref'");
        assert!(kw.contains(&"reactive"), "must contain 'reactive'");
        assert!(kw.contains(&"computed"), "must contain 'computed'");
        assert!(kw.contains(&"watch"), "must contain 'watch'");
        assert!(kw.contains(&"onMounted"), "must contain 'onMounted'");
        // Vue compiler macros
        assert!(kw.contains(&"defineProps"), "must contain 'defineProps'");
        assert!(kw.contains(&"defineEmits"), "must contain 'defineEmits'");
    }

    #[test]
    fn test_keywords_for_language_unknown_uses_default() {
        let kw = super::keywords_for_language("haskell");
        assert!(kw.contains(&"if"), "default must contain 'if'");
        assert!(kw.contains(&"for"), "default must contain 'for'");
        assert!(kw.contains(&"while"), "default must contain 'while'");
        assert!(kw.contains(&"return"), "default must contain 'return'");
    }

    #[test]
    fn test_try_separator_correction_converts_double_colon_to_dot() {
        assert_eq!(
            super::PathfinderServer::try_separator_correction("cache.rs::AstCache::get_or_parse"),
            Some("cache.rs::AstCache.get_or_parse".to_string())
        );
        assert_eq!(
            super::PathfinderServer::try_separator_correction("file.rs::Struct::method::inner"),
            Some("file.rs::Struct.method.inner".to_string())
        );
        assert_eq!(
            super::PathfinderServer::try_separator_correction("file.rs::simple_symbol"),
            None
        );
        assert_eq!(
            super::PathfinderServer::try_separator_correction("file.rs"),
            None
        );
    }
}
