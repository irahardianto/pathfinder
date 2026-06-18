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
    /// The Java programming language.
    Java,
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
            },
            Self::Python => &LanguageNodeTypes {
                function_kinds: &["function_definition", "decorated_definition"],
                class_kinds: &["class_definition"],
                method_kinds: &[], // Python treats methods as functions inside classes
                impl_kinds: &[],
                constant_kinds: &[],
                module_kinds: &[],
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
            },
            Self::Java => &LanguageNodeTypes {
                // Instance methods, static methods, and constructors
                function_kinds: &["method_declaration", "constructor_declaration"],
                class_kinds: &[
                    "class_declaration",
                    "interface_declaration",
                    "enum_declaration",
                    "annotation_type_declaration",
                    "record_declaration",
                ],
                // Java methods are functions inside classes (like Python) — no separate method_kinds
                method_kinds: &[],
                // Java has no impl blocks
                impl_kinds: &[],
                // Excluded: `field_declaration` covers ALL fields (private, protected, public),
                // which is too noisy. Only actual constants would qualify but filtering
                // `static final` requires value inspection beyond node kind.
                constant_kinds: &[],
                // Java packages are directory-based, not AST nodes
                module_kinds: &[],
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

#[cfg(test)]
#[path = "language_test.rs"]
mod tests;
