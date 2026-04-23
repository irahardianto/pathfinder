use crate::cache::AstCache;
use crate::error::SurgeonError;
use crate::language::SupportedLanguage;
use crate::surgeon::{BodyRange, ExtractedSymbol, FullRange, Surgeon, SymbolRange};
use crate::symbols::{
    did_you_mean, extract_symbols_from_multizone, extract_symbols_from_tree, find_enclosing_symbol,
    resolve_symbol_chain,
};
use pathfinder_common::types::{SemanticPath, SymbolScope, VersionHash};
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
            Vec<u8>,
            pathfinder_common::types::VersionHash,
            Vec<ExtractedSymbol>,
        ),
        SurgeonError,
    > {
        let lang = SupportedLanguage::detect(file_path)
            .ok_or_else(|| SurgeonError::UnsupportedLanguage(file_path.to_path_buf()))?;

        let abs_path = workspace_root.join(file_path);

        // ── Vue SFC: multi-zone parse path ────────────────────────────────────
        if lang == SupportedLanguage::Vue {
            let (multi, content_hash) = self.cache.get_or_parse_vue(&abs_path).await?;
            let symbols = extract_symbols_from_multizone(&multi);
            // Return the script tree as the representative tree (used only for
            // body-range / full-range ops, which are only valid on script symbols).
            // If no script tree, synthesize a minimal fallback: Vue SFCs without
            // a <script> block can still be read for template/style symbols.
            let (tree, source) = if let Some(script_tree) = multi.script_tree {
                (script_tree, multi.source)
            } else {
                // Template-only SFC: parse the source as TypeScript to get an
                // empty-but-valid Tree (the caller only reads symbols, not the tree).
                let (t, s) = self.cache.get_or_parse(&abs_path, lang).await?;
                (t, s)
            };
            return Ok((lang, tree, source, content_hash, symbols));
        }

        // ── All other languages: single-zone path (unchanged) ─────────────────
        let (tree, source) = self.cache.get_or_parse(&abs_path, lang).await?;
        let symbols = extract_symbols_from_tree(&tree, &source, lang);
        let hash = pathfinder_common::types::VersionHash::compute(&source);
        Ok((lang, tree, source, hash, symbols))
    }

    /// Find the body node byte range for a resolved symbol node.
    ///
    /// Walks tree-sitter child nodes to find the body/block field. Returns
    /// `(open_brace_byte, close_brace_byte)` or an error if the target has
    /// no body.
    fn find_body_bytes(
        tree: &tree_sitter::Tree,
        source: &[u8],
        symbol_byte_range: std::ops::Range<usize>,
        symbol_path: &str,
    ) -> Result<(usize, usize), SurgeonError> {
        let root = tree.root_node();

        // Find the tree-sitter node that exactly matches the symbol's byte range
        let sym_node = root
            .named_descendant_for_byte_range(symbol_byte_range.start, symbol_byte_range.end)
            .ok_or_else(|| SurgeonError::ParseError {
                path: std::path::PathBuf::from(symbol_path),
                reason: "symbol node not found in AST".to_owned(),
            })?;

        // Try the `body` field first (covers Go, TypeScript, JavaScript, Python, Rust)
        let body_node = sym_node
            .child_by_field_name("body")
            // Fall back to walking named children for any unusual grammar.
            .or_else(|| {
                let mut cursor = sym_node.walk();
                // Materialize the result so cursor is dropped before or_else returns
                let found = sym_node.named_children(&mut cursor).find(|c| {
                    matches!(
                        c.kind(),
                        "block"
                            | "statement_block"
                            | "compound_statement"
                            | "class_body"
                            | "declaration_list"
                    )
                });
                found
            });

        if let Some(body) = body_node {
            // Return the byte offsets of the opening/closing brace characters.
            // Most grammars include the braces in the body node range.
            Ok((body.start_byte(), body.end_byte()))
        } else {
            // Check if the symbol kind is simply not body-bearing
            let source_snippet = source
                .get(symbol_byte_range)
                .and_then(|b| std::str::from_utf8(b).ok())
                .unwrap_or("<non-utf8>")
                .chars()
                .take(80)
                .collect::<String>();

            Err(SurgeonError::InvalidTarget {
                path: symbol_path.to_owned(),
                reason: format!(
                    "symbol has no block body (snippet: \"{source_snippet}...\"). \
                     Use replace_full for declarations without a body."
                ),
            })
        }
    }

    /// Detect the actual indentation column of a block body.
    ///
    /// For brace-delimited blocks (`{ ... }`), scans past the opening brace to find
    /// the first non-empty line inside the block. For non-brace blocks (e.g., Python),
    /// uses the line containing the start byte.
    fn detect_body_indent(
        source: &[u8],
        start_byte: usize,
        end_byte: usize,
        is_brace_block: bool,
        fallback_indent: usize,
    ) -> usize {
        if !is_brace_block {
            // Non-brace block: use indentation of the line containing start_byte
            let line_start = source
                .get(..start_byte)
                .unwrap_or(b"")
                .iter()
                .rposition(|&b| b == b'\n')
                .map_or(0, |pos| pos + 1);

            if let Some(line_bytes) = source.get(line_start..end_byte) {
                if let Ok(full_str) = std::str::from_utf8(line_bytes) {
                    if let Some(line) = full_str.lines().next() {
                        return line.len() - line.trim_start().len();
                    }
                }
            }
            return fallback_indent;
        }

        // Brace-delimited block: find first non-empty line inside the block
        let block_bytes = source.get((start_byte + 1)..end_byte);
        let block_str = match block_bytes {
            Some(bytes) => match std::str::from_utf8(bytes) {
                Ok(s) => s,
                Err(_) => return fallback_indent,
            },
            None => return fallback_indent,
        };

        // Find the first line that is purely inside the block and not on the same line as `{`
        let mut lines = block_str.split('\n');
        let _same_line_as_brace = lines.next();

        for line in lines {
            if !line.trim().is_empty() {
                return line.len() - line.trim_start().len();
            }
        }

        fallback_indent
    }

    fn expand_to_full_start_byte(source: &[u8], mut start_byte: usize) -> usize {
        loop {
            let line_start = source
                .get(..start_byte)
                .unwrap_or(b"")
                .iter()
                .rposition(|&b| b == b'\n')
                .map_or(0, |pos| pos + 1);

            let (prev_line_start, prev_line_end) = if line_start == 0 {
                if start_byte == 0 {
                    break;
                }
                (0, start_byte)
            } else {
                let end = line_start - 1; // before \n
                let start = source
                    .get(..end)
                    .unwrap_or(b"")
                    .iter()
                    .rposition(|&b| b == b'\n')
                    .map_or(0, |pos| pos + 1);
                (start, end)
            };

            let prev_line = source.get(prev_line_start..prev_line_end).unwrap_or(b"");
            let trimmed = String::from_utf8_lossy(prev_line);
            let trimmed_ref = trimmed.trim();

            if trimmed_ref.is_empty() {
                break;
            }

            if trimmed_ref.starts_with("//")
                || trimmed_ref.starts_with("/*")
                || trimmed_ref.starts_with('*')
                || trimmed_ref.starts_with('#')
                || trimmed_ref.starts_with('@')
            {
                start_byte = prev_line_start;
            } else {
                break;
            }
        }
        start_byte
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

        let (lang, _tree, source, version_hash, symbols) = self
            .cached_parse(workspace_root, &semantic_path.file_path)
            .await?;

        let symbol =
            resolve_symbol_chain(&symbols, chain).ok_or_else(|| SurgeonError::SymbolNotFound {
                path: semantic_path.to_string(),
                did_you_mean: did_you_mean(&symbols, chain, 3),
            })?;

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
            version_hash,
            language: language_str.to_string(),
        })
    }

    #[instrument(skip(self, workspace_root))]
    async fn read_source_file(
        &self,
        workspace_root: &Path,
        file_path: &Path,
    ) -> Result<(String, VersionHash, String, Vec<ExtractedSymbol>), SurgeonError> {
        let (lang, _tree, source, version_hash, symbols) =
            self.cached_parse(workspace_root, file_path).await?;

        let content = std::str::from_utf8(&source)
            .map_err(|_| SurgeonError::ParseError {
                path: file_path.to_path_buf(),
                reason: "Symbol source is not valid UTF-8".into(),
            })?
            .to_string();

        Ok((content, version_hash, lang.as_str().to_string(), symbols))
    }

    #[instrument(skip(self, workspace_root))]
    async fn extract_symbols(
        &self,
        workspace_root: &Path,
        file_path: &Path,
    ) -> Result<Vec<ExtractedSymbol>, SurgeonError> {
        let (_, _, _, _, symbols) = self.cached_parse(workspace_root, file_path).await?;
        Ok(symbols)
    }

    #[instrument(skip(self, workspace_root))]
    async fn enclosing_symbol(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: usize,
    ) -> Result<Option<String>, SurgeonError> {
        let (_, _, _, _, symbols) = self.cached_parse(workspace_root, file_path).await?;
        // `find_enclosing_symbol` uses 0-indexed lines; `line` is 1-indexed.
        Ok(find_enclosing_symbol(&symbols, line.saturating_sub(1)))
    }

    #[instrument(skip(self, workspace_root))]
    async fn node_type_at_position(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: usize,
        column: usize,
    ) -> Result<String, SurgeonError> {
        let (_, tree, _, _, _) = self.cached_parse(workspace_root, file_path).await?;

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

    #[instrument(skip(self, workspace_root), fields(path = %semantic_path))]
    async fn resolve_body_range(
        &self,
        workspace_root: &Path,
        semantic_path: &SemanticPath,
    ) -> Result<(BodyRange, Vec<u8>, VersionHash), SurgeonError> {
        let chain =
            semantic_path
                .symbol_chain
                .as_ref()
                .ok_or_else(|| SurgeonError::SymbolNotFound {
                    path: semantic_path.to_string(),
                    did_you_mean: vec![],
                })?;

        // Use the shared parse/cache/extract pipeline
        let (_lang, tree, source, version_hash, symbols) = self
            .cached_parse(workspace_root, &semantic_path.file_path)
            .await?;

        let symbol =
            resolve_symbol_chain(&symbols, chain).ok_or_else(|| SurgeonError::SymbolNotFound {
                path: semantic_path.to_string(),
                did_you_mean: did_you_mean(&symbols, chain, 3),
            })?;

        let last_newline_pos = source
            .get(..symbol.byte_range.start)
            .unwrap_or(&[])
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |pos| pos + 1);
        let indent_column = symbol.byte_range.start.saturating_sub(last_newline_pos);

        let (start_byte, end_byte) = Self::find_body_bytes(
            &tree,
            &source,
            symbol.byte_range.clone(),
            &semantic_path.to_string(),
        )?;

        // Detect actual body indentation
        let is_brace_block = source.get(start_byte) == Some(&b'{');
        let fallback_indent = indent_column + 4;
        let body_indent_column = Self::detect_body_indent(
            &source,
            start_byte,
            end_byte,
            is_brace_block,
            fallback_indent,
        );

        Ok((
            BodyRange {
                start_byte,
                end_byte,
                indent_column,
                body_indent_column,
            },
            source,
            version_hash,
        ))
    }

    #[instrument(skip(self, workspace_root), fields(path = %semantic_path))]
    async fn resolve_full_range(
        &self,
        workspace_root: &Path,
        semantic_path: &SemanticPath,
    ) -> Result<(FullRange, Vec<u8>, VersionHash), SurgeonError> {
        let chain =
            semantic_path
                .symbol_chain
                .as_ref()
                .ok_or_else(|| SurgeonError::SymbolNotFound {
                    path: semantic_path.to_string(),
                    did_you_mean: vec![],
                })?;

        let (_lang, _tree, source, version_hash, symbols) = self
            .cached_parse(workspace_root, &semantic_path.file_path)
            .await?;

        let symbol =
            resolve_symbol_chain(&symbols, chain).ok_or_else(|| SurgeonError::SymbolNotFound {
                path: semantic_path.to_string(),
                did_you_mean: did_you_mean(&symbols, chain, 3),
            })?;

        let start_byte = Self::expand_to_full_start_byte(&source, symbol.byte_range.start);
        let end_byte = symbol.byte_range.end;

        let last_newline_pos = source
            .get(..symbol.byte_range.start)
            .unwrap_or(b"")
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |pos| pos + 1);
        let indent_column = symbol.byte_range.start.saturating_sub(last_newline_pos);

        Ok((
            FullRange {
                start_byte,
                end_byte,
                indent_column,
            },
            source,
            version_hash,
        ))
    }

    #[instrument(skip(self, workspace_root), fields(path = %semantic_path))]
    async fn resolve_symbol_range(
        &self,
        workspace_root: &Path,
        semantic_path: &SemanticPath,
    ) -> Result<(SymbolRange, Vec<u8>, VersionHash), SurgeonError> {
        let (full_range, source, hash) = self
            .resolve_full_range(workspace_root, semantic_path)
            .await?;

        Ok((
            SymbolRange {
                start_byte: full_range.start_byte,
                end_byte: full_range.end_byte,
                indent_column: full_range.indent_column,
            },
            source,
            hash,
        ))
    }

    fn invalidate_cache(&self, path: &std::path::Path) {
        self.cache.invalidate(path);
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
        | "char_literal" // C/C++/Java
    )
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

    // ── node_type_at_position integration tests ───────────────────────────────

    #[tokio::test]
    async fn test_node_type_at_position_code_line() {
        let surgeon = TreeSitterSurgeon::new(2);
        let mut file = Builder::new().suffix(".go").tempfile().unwrap();
        // Line 1: package main (1-indexed) — code
        writeln!(file, "package main\n\nfunc Hello() {{}}\n").unwrap();
        let path = file.path().to_path_buf();
        let workspace_root = PathBuf::from("/");
        let relative = path.strip_prefix("/").unwrap();

        let node_type = surgeon
            .node_type_at_position(&workspace_root, relative, 1, 0)
            .await
            .unwrap();

        assert_eq!(node_type, "code", "package declaration should be code");
    }

    #[tokio::test]
    async fn test_node_type_at_position_comment_line() {
        let surgeon = TreeSitterSurgeon::new(2);
        let mut file = Builder::new().suffix(".go").tempfile().unwrap();
        // Line 1: // This is a comment
        writeln!(file, "// This is a comment\npackage main\n").unwrap();
        let path = file.path().to_path_buf();
        let workspace_root = PathBuf::from("/");
        let relative = path.strip_prefix("/").unwrap();

        let node_type = surgeon
            .node_type_at_position(&workspace_root, relative, 1, 3)
            .await
            .unwrap();

        assert_eq!(
            node_type, "comment",
            "// comment line should be classified as comment"
        );
    }

    #[tokio::test]
    async fn test_node_type_at_position_string_literal() {
        let surgeon = TreeSitterSurgeon::new(2);
        let mut file = Builder::new().suffix(".go").tempfile().unwrap();
        // Line 3: msg := "hello world"
        writeln!(
            file,
            "package main\n\nfunc main() {{\n\tmsg := \"hello world\"\n\t_ = msg\n}}\n"
        )
        .unwrap();
        let path = file.path().to_path_buf();
        let workspace_root = PathBuf::from("/");
        let relative = path.strip_prefix("/").unwrap();

        // Line 4 (1-indexed), column 9 is inside "hello world"
        let node_type = surgeon
            .node_type_at_position(&workspace_root, relative, 4, 10)
            .await
            .unwrap();

        assert_eq!(
            node_type, "string",
            "text inside string literal should be classified as string"
        );
    }

    // ── Vue SFC multi-zone integration tests ─────────────────────────────────

    const BASIC_VUE_SFC: &[u8] = br#"<template>
  <div class="app">
    <MyButton @click="doThing">Click me</MyButton>
  </div>
