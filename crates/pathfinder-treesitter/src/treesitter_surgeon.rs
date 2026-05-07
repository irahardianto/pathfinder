use crate::cache::AstCache;
use crate::error::SurgeonError;
use crate::language::SupportedLanguage;
use crate::surgeon::{ExtractedSymbol, Surgeon};
use crate::symbols::{
    did_you_mean, extract_symbols_from_multizone, extract_symbols_from_tree, find_enclosing_symbol,
    resolve_symbol_chain,
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
            let symbols = extract_symbols_from_multizone(&multi);
            let (tree, source) = if let Some(script_tree) = multi.script_tree {
                (script_tree, multi.source)
            } else {
                let (t, s) = self.cache.get_or_parse(&abs_path, lang).await?;
                (t, s)
            };
            return Ok((lang, tree, source, symbols));
        }

        // ── All other languages: single-zone path (unchanged) ─────────────────
        let (tree, source) = self.cache.get_or_parse(&abs_path, lang).await?;
        let symbols = extract_symbols_from_tree(&tree, &source, lang);
        Ok((lang, tree, source, symbols))
    }

    // Private helpers removed (edit-only: find_body_bytes, detect_body_indent,
    // expand_to_full_start_byte) — no longer needed without edit tools.
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

    #[instrument(skip(self, workspace_root))]
    async fn enclosing_symbol(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: usize,
    ) -> Result<Option<String>, SurgeonError> {
        let (_, _, _, symbols) = self.cached_parse(workspace_root, file_path).await?;
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

        let (content, lang, symbols) = surgeon
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

    // ── Edge case: Go method extraction with receiver ───────────────────────────
    // NOTE: Go methods with receivers are extracted as top-level functions.
    // The path format is `file::Handle` (not `file::Server.Handle`).
    // This is a known limitation — Go methods aren't nested under their receiver type.

    #[tokio::test]
    async fn test_extract_go_method_with_receiver_as_top_level() {
        let surgeon = TreeSitterSurgeon::new(2);
        let mut file = Builder::new().suffix(".go").tempfile().unwrap();
        writeln!(
            file,
            "package main\n\ntype Server struct {{}}\n\nfunc (s *Server) Handle() {{\n\t// handle logic\n}}\n"
        )
        .unwrap();
        let path = file.path().to_path_buf();
        let workspace_root = PathBuf::from("/");
        let relative = path.strip_prefix("/").unwrap();

        // Go methods with receivers are extracted as top-level functions
        let sp = SemanticPath::parse(&format!("{}::Handle", relative.display())).unwrap();

        let scope = surgeon
            .read_symbol_scope(&workspace_root, &sp)
            .await
            .unwrap();

        assert_eq!(scope.language, "go");
        assert!(scope.content.contains("func (s *Server) Handle()"));
    }

    // ── Edge case: TypeScript class method ────────────────────────────────────

    #[tokio::test]
    async fn test_extract_typescript_class_method() {
        let surgeon = TreeSitterSurgeon::new(2);
        let mut file = Builder::new().suffix(".ts").tempfile().unwrap();
        writeln!(
            file,
            "class Foo {{\n  private count: number;\n\n  bar(): number {{\n    return this.count;\n  }}\n}}\n"
        )
        .unwrap();
        let path = file.path().to_path_buf();
        let workspace_root = PathBuf::from("/");
        let relative = path.strip_prefix("/").unwrap();

        // TypeScript methods use '.' separator within the symbol chain
        let sp = SemanticPath::parse(&format!("{}::Foo.bar", relative.display())).unwrap();

        let scope = surgeon
            .read_symbol_scope(&workspace_root, &sp)
            .await
            .unwrap();

        assert_eq!(scope.language, "typescript");
        assert!(scope.content.contains("bar()"));
    }

    // ── Edge case: TypeScript arrow function ──────────────────────────────────

    #[tokio::test]
    async fn test_extract_typescript_arrow_function() {
        let surgeon = TreeSitterSurgeon::new(2);
        let mut file = Builder::new().suffix(".ts").tempfile().unwrap();
        writeln!(file, "const fn = () => {{\n  return 42;\n}};\n").unwrap();
        let path = file.path().to_path_buf();
        let workspace_root = PathBuf::from("/");
        let relative = path.strip_prefix("/").unwrap();

        let sp = SemanticPath::parse(&format!("{}::fn", relative.display())).unwrap();

        let scope = surgeon
            .read_symbol_scope(&workspace_root, &sp)
            .await
            .unwrap();

        assert_eq!(scope.language, "typescript");
        assert!(scope.content.contains("fn"));
    }

    // ── Edge case: Python decorator + function ────────────────────────────────

    #[tokio::test]
    async fn test_extract_python_decorator_function() {
        let surgeon = TreeSitterSurgeon::new(2);
        let mut file = Builder::new().suffix(".py").tempfile().unwrap();
        writeln!(file, "@decorator\ndef func():\n    pass\n").unwrap();
        let path = file.path().to_path_buf();
        let workspace_root = PathBuf::from("/");
        let relative = path.strip_prefix("/").unwrap();

        let sp = SemanticPath::parse(&format!("{}::func", relative.display())).unwrap();

        let scope = surgeon
            .read_symbol_scope(&workspace_root, &sp)
            .await
            .unwrap();

        assert_eq!(scope.language, "python");
        // The scope should include the decorator
        assert!(scope.content.contains("@decorator") || scope.content.contains("def func"));
    }

    // ── Edge case: Empty function body ────────────────────────────────────────

    #[tokio::test]
    async fn test_extract_empty_function_body() {
        let surgeon = TreeSitterSurgeon::new(2);
        let mut file = Builder::new().suffix(".go").tempfile().unwrap();
        writeln!(file, "package main\n\nfunc foo() {{}}\n").unwrap();
        let path = file.path().to_path_buf();
        let workspace_root = PathBuf::from("/");
        let relative = path.strip_prefix("/").unwrap();

        let sp = SemanticPath::parse(&format!("{}::foo", relative.display())).unwrap();

        let scope = surgeon
            .read_symbol_scope(&workspace_root, &sp)
            .await
            .unwrap();

        assert_eq!(scope.language, "go");
        assert!(scope.content.contains("func foo()"));
    }

    // ── Edge case: Bare file (unsupported language) ───────────────────────────

    #[tokio::test]
    async fn test_extract_bare_file_unsupported_language() {
        let surgeon = TreeSitterSurgeon::new(2);
        let mut file = Builder::new().suffix(".txt").tempfile().unwrap();
        writeln!(file, "This is just plain text with no parseable symbols.\n").unwrap();
        let path = file.path().to_path_buf();
        let workspace_root = PathBuf::from("/");
        let relative = path.strip_prefix("/").unwrap();

        // Bare file path (no symbol chain) — .txt is not a supported language
        let sp = SemanticPath::parse(relative.to_string_lossy().as_ref()).unwrap();

        let err = surgeon
            .read_source_file(&workspace_root, &sp.file_path)
            .await
            .unwrap_err();

        match err {
            SurgeonError::UnsupportedLanguage(_) => {
                // Expected
            }
            _ => panic!("Expected UnsupportedLanguage error, got: {err:?}"),
        }
    }

    // ── Edge case: Nested impl blocks (Rust) ───────────────────────────────────

    #[tokio::test]
    async fn test_extract_nested_impl_block() {
        let surgeon = TreeSitterSurgeon::new(2);
        let mut file = Builder::new().suffix(".rs").tempfile().unwrap();
        writeln!(
            file,
            "struct Foo {{}}\n\nimpl Foo {{\n    fn outer() {{\n        struct Baz {{}}\n        impl Baz {{\n            fn inner() {{}}\n        }}\n    }}\n}}\n"
        )
        .unwrap();
        let path = file.path().to_path_buf();
        let workspace_root = PathBuf::from("/");
        let relative = path.strip_prefix("/").unwrap();

        // Rust impl methods use '.' separator within the symbol chain
        let sp = SemanticPath::parse(&format!("{}::Foo.outer", relative.display())).unwrap();

        let scope = surgeon
            .read_symbol_scope(&workspace_root, &sp)
            .await
            .unwrap();

        assert_eq!(scope.language, "rust");
        assert!(scope.content.contains("fn outer"));
    }
}
