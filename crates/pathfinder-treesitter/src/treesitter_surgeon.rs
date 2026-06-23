use crate::cache::AstCache;
use crate::error::SurgeonError;
use crate::language::SupportedLanguage;
use crate::surgeon::{ExtractedSymbol, Surgeon, SymbolKind};
use crate::symbols::{
    did_you_mean, extract_symbols_from_multizone, extract_symbols_from_tree, find_enclosing_symbol,
    find_enclosing_symbol_ref, resolve_symbol_chain,
};
use pathfinder_common::types::{SemanticPath, SymbolScope};
use std::path::Path;
use tracing::instrument;

/// Convert a `SymbolKind` to its string representation for `parent_kind`.
///
/// Matches the vocabulary used in `find_symbol.rs::symbol_kind_to_filter_string`.
fn symbol_kind_to_parent_kind(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Test | SymbolKind::Function | SymbolKind::Method => "function",
        SymbolKind::Class => "class",
        SymbolKind::Struct => "struct",
        SymbolKind::Impl => "impl",
        SymbolKind::Constant => "constant",
        SymbolKind::Interface => "interface",
        SymbolKind::Enum => "enum",
        SymbolKind::Module => "module",
        SymbolKind::Zone => "zone",
        SymbolKind::Component => "component",
        SymbolKind::HtmlElement => "element",
        SymbolKind::CssSelector => "selector",
        SymbolKind::CssAtRule => "at-rule",
    }
}

/// The concrete implementation of the `Surgeon` trait powered by tree-sitter.
#[derive(Debug)]
pub struct TreeSitterSurgeon {
    cache: AstCache,
}

impl TreeSitterSurgeon {
    /// Create a new surgeon with a specified max cache size.
    #[must_use]
    pub fn new(max_cache_entries: usize) -> Self {
        Self {
            cache: AstCache::new(max_cache_entries),
        }
    }

    /// Read the file and parse it into symbols, returning the full cached data.
    ///
    /// For Vue SFCs this delegates to the multi-zone path (`get_or_parse_vue`)
    /// and returns flattened multi-zone symbols. For all other languages the
    /// existing single-zone path is used unchanged.
    async fn cached_parse(
        &self,
        workspace_root: &Path,
        file_path: &Path,
    ) -> Result<
        (
            SupportedLanguage,
            tree_sitter::Tree,
            std::sync::Arc<[u8]>,
            Vec<ExtractedSymbol>,
        ),
        SurgeonError,
    > {
        let lang = SupportedLanguage::detect(file_path)
            .ok_or_else(|| SurgeonError::UnsupportedLanguage(file_path.to_path_buf()))?;

        let abs_path = workspace_root.join(file_path);

        // ── Vue SFC: multi-zone parse path ────────────────────────────────────
        if lang == SupportedLanguage::Vue {
            let (multi, _content_hash) = self.cache.get_or_parse_vue(&abs_path).await?;
            let multi_clone = multi.clone(); // CLONE: cloned for spawn_blocking
            let abs_path_clone = abs_path.clone(); // CLONE: cloned for spawn_blocking error path
            let symbols =
                tokio::task::spawn_blocking(move || extract_symbols_from_multizone(&multi_clone))
                    .await
                    .map_err(|_| SurgeonError::ParseError {
                        path: abs_path_clone,
                        reason: "spawn_blocking task panicked during symbol extraction".into(),
                    })?;
            let (tree, source) = if let Some(script_tree) = multi.script_tree {
                (script_tree, multi.source)
            } else {
                let (t, s) = self.cache.get_or_parse(&abs_path, lang).await?;
                (t, s)
            };
            return Ok((lang, tree, source, symbols));
        }

        // ── All other languages: single-zone path ─────────────────────────────
        let (tree, source) = self.cache.get_or_parse(&abs_path, lang).await?;
        let tree_clone = tree.clone(); // CLONE: cloned for spawn_blocking
        let source_clone = source.clone(); // CLONE: cloned for spawn_blocking
        let abs_path_clone = abs_path.clone(); // CLONE: cloned for spawn_blocking error path
        let symbols = tokio::task::spawn_blocking(move || {
            extract_symbols_from_tree(&tree_clone, &source_clone, lang)
        })
        .await
        .map_err(|_| SurgeonError::ParseError {
            path: abs_path_clone,
            reason: "spawn_blocking task panicked during symbol extraction".into(),
        })?;
        Ok((lang, tree, source, symbols))
    }
}

