use std::path::Path;
use tree_sitter::Language;

/// Language node types used for extracting symbols from the AST.
#[derive(Debug)]
pub struct LanguageNodeTypes {
    /// Node kinds that represent functions.
    pub function_kinds: &'static [&'static str],
    /// Node kinds that represent classes.
    pub class_kinds: &'static [&'static str],
    /// Node kinds that represent methods.
    pub method_kinds: &'static [&'static str],
    /// Node kinds that represent impl blocks (e.g. `impl_item` in Rust).
    /// When non-empty, the extractor will descent into these nodes and extract
    /// their child function items as `SymbolKind::Method` under the impl type.
    pub impl_kinds: &'static [&'static str],
    /// Node kinds that represent constants.
    pub constant_kinds: &'static [&'static str],
    /// Node kinds that represent scoped module blocks.
    /// Contents are extracted as named children under the module's path segment.
    /// Example: Rust `mod tests { fn foo() {} }` → `tests` (Module) with child `foo`.
    pub module_kinds: &'static [&'static str],
    /// Node kinds that represent the body block of a declaration.
    ///
    /// Used as a language-aware fallback in `find_body_bytes` when
    /// `child_by_field_name("body")` returns `None`. The primary `body` field
    /// lookup covers most grammars; this list provides defense-in-depth for
    /// grammars that rename the field or use an unusual body node kind.
    ///
    /// Node kinds are specific to each language's tree-sitter grammar.
    pub body_kinds: &'static [&'static str],
    /// Line prefixes that indicate metadata lines (doc comments, decorators,
    /// attributes) to absorb when expanding a symbol's full range upward.
    ///
    /// Used by `expand_to_full_start_byte` to walk backward from a symbol's
    /// start byte and include preceding comments/decorators in the range.
    /// Language-specific to avoid incorrectly absorbing unrelated lines:
    /// e.g., `#` is a Rust attribute but has no meaning in Go, `@` is a
    /// decorator in TypeScript/Python but not valid in Rust or Go.
    pub metadata_prefixes: &'static [&'static str],
}

/// The programming languages natively supported by the Surgeon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SupportedLanguage {
    /// The Go programming language.
    Go,
    /// The TypeScript programming language.
    TypeScript,
    /// The TSX (TypeScript + JSX) file extension.
    Tsx,
    /// The JavaScript programming language.
    JavaScript,
    /// The Python programming language.
    Python,
    /// The Rust programming language.
    Rust,
    /// Vue Single-File Component (Phase 1: <script> block parsed as TypeScript).
    Vue,
}

impl SupportedLanguage {
    /// Attempt to map a file extension to a `SupportedLanguage`.
    #[must_use]
    pub fn detect(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?;
        match ext {
            "go" => Some(Self::Go),
            "ts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            "js" | "jsx" => Some(Self::JavaScript),
            "py" => Some(Self::Python),
            "rs" => Some(Self::Rust),
            "vue" => Some(Self::Vue),
            _ => None,
        }
    }

