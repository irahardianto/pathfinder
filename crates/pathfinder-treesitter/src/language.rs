use crate::surgeon::ExtractedSymbol;
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
    /// When non-empty, the extractor will descend into these nodes and extract
    /// their child function items as `SymbolKind::Method` under the impl type.
    pub impl_kinds: &'static [&'static str],
    /// Node kinds that represent constants.
    pub constant_kinds: &'static [&'static str],
    /// Node kinds that represent scoped module blocks.
    /// Contents are extracted as named children under the module's path segment.
    /// Example: Rust `mod tests { fn foo() {} }` → `tests` (Module) with child `foo`.
    pub module_kinds: &'static [&'static str],
}

/// Language grammar specification for query-driven AST extraction (TS-B5, TS-B6).
pub struct LanguageGrammar {
    /// Unique grammar identifier (e.g. "rust", "typescript", "go").
    pub id: &'static str,
    /// File extensions associated with this grammar.
    pub extensions: &'static [&'static str],
    /// Grammar loader function.
    pub grammar: fn() -> tree_sitter::Language,
    /// ORDERED tags queries. TypeScript MUST be [`javascript_tags`, `typescript_tags`] (TS-B5).
    pub tags_queries: &'static [&'static str],
    /// Injections query for multi-grammar parsing (TS-B6).
    pub injections_query: Option<&'static str>,
    /// Language-specific post-processing hook (TS-B3).
    pub post_process: Option<fn(&mut Vec<ExtractedSymbol>)>,
}

impl LanguageGrammar {
    /// Return the corresponding LSP language ID (TS-C3, N:1 mapping).
    #[must_use]
    pub fn lsp_language_id(&self) -> &'static str {
        match self.id {
            "go" => "go",
            "typescript" | "tsx" => "typescript",
            "javascript" => "javascript",
            "python" => "python",
            "rust" => "rust",
            "java" => "java",
            "vue" => "vue",
            "html" => "html",
            "css" => "css",
            other => other,
        }
    }

    /// Look up a grammar by file extension.
    #[must_use]
    pub fn find_by_extension(ext: &str) -> Option<&'static Self> {
        GRAMMARS
            .iter()
            .find(|g| g.extensions.iter().any(|&e| e.eq_ignore_ascii_case(ext)))
    }

    /// Look up a grammar by ID.
    #[must_use]
    pub fn find_by_id(id: &str) -> Option<&'static Self> {
        GRAMMARS.iter().find(|g| g.id == id)
    }

    /// Return all registered language grammars.
    #[must_use]
    pub fn all() -> &'static [Self] {
        GRAMMARS
    }
}

/// Static registry of language grammar specifications (TS-B5, TS-B6, TS-C3).
pub static GRAMMARS: &[LanguageGrammar] = &[
    LanguageGrammar {
        id: "go",
        extensions: &["go"],
        grammar: || tree_sitter_go::LANGUAGE.into(),
        tags_queries: &[tree_sitter_go::TAGS_QUERY],
        injections_query: None,
        post_process: Some(crate::symbols::post_process_go),
    },
    LanguageGrammar {
        id: "typescript",
        extensions: &["ts"],
        grammar: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        tags_queries: &[
            tree_sitter_javascript::TAGS_QUERY,
            tree_sitter_typescript::TAGS_QUERY,
        ],
        injections_query: None,
        post_process: Some(crate::symbols::post_process_typescript),
    },
    LanguageGrammar {
        id: "tsx",
        extensions: &["tsx"],
        grammar: || tree_sitter_typescript::LANGUAGE_TSX.into(),
        tags_queries: &[
            tree_sitter_javascript::TAGS_QUERY,
            tree_sitter_typescript::TAGS_QUERY,
        ],
        injections_query: None,
        post_process: Some(crate::symbols::post_process_typescript),
    },
    LanguageGrammar {
        id: "javascript",
        extensions: &["js", "jsx"],
        grammar: || tree_sitter_javascript::LANGUAGE.into(),
        tags_queries: &[tree_sitter_javascript::TAGS_QUERY],
        injections_query: Some(tree_sitter_javascript::INJECTIONS_QUERY),
        post_process: Some(crate::symbols::post_process_javascript),
    },
    LanguageGrammar {
        id: "python",
        extensions: &["py"],
        grammar: || tree_sitter_python::LANGUAGE.into(),
        tags_queries: &[tree_sitter_python::TAGS_QUERY],
        injections_query: None,
        post_process: Some(crate::symbols::post_process_python),
    },
    LanguageGrammar {
        id: "rust",
        extensions: &["rs"],
        grammar: || tree_sitter_rust::LANGUAGE.into(),
        tags_queries: &[tree_sitter_rust::TAGS_QUERY],
        injections_query: Some(tree_sitter_rust::INJECTIONS_QUERY),
        post_process: Some(crate::symbols::post_process_rust),
    },
    LanguageGrammar {
        id: "java",
        extensions: &["java"],
        grammar: || tree_sitter_java::LANGUAGE.into(),
        tags_queries: &[tree_sitter_java::TAGS_QUERY],
        injections_query: None,
        post_process: Some(crate::symbols::post_process_java),
    },
    LanguageGrammar {
        id: "vue",
        extensions: &["vue"],
        grammar: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        tags_queries: &[
            tree_sitter_javascript::TAGS_QUERY,
            tree_sitter_typescript::TAGS_QUERY,
        ],
        injections_query: None,
        post_process: None,
    },
    LanguageGrammar {
        id: "html",
        extensions: &["html", "htm"],
        grammar: || tree_sitter_html::LANGUAGE.into(),
        tags_queries: &[],
        injections_query: Some(tree_sitter_html::INJECTIONS_QUERY),
        post_process: None,
    },
    LanguageGrammar {
        id: "css",
        extensions: &["css"],
        grammar: || tree_sitter_css::LANGUAGE.into(),
        tags_queries: &[],
        injections_query: None,
        post_process: None,
    },
];

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
    /// Vue Single-File Component (Phase 1: `<script>` block parsed as TypeScript).
    Vue,
    /// The Java programming language.
    Java,
}

