//! `find_symbol` tool — Resolve a bare symbol name to its `file::symbol` semantic path(s).

use crate::server::helpers::io_error_data;
use crate::server::types::{FindSymbolParams, FindSymbolResponse, FoundSymbol};
use crate::server::PathfinderServer;
use futures::StreamExt as _;
use pathfinder_search::SearchParams;
use pathfinder_treesitter::surgeon::SymbolKind;
use rmcp::handler::server::wrapper::Json;
use rmcp::model::ErrorData;
use std::path::Path;

const SEARCH_CONCURRENCY: usize = 16;
const ENRICHMENT_CONCURRENCY: usize = 32;

const DEFINITION_KEYWORDS: &[&str] = &[
    "fn",
    "function",
    "def",
    "struct",
    "class",
    "interface",
    "type",
    "enum",
    "trait",
    "const",
    "static",
    "var",
    "let",
    "mod",
    "impl",
];

#[inline]
fn is_valid_identifier_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

#[inline]
fn is_valid_identifier_continue(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn extract_identifier_prefix(token: &str) -> Option<&str> {
    let mut chars = token.char_indices();

    let (_, first) = chars.next()?;
    if !is_valid_identifier_start(first) {
        return None;
    }

    let mut end_idx = 1;
    for (idx, ch) in chars {
        if is_valid_identifier_continue(ch) {
            end_idx = idx + 1;
        } else {
            break;
        }
    }

    Some(&token[..end_idx])
}

fn truncate_preview(content: &str, max_chars: usize) -> String {
    if content.is_empty() {
        return String::new();
    }

    if content.len() <= max_chars {
        return content.to_string();
    }

    if content.is_ascii() {
        return format!("{}...", &content[..max_chars]);
    }

    let Some((idx, _)) = content.char_indices().nth(max_chars) else {
        return content.to_string();
    };
    format!("{}...", &content[..idx])
}

/// Helper to calculate relevance score for sorting.
///
/// - 3 points for exact name match
/// - 2 points for prefix match
/// - 1 point for contains match
#[must_use]
fn relevance_score(query: &str, match_text: &str) -> u8 {
    match match_text == query {
        true => 3,
        false if match_text.starts_with(query) => 2,
        false if match_text.contains(query) => 1,
        false => 0,
    }
}

/// Intermediate struct to hold match data before enrichment.
struct MatchToEnrich {
    file: String,
    line: u64,
    content: String,
}

/// Result of enriching a match with Tree-sitter.
struct EnrichedMatch {
    semantic_path: String,
    kind: &'static str,
    file: String,
    line: u64,
    preview: String,
}

impl PathfinderServer {
    /// Core logic for the `find_symbol` tool.
    ///
    /// Runs ripgrep across the workspace with language-aware definition patterns
    /// (from spec 007), then enriches each match with Tree-sitter to get the
    /// enclosing semantic path and symbol kind.
    ///
    /// Performance: Uses parallel execution for both searches and Tree-sitter enrichments.
    ///
    /// Results are:
    /// 1. Deduplicated by semantic path
    /// 2. Sorted by relevance (exact match > prefix match > contains match)
    /// 3. Filtered by optional `kind` parameter
    /// 4. Limited to `max_results` entries
    pub(crate) async fn find_symbol_impl(
        &self,
        params: FindSymbolParams,
    ) -> Result<Json<FindSymbolResponse>, ErrorData> {
        let start = std::time::Instant::now();

        tracing::info!(
            tool = "find_symbol",
            name = %params.name,
            kind = ?params.kind,
            path_glob = %params.path_glob,
            max_results = params.max_results,
            "find_symbol: start"
        );

        if params.name.trim().is_empty() {
            return Err(io_error_data("name must not be empty"));
        }

        // Validate name doesn't contain path traversal or regex metacharacters
        let name_lower = params.name.to_lowercase();
        if name_lower.contains("..") || name_lower.contains('/') || name_lower.contains('\\') {
            return Err(io_error_data("name must not contain path separators"));
        }

        // Check if path_glob already filters by extension
        let has_ext_filter = params.path_glob.contains("*.rs")
            || params.path_glob.contains("*.ts")
            || params.path_glob.contains("*.tsx")
            || params.path_glob.contains("*.js")
            || params.path_glob.contains("*.jsx")
            || params.path_glob.contains("*.py")
            || params.path_glob.contains("*.go")
            || params.path_glob.contains("*.java");

        // Build patterns for all supported languages unless filtered
        let extensions = if has_ext_filter {
            vec![("any", params.path_glob.clone())]
        } else {
            vec![
                ("rs", String::from("**/*.rs")),
                ("ts", String::from("**/*.ts")),
                ("tsx", String::from("**/*.tsx")),
                ("js", String::from("**/*.js")),
                ("jsx", String::from("**/*.jsx")),
                ("py", String::from("**/*.py")),
                ("go", String::from("**/*.go")),
                ("java", String::from("**/*.java")),
            ]
        };

        // Phase 1: Build all search tasks
        let mut search_params_list: Vec<SearchParams> = Vec::new();
        for (ext, glob) in extensions {
            let ext_patterns =
                crate::server::tools::navigation::definition_patterns(ext, &params.name);
            for pattern in ext_patterns {
                search_params_list.push(SearchParams {
                    workspace_root: self.workspace_root.path().to_path_buf(),
                    query: pattern,
                    is_regex: true,
                    path_glob: glob.clone(),
                    exclude_glob: "**/node_modules/**".to_string(),
                    max_results: 100,
                    offset: 0,
                    context_lines: 0,
                });
            }
        }

        // When the user's path_glob is not an extension filter and is not a
        // catch-all, extract a directory prefix for post-search filtering.
        let user_path_prefix: Option<String> =
            if !has_ext_filter && !params.path_glob.is_empty() && params.path_glob != "**/*" {
                let trimmed = params
                    .path_glob
                    .trim_end_matches("/**")
                    .trim_end_matches("/*");
                if trimmed.is_empty() || trimmed == "." {
                    None
                } else {
                    Some(format!("{trimmed}/"))
                }
            } else {
                None
            };

        // Phase 2: Parallel searches
        tracing::debug!(
            tool = "find_symbol",
            num_searches = search_params_list.len(),
            "find_symbol: starting parallel searches"
        );

        let search_results: Vec<pathfinder_search::SearchResult> =
            futures::stream::iter(search_params_list)
                .map(|search_params| async move {
                    let query = search_params.query.clone();
                    let result = self.scout.search(&search_params).await;
                    match result {
                        Ok(r) => r,
                        Err(e) => {
                            tracing::warn!(
                                tool = "find_symbol",
                                error = %e,
                                pattern = %query,
                                "scout search failed"
                            );
                            pathfinder_search::SearchResult {
                                matches: Vec::new(),
                                total_matches: 0,
                                truncated: false,
                                files_searched: 0,
                                files_in_scope: 0,
                                binary_skipped: 0,
                                gitignored_skipped: 0,
                                other_skipped: 0,
                            }
                        }
                    }
                })
                .buffer_unordered(SEARCH_CONCURRENCY)
                .collect()
                .await;

        // Phase 3: Flatten results and filter matches
        // OPT-1: Pre-compute canonical workspace root once to avoid the
        // expensive canonicalize() syscall (10-100µs) on every match.
        let canonical_root = self
            .workspace_root
            .path()
            .canonicalize()
            .unwrap_or_else(|_| self.workspace_root.path().to_path_buf());

        let mut matches_to_enrich: Vec<MatchToEnrich> = Vec::new();
        for result in search_results {
            for m in result.matches {
                // Validate file path is safe (no path traversal)
                if m.file.contains("..") || m.file.starts_with('/') || m.file.starts_with('\\') {
                    tracing::warn!(
                        tool = "find_symbol",
                        file = %m.file,
                        "skipping potentially unsafe path"
                    );
                    continue;
                }

                let file_path = Path::new(&m.file);

                // Skip non-workspace files
                if !is_workspace_file(file_path, self.workspace_root.path(), &canonical_root) {
                    continue;
                }

                // Apply sandbox policy check
                if let Err(e) = self.sandbox.check(file_path) {
                    tracing::warn!(
                        tool = "find_symbol",
                        file = %m.file,
                        error = %e,
                        "sandbox denied access to file"
                    );
                    continue;
                }

                // Apply user's path_glob as directory filter
                if let Some(ref prefix) = user_path_prefix {
                    if !m.file.starts_with(prefix.as_str()) {
                        continue;
                    }
                }

                matches_to_enrich.push(MatchToEnrich {
                    file: m.file,
                    line: m.line,
                    content: m.content,
                });
            }
        }

        // OPT-4: Deduplicate by (file, line) before enrichment.
        // Multiple ripgrep patterns can match the same definition (e.g. `fn\s+foo`
        // and `\bfoo\b`). Deduplicating here avoids redundant Tree-sitter file
        // parses + AST walks — the most expensive per-match operation.
        let pre_dedup_count = matches_to_enrich.len();
        {
            let mut seen_locations = std::collections::HashSet::new();
            matches_to_enrich.retain(|m| seen_locations.insert((m.file.clone(), m.line)));
        }
        let dedup_eliminated = pre_dedup_count - matches_to_enrich.len();

        // Phase 4: Parallel enrichments
        tracing::debug!(
            tool = "find_symbol",
            num_matches = matches_to_enrich.len(),
            dedup_eliminated = dedup_eliminated,
            "find_symbol: starting parallel enrichments"
        );

        let kind_filter = params.kind.clone();
        let enriched_with_degraded: Vec<(Option<EnrichedMatch>, bool)> =
            futures::stream::iter(matches_to_enrich)
                .map(|m| {
                    let kind_filter = kind_filter.clone();
                    async move {
                        let file_path = Path::new(&m.file);
                        // m.line is 1-indexed from ripgrep; tree-sitter also expects 1-indexed.
                        // Default to 1 (first line) if conversion fails, preserving valid indexing.
                        let line_usize = usize::try_from(m.line).unwrap_or(1);

                        // Enrich with Tree-sitter to get symbol name and kind.
                        // Use enclosing_symbol_detail() to get the full ExtractedSymbol
                        // including SymbolKind — this provides accurate kind classification
                        // for all languages with treesitter support (including Java methods
                        // which lack a keyword like `fn`/`def`/`function`).
                        let fallback_name = extract_name_from_line(&m.content);
                        let fallback_kind = infer_kind_from_line(&m.content);

                        let (symbol_name, kind, is_degraded) = match self
                            .surgeon
                            .enclosing_symbol_detail(
                                self.workspace_root.path(),
                                file_path,
                                line_usize,
                            )
                            .await
                        {
                            Ok(Some(detail)) => {
                                // Use the semantic path from treesitter for the symbol name
                                let symbol_part = match detail.semantic_path.find("::") {
                                    Some(pos) => detail.semantic_path[pos + 2..].to_string(),
                                    None => detail.semantic_path,
                                };

                                // Validate symbol name is non-empty
                                if symbol_part.is_empty() {
                                    tracing::warn!(
                                        tool = "find_symbol",
                                        file = %m.file,
                                        line = m.line,
                                        "Tree-sitter returned empty symbol name, using fallback"
                                    );
                                    (fallback_name, fallback_kind, true)
                                } else {
                                    // Use SymbolKind from treesitter — accurate for all
                                    // supported languages including Java, Go, Python, etc.
                                    let ts_kind = symbol_kind_to_filter_string(detail.kind);
                                    (symbol_part, ts_kind, false)
                                }
                            }
                            Ok(None) | Err(_) => (fallback_name, fallback_kind, true),
                        };

                        // Validate symbol name is non-empty
                        if symbol_name.trim().is_empty() {
                            tracing::warn!(
                                tool = "find_symbol",
                                file = %m.file,
                                line = m.line,
                                "empty symbol name, skipping match"
                            );
                            return (None, is_degraded);
                        }

                        // Filter by kind if specified
                        if let Some(ref filter_kind) = kind_filter {
                            if !kind_matches_filter(kind, filter_kind) {
                                return (None, is_degraded);
                            }
                        }

                        let semantic_path = format!("{}::{}", m.file, symbol_name);
                        let preview = truncate_preview(&m.content, 100);

                        (
                            Some(EnrichedMatch {
                                semantic_path,
                                kind,
                                file: m.file,
                                line: m.line,
                                preview,
                            }),
                            is_degraded,
                        )
                    }
                })
                .buffer_unordered(ENRICHMENT_CONCURRENCY)
                .collect()
                .await;

        // Phase 5: Build FoundSymbol list, deduplicate, sort, truncate
        let mut any_degraded = false;
        let mut all_matches: Vec<FoundSymbol> = Vec::new();

        for (enriched, is_degraded) in enriched_with_degraded {
            if is_degraded {
                any_degraded = true;
            }
            if let Some(e) = enriched {
                all_matches.push(FoundSymbol {
                    semantic_path: e.semantic_path,
                    kind: e.kind.to_owned(),
                    file: e.file,
                    line: e.line,
                    preview: e.preview,
                });
            }
        }

        // Deduplicate by semantic path
        let mut seen = std::collections::HashSet::new();
        let mut unique_matches: Vec<FoundSymbol> = Vec::new();
        for m in all_matches {
            if seen.insert(m.semantic_path.clone()) {
                unique_matches.push(m);
            }
        }

        // Sort by relevance (exact match > prefix match > contains match)
        unique_matches.sort_by(|a, b| {
            let score_a = relevance_score(
                &params.name,
                &extract_symbol_name_from_path(&a.semantic_path),
            );
            let score_b = relevance_score(
                &params.name,
                &extract_symbol_name_from_path(&b.semantic_path),
            );
            match score_b.cmp(&score_a) {
                std::cmp::Ordering::Equal => match a.file.cmp(&b.file) {
                    std::cmp::Ordering::Equal => a.line.cmp(&b.line),
                    other => other,
                },
                other => other,
            }
        });

        // Limit results
        let total_found = unique_matches.len();
        unique_matches.truncate(params.max_results as usize);

        let duration_ms = start.elapsed().as_millis();
        let search_strategy = if any_degraded {
            "ripgrep+fallback".to_string()
        } else {
            "ripgrep+treesitter".to_string()
        };

        tracing::info!(
            tool = "find_symbol",
            total_found,
            returned = unique_matches.len(),
            duration_ms,
            search_strategy = %search_strategy,
            "find_symbol: complete"
        );

        Ok(Json(FindSymbolResponse {
            symbols: unique_matches,
            total_found: u32::try_from(total_found).unwrap_or(u32::MAX),
            search_strategy,
            duration_ms: Some(u64::try_from(duration_ms).unwrap_or(u64::MAX)),
        }))
    }
}

/// Extract just the symbol name from a semantic path (e.g., "`src/auth.ts::AuthService.login`" -> "AuthService.login")
fn extract_symbol_name_from_path(semantic_path: &str) -> String {
    semantic_path
        .split("::")
        .skip(1)
        .collect::<Vec<_>>()
        .join("::")
}

/// Map a treesitter [`SymbolKind`] to the filter-string vocabulary used by
/// [`kind_matches_filter`]. This is the authoritative kind source when
/// treesitter succeeds — it replaces the heuristic `infer_kind_from_line`.
fn symbol_kind_to_filter_string(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Test => "function",
        SymbolKind::Class => "class",
        SymbolKind::Struct => "struct",
        SymbolKind::Impl => "impl",
        SymbolKind::Constant => "constant",
        SymbolKind::Interface => "interface",
        SymbolKind::Enum => "enum",
        SymbolKind::Module => "module",
        // Vue-specific kinds and other non-standard kinds fall through
        // to "unknown" — agents won't typically filter for these.
        _ => "unknown",
    }
}

