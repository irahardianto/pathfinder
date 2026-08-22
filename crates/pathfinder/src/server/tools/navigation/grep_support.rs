//! Grep support helpers for navigation fallback and cross-file symbol search.
//!
//! Extracted from `search.rs` and `find_symbol.rs` (WS-5 / RSP-A4).
//! Provides `grep_candidates`, `enrich_matches`, `apply_filter_mode`, and `find_symbol_impl_inner`.

use crate::server::helpers::invalid_params_error;
use crate::server::types::{FindSymbolResponse, FoundSymbol};
use crate::server::PathfinderServer;
use futures::StreamExt as _;
use pathfinder_common::types::FilterMode;
use pathfinder_search::{SearchMatch, SearchParams as ScoutSearchParams};
use pathfinder_treesitter::language::SupportedLanguage;
use pathfinder_treesitter::surgeon::SymbolKind;
use rmcp::model::ErrorData;
use std::path::Path;

/// Maximum number of concurrent Tree-sitter enrichment futures per search call.
const ENRICHMENT_CONCURRENCY: usize = 32;
const SEARCH_CONCURRENCY: usize = 16;

/// Per-match enrichment output: `(enclosing_semantic_path, node_type, is_definition)`.
type EnrichResult = (Option<String>, String, Option<bool>);

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

/// Parameters for `find_symbol_impl_inner`.
#[derive(Debug, Clone, Default)]
pub struct FindSymbolParams {
    pub query: String,
    pub kind: Option<String>,
    pub path_glob: String,
    pub max_results: u32,
}

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
        return String::default();
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

/// Validates `find_symbol` input parameters.
fn validate_find_symbol_params(params: &FindSymbolParams) -> Result<(), ErrorData> {
    if params.query.trim().is_empty() {
        return Err(invalid_params_error("query must not be empty"));
    }

    let name_lower = params.query.to_lowercase();
    if name_lower.contains("..") || name_lower.contains('/') || name_lower.contains('\\') {
        return Err(invalid_params_error(
            "query must not contain path separators",
        ));
    }

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

    Ok(())
}

/// Returns `true` when `path_glob` already scopes the search to a single
/// known source-file extension.
fn glob_has_ext_filter(path_glob: &str) -> bool {
    infer_single_ext_from_glob(path_glob).is_some()
        || path_glob.contains("*.rs")
        || path_glob.contains("*.ts")
        || path_glob.contains("*.tsx")
        || path_glob.contains("*.js")
        || path_glob.contains("*.jsx")
        || path_glob.contains("*.py")
        || path_glob.contains("*.go")
        || path_glob.contains("*.java")
}

/// Determines which (extension, glob) pairs to search.
fn resolve_search_extensions(path_glob: &str) -> Vec<(&'static str, String)> {
    if let Some(ext) = infer_single_ext_from_glob(path_glob) {
        return vec![(ext, path_glob.to_string())];
    }
    if glob_has_ext_filter(path_glob) {
        return vec![("any", path_glob.to_string())];
    }
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
}

/// Computes directory prefix for post-search filtering.
fn compute_user_path_prefix(path_glob: &str, has_ext_filter: bool) -> Option<String> {
    if has_ext_filter || path_glob.is_empty() || path_glob == "**/*" {
        return None;
    }
    let trimmed = path_glob.trim_end_matches("/**").trim_end_matches("/*");
    if trimmed.is_empty() || trimmed == "." {
        None
    } else {
        Some(format!("{trimmed}/"))
    }
}

/// Builds search params for definition patterns.
fn build_search_params_list(
    workspace_root: &Path,
    extensions: Vec<(&str, String)>,
    query: &str,
) -> Vec<ScoutSearchParams> {
    let mut search_params_list = Vec::new();
    for (ext, glob) in extensions {
        let ext_patterns = crate::server::tools::navigation::definition_patterns(ext, query);
        for pattern in ext_patterns {
            search_params_list.push(ScoutSearchParams {
                workspace_root: workspace_root.to_path_buf(),
                query: pattern,
                is_regex: true,
                path_glob: glob.clone(),
                exclude_glob: vec!["**/node_modules/**".to_string()],
                max_results: 100,
                offset: 0,
                context_lines: 0,
                ..Default::default()
            });
        }
    }
    search_params_list
}