static GO_NODE_TYPES: LanguageNodeTypes = LanguageNodeTypes {
    function_kinds: &["function_declaration"],
    class_kinds: &["type_spec", "type_alias"],
    method_kinds: &["method_declaration", "method_spec", "method_elem"],
    impl_kinds: &[],
    constant_kinds: &["const_declaration", "var_declaration"],
    module_kinds: &[],
};

static TS_NODE_TYPES: LanguageNodeTypes = LanguageNodeTypes {
    function_kinds: &["function_declaration", "generator_function_declaration"],
    class_kinds: &[
        "class_declaration",
        "interface_declaration",
        "enum_declaration",
        "abstract_class_declaration",
        "type_alias_declaration",
    ],
    method_kinds: &[
        "method_definition",
        "method_signature",
        "abstract_method_signature",
    ],
    impl_kinds: &[],
    constant_kinds: &["lexical_declaration", "variable_declaration"],
    module_kinds: &["internal_module"],
};

static PYTHON_NODE_TYPES: LanguageNodeTypes = LanguageNodeTypes {
    function_kinds: &["function_definition"],
    class_kinds: &["class_definition"],
    method_kinds: &[],
    impl_kinds: &[],
    constant_kinds: &[],
    module_kinds: &[],
};

static RUST_NODE_TYPES: LanguageNodeTypes = LanguageNodeTypes {
    function_kinds: &["function_item", "function_signature_item"],
    class_kinds: &[
        "struct_item",
        "enum_item",
        "trait_item",
        "type_item",
        "union_item",
    ],
    method_kinds: &[],
    impl_kinds: &["impl_item"],
    constant_kinds: &["const_item", "static_item"],
    module_kinds: &["mod_item"],
};

static JAVA_NODE_TYPES: LanguageNodeTypes = LanguageNodeTypes {
    function_kinds: &["method_declaration", "constructor_declaration"],
    class_kinds: &[
        "class_declaration",
        "interface_declaration",
        "enum_declaration",
        "annotation_type_declaration",
        "record_declaration",
    ],
    method_kinds: &[],
    impl_kinds: &[],
    constant_kinds: &[],
    module_kinds: &[],
};

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
            "java" => Some(Self::Java),
            _ => None,
        }
    }

    /// Return the string representation of the language.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Go => "go",
            Self::TypeScript | Self::Tsx => "typescript",
            Self::Vue => "vue",
            Self::JavaScript => "javascript",
            Self::Python => "python",
            Self::Rust => "rust",
            Self::Java => "java",
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
            Self::Java => tree_sitter_java::LANGUAGE.into(),
        }
    }

    /// Get the associated `LanguageGrammar` definition.
    #[must_use]
    pub fn grammar_def(&self) -> &'static LanguageGrammar {
        match self {
            Self::Go => &GRAMMARS[0],
            Self::TypeScript => &GRAMMARS[1],
            Self::Tsx => &GRAMMARS[2],
            Self::JavaScript => &GRAMMARS[3],
            Self::Python => &GRAMMARS[4],
            Self::Rust => &GRAMMARS[5],
            Self::Java => &GRAMMARS[6],
            Self::Vue => &GRAMMARS[7],
        }
    }

    /// Return the corresponding LSP language ID (TS-C3, N:1 mapping).
    #[must_use]
    pub fn lsp_language_id(self) -> &'static str {
        self.grammar_def().lsp_language_id()
    }

    /// Get the node type maps for building semantic paths.
    #[must_use]
    pub const fn node_types(&self) -> &'static LanguageNodeTypes {
        match self {
            Self::Go => &GO_NODE_TYPES,
            Self::TypeScript | Self::Tsx | Self::JavaScript | Self::Vue => &TS_NODE_TYPES,
            Self::Python => &PYTHON_NODE_TYPES,
            Self::Rust => &RUST_NODE_TYPES,
            Self::Java => &JAVA_NODE_TYPES,
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

    // Find the closing </script> tag, skipping JS string literals and comments
    let Some(script_close_start) = find_closing_script_tag(text, script_open_end) else {
        return Vec::new();
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

fn find_closing_script_tag(source: &str, start_offset: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut i = start_offset;

    while i < len {
        if bytes[i] == b'/' {
            if i + 1 < len && bytes[i + 1] == b'/' {
                // Line comment
                i += 2;
                while i < len && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            } else if i + 1 < len && bytes[i + 1] == b'*' {
                // Block comment
                i += 2;
                while i + 1 < len && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
                continue;
            }
        } else if bytes[i] == b'\'' || bytes[i] == b'"' {
            let quote = bytes[i];
            i += 1;
            while i < len {
                if bytes[i] == b'\\' {
                    i += 2;
                } else if bytes[i] == quote {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            continue;
        } else if bytes[i] == b'`' {
            i += 1;
            while i < len {
                if bytes[i] == b'\\' {
                    i += 2;
                } else if bytes[i] == b'`' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            continue;
        } else if bytes[i] == b'<'
            && (source[i..].starts_with("</script>")
                || (source[i..].starts_with("</script")
                    && matches!(
                        bytes.get(i + "</script".len()),
                        Some(b'>' | b' ' | b'\t' | b'\n' | b'\r')
                    )))
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
#[path = "language_test.rs"]
mod tests;