/// Heuristic kind classification from a source line's text content.
///
/// Used as a fallback when treesitter is unavailable (unsupported language,
/// parse failure, etc.). For languages with treesitter support, the
/// authoritative kind comes from [`symbol_kind_to_filter_string`].
fn infer_kind_from_line(line: &str) -> &'static str {
    let lower = line.to_lowercase();
    if lower.contains("fn ")
        || lower.contains("func ")
        || lower.contains("function ")
        || lower.contains("def ")
    {
        "function"
    } else if lower.contains("struct ") {
        "struct"
    } else if lower.contains("class ") || lower.contains("type ") {
        "class"
    } else if lower.contains("enum ") {
        "enum"
    } else if lower.contains("trait ") || lower.contains("interface ") {
        "interface"
    } else if lower.contains("const ")
        || lower.contains("static ")
        || lower.contains("var ")
        || lower.contains("let ")
    {
        "constant"
    } else if lower.contains("mod ") || lower.contains("module ") {
        "module"
    } else if lower.contains("impl ") {
        "impl"
    } else {
        "unknown"
    }
}

/// Extract symbol name from line content (fallback when Tree-sitter unavailable)
///
/// Uses keyword scanning with static list; no regex compilation per call.
fn extract_name_from_line(line: &str) -> String {
    let trimmed = line.trim();
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();

    for window in tokens.windows(2) {
        if DEFINITION_KEYWORDS.contains(&window[0]) {
            if let Some(ident) = extract_identifier_prefix(window[1]) {
                return ident.to_string();
            }
        }
    }

    tokens
        .first()
        .and_then(|s| extract_identifier_prefix(s).map(ToString::to_string))
        .unwrap_or_else(|| tokens.first().map(ToString::to_string).unwrap_or_default())
}