#[async_trait::async_trait]
impl Surgeon for TreeSitterSurgeon {
    #[instrument(skip(self, workspace_root), fields(path = %semantic_path))]
    async fn read_symbol_scope(
        &self,
        workspace_root: &Path,
        semantic_path: &SemanticPath,
    ) -> Result<SymbolScope, SurgeonError> {
        let chain = semantic_path.symbol_chain.as_ref().ok_or_else(|| {
            SurgeonError::SymbolNotFound {
                path: semantic_path.to_string(),
                did_you_mean: vec![], // It's just a file, so it doesn't have symbols inside the request
            }
        })?;

        let (lang, _tree, source, symbols) = self
            .cached_parse(workspace_root, &semantic_path.file_path)
            .await?;

        let symbol =
            resolve_symbol_chain(&symbols, chain).ok_or_else(|| SurgeonError::SymbolNotFound {
                path: semantic_path.to_string(),
                did_you_mean: did_you_mean(&symbols, chain, 3),
            })?;

        let (parent_kind, parent_name) = if chain.segments.len() > 1 {
            let parent_chain = pathfinder_common::types::SymbolChain {
                segments: chain.segments[..chain.segments.len() - 1].to_vec(),
            };
            resolve_symbol_chain(&symbols, &parent_chain).map(|parent| {
                (
                    Some(symbol_kind_to_parent_kind(parent.kind).to_string()),
                    Some(parent.name.clone()),
                )
            })
        } else {
            None
        }
        .unzip();

        let symbol_bytes =
            source
                .get(symbol.byte_range.clone())
                .ok_or_else(|| SurgeonError::ParseError {
                    path: semantic_path.file_path.clone(),
                    reason: "Symbol byte range out of bounds".into(),
                })?;
        let content = std::str::from_utf8(symbol_bytes)
            .map_err(|_| SurgeonError::ParseError {
                path: semantic_path.file_path.clone(),
                reason: "Symbol source is not valid UTF-8".into(),
            })?
            .to_string();

        let language_str = lang.as_str();

        Ok(SymbolScope {
            content,
            start_line: symbol.start_line,
            end_line: symbol.end_line,
            name_column: symbol.name_column,
            language: language_str.to_string(),
            parent_kind: parent_kind.flatten(),
            parent_name: parent_name.flatten(),
        })
    }

    #[instrument(skip(self, workspace_root))]
    async fn read_source_file(
        &self,
        workspace_root: &Path,
        file_path: &Path,
    ) -> Result<(String, String, Vec<ExtractedSymbol>), SurgeonError> {
        let (lang, _tree, source, symbols) = self.cached_parse(workspace_root, file_path).await?;

        let content = std::str::from_utf8(&source)
            .map_err(|_| SurgeonError::ParseError {
                path: file_path.to_path_buf(),
                reason: "Symbol source is not valid UTF-8".into(),
            })?
            .to_string();

        Ok((content, lang.as_str().to_string(), symbols))
    }

    #[instrument(skip(self, workspace_root))]
    async fn extract_symbols(
        &self,
        workspace_root: &Path,
        file_path: &Path,
    ) -> Result<Vec<ExtractedSymbol>, SurgeonError> {
        let (_, _, _, symbols) = self.cached_parse(workspace_root, file_path).await?;
        Ok(symbols)
    }

    #[instrument(skip(self, workspace_root, content))]
    async fn extract_symbols_preloaded(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        content: std::sync::Arc<[u8]>,
        mtime: std::time::SystemTime,
    ) -> Result<Vec<ExtractedSymbol>, SurgeonError> {
        let lang = SupportedLanguage::detect(file_path)
            .ok_or_else(|| SurgeonError::UnsupportedLanguage(file_path.to_path_buf()))?;

        let abs_path = workspace_root.join(file_path);

        if lang == SupportedLanguage::Vue {
            let (multi, _hash) = self
                .cache
                .get_or_parse_vue_preloaded(&abs_path, &content, mtime)
                .await?;
            let multi_clone = multi.clone(); // CLONE: cloned for spawn_blocking
            let abs_path_clone = abs_path.clone(); // CLONE: cloned for spawn_blocking error path
            let symbols =
                tokio::task::spawn_blocking(move || extract_symbols_from_multizone(&multi_clone))
                    .await
                    .map_err(|_| SurgeonError::ParseError {
                        path: abs_path_clone,
                        reason: "spawn_blocking task panicked during symbol extraction".into(),
                    })?;
            return Ok(symbols);
        }

        let (tree, source) = self
            .cache
            .get_or_parse_preloaded(&abs_path, lang, content, mtime)
            .await?;
        let tree_clone = tree.clone(); // CLONE: cloned for spawn_blocking
        let source_clone = source.clone(); // CLONE: cloned for spawn_blocking
        let abs_path_clone = abs_path.clone(); // CLONE: cloned for spawn_blocking error path
        let symbols = tokio::task::spawn_blocking(move || {
            extract_symbols_from_tree(&tree_clone, &source_clone, lang)
        })
        .await
        .map_err(|_| SurgeonError::ParseError {
            path: abs_path_clone,
            reason: "spawn_blocking task panicked during symbol extraction".into(),
        })?;
        Ok(symbols)
    }