    /// Return the string representation of the language.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Go => "go",
            Self::TypeScript => "typescript",
            Self::Vue => "vue",
            Self::Tsx => "tsx",
            Self::JavaScript => "javascript",
            Self::Python => "python",
            Self::Rust => "rust",
        }
    }

    /// Load the corresponding tree-sitter language grammar.
    #[must_use]
    pub fn grammar(&self) -> Language {
        match self {
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::TypeScript | Self::Vue => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
        }
    }

    /// Get the node type maps for building semantic paths.
    #[must_use]
    pub const fn node_types(&self) -> &'static LanguageNodeTypes {
        match self {
            Self::Go => &LanguageNodeTypes {
                function_kinds: &["function_declaration"],
                class_kinds: &["type_spec", "type_alias"],
                method_kinds: &["method_declaration", "method_spec", "method_elem"],
                impl_kinds: &[],
                constant_kinds: &["const_declaration", "var_declaration"],
                module_kinds: &[],
                body_kinds: &["block", "field_declaration_list", "method_spec_list"],
                metadata_prefixes: &["//", "/*", "*"],
            },
            Self::TypeScript | Self::Tsx | Self::JavaScript | Self::Vue => &LanguageNodeTypes {
                function_kinds: &["function_declaration", "generator_function_declaration"],
                class_kinds: &[
                    "class_declaration",
                    "interface_declaration",
                    "enum_declaration",
                    "abstract_class_declaration",
                    "type_alias_declaration",
                ],
                method_kinds: &["method_definition"],
                impl_kinds: &[],
                constant_kinds: &["lexical_declaration", "variable_declaration"],
                module_kinds: &["internal_module"],
                body_kinds: &["statement_block", "class_body", "enum_body", "object_type"],
                metadata_prefixes: &["//", "/*", "*", "@"],
            },
            Self::Python => &LanguageNodeTypes {
                function_kinds: &["function_definition", "decorated_definition"],
                class_kinds: &["class_definition"],
                method_kinds: &[], // Python treats methods as functions inside classes
                impl_kinds: &[],
                constant_kinds: &[],
                module_kinds: &[],
                body_kinds: &["block", "compound_statement"],
                metadata_prefixes: &["@", "#"],
            },
            Self::Rust => &LanguageNodeTypes {
                function_kinds: &["function_item"],
                class_kinds: &["struct_item", "enum_item", "trait_item", "type_item"],
                method_kinds: &[],
                // `impl_item` nodes contain associated functions — handled separately
                // so that methods are grouped under the implementing type's name.
                impl_kinds: &["impl_item"],
                constant_kinds: &["const_item", "static_item"],
                module_kinds: &["mod_item"],
                body_kinds: &[
                    "block",
                    "declaration_list",
                    "field_declaration_list",
                    "enum_variant_list",
                ],
                metadata_prefixes: &["//", "/*", "*", "#"],
            },
        }
    }

    /// Pre-process source bytes before parsing.
    ///
    /// For most languages this is a no-op (returns a reference to the original slice).
    /// For Vue SFCs ([`SupportedLanguage::Vue`]) this extracts the `<script>` or
    /// `<script setup>` block content so the TypeScript grammar can parse it.
    ///
    /// Using `Cow` avoids an allocation for the common case (all non-Vue languages).
    #[must_use]
    pub fn preprocess_source<'a>(&self, source: &'a [u8]) -> std::borrow::Cow<'a, [u8]> {
        if *self == Self::Vue {
            std::borrow::Cow::Owned(extract_vue_script(source))
        } else {
            std::borrow::Cow::Borrowed(source)
        }
    }
}

/// Extract the content of the first `<script>` or `<script setup ...>` block from a Vue SFC.
///
/// Returns only the content *between* the opening `<script ...>` and closing `</script>` tags,
/// preserving the line count by inserting blank lines for the lines before the script block.
/// This ensures that line numbers reported by tree-sitter match the original file.
///
/// Returns an empty `Vec` if no script block is found — the parser will create a valid but
/// empty AST, avoiding a hard error on templateonly Vue files.
#[must_use]
pub fn extract_vue_script(source: &[u8]) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(source) else {
        return Vec::new();
    };

    // Find the opening <script ...> tag (handles <script> and <script setup lang="ts"> etc.)
    let script_open_end = {
        let mut pos = None;
        let bytes = text.as_bytes();
        let mut i = 0;
        while i + 7 < bytes.len() {
            // Look for '<script' (case-sensitive per HTML spec for SFCs)
            if bytes[i..].starts_with(b"<script") {
                let tag_start = i;
                // Find the closing '>' of the opening tag
                if let Some(rel) = bytes[i..].iter().position(|&b| b == b'>') {
                    let gt_pos = i + rel;
                    // Make sure this isn't </script>
                    if bytes[tag_start + 1] != b'/' {
                        pos = Some(gt_pos + 1); // byte after '>'
                        break;
                    }
                }
            }
            i += 1;
        }
        match pos {
            Some(p) => p,
            None => return Vec::new(), // No <script> tag found
        }
    };

    // Find the closing </script> tag
    let script_close_start = match text[script_open_end..].find("</script>") {
        Some(rel) => script_open_end + rel,
        None => return Vec::new(),
    };

    let script_content = &text[script_open_end..script_close_start];

    // Pad the prefix with spaces and preserve newlines so that both
    // tree-sitter byte offsets AND line numbers match the original SFC.
    let mut result = Vec::with_capacity(script_close_start);
    for &b in &text.as_bytes()[..script_open_end] {
        if b == b'\n' {
            result.push(b'\n');
        } else {
            result.push(b' ');
        }
    }
    result.extend_from_slice(script_content.as_bytes());
    result
}

