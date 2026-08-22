use crate::cache::AstCache;
use crate::error::SurgeonError;
use crate::language::SupportedLanguage;
use crate::surgeon::{ExtractedSymbol, Surgeon, SymbolKind};
use crate::symbols::{
    did_you_mean_file_qualified, find_enclosing_symbol, find_enclosing_symbol_ref,
    resolve_symbol_chain,
};
use pathfinder_common::types::{SemanticPath, SymbolScope};
use std::path::Path;
use std::sync::Arc;
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

    /// Invalidate any cached AST or symbols for the given file.
    pub fn invalidate(&self, file: &Path) {
        self.cache.invalidate(file);
    }

    /// Read the file and parse it into symbols, returning the full cached data.
    ///
    /// Symbols are cached directly alongside the AST in `AstCache` (TS-A13),
    /// avoiding redundant `spawn_blocking` and AST walks on cache hits.
    async fn cached_parse(
        &self,
        workspace_root: &Path,
        file_path: &Path,
    ) -> Result<
        (
            SupportedLanguage,
            tree_sitter::Tree,
            Arc<[u8]>,
            Arc<Vec<ExtractedSymbol>>,
        ),
        SurgeonError,
    > {
        let lang = SupportedLanguage::detect(file_path)
            .ok_or_else(|| SurgeonError::UnsupportedLanguage(file_path.to_path_buf()))?;

        let abs_path = workspace_root.join(file_path);

        // ── Vue SFC: multi-zone parse path ────────────────────────────────────
        if lang == SupportedLanguage::Vue {
            let (multi, symbols, _content_hash) = self.cache.get_or_parse_vue(&abs_path).await?;
            let (tree, source) = if let Some(script_tree) = multi.script_tree {
                (script_tree, multi.source)
            } else {
                let (t, s, _) = self.cache.get_or_parse(&abs_path, lang).await?;
                (t, s)
            };
            return Ok((lang, tree, source, symbols));
        }

        // ── All other languages: single-zone path ─────────────────────────────
        let (tree, source, symbols) = self.cache.get_or_parse(&abs_path, lang).await?;
        Ok((lang, tree, source, symbols))
    }

    /// Read raw source file and detected language without symbols.
    ///
    /// # Errors
    /// Returns `SurgeonError::ParseError` if reading or decoding the file fails.
    #[instrument(skip(self, workspace_root))]
    pub async fn read_source_file_raw(
        &self,
        workspace_root: &Path,
        file_path: &Path,
    ) -> Result<(String, String), SurgeonError> {
        let (lang, _tree, source, _symbols) = self.cached_parse(workspace_root, file_path).await?;
        let content = std::str::from_utf8(&source)
            .map_err(|_| SurgeonError::ParseError {
                path: file_path.to_path_buf(),
                reason: "Symbol source is not valid UTF-8".into(),
            })?
            .to_string();
        Ok((content, lang.as_str().to_string()))
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
        let chain =
            semantic_path
                .symbol_chain
                .as_ref()
                .ok_or_else(|| SurgeonError::SymbolNotFound {
                    path: semantic_path.to_string(),
                    did_you_mean: vec![],
                })?;

        let (lang, _tree, source, symbols) = self
            .cached_parse(workspace_root, &semantic_path.file_path)
            .await?;

        let symbol =
            resolve_symbol_chain(&symbols, chain).ok_or_else(|| SurgeonError::SymbolNotFound {
                path: semantic_path.to_string(),
                did_you_mean: did_you_mean_file_qualified(
                    &semantic_path.file_path,
                    &symbols,
                    chain,
                    3,
                ),
            })?;

        if symbol.byte_range.is_empty() && symbol.kind != SymbolKind::Zone {
            return Err(SurgeonError::ParseError {
                path: semantic_path.file_path.clone(),
                reason: format!("Symbol '{}' has empty byte range", symbol.semantic_path),
            });
        }

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

        // TS-C1: Read from attached_range if non-empty, otherwise byte_range
        let target_range = if symbol.attached_range.is_empty() {
            &symbol.byte_range
        } else {
            &symbol.attached_range
        };

        let symbol_bytes =
            source
                .get(target_range.clone())
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

        Ok((content, lang.as_str().to_string(), (*symbols).clone()))
    }

    #[instrument(skip(self, workspace_root))]
    async fn extract_symbols(
        &self,
        workspace_root: &Path,
        file_path: &Path,
    ) -> Result<Vec<ExtractedSymbol>, SurgeonError> {
        let (_, _, _, symbols) = self.cached_parse(workspace_root, file_path).await?;
        Ok((*symbols).clone())
    }

    #[instrument(skip(self, workspace_root, content))]
    async fn extract_symbols_preloaded(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        content: Arc<[u8]>,
        mtime: std::time::SystemTime,
    ) -> Result<Vec<ExtractedSymbol>, SurgeonError> {
        let lang = SupportedLanguage::detect(file_path)
            .ok_or_else(|| SurgeonError::UnsupportedLanguage(file_path.to_path_buf()))?;

        let abs_path = workspace_root.join(file_path);

        if lang == SupportedLanguage::Vue {
            let (_multi, symbols, _hash) = self
                .cache
                .get_or_parse_vue_preloaded(&abs_path, &content, mtime)
                .await?;
            return Ok((*symbols).clone());
        }

        let (_tree, _src, symbols) = self
            .cache
            .get_or_parse_preloaded(&abs_path, lang, content, mtime)
            .await?;
        Ok((*symbols).clone())
    }

    #[instrument(skip(self, workspace_root))]
    async fn enclosing_symbol(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: usize,
    ) -> Result<Option<String>, SurgeonError> {
        let row = line.saturating_sub(1);
        let (_, _, _, symbols) = self.cached_parse(workspace_root, file_path).await?;
        Ok(find_enclosing_symbol(&symbols, row))
    }

    #[instrument(skip(self, workspace_root))]
    async fn enclosing_symbol_detail(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: usize,
    ) -> Result<Option<ExtractedSymbol>, SurgeonError> {
        let row = line.saturating_sub(1);
        let (_, _, _, symbols) = self.cached_parse(workspace_root, file_path).await?;
        Ok(find_enclosing_symbol_ref(&symbols, row).cloned())
    }

    #[instrument(skip(self, workspace_root))]
    async fn node_type_at_position(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: usize,
        column: usize,
    ) -> Result<String, SurgeonError> {
        let (lang, tree, _source, _symbols) =
            match self.cached_parse(workspace_root, file_path).await {
                Ok(tuple) => tuple,
                Err(SurgeonError::UnsupportedLanguage(_)) => {
                    return Ok("code".to_string());
                }
                Err(e) => return Err(e),
            };

        let row = line.saturating_sub(1);
        let target_point = tree_sitter::Point { row, column };

        let root = tree.root_node();
        let leaf = root
            .descendant_for_point_range(target_point, target_point)
            .unwrap_or(root);

        let mut current = Some(leaf);
        while let Some(node) = current {
            if is_comment_node(node.kind(), lang) {
                return Ok("comment".to_string());
            }
            if is_string_node(node.kind(), lang) {
                return Ok("string".to_string());
            }
            current = node.parent();
        }

        Ok("code".to_string())
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

    fn invalidate(&self, workspace_root: &Path, file_path: &Path) {
        let abs_path = workspace_root.join(file_path);
        self.cache.invalidate(&abs_path);
    }
}