    #[instrument(skip(self, workspace_root))]
    async fn enclosing_symbol(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: usize,
    ) -> Result<Option<String>, SurgeonError> {
        let (_, _, _, symbols) = self.cached_parse(workspace_root, file_path).await?;
        Ok(find_enclosing_symbol(&symbols, line.saturating_sub(1)))
    }

    #[instrument(skip(self, workspace_root))]
    async fn enclosing_symbol_detail(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: usize,
    ) -> Result<Option<ExtractedSymbol>, SurgeonError> {
        let (_, _, _, symbols) = self.cached_parse(workspace_root, file_path).await?;
        Ok(find_enclosing_symbol_ref(&symbols, line.saturating_sub(1)).cloned())
    }

    #[instrument(skip(self, workspace_root))]
    async fn node_type_at_position(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: usize,
        column: usize,
    ) -> Result<String, SurgeonError> {
        let (_, tree, ..) = self.cached_parse(workspace_root, file_path).await?;

        // Convert 1-indexed line to 0-indexed row for Tree-sitter
        let row = line.saturating_sub(1);
        let point = tree_sitter::Point { row, column };

        // Find the deepest AST node at this position
        let root = tree.root_node();
        let Some(mut node) = root.descendant_for_point_range(point, point) else {
            // No node found — treat as code (safe default)
            return Ok("code".to_owned());
        };

        // Walk up the ancestor chain classifying by node kind
        loop {
            let kind = node.kind();
            if is_comment_node(kind) {
                return Ok("comment".to_owned());
            }
            if is_string_node(kind) {
                return Ok("string".to_owned());
            }
            match node.parent() {
                Some(parent) => node = parent,
                None => break,
            }
        }

        Ok("code".to_owned())
    }

    #[instrument(skip(self, workspace_root, config))]
    async fn generate_skeleton(
        &self,
        workspace_root: &Path,
        path: &Path,
        config: &crate::repo_map::SkeletonConfig<'_>,
    ) -> Result<crate::repo_map::RepoMapResult, SurgeonError> {
        crate::repo_map::generate_skeleton_text(self, workspace_root, path, config).await
    }
}

// ── Node-type classification helpers ───────────────────────────────────────

/// Returns `true` if the tree-sitter node kind is a comment variant.
///
/// Covers all Tier 1 languages (Go, TypeScript, Python, Rust) plus common patterns
/// from Tier 2 (Rust, Java, C/C++). Node kind names come from the respective
/// tree-sitter grammars bundled with each language crate.
fn is_comment_node(kind: &str) -> bool {
    matches!(
        kind,
        // All Tier 1 + Tier 2 comment node kinds (unique per tree-sitter grammar)
        "comment"         // Go, TypeScript, Python, JavaScript
        | "line_comment"  // Rust, Java, C, C++
        | "block_comment" // Rust, Java, C, C++
        | "doc_comment"   // Rust
        | "html_comment" // JSX
    )
}

/// Returns `true` if the tree-sitter node kind is a string literal variant.
///
/// Covers all Tier 1 languages. Template literals (JS/TS backtick strings)
/// are included because they count as string context for `filter_mode`.
fn is_string_node(kind: &str) -> bool {
    matches!(
        kind,
        // All Tier 1 + Tier 2 string node kinds (unique per tree-sitter grammar)
        "string"                      // Python, Go
        | "string_literal"            // Rust, Java, C, C++
        | "raw_string_literal"        // Go, Rust
        | "interpreted_string_literal" // Go
        | "template_string"           // TypeScript
        | "template_literal"          // JavaScript
        | "string_fragment"           // TypeScript/JavaScript
        | "template_substitution"     // ${...} inside template literals
        | "jsx_text"                  // JSX inline text
        | "string_content"            // Python multi-line strings
        | "concatenated_string"       // Python implicit string concat
        | "char_literal"              // C/C++/Java
        | "text_block" // Java 15+ multi-line text blocks
    )
}

#[cfg(test)]
#[path = "treesitter_surgeon_test.rs"]
mod tests;
