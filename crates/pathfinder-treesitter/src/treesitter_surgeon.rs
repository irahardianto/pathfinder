use crate::cache::AstCache;
use crate::error::SurgeonError;
use crate::language::SupportedLanguage;
use crate::surgeon::{ExtractedSymbol, Surgeon};
use crate::symbols::{
    did_you_mean, extract_symbols_from_tree, find_enclosing_symbol, resolve_symbol_chain,
};
use pathfinder_common::types::{SemanticPath, SymbolScope};
use std::path::Path;
use tracing::instrument;

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
    async fn cached_parse(
        &self,
        workspace_root: &Path,
        file_path: &Path,
    ) -> Result<
        (
            SupportedLanguage,
            Vec<u8>,
            pathfinder_common::types::VersionHash,
            Vec<ExtractedSymbol>,
        ),
        SurgeonError,
    > {
        let lang = SupportedLanguage::detect(file_path)
            .ok_or_else(|| SurgeonError::UnsupportedLanguage(file_path.to_path_buf()))?;

        let abs_path = workspace_root.join(file_path);

        // This handles reading from disk and caching the parsed Tree + source bytes
        let (tree, source) = self.cache.get_or_parse(&abs_path, lang).await?;

        // Extract symbols via TreeCursor
        let symbols = extract_symbols_from_tree(&tree, &source, lang);

        let hash = pathfinder_common::types::VersionHash::compute(&source);

        Ok((lang, source, hash, symbols))
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

        let (lang, source, version_hash, symbols) = self
            .cached_parse(workspace_root, &semantic_path.file_path)
            .await?;

        let symbol =
            resolve_symbol_chain(&symbols, chain).ok_or_else(|| SurgeonError::SymbolNotFound {
                path: semantic_path.to_string(),
                did_you_mean: did_you_mean(&symbols, chain, 3),
            })?;

        let content = std::str::from_utf8(&source[symbol.byte_range.clone()])
            .map_err(|_| SurgeonError::ParseError("Symbol source is not valid UTF-8".into()))?
            .to_string();

        let language_str = match lang {
            SupportedLanguage::Go => "go",
            SupportedLanguage::TypeScript => "typescript",
            SupportedLanguage::Tsx => "tsx",
            SupportedLanguage::JavaScript => "javascript",
            SupportedLanguage::Python => "python",
            SupportedLanguage::Rust => "rust",
        };

        Ok(SymbolScope {
            content,
            start_line: symbol.start_line,
            end_line: symbol.end_line,
            version_hash,
            language: language_str.to_string(),
        })
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
    async fn generate_skeleton(
        &self,
        workspace_root: &Path,
        path: &Path,
        max_tokens: u32,
        depth: u32,
        visibility: &str,
    ) -> Result<crate::repo_map::RepoMapResult, SurgeonError> {
        crate::repo_map::generate_skeleton_text(
            self,
            workspace_root,
            path,
            max_tokens,
            depth,
            visibility,
        )
        .await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::Builder;

    #[tokio::test]
    async fn test_read_symbol_scope_go() {
        let surgeon = TreeSitterSurgeon::new(2);
        let mut file = Builder::new().suffix(".go").tempfile().unwrap();
        writeln!(file, "package main\n\nfunc Login() {{ println(\"hi\") }}\n").unwrap();
        let path = file.path().to_path_buf();
        // Since NamedTempFile gives an absolute path, we can pretend
        // the workspace root is `/` and the relative path is `path` without prefix `/`.
        let workspace_root = PathBuf::from("/");
        // Hack for testing: absolute paths passed as relative inside SemanticPath
        // will just join properly if workspace_root is `/`
        let relative = path.strip_prefix("/").unwrap();

        let sp = SemanticPath::parse(&format!("{}::Login", relative.display())).unwrap();

        let scope = surgeon
            .read_symbol_scope(&workspace_root, &sp)
            .await
            .unwrap();

        assert_eq!(scope.language, "go");
        assert_eq!(scope.content, "func Login() { println(\"hi\") }");
        assert_eq!(scope.start_line, 2);
        assert_eq!(scope.end_line, 2);
    }

    #[tokio::test]
    async fn test_read_symbol_scope_not_found() {
        let surgeon = TreeSitterSurgeon::new(2);
        let mut file = Builder::new().suffix(".go").tempfile().unwrap();
        writeln!(file, "package main\n\nfunc Login() {{ println(\"hi\") }}\n").unwrap();
        let path = file.path().to_path_buf();
        let workspace_root = PathBuf::from("/");
        let relative = path.strip_prefix("/").unwrap();

        let sp = SemanticPath::parse(&format!("{}::Logn", relative.display())).unwrap(); // typo

        let err = surgeon
            .read_symbol_scope(&workspace_root, &sp)
            .await
            .unwrap_err();
        match err {
            SurgeonError::SymbolNotFound { did_you_mean, .. } => {
                assert_eq!(did_you_mean, vec!["Login"]);
            }
            _ => panic!("Expected SymbolNotFound"),
        }
    }
}