/// Check if symbol kind matches the filter.
///
/// The mapping is intentionally broad: `"function"` and `"method"` are treated
/// as synonyms because different languages classify the same concept differently
/// (e.g. Java methods are extracted as `SymbolKind::Function`). An agent asking
/// for `kind="method"` should still find Java methods.
fn kind_matches_filter(kind: &str, filter: &str) -> bool {
    // Use eq_ignore_ascii_case to avoid to_lowercase() allocations.
    // All kind/filter strings are ASCII-only ("function", "struct", etc.).
    if kind.eq_ignore_ascii_case(filter) {
        return true;
    }

    // Normalize filter once for matching. All arms are ASCII lowercase literals.
    if filter.eq_ignore_ascii_case("function")
        || filter.eq_ignore_ascii_case("method")
        || filter.eq_ignore_ascii_case("fn")
    {
        // "function" and "method" are symmetric — both accept function-like kinds
        kind.eq_ignore_ascii_case("function")
            || kind.eq_ignore_ascii_case("method")
            || kind.eq_ignore_ascii_case("fn")
    } else if filter.eq_ignore_ascii_case("class") {
        kind.eq_ignore_ascii_case("class")
            || kind.eq_ignore_ascii_case("struct")
            || kind.eq_ignore_ascii_case("interface")
    } else if filter.eq_ignore_ascii_case("struct") {
        kind.eq_ignore_ascii_case("struct")
    } else if filter.eq_ignore_ascii_case("interface") {
        kind.eq_ignore_ascii_case("interface")
    } else if filter.eq_ignore_ascii_case("enum") {
        kind.eq_ignore_ascii_case("enum")
    } else if filter.eq_ignore_ascii_case("constant") {
        kind.eq_ignore_ascii_case("constant")
            || kind.eq_ignore_ascii_case("const")
            || kind.eq_ignore_ascii_case("static")
            || kind.eq_ignore_ascii_case("let")
    } else if filter.eq_ignore_ascii_case("module") {
        kind.eq_ignore_ascii_case("module")
            || kind.eq_ignore_ascii_case("mod")
            || kind.eq_ignore_ascii_case("namespace")
    } else {
        false
    }
}

