//! `search` tool (symbol mode) — Resolve a bare symbol name to its `file::symbol` semantic path(s).

use crate::server::helpers::invalid_params_error;
use crate::server::types::{FindSymbolResponse, FoundSymbol, SearchParams as SearchToolParams};
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

/// Returns `true` if the given kind string is a valid canonical value or accepted alias.
///
/// Canonical values: `function`, `class`, `struct`, `interface`, `enum`, `constant`, `module`, `impl`.
/// Aliases (case-insensitive): `method`/`fn` → function; `trait` → interface;
/// `const`/`static`/`let` → constant; `mod`/`namespace` → module.
/// `class` also matches struct and interface.
/// `unknown` is a valid internal kind but not a useful filter value — excluded intentionally.
fn is_valid_kind_filter(kind: &str) -> bool {
    matches!(
        kind.to_ascii_lowercase().as_str(),
        "function"
            | "method"
            | "fn"
            | "class"
            | "struct"
            | "interface"
            | "trait"
            | "enum"
            | "type"
            | "constant"
            | "const"
            | "static"
            | "let"
            | "module"
            | "mod"
            | "namespace"
            | "impl"
    )
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
    pub(crate) async fn find_symbol_impl_inner(
        &self,
        params: SearchToolParams,
    ) -> Result<Json<FindSymbolResponse>, ErrorData> {
        let start = std::time::Instant::now();

        tracing::info!(
            tool = "find_symbol",
            query = %params.query,
            kind = ?params.kind,
            path_glob = %params.path_glob,
            max_results = params.max_results,
            "find_symbol_inner: start"
        );

        if params.query.trim().is_empty() {
            return Err(invalid_params_error("query must not be empty"));
        }

        // Validate name doesn't contain path traversal or regex metacharacters
        let name_lower = params.query.to_lowercase();
        if name_lower.contains("..") || name_lower.contains('/') || name_lower.contains('\\') {
            return Err(invalid_params_error(
                "query must not contain path separators",
            ));
        }

        // Validate kind filter early — invalid values silently return 0 results otherwise.
        // Return a descriptive error so agents immediately know what values are accepted.
        if let Some(ref kind) = params.kind {
            if !is_valid_kind_filter(kind) {
                return Err(invalid_params_error(format!(
                    "Unknown kind filter: \"{kind}\". \
                     Canonical values: function, class, struct, interface, enum, type, constant, module, impl. \
                     Accepted aliases (case-insensitive): method/fn -> function; trait -> interface; \
                     const/static/let -> constant; mod/namespace -> module. \
                     class also matches struct and interface; type matches class, struct, interface, trait, and enum."
                )));
            }
        }

        // Check if path_glob already filters by a single known extension.
        // When it does, use the proper language-specific definition patterns for that
        // extension instead of the "any" catch-all (which falls back to a bare \bname\b
        // word boundary search and produces false positives in call sites, imports, etc.).
        //
        // Examples:
        //   "**/*.rs"      → inferred ext "rs" → Rust fn/struct/enum patterns
        //   "src/**/*.ts"  → inferred ext "ts" → TS function/class/const patterns
        //   "**/*.go"      → inferred ext "go" → Go func/type patterns
        //   "**/*"         → no ext, has_ext_filter = false → all languages searched
        //   "**/*.{ts,tsx}"→ multi-ext brace expansion → "any" fallback (ambiguous)
        let inferred_single_ext = infer_single_ext_from_glob(&params.path_glob);
        let has_ext_filter = inferred_single_ext.is_some()
            || params.path_glob.contains("*.rs")
            || params.path_glob.contains("*.ts")
            || params.path_glob.contains("*.tsx")
            || params.path_glob.contains("*.js")
            || params.path_glob.contains("*.jsx")
            || params.path_glob.contains("*.py")
            || params.path_glob.contains("*.go")
            || params.path_glob.contains("*.java");

        // Build patterns for all supported languages unless filtered.
        // When a single extension is detected, run only that extension's patterns
        // against the user-provided glob (which already scopes the file set).
        let extensions: Vec<(&str, String)> = if let Some(ext) = inferred_single_ext {
            // Single extension detected — use language-specific patterns + user glob.
            vec![(ext, params.path_glob.clone())]
        } else if has_ext_filter {
            // Multi-extension or unrecognised glob pattern — fall back to bare word search.
            // This is less precise but avoids producing zero results for unusual globs.
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
                crate::server::tools::navigation::definition_patterns(ext, &params.query);
            for pattern in ext_patterns {
                search_params_list.push(SearchParams {
                    workspace_root: self.workspace_root.path().to_path_buf(),
                    query: pattern,
                    is_regex: true,
                    path_glob: glob.clone(),
                    exclude_glob: vec!["**/node_modules/**".to_string()],
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
                &params.query,
                &extract_symbol_name_from_path(&a.semantic_path),
            );
            let score_b = relevance_score(
                &params.query,
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
            did_you_mean: None,
            hint: None,
        }))
    }

    /// Resolve a bare symbol name to its `file::symbol` semantic path(s).
    ///
    /// This is the public entry point that wraps `find_symbol_impl_inner`
    /// with did-you-mean suggestions when no exact matches are found.
    ///
    /// Performance: Uses parallel execution for both searches and Tree-sitter enrichments.
    ///
    /// Results are:
    /// 1. Deduplicated by semantic path
    /// 2. Sorted by relevance (exact match > prefix match > contains match)
    /// 3. Filtered by optional `kind` parameter
    /// 4. Limited to `max_results` entries
    /// 5. When empty, includes `did_you_mean` suggestions if similar symbols exist
    pub(crate) async fn find_symbol_impl(
        &self,
        params: SearchToolParams,
    ) -> Result<Json<FindSymbolResponse>, ErrorData> {
        let start = std::time::Instant::now();
        let query = params.query.clone();
        let path_glob = params.path_glob.clone();

        let Json(result) = self.find_symbol_impl_inner(params).await?;

        if result.symbols.is_empty() && !query.is_empty() {
            let suggestions = self.compute_symbol_did_you_mean(&query, &path_glob).await;

            if !suggestions.is_empty() {
                return Ok(Json(FindSymbolResponse {
                    symbols: vec![],
                    total_found: 0,
                    search_strategy: result.search_strategy,
                    duration_ms: Some(crate::server::helpers::millis_to_u64(
                        start.elapsed().as_millis(),
                    )),
                    did_you_mean: Some(suggestions),
                    hint: Some(
                        "No exact symbol match found. Check did_you_mean for suggested alternative paths."
                            .to_string(),
                    ),
                }));
            }
        }

        Ok(Json(result))
    }

    /// Compute did-you-mean suggestions for a failed symbol search.
    /// Uses text search to find definitions and usages containing the query,
    /// then extracts symbol names from those matches.
    async fn compute_symbol_did_you_mean(&self, query: &str, path_glob: &str) -> Vec<String> {
        let search_params = crate::server::types::SearchParams {
            query: query.to_string(),
            mode: crate::server::types::SearchMode::Text,
            path_glob: path_glob.to_string(),
            max_results: 50,
            ..Default::default()
        };

        let Ok(Json(response)) = self.search_codebase_impl(search_params).await else {
            return vec![];
        };

        let mut suggestions: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for m in response.matches {
            if !m.content.is_empty() {
                let is_likely_definition = m.is_definition == Some(true)
                    || DEFINITION_KEYWORDS
                        .iter()
                        .any(|kw| m.content.contains(&format!("{kw} ")));

                if is_likely_definition {
                    let name = extract_name_from_line(&m.content);
                    if !name.is_empty() && !name.contains(char::is_whitespace) {
                        let semantic_path = format!("{}::{}", m.file, name);
                        if seen.insert(semantic_path.clone()) {
                            suggestions.push(semantic_path);
                        }
                    }
                }
            }

            if let Some(ref path) = m.enclosing_semantic_path {
                if !path.is_empty() {
                    let semantic_path = format!("{}::{}", m.file, path);
                    if seen.insert(semantic_path.clone()) {
                        suggestions.push(semantic_path);
                    }
                }
            }
        }

        suggestions.truncate(5);
        suggestions
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
    } else if filter.eq_ignore_ascii_case("interface") || filter.eq_ignore_ascii_case("trait") {
        kind.eq_ignore_ascii_case("interface") || kind.eq_ignore_ascii_case("trait")
    } else if filter.eq_ignore_ascii_case("enum") {
        kind.eq_ignore_ascii_case("enum")
    } else if filter.eq_ignore_ascii_case("type") {
        kind.eq_ignore_ascii_case("class")
            || kind.eq_ignore_ascii_case("struct")
            || kind.eq_ignore_ascii_case("interface")
            || kind.eq_ignore_ascii_case("trait")
            || kind.eq_ignore_ascii_case("enum")
    } else if filter.eq_ignore_ascii_case("constant")
        || filter.eq_ignore_ascii_case("const")
        || filter.eq_ignore_ascii_case("static")
        || filter.eq_ignore_ascii_case("let")
    {
        // Canonical kind from tree-sitter is always "constant".
        // Aliases on the filter side (const/static/let) must also resolve to
        // the constant category so agents aren't surprised by 0 results.
        kind.eq_ignore_ascii_case("constant")
            || kind.eq_ignore_ascii_case("const")
            || kind.eq_ignore_ascii_case("static")
            || kind.eq_ignore_ascii_case("let")
    } else if filter.eq_ignore_ascii_case("module")
        || filter.eq_ignore_ascii_case("mod")
        || filter.eq_ignore_ascii_case("namespace")
    {
        // Canonical kind from tree-sitter is always "module".
        // Aliases on the filter side must also resolve correctly.
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

/// Extract a single canonical extension from a glob pattern string.
///
/// Returns `Some(ext)` only when the glob unambiguously targets a single known
/// source language (e.g. `"**/*.rs"` → `Some("rs")`). Returns `None` for:
/// - Multi-extension brace expansions: `"**/*.{ts,tsx}"`, `"**/*.{js,jsx}"`
/// - Extension-agnostic globs:         `"**/*"`, `"src/*"`, `"target_dir/*"`
/// - Unknown extensions:               `"**/*.xyz"`
///
/// This is intentionally conservative: when in doubt, `None` is returned and the
/// caller falls back to the appropriate strategy (either all-languages search or
/// the "any" bare-word fallback).
fn infer_single_ext_from_glob(glob: &str) -> Option<&'static str> {
    // Brace expansion (multi-ext) — bail immediately.
    if glob.contains('{') {
        return None;
    }

    // Extension markers paired with their canonical names.
    //
    // IMPORTANT: order matters — longer extensions must come before their prefixes.
    // "*.tsx" must appear before "*.ts" because "*.tsx" contains "*.ts" as a substring.
    // The matching loop stops at the first hit, but we still scan all markers to detect
    // multi-extension globs. We therefore use an anchored suffix check: a marker only
    // matches when the next character after it is NOT alphanumeric (i.e., no additional
    // extension characters follow). This prevents "*.ts" from matching inside "*.tsx".
    let known: &[(&str, &'static str)] = &[
        ("*.tsx", "tsx"), // before *.ts
        ("*.jsx", "jsx"), // before *.js
        ("*.rs", "rs"),
        ("*.ts", "ts"),
        ("*.js", "js"),
        ("*.py", "py"),
        ("*.go", "go"),
        ("*.java", "java"),
        ("*.vue", "vue"),
    ];

    let mut matched: Option<&'static str> = None;
    for (marker, ext) in known {
        // Anchored check: "*.ts" must not be followed by another alphabetic char
        // (which would indicate a longer extension like tsx).
        let found = glob.match_indices(marker).any(|(pos, _)| {
            let after_pos = pos + marker.len();
            !glob[after_pos..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
        });
        if found {
            if matched.is_some() {
                // More than one extension detected — ambiguous, bail.
                return None;
            }
            matched = Some(ext);
        }
    }
    matched
}

#[cfg(test)]
#[path = "find_symbol_test.rs"]
mod tests;