impl PathfinderServer {
    /// Runs Ripgrep across the workspace and enriches each match with Tree-sitter metadata.
    ///
    /// - Populates `enclosing_semantic_path` and `is_definition` on matches.
    /// - Applies `filter_mode` (code vs comments) unless degraded (unsupported language).
    pub(crate) async fn grep_candidates(
        &self,
        search_params: &ScoutSearchParams,
        filter_mode: FilterMode,
    ) -> Result<Vec<SearchMatch>, ErrorData> {
        let result = self.scout.search(search_params).await.map_err(|e| {
            tracing::warn!(
                tool = "grep_candidates",
                error = %e,
                "scout search failed"
            );
            crate::server::helpers::io_error_data(e.to_string())
        })?;

        let mut enriched_matches = result.matches;
        let node_types = self.enrich_matches(&mut enriched_matches).await;

        let degraded = enriched_matches
            .iter()
            .any(|m| SupportedLanguage::detect(Path::new(&m.file)).is_none());

        let filter_was_bypassed = degraded && filter_mode != FilterMode::All;
        let filtered_matches = if filter_was_bypassed {
            enriched_matches
        } else {
            apply_filter_mode(enriched_matches, &node_types, filter_mode)
        };

        Ok(filtered_matches)
    }

    /// Enrich a slice of search matches with Tree-sitter metadata.
    ///
    /// - Populates `enclosing_semantic_path` on each match.
    /// - Returns a parallel `Vec<String>` of node-type classifications (`"code"`,
    ///   `"comment"`, or `"string"`).
    pub(crate) async fn enrich_matches(&self, matches: &mut [SearchMatch]) -> Vec<String> {
        let snapshots: Vec<(String, u64, u64)> = matches
            .iter()
            .map(|m| (m.file.clone(), m.line, m.column))
            .collect();

        let enrichment: Vec<EnrichResult> = futures::stream::iter(snapshots)
            .map(|(file, line_u64, column_u64)| async move {
                let file_path = Path::new(&file);
                let line = usize::try_from(line_u64).unwrap_or(usize::MAX);
                let column = usize::try_from(column_u64).unwrap_or(0);

                let detail = self
                    .surgeon
                    .enclosing_symbol_detail(self.workspace_root.path(), file_path, line)
                    .await
                    .ok()
                    .flatten();

                let (symbol, is_definition) = if let Some(ref sym) = detail {
                    let path = format!("{file}::{}", sym.semantic_path);
                    debug_assert!(line > 0, "ripgrep line should be 1-indexed, got 0");
                    let is_def = sym.start_line == line.saturating_sub(1);
                    (Some(path), Some(is_def))
                } else {
                    (None, None)
                };

                let node_type = self
                    .surgeon
                    .node_type_at_position(self.workspace_root.path(), file_path, line, column)
                    .await
                    .unwrap_or_else(|_| "code".to_owned());

                (symbol, node_type, is_definition)
            })
            .buffered(ENRICHMENT_CONCURRENCY)
            .collect()
            .await;

        enrichment
            .into_iter()
            .zip(matches.iter_mut())
            .map(|((symbol, node_type, is_definition), m)| {
                m.enclosing_semantic_path = symbol;
                m.is_definition = is_definition;
                node_type
            })
            .collect()
    }