/// Check if file is in workspace (not in `node_modules`, target, .git, etc.)
///
/// Takes a pre-computed `canonical_root` to avoid the expensive `canonicalize()`
/// syscall per match. The caller pre-computes this once before the loop.
///
/// Fast path: if the path is relative and contains no `..` traversal, it is
/// unconditionally in-workspace (only skip-pattern filtering applies). This
/// covers ~99% of ripgrep output.
fn is_workspace_file(path: &Path, workspace_root: &Path, canonical_root: &Path) -> bool {
    let path_str = path.to_string_lossy();

    let skip_patterns = [
        "/node_modules/",
        "/target/",
        "/vendor/",
        "/.git/",
        "/dist/",
        "/build/",
    ];

    for pattern in &skip_patterns {
        if path_str.contains(pattern) {
            return false;
        }
    }

    // Fast path: relative paths without traversal are guaranteed in-workspace.
    // The caller (find_symbol_impl Phase 3) already rejects paths containing
    // ".." or starting with "/" / "\\", so this fast path handles the
    // overwhelming majority of matches without any syscall.
    if !path.is_absolute() && !path_str.contains("..") {
        return true;
    }

    // Slow path: only reached for edge cases (absolute paths, symlinks).
    // Uses the pre-computed canonical root — avoids redundant canonicalize().
    let Ok(full_path) = workspace_root.join(path).canonicalize() else {
        return false;
    };

    full_path.starts_with(canonical_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_identifier_start() {
        assert!(is_valid_identifier_start('a'));
        assert!(is_valid_identifier_start('Z'));
        assert!(is_valid_identifier_start('_'));
        assert!(!is_valid_identifier_start('1'));
        assert!(!is_valid_identifier_start('-'));
        assert!(!is_valid_identifier_start(' '));
    }

    #[test]
    fn test_is_valid_identifier_continue() {
        assert!(is_valid_identifier_continue('a'));
        assert!(is_valid_identifier_continue('5'));
        assert!(is_valid_identifier_continue('_'));
        assert!(!is_valid_identifier_continue('-'));
        assert!(!is_valid_identifier_continue(' '));
        assert!(!is_valid_identifier_continue('('));
    }

    #[test]
    fn test_extract_identifier_prefix() {
        assert_eq!(extract_identifier_prefix("my_func("), Some("my_func"));
        assert_eq!(extract_identifier_prefix("MyStruct {"), Some("MyStruct"));
        assert_eq!(
            extract_identifier_prefix("_private_var,"),
            Some("_private_var")
        );
        assert_eq!(extract_identifier_prefix("foo::bar"), Some("foo"));
        assert_eq!(extract_identifier_prefix("123invalid"), None);
        assert_eq!(extract_identifier_prefix(""), None);
    }

    #[test]
    fn test_truncate_preview() {
        // Empty string
        assert_eq!(truncate_preview("", 10), "");

        // Short string, no truncation
        assert_eq!(truncate_preview("hello", 10), "hello");

        // ASCII truncation
        let long_ascii = "this is a very long string that needs truncation";
        assert_eq!(truncate_preview(long_ascii, 10), "this is a ...");

        // Unicode handling
        let unicode = "こんにちは世界";
        assert_eq!(truncate_preview(unicode, 5), "こんにちは...");

        // ASCII fast path (byte count == char count)
        let ascii = "abcdefghijklmnop";
        assert_eq!(truncate_preview(ascii, 10), "abcdefghij...");
    }

    #[test]
    fn test_extract_name_from_line_basic() {
        assert_eq!(extract_name_from_line("fn my_function() {"), "my_function");
        assert_eq!(
            extract_name_from_line("function myFunction() {"),
            "myFunction"
        );
        assert_eq!(
            extract_name_from_line("def my_definition(self):"),
            "my_definition"
        );
        assert_eq!(extract_name_from_line("struct MyStruct {"), "MyStruct");
        assert_eq!(extract_name_from_line("class MyClass {"), "MyClass");
    }

    #[test]
    fn test_extract_name_from_line_with_suffix() {
        // Function name followed by parens
        assert_eq!(
            extract_name_from_line("fn foo_bar(a: i32) -> String"),
            "foo_bar"
        );

        // Struct followed by generic
        assert_eq!(extract_name_from_line("struct Foo<T> {"), "Foo");

        // With path separator in content
        assert_eq!(extract_name_from_line("let x = a::b::c"), "x");
    }

    #[test]
    fn test_extract_name_from_line_fallback() {
        // No keyword match, should use first token
        assert_eq!(
            extract_name_from_line("some_random_line without_keyword"),
            "some_random_line"
        );

        // Empty line
        assert_eq!(extract_name_from_line(""), "");

        // Just whitespace
        assert_eq!(extract_name_from_line("   "), "");
    }

    #[test]
    fn test_relevance_score() {
        assert_eq!(relevance_score("foo", "foo"), 3);
        assert_eq!(relevance_score("foo", "foobar"), 2);
        assert_eq!(relevance_score("foo", "myfoothing"), 1);
        assert_eq!(relevance_score("foo", "barbaz"), 0);
        assert_eq!(relevance_score("MyStruct", "MyStruct"), 3);
    }

    #[test]
    fn test_extract_symbol_name_from_path() {
        assert_eq!(
            extract_symbol_name_from_path("src/auth.ts::AuthService.login"),
            "AuthService.login"
        );
        assert_eq!(
            extract_symbol_name_from_path("lib.rs::foo::bar::baz"),
            "foo::bar::baz"
        );
        assert_eq!(extract_symbol_name_from_path("single_token"), "");
    }

    #[test]
    fn test_infer_kind_from_line() {
        // Rust
        assert_eq!(infer_kind_from_line("fn foo() {"), "function");
        assert_eq!(infer_kind_from_line("pub async fn bar() {"), "function");
        // JavaScript/TypeScript
        assert_eq!(infer_kind_from_line("function bar() {"), "function");
        // Python
        assert_eq!(infer_kind_from_line("def baz():"), "function");
        // Go — `func` was previously missing
        assert_eq!(infer_kind_from_line("func main() {"), "function");
        assert_eq!(
            infer_kind_from_line("func (s *Server) Handle() {"),
            "function"
        );

        assert_eq!(infer_kind_from_line("struct Foo {"), "struct");
        assert_eq!(infer_kind_from_line("class Bar {"), "class");
        assert_eq!(infer_kind_from_line("interface Baz {"), "interface");
        assert_eq!(infer_kind_from_line("trait Qux {"), "interface");

        assert_eq!(infer_kind_from_line("const X = 5;"), "constant");
        assert_eq!(infer_kind_from_line("static Y: i32 = 10;"), "constant");

        assert_eq!(infer_kind_from_line("mod utils;"), "module");
        assert_eq!(infer_kind_from_line("impl Foo {"), "impl");

        // Java methods have no fn/def/function keyword — heuristic returns "unknown".
        // This is expected; Fix 1 uses treesitter SymbolKind as the primary source.
        assert_eq!(
            infer_kind_from_line("    public void processPayment(String txId) {"),
            "unknown"
        );
        assert_eq!(infer_kind_from_line("something_unrecognized"), "unknown");
    }

    #[test]
    fn test_symbol_kind_to_filter_string() {
        use pathfinder_treesitter::surgeon::SymbolKind;

        assert_eq!(
            symbol_kind_to_filter_string(SymbolKind::Function),
            "function"
        );
        assert_eq!(symbol_kind_to_filter_string(SymbolKind::Method), "function");
        assert_eq!(symbol_kind_to_filter_string(SymbolKind::Test), "function");
        assert_eq!(symbol_kind_to_filter_string(SymbolKind::Class), "class");
        assert_eq!(symbol_kind_to_filter_string(SymbolKind::Struct), "struct");
        assert_eq!(symbol_kind_to_filter_string(SymbolKind::Impl), "impl");
        assert_eq!(
            symbol_kind_to_filter_string(SymbolKind::Constant),
            "constant"
        );
        assert_eq!(
            symbol_kind_to_filter_string(SymbolKind::Interface),
            "interface"
        );
        assert_eq!(symbol_kind_to_filter_string(SymbolKind::Enum), "enum");
        assert_eq!(symbol_kind_to_filter_string(SymbolKind::Module), "module");
        // Vue-specific kinds fall through to "unknown"
        assert_eq!(symbol_kind_to_filter_string(SymbolKind::Zone), "unknown");
        assert_eq!(
            symbol_kind_to_filter_string(SymbolKind::Component),
            "unknown"
        );
    }

    #[test]
    fn test_kind_matches_filter() {
        // Exact matches
        assert!(kind_matches_filter("function", "function"));
        assert!(kind_matches_filter("struct", "struct"));
        assert!(kind_matches_filter("class", "class"));

        // Cross-language mappings: filter="function" accepts method/fn kinds
        assert!(kind_matches_filter("fn", "function"));
        assert!(kind_matches_filter("method", "function"));
        assert!(kind_matches_filter("interface", "class"));
        assert!(kind_matches_filter("const", "constant"));
        assert!(kind_matches_filter("mod", "module"));

        // Symmetric: filter="method" also accepts function/fn kinds
        // This is critical for Java: methods are extracted as SymbolKind::Function
        // (mapped to kind="function"), but agents may search with kind="method".
        assert!(kind_matches_filter("function", "method"));
        assert!(kind_matches_filter("fn", "method"));
        assert!(kind_matches_filter("method", "method"));

        // filter="fn" also works symmetrically
        assert!(kind_matches_filter("function", "fn"));
        assert!(kind_matches_filter("method", "fn"));

        // Case insensitive
        assert!(kind_matches_filter("FUNCTION", "function"));
        assert!(kind_matches_filter("struct", "STRUCT"));
        assert!(kind_matches_filter("METHOD", "function"));
        assert!(kind_matches_filter("function", "METHOD"));

        // No match
        assert!(!kind_matches_filter("class", "function"));
        assert!(!kind_matches_filter("enum", "function"));
        assert!(!kind_matches_filter("unknown", "class"));
        assert!(!kind_matches_filter("class", "method"));
        assert!(!kind_matches_filter("enum", "method"));
    }

    #[test]
    fn test_is_workspace_file_relative_fast_path() -> Result<(), Box<dyn std::error::Error>> {
        // OPT-1: relative paths without ".." should hit the fast path
        // and return true without any syscall.
        let dir = tempfile::tempdir()?;
        let canonical = dir.path().canonicalize()?;

        // Normal source files — fast path returns true
        assert!(is_workspace_file(
            Path::new("src/main.rs"),
            dir.path(),
            &canonical
        ));
        assert!(is_workspace_file(
            Path::new("crates/pathfinder/src/lib.rs"),
            dir.path(),
            &canonical
        ));
        assert!(is_workspace_file(
            Path::new("README.md"),
            dir.path(),
            &canonical
        ));
        Ok(())
    }

    #[test]
    fn test_is_workspace_file_skip_patterns() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let canonical = dir.path().canonicalize()?;

        // Skip patterns should reject even on fast path
        assert!(!is_workspace_file(
            Path::new("src/node_modules/lodash/index.js"),
            dir.path(),
            &canonical
        ));
        assert!(!is_workspace_file(
            Path::new("some/target/debug/main"),
            dir.path(),
            &canonical
        ));
        assert!(!is_workspace_file(
            Path::new("project/.git/objects/abc"),
            dir.path(),
            &canonical
        ));
        assert!(!is_workspace_file(
            Path::new("app/vendor/github.com/pkg"),
            dir.path(),
            &canonical
        ));
        assert!(!is_workspace_file(
            Path::new("frontend/dist/bundle.js"),
            dir.path(),
            &canonical
        ));
        assert!(!is_workspace_file(
            Path::new("app/build/output.js"),
            dir.path(),
            &canonical
        ));
        Ok(())
    }

    #[test]
    fn test_is_workspace_file_traversal_slow_path() -> Result<(), Box<dyn std::error::Error>> {
        // Paths with ".." should take the slow path (canonicalize)
        let dir = tempfile::tempdir()?;
        let canonical = dir.path().canonicalize()?;

        // ".." traversal that stays within workspace is still valid
        // but takes the slow path. The joined path may or may not resolve.
        // Create a nested dir so the traversal resolves back to workspace.
        std::fs::create_dir_all(dir.path().join("a/b"))?;
        std::fs::write(dir.path().join("test.txt"), "hello")?;
        assert!(is_workspace_file(
            Path::new("a/b/../../test.txt"),
            dir.path(),
            &canonical
        ));
        Ok(())
    }

    #[test]
    fn test_is_workspace_file_traversal_outside_workspace() -> Result<(), Box<dyn std::error::Error>>
    {
        let dir = tempfile::tempdir()?;
        let canonical = dir.path().canonicalize()?;

        // Traversal outside workspace should fail (canonicalized path won't
        // start with canonical root)
        assert!(!is_workspace_file(
            Path::new("../../etc/passwd"),
            dir.path(),
            &canonical
        ));
        Ok(())
    }

    #[test]
    fn test_pre_enrichment_dedup() {
        // OPT-4: verify dedup by (file, line) retains first occurrence
        // and removes duplicates.
        let mut matches = vec![
            MatchToEnrich {
                file: "src/main.rs".to_string(),
                line: 10,
                content: "fn foo() {".to_string(),
            },
            MatchToEnrich {
                file: "src/main.rs".to_string(),
                line: 10,
                content: "fn foo() {".to_string(), // duplicate
            },
            MatchToEnrich {
                file: "src/main.rs".to_string(),
                line: 20,
                content: "fn bar() {".to_string(), // same file, different line
            },
            MatchToEnrich {
                file: "src/lib.rs".to_string(),
                line: 10,
                content: "fn baz() {".to_string(), // different file, same line
            },
        ];

        let pre_count = matches.len();
        {
            let mut seen = std::collections::HashSet::new();
            matches.retain(|m| seen.insert((m.file.clone(), m.line)));
        }

        assert_eq!(matches.len(), 3);
        assert_eq!(pre_count - matches.len(), 1);
        assert_eq!(matches[0].file, "src/main.rs");
        assert_eq!(matches[0].line, 10);
        assert_eq!(matches[1].file, "src/main.rs");
        assert_eq!(matches[1].line, 20);
        assert_eq!(matches[2].file, "src/lib.rs");
        assert_eq!(matches[2].line, 10);
    }

    #[test]
    fn test_pre_enrichment_dedup_preserves_all_unique() {
        // OPT-4: when all entries are unique, none should be removed.
        let mut matches = vec![
            MatchToEnrich {
                file: "a.rs".to_string(),
                line: 1,
                content: "fn a() {".to_string(),
            },
            MatchToEnrich {
                file: "b.rs".to_string(),
                line: 2,
                content: "fn b() {".to_string(),
            },
            MatchToEnrich {
                file: "c.rs".to_string(),
                line: 3,
                content: "fn c() {".to_string(),
            },
        ];

        let pre_count = matches.len();
        {
            let mut seen = std::collections::HashSet::new();
            matches.retain(|m| seen.insert((m.file.clone(), m.line)));
        }

        assert_eq!(matches.len(), pre_count);
        assert_eq!(matches.len(), 3);
    }
}
