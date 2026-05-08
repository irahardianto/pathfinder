use crate::error::SurgeonError;
use crate::language::SupportedLanguage;
use tracing::instrument;
use tree_sitter::{Parser, Tree};

/// A stateless parser wrapper that converts source code to an AST using tree-sitter.
#[derive(Debug, Default)]
pub struct AstParser;

impl AstParser {
    /// Parse the given source code bytes into a tree-sitter `Tree`.
    ///
    /// This is a pure function that does no caching. Since tree-sitter `Parser`
    /// instances are lightweight but not thread-safe (`!Send`), this function
    /// creates a new parser per call.
    ///
    /// # Errors
    ///
    /// Returns a `SurgeonError` if the parser cannot be created or parsing fails.
    #[instrument(skip_all, fields(language = ?lang))]
    pub fn parse_source(
        path: &std::path::Path,
        lang: SupportedLanguage,
        source: &[u8],
    ) -> Result<Tree, SurgeonError> {
        let mut parser = Parser::new();

        parser
            .set_language(&lang.grammar())
            .map_err(|e| SurgeonError::ParseError {
                path: path.to_path_buf(),
                reason: format!("Failed to set language: {e}"),
            })?;

        // The timeout forces tree-sitter to give up on pathological inputs.
        // We set it to 500ms which is way more than enough for normal files.
        parser.set_timeout_micros(500_000);

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| SurgeonError::ParseError {
                path: path.to_path_buf(),
                reason: "Parser returned None or timed out".into(),
            })?;

        Ok(tree)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_go_source() {
        let source = b"package main\n\nfunc main() {\n\tprintln(\"Hello\")\n}";
        let tree = AstParser::parse_source(
            std::path::Path::new("dummy.go"),
            SupportedLanguage::Go,
            source,
        )
        .expect("should parse");
        let root = tree.root_node();
        assert_eq!(root.kind(), "source_file");
        // Ensure it found the function_declaration
        assert_eq!(root.child_count(), 2);
    }

    #[test]
    fn test_parse_typescript_source() {
        let source = b"export class User {\n  private id: string;\n  constructor() {}\n}";
        let tree = AstParser::parse_source(
            std::path::Path::new("dummy.ts"),
            SupportedLanguage::TypeScript,
            source,
        )
        .expect("should parse");
        let root = tree.root_node();
        assert_eq!(root.kind(), "program");
    }

    #[test]
    fn test_parse_invalid_source_returns_tree_with_errors() {
        // Tree-sitter is fault-tolerant and always returns a tree, even for invalid syntax.
        // It injects ERROR nodes.
        let source = b"func this is not valid go { { ++ }";
        let tree = AstParser::parse_source(
            std::path::Path::new("dummy.go"),
            SupportedLanguage::Go,
            source,
        )
        .expect("should still parse");
        assert!(tree.root_node().has_error());
    }

    #[test]
    fn test_parse_empty_source() {
        let source = b"";
        let tree = AstParser::parse_source(
            std::path::Path::new("empty.rs"),
            SupportedLanguage::Rust,
            source,
        )
        .expect("should parse empty source");
        let root = tree.root_node();
        assert_eq!(root.child_count(), 0);
    }

    #[test]
    fn test_parse_rust_source() {
        let source = br#"
fn main() {
    println!("Hello, world!");
}

struct MyStruct {
    field: i32,
}

impl MyStruct {
    fn new() -> Self {
        Self { field: 0 }
    }
}
"#;
        let tree = AstParser::parse_source(
            std::path::Path::new("test.rs"),
            SupportedLanguage::Rust,
            source,
        )
        .expect("should parse Rust");
        let root = tree.root_node();
        assert_eq!(root.kind(), "source_file");
        // Should have at least a few nodes (functions, structs, impls)
        assert!(root.child_count() >= 2);
    }

    #[test]
    fn test_parse_python_source() {
        let source = b"def hello():\n    print('world')\n\nclass MyClass:\n    pass";
        let tree = AstParser::parse_source(
            std::path::Path::new("test.py"),
            SupportedLanguage::Python,
            source,
        )
        .expect("should parse Python");
        let root = tree.root_node();
        assert!(root.child_count() > 0);
    }

    #[test]
    fn test_parse_large_source() {
        // Test with a larger file to ensure it handles reasonable sizes
        let mut source = Vec::new();
        for i in 0..1000 {
            source.extend_from_slice(format!("fn func_{i}() -> i32 {{ {i} }}\n").as_bytes());
        }
        let tree = AstParser::parse_source(
            std::path::Path::new("large.rs"),
            SupportedLanguage::Rust,
            &source,
        )
        .expect("should parse large source");
        let root = tree.root_node();
        assert_eq!(root.kind(), "source_file");
        // Should have many function items
        assert!(root.child_count() > 0);
    }

    #[test]
    fn test_parse_with_unicode() {
        let source =
            "fn main() {\n    let msg = \"Hello, 世界! 🌍\";\n    println!(\"{}\", msg);\n}"
                .as_bytes();
        let tree = AstParser::parse_source(
            std::path::Path::new("unicode.rs"),
            SupportedLanguage::Rust,
            source,
        )
        .expect("should parse unicode");
        let root = tree.root_node();
        assert_eq!(root.kind(), "source_file");
    }

    #[test]
    fn test_parse_javascript_source() {
        let source =
            b"function greet(name) {\n  return `Hello, ${name}!`;\n}\n\nconst arrow = () => 42;";
        let tree = AstParser::parse_source(
            std::path::Path::new("test.js"),
            SupportedLanguage::JavaScript,
            source,
        )
        .expect("should parse JavaScript");
        let root = tree.root_node();
        assert!(root.child_count() > 0);
    }

    #[test]
    fn test_parse_tsx_source() {
        let source = br"import React from 'react';

interface Props {
  title: string;
}

export const Button: React.FC<Props> = ({ title }) => {
  return <button>{title}</button>;
}";
        let tree = AstParser::parse_source(
            std::path::Path::new("Button.tsx"),
            SupportedLanguage::Tsx,
            source,
        )
        .expect("should parse TSX");
        let root = tree.root_node();
        assert!(root.child_count() > 0);
    }

    #[test]
    fn test_parse_vue_source() {
        let source = br#"<script setup lang="ts">
import { ref } from 'vue';

const count = ref(0);
</script>

<template>
  <div>{{ count }}</div>
</template>"#;
        let tree = AstParser::parse_source(
            std::path::Path::new("Component.vue"),
            SupportedLanguage::Vue,
            source,
        )
        .expect("should parse Vue");
        let root = tree.root_node();
        assert!(root.child_count() > 0);
    }
}