    /// Core logic for cross-file symbol search and did-you-mean resolution.
    pub(crate) async fn find_symbol_impl_inner(
        &self,
        params: FindSymbolParams,
    ) -> Result<FindSymbolResponse, ErrorData> {
        let start = std::time::Instant::now();

        tracing::info!(
            tool = "find_symbol_inner",
            query = %params.query,
            kind = ?params.kind,
            path_glob = %params.path_glob,
            max_results = params.max_results,
            "find_symbol_inner: start"
        );

        validate_find_symbol_params(&params)?;

        let has_ext_filter = glob_has_ext_filter(&params.path_glob);
        let extensions = resolve_search_extensions(&params.path_glob);
        let user_path_prefix = compute_user_path_prefix(&params.path_glob, has_ext_filter);
        let search_params_list =
            build_search_params_list(self.workspace_root.path(), extensions, &params.query);

        let search_results = self.run_parallel_searches(search_params_list).await;

        let pre_dedup_count: usize = search_results.iter().map(|r| r.matches.len()).sum();
        let matches_to_enrich =
            self.filter_and_dedup_matches(search_results, user_path_prefix.as_deref());
        let dedup_eliminated = pre_dedup_count.saturating_sub(matches_to_enrich.len());

        tracing::debug!(
            tool = "find_symbol_inner",
            num_matches = matches_to_enrich.len(),
            dedup_eliminated,
            "find_symbol_inner: starting parallel enrichments"
        );

        let enriched_with_degraded = self
            .enrich_find_symbol_matches(matches_to_enrich, params.kind.clone())
            .await;

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

        let mut seen = std::collections::HashSet::new();
        let mut unique_matches: Vec<FoundSymbol> = Vec::new();
        for m in all_matches {
            if seen.insert(m.semantic_path.clone()) {
                unique_matches.push(m);
            }
        }

        unique_matches.sort_by(|a, b| {
            let score_a = relevance_score(
                &params.query,
                &extract_symbol_name_from_path(&a.semantic_path),
            );
            let score_b = relevance_score(
                &params.query,
                &extract_symbol_name_from_path(&b.semantic_path),
            );
            score_b
                .cmp(&score_a)
                .then_with(|| a.file.cmp(&b.file))
                .then_with(|| a.line.cmp(&b.line))
        });

        let total_found = unique_matches.len();
        unique_matches.truncate(params.max_results as usize);

        let duration_ms = start.elapsed().as_millis();
        let search_strategy = if any_degraded {
            "ripgrep+fallback".to_string()
        } else {
            "ripgrep+treesitter".to_string()
        };

        tracing::info!(
            tool = "find_symbol_inner",
            total_found,
            returned = unique_matches.len(),
            duration_ms,
            search_strategy = %search_strategy,
            "find_symbol_inner: complete"
        );

        Ok(FindSymbolResponse {
            symbols: unique_matches,
            total_found: u32::try_from(total_found).unwrap_or(u32::MAX),
            search_strategy,
            duration_ms: Some(u64::try_from(duration_ms).unwrap_or(u64::MAX)),
            did_you_mean: None,
            hint: None,
        })
    }

    async fn run_parallel_searches(
        &self,
        search_params_list: Vec<ScoutSearchParams>,
    ) -> Vec<pathfinder_search::SearchResult> {
        futures::stream::iter(search_params_list)
            .map(|search_params| async move {
                let query = search_params.query.clone();
                match self.scout.search(&search_params).await {
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
                        }
                    }
                }
            })
            .buffer_unordered(SEARCH_CONCURRENCY)
            .collect()
            .await
    }

    fn filter_and_dedup_matches(
        &self,
        search_results: Vec<pathfinder_search::SearchResult>,
        user_path_prefix: Option<&str>,
    ) -> Vec<MatchToEnrich> {
        let canonical_root = self
            .workspace_root
            .path()
            .canonicalize()
            .unwrap_or_else(|_| self.workspace_root.path().to_path_buf());

        let mut matches_to_enrich: Vec<MatchToEnrich> = Vec::new();
        for result in search_results {
            for m in result.matches {
                if !self.is_admissible_match(&m, user_path_prefix, &canonical_root) {
                    continue;
                }
                matches_to_enrich.push(MatchToEnrich {
                    file: m.file,
                    line: m.line,
                    content: m.content,
                });
            }
        }

        let mut seen_locations = std::collections::HashSet::new();
        matches_to_enrich.retain(|m| seen_locations.insert((m.file.clone(), m.line)));
        matches_to_enrich
    }

    fn is_admissible_match(
        &self,
        m: &pathfinder_search::SearchMatch,
        user_path_prefix: Option<&str>,
        canonical_root: &Path,
    ) -> bool {
        if m.file.contains("..") || m.file.starts_with('/') || m.file.starts_with('\\') {
            tracing::warn!(
                tool = "find_symbol",
                file = %m.file,
                "skipping potentially unsafe path"
            );
            return false;
        }

        let file_path = Path::new(&m.file);

        if !is_workspace_file_path(file_path, self.workspace_root.path(), canonical_root) {
            return false;
        }

        if let Err(e) = self.sandbox.check(file_path) {
            tracing::warn!(
                tool = "find_symbol",
                file = %m.file,
                error = %e,
                "sandbox denied access to file"
            );
            return false;
        }

        if let Some(prefix) = user_path_prefix {
            if !m.file.starts_with(prefix) {
                return false;
            }
        }

        true
    }

    async fn enrich_one_match(
        &self,
        m: MatchToEnrich,
        kind_filter: Option<&str>,
    ) -> (Option<EnrichedMatch>, bool) {
        let file_path = Path::new(&m.file);
        let line_usize = usize::try_from(m.line).unwrap_or(1);

        let fallback_name = extract_name_from_line(&m.content);
        let fallback_kind = infer_kind_from_line(&m.content);

        let (symbol_name, kind, is_degraded) = match self
            .surgeon
            .enclosing_symbol_detail(self.workspace_root.path(), file_path, line_usize)
            .await
        {
            Ok(Some(detail)) => {
                let symbol_part = match detail.semantic_path.find("::") {
                    Some(pos) => detail.semantic_path[pos + 2..].to_string(),
                    None => detail.semantic_path,
                };

                if symbol_part.is_empty() {
                    tracing::warn!(
                        tool = "find_symbol",
                        file = %m.file,
                        line = m.line,
                        "Tree-sitter returned empty symbol name, using fallback"
                    );
                    (fallback_name, fallback_kind, true)
                } else {
                    let ts_kind = symbol_kind_to_filter_string(detail.kind);
                    (symbol_part, ts_kind, false)
                }
            }
            Ok(None) | Err(_) => (fallback_name, fallback_kind, true),
        };

        if symbol_name.trim().is_empty() {
            tracing::warn!(
                tool = "find_symbol",
                file = %m.file,
                line = m.line,
                "empty symbol name, skipping match"
            );
            return (None, is_degraded);
        }

        if let Some(filter_kind) = kind_filter {
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

    async fn enrich_find_symbol_matches(
        &self,
        matches_to_enrich: Vec<MatchToEnrich>,
        kind_filter: Option<String>,
    ) -> Vec<(Option<EnrichedMatch>, bool)> {
        futures::stream::iter(matches_to_enrich)
            .map(|m| {
                let kind_filter = kind_filter.clone();
                async move { self.enrich_one_match(m, kind_filter.as_deref()).await }
            })
            .buffer_unordered(ENRICHMENT_CONCURRENCY)
            .collect()
            .await
    }
}

/// Apply `filter_mode` to a list of enriched matches using pre-computed node types.
pub(crate) fn apply_filter_mode(
    matches: Vec<SearchMatch>,
    node_types: &[String],
    mode: FilterMode,
) -> Vec<SearchMatch> {
    match mode {
        FilterMode::All => matches,
        FilterMode::CodeOnly => matches
            .into_iter()
            .zip(node_types.iter())
            .filter(|(_, t)| t.as_str() == "code")
            .map(|(m, _)| m)
            .collect(),
        FilterMode::CommentsOnly => matches
            .into_iter()
            .zip(node_types.iter())
            .filter(|(_, t)| t.as_str() == "comment" || t.as_str() == "string")
            .map(|(m, _)| m)
            .collect(),
    }
}

fn extract_symbol_name_from_path(semantic_path: &str) -> String {
    semantic_path
        .split("::")
        .skip(1)
        .collect::<Vec<_>>()
        .join("::")
}

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
        _ => "unknown",
    }
}

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