</template>
<script setup lang="ts">
import { ref } from 'vue'
const count = ref(0)
function doThing() { count.value++ }
</script>
<style scoped>
.app { color: red; }
#main { font-size: 16px; }
</style>"#;

    #[tokio::test]
    async fn test_read_source_file_vue_returns_all_zones() {
        let surgeon = TreeSitterSurgeon::new(2);
        let mut file = Builder::new().suffix(".vue").tempfile().unwrap();
        file.write_all(BASIC_VUE_SFC).unwrap();
        let workspace_root = PathBuf::from("/");
        let relative = file.path().strip_prefix("/").unwrap();

        let (content, _hash, lang, symbols) = surgeon
            .read_source_file(&workspace_root, relative)
            .await
            .unwrap();

        assert_eq!(lang, "vue");
        assert!(!content.is_empty(), "should return original SFC content");

        // Script symbols at top level
        let func_sym = symbols.iter().find(|s| s.name == "doThing");
        assert!(func_sym.is_some(), "script function should be at top level");

        // Template zone container
        let template_sym = symbols.iter().find(|s| s.name == "template");
        assert!(template_sym.is_some(), "template zone container must exist");
        let template_children = &template_sym.unwrap().children;
        assert!(
            template_children.iter().any(|c| c.name == "MyButton"),
            "MyButton component must be a template child"
        );

        // Style zone container
        let style_sym = symbols.iter().find(|s| s.name == "style");
        assert!(style_sym.is_some(), "style zone container must exist");
        let style_children = &style_sym.unwrap().children;
        assert!(
            style_children.iter().any(|c| c.name == ".app"),
            ".app CSS class must be a style child"
        );
    }

    #[tokio::test]
    async fn test_enclosing_symbol_inside_template_zone() {
        let surgeon = TreeSitterSurgeon::new(2);
        let mut file = Builder::new().suffix(".vue").tempfile().unwrap();
        file.write_all(BASIC_VUE_SFC).unwrap();
        let workspace_root = PathBuf::from("/");
        let relative = file.path().strip_prefix("/").unwrap();

        // Line 3 is inside the <template> zone (MyButton line)
        let enc = surgeon
            .enclosing_symbol(&workspace_root, relative, 3)
            .await
            .unwrap();

        assert!(enc.is_some(), "should find an enclosing symbol on line 3");
        let path = enc.unwrap();
        assert!(
            path.starts_with("template"),
            "enclosing symbol should be prefixed with 'template', got: '{path}'"
        );
    }
}