fn is_comment_node(kind: &str, lang: SupportedLanguage) -> bool {
    match lang {
        SupportedLanguage::Go
        | SupportedLanguage::TypeScript
        | SupportedLanguage::Tsx
        | SupportedLanguage::JavaScript
        | SupportedLanguage::Vue
        | SupportedLanguage::Python => kind == "comment",
        SupportedLanguage::Rust => kind == "line_comment" || kind == "block_comment",
        SupportedLanguage::Java => {
            kind == "line_comment" || kind == "block_comment" || kind == "comment"
        }
    }
}

fn is_string_node(kind: &str, lang: SupportedLanguage) -> bool {
    match lang {
        SupportedLanguage::Go => {
            kind == "interpreted_string_literal" || kind == "raw_string_literal"
        }
        SupportedLanguage::TypeScript
        | SupportedLanguage::Tsx
        | SupportedLanguage::JavaScript
        | SupportedLanguage::Vue => {
            kind == "string"
                || kind == "string_fragment"
                || kind == "template_string"
                || kind == "template_substitution"
        }
        SupportedLanguage::Python => kind == "string" || kind == "string_content",
        SupportedLanguage::Rust => {
            kind == "string_literal" || kind == "raw_string_literal" || kind == "char_literal"
        }
        SupportedLanguage::Java => {
            kind == "string_literal"
                || kind == "character_literal"
                || kind == "text_block"
                || kind == "string_fragment"
        }
    }
}

#[cfg(test)]
#[path = "treesitter_surgeon_test.rs"]
mod tests;