fn kind_matches_filter(kind: &str, filter: &str) -> bool {
    let k = kind.to_ascii_lowercase();
    let f = filter.to_ascii_lowercase();
    if k == f {
        return true;
    }
    match f.as_str() {
        "function" | "method" | "fn" => matches!(k.as_str(), "function" | "method" | "fn"),
        "class" => matches!(k.as_str(), "class" | "struct" | "interface"),
        "struct" => k == "struct",
        "interface" | "trait" => matches!(k.as_str(), "interface" | "trait"),
        "enum" => k == "enum",
        "type" => matches!(
            k.as_str(),
            "class" | "struct" | "interface" | "trait" | "enum"
        ),
        "constant" | "const" | "static" | "let" => {
            matches!(k.as_str(), "constant" | "const" | "static" | "let")
        }
        "module" | "mod" | "namespace" => {
            matches!(k.as_str(), "module" | "mod" | "namespace")
        }
        _ => false,
    }
}

fn is_workspace_file_path(path: &Path, workspace_root: &Path, canonical_root: &Path) -> bool {
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

    if !path.is_absolute() && !path_str.contains("..") {
        return true;
    }

    let Ok(full_path) = workspace_root.join(path).canonicalize() else {
        return false;
    };

    full_path.starts_with(canonical_root)
}

fn infer_single_ext_from_glob(glob: &str) -> Option<&'static str> {
    if glob.contains('{') {
        return None;
    }

    let known: &[(&str, &'static str)] = &[
        ("*.tsx", "tsx"),
        ("*.jsx", "jsx"),
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
        let found = glob.match_indices(marker).any(|(pos, _)| {
            let after_pos = pos + marker.len();
            !glob[after_pos..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
        });
        if found {
            if matched.is_some() {
                return None;
            }
            matched = Some(ext);
        }
    }
    matched
}