/// Count the number of `ERROR` and `MISSING` nodes in a Tree-sitter parse
/// of the given source, using the language appropriate for the file path.
///
/// Returns `None` if the file type is not supported (non-source files).
/// Used by `replace_batch` for post-apply structural validation.
#[must_use]
pub fn count_parse_errors(source: &[u8], file_path: &std::path::Path) -> Option<usize> {
    let lang = SupportedLanguage::detect(file_path)?;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang.grammar()).ok()?;
    let tree = parser.parse(source, None)?;
    Some(count_error_nodes_recursive(tree.root_node()))
}

fn count_error_nodes_recursive(node: tree_sitter::Node) -> usize {
    let mut count = usize::from(node.is_error() || node.is_missing());
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        count += count_error_nodes_recursive(child);
    }
    count
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_language() {
        assert_eq!(
            SupportedLanguage::detect(Path::new("main.go")),
            Some(SupportedLanguage::Go)
        );
        assert_eq!(
            SupportedLanguage::detect(Path::new("app.ts")),
            Some(SupportedLanguage::TypeScript)
        );
        assert_eq!(
            SupportedLanguage::detect(Path::new("app.tsx")),
            Some(SupportedLanguage::Tsx)
        );
        assert_eq!(
            SupportedLanguage::detect(Path::new("script.js")),
            Some(SupportedLanguage::JavaScript)
        );
        assert_eq!(
            SupportedLanguage::detect(Path::new("script.py")),
            Some(SupportedLanguage::Python)
        );
        assert_eq!(
            SupportedLanguage::detect(Path::new("lib.rs")),
            Some(SupportedLanguage::Rust)
        );

        assert_eq!(
            SupportedLanguage::detect(Path::new("App.vue")),
            Some(SupportedLanguage::Vue)
        );

        assert_eq!(SupportedLanguage::detect(Path::new("text.txt")), None);
        assert_eq!(SupportedLanguage::detect(Path::new("Makefile")), None);
        assert_eq!(SupportedLanguage::detect(Path::new(".gitignore")), None);
    }

    #[test]
    fn test_grammar_loads_successfully() {
        // Just verify these don't panic or return invalid grammars
        let _go = SupportedLanguage::Go.grammar();
        let _ts = SupportedLanguage::TypeScript.grammar();
        let _py = SupportedLanguage::Python.grammar();
        let _vue = SupportedLanguage::Vue.grammar();
    }

    #[test]
    fn test_extract_vue_script_basic() {
        let sfc =
            b"<template><div>Hello</div></template>\n<script>\nexport default {}\n</script>\n";
        let result = extract_vue_script(sfc);
        // 2 newlines before script block (one after </template>, one after <script>)
        assert!(!result.is_empty());
        let text = std::str::from_utf8(&result).unwrap();
        assert!(text.contains("export default {}"));
        // Should start with padded spaces matching bytes in <template> section
        assert!(text.starts_with(' '));
    }

    #[test]
    fn test_extract_vue_script_setup() {
        let sfc = b"<template><p>Hello</p></template>\n<script setup lang=\"ts\">\nconst count = ref(0)\n</script>\n";
        let result = extract_vue_script(sfc);
        let text = std::str::from_utf8(&result).unwrap();
        assert!(text.contains("const count = ref(0)"));
    }

    #[test]
    fn test_extract_vue_script_no_script_block() {
        let sfc = b"<template><p>Template only</p></template>\n";
        let result = extract_vue_script(sfc);
        // No script block -> returns empty (parser creates valid empty AST)
        assert!(result.is_empty() || std::str::from_utf8(&result).unwrap().trim().is_empty());
    }

    // ── body_kinds field tests ────────────────────────────────────────────────

    #[test]
    fn test_rust_body_kinds_includes_field_declaration_list() {
        let types = SupportedLanguage::Rust.node_types();
        assert!(
            types.body_kinds.contains(&"field_declaration_list"),
            "Rust body_kinds must include field_declaration_list for struct bodies"
        );
        assert!(
            types.body_kinds.contains(&"enum_variant_list"),
            "Rust body_kinds must include enum_variant_list for enum bodies"
        );
        assert!(
            types.body_kinds.contains(&"block"),
            "Rust body_kinds must include block for function bodies"
        );
    }

    #[test]
    fn test_go_body_kinds_does_not_include_rust_specific_kinds() {
        let types = SupportedLanguage::Go.node_types();
        assert!(
            !types.body_kinds.contains(&"enum_variant_list"),
            "Go should not list enum_variant_list — Rust-specific kind"
        );
        assert!(
            types.body_kinds.contains(&"block"),
            "Go body_kinds must include block for function bodies"
        );
    }

    #[test]
    fn test_typescript_body_kinds_includes_enum_body() {
        let types = SupportedLanguage::TypeScript.node_types();
        assert!(
            types.body_kinds.contains(&"enum_body"),
            "TypeScript body_kinds must include enum_body"
        );
        assert!(
            types.body_kinds.contains(&"statement_block"),
            "TypeScript body_kinds must include statement_block for function bodies"
        );
    }

    // ── metadata_prefixes field tests ─────────────────────────────────────────

    #[test]
    fn test_metadata_prefixes_rust_has_hash_not_at() {
        let types = SupportedLanguage::Rust.node_types();
        assert!(
            types.metadata_prefixes.contains(&"#"),
            "Rust must expand '#' prefixes (attribute lines like #[derive(...)])"
        );
        assert!(
            !types.metadata_prefixes.contains(&"@"),
            "Rust must NOT expand '@' prefixes — not a valid Rust decorator"
        );
    }

    #[test]
    fn test_metadata_prefixes_go_has_no_hash_no_at() {
        let types = SupportedLanguage::Go.node_types();
        assert!(
            !types.metadata_prefixes.contains(&"#"),
            "Go must NOT expand '#' prefixes — not valid Go syntax"
        );
        assert!(
            !types.metadata_prefixes.contains(&"@"),
            "Go must NOT expand '@' prefixes — not valid Go syntax"
        );
    }

    #[test]
    fn test_metadata_prefixes_typescript_has_at_not_hash() {
        let types = SupportedLanguage::TypeScript.node_types();
        assert!(
            types.metadata_prefixes.contains(&"@"),
            "TypeScript must expand '@' prefixes (decorator lines like @Injectable())"
        );
        assert!(
            !types.metadata_prefixes.contains(&"#"),
            "TypeScript must NOT expand '#' — not a valid TS decorator prefix"
        );
    }

    #[test]
    fn test_metadata_prefixes_python_has_at_and_hash() {
        let types = SupportedLanguage::Python.node_types();
        assert!(
            types.metadata_prefixes.contains(&"@"),
            "Python must expand '@' prefixes (decorator lines)"
        );
        assert!(
            types.metadata_prefixes.contains(&"#"),
            "Python must expand '#' prefixes (comment lines above defs)"
        );
    }
}
