use std::path::Path;
use tree_sitter::Language;

/// Language node types used for extracting symbols from the AST.
#[derive(Debug)]
pub struct LanguageNodeTypes {
    pub function_kinds: &'static [&'static str],
    pub class_kinds: &'static [&'static str],
    pub method_kinds: &'static [&'static str],
    pub constant_kinds: &'static [&'static str],
}

/// The programming languages natively supported by the Surgeon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SupportedLanguage {
    Go,
    TypeScript,
    Tsx,
    JavaScript,
    Python,
    Rust,
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
            _ => None,
        }
    }

    /// Load the corresponding tree-sitter language grammar.
    #[must_use]
    pub fn grammar(&self) -> Language {
        match self {
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
        }
    }

    /// Get the node type maps for building semantic paths.
    #[must_use]
    pub fn node_types(&self) -> &'static LanguageNodeTypes {
        match self {
            Self::Go => &LanguageNodeTypes {
                function_kinds: &["function_declaration"],
                class_kinds: &["type_declaration"],
                method_kinds: &["method_declaration"],
                constant_kinds: &["const_declaration", "var_declaration"],
            },
            Self::TypeScript | Self::Tsx | Self::JavaScript => &LanguageNodeTypes {
                function_kinds: &["function_declaration", "generator_function_declaration"],
                class_kinds: &["class_declaration", "interface_declaration"],
                method_kinds: &["method_definition"],
                constant_kinds: &["lexical_declaration", "variable_declaration"],
            },
            Self::Python => &LanguageNodeTypes {
                function_kinds: &["function_definition", "decorated_definition"],
                class_kinds: &["class_definition"],
                method_kinds: &[], // Python treats methods as functions inside classes
                constant_kinds: &[],
            },
            Self::Rust => &LanguageNodeTypes {
                function_kinds: &["function_item"],
                class_kinds: &["struct_item", "enum_item", "trait_item"],
                method_kinds: &[], // Inside impl_item
                constant_kinds: &["const_item", "static_item"],
            },
        }
    }
}

#[cfg(test)]
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
    }
}
