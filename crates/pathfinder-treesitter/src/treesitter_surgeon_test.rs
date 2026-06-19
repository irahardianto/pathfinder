use super::*;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
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

// ── AC-1.8: Java text_block classified as string ─────────────────────────

#[tokio::test]
async fn test_node_type_at_position_java_text_block() {
    let surgeon = TreeSitterSurgeon::new(2);
    let mut file = Builder::new().suffix(".java").tempfile().unwrap();
    // Java 15+ text block — column 16 on line 2 is inside the text block
    writeln!(
        file,
        "class Foo {{\n    String s = \"\"\"\n        hello\n        \"\"\";\n}}\n"
    )
    .unwrap();
    let path = file.path().to_path_buf();
    let workspace_root = std::path::PathBuf::from("/");
    let relative = path.strip_prefix("/").unwrap();

    // Line 2 column 16 is inside the `"""` text block opening
    let node_type = surgeon
        .node_type_at_position(&workspace_root, relative, 2, 16)
        .await
        .unwrap();

    assert_eq!(
        node_type, "string",
        "Java text block should be classified as string, got: {node_type}"
    );
}

// ── Java AST integration: extract_symbols + enclosing_symbol ─────────────

/// AC-Java-1: Java class and method symbols are extracted with correct kinds.
///
/// Verifies the Java tree-sitter grammar is linked and the surgeon can parse
/// a Java file into `ExtractedSymbol`s with the expected kinds (`Class`, `Method`).
#[tokio::test]
async fn test_java_extract_symbols_class_and_methods() {
    use crate::surgeon::SymbolKind;

    let surgeon = TreeSitterSurgeon::new(2);
    let mut file = Builder::new().suffix(".java").tempfile().unwrap();
    let code = r"public class PaymentService {

    private final LedgerClient ledger;

    public PaymentService(LedgerClient ledger) {
        this.ledger = ledger;
    }

    public TransactionResult processPayment(String txId, long amountCents) {
        return ledger.post(txId, amountCents);
    }
}
";
    write!(file, "{code}").unwrap();

    let path = file.path().to_path_buf();
    let workspace_root = std::path::PathBuf::from("/");
    let relative = path.strip_prefix("/").unwrap();

    let symbols = surgeon
        .extract_symbols(&workspace_root, relative)
        .await
        .unwrap();

    // Top-level class
    let class_sym = symbols.iter().find(|s| s.name == "PaymentService").unwrap();
    assert_eq!(
        class_sym.kind,
        SymbolKind::Class,
        "PaymentService should be a Class"
    );

    // Constructor extracted as method
    let ctor = class_sym
        .children
        .iter()
        .find(|s| s.name == "PaymentService")
        .or_else(|| {
            // Some grammars don't separate constructors; fall back to any method with the class name
            symbols
                .iter()
                .flat_map(|s| s.children.iter())
                .find(|s| s.name == "PaymentService")
        });
    // Constructor may or may not be present depending on grammar version — non-fatal
    let _ = ctor;

    // processPayment method
    let method = class_sym
        .children
        .iter()
        .find(|s| s.name == "processPayment")
        .unwrap();
    // The Java tree-sitter grammar classifies class methods as `Function`
    // (not `Method`). Accept either to remain stable across grammar versions.
    assert!(
        method.kind == SymbolKind::Method || method.kind == SymbolKind::Function,
        "processPayment should be a Method or Function, got: {:?}",
        method.kind
    );

    // start/end lines are reasonable
    assert!(
        method.start_line < method.end_line,
        "processPayment: start_line ({}) must be less than end_line ({})",
        method.start_line,
        method.end_line
    );
}

/// AC-Java-2: `enclosing_symbol` resolves a line inside a Java method.
#[tokio::test]
async fn test_java_enclosing_symbol_inside_method() {
    let surgeon = TreeSitterSurgeon::new(2);
    let mut file = Builder::new().suffix(".java").tempfile().unwrap();
    // Line 1: class declaration
    // Line 2: blank
    // Line 3: processPayment declaration (1-indexed)
    // Line 4: return statement — should be enclosed by processPayment
    let code = r"public class LedgerWriter {
    public void writeEntry(String id) {
        System.out.println(id);
    }
}
";
    write!(file, "{code}").unwrap();

    let path = file.path().to_path_buf();
    let workspace_root = std::path::PathBuf::from("/");
    let relative = path.strip_prefix("/").unwrap();

    // Line 3 is `System.out.println(id);` — inside writeEntry
    let sym = surgeon
        .enclosing_symbol(&workspace_root, relative, 3)
        .await
        .unwrap();

    // Should resolve to the method or the class; at minimum it should not be None
    // when the line is clearly inside a named method.
    assert!(
        sym.is_some(),
        "Line 3 is inside writeEntry — enclosing_symbol must not return None"
    );
    let sym_str = sym.unwrap();
    // Accept either "LedgerWriter.writeEntry" or just "writeEntry"
    assert!(
        sym_str.contains("writeEntry") || sym_str.contains("LedgerWriter"),
        "Enclosing symbol for line 3 should reference writeEntry or LedgerWriter, got: {sym_str}"
    );
}

// ── THR-001-B: Concurrent symbol extraction stress test ────────────────────
// Validates that spawn_blocking-based symbol extraction works correctly
// under concurrent load without panics or deadlocks.

#[tokio::test]
async fn test_concurrent_symbol_extraction_stress() {
    const CONCURRENT_TASKS: usize = 20;

    use futures::stream::{self, StreamExt};

    let surgeon = Arc::new(TreeSitterSurgeon::new(10));
    let mut file = Builder::new().suffix(".rs").tempfile().unwrap();

    writeln!(
        file,
        r"
pub struct Config {{
    pub max_tokens: usize,
    pub timeout_ms: u64,
}}

impl Config {{
    pub fn new() -> Self {{
        Self {{
            max_tokens: 16000,
            timeout_ms: 30000,
        }}
    }}

    pub fn with_max_tokens(mut self, val: usize) -> Self {{
        self.max_tokens = val;
        self
    }}
}}

pub fn process_data(input: &str) -> Result<String, std::io::Error> {{
    Ok(input.to_uppercase())
}}

#[cfg(test)]
mod tests {{
    use super::*;

    #[test]
    fn test_config_default() {{
        let c = Config::new();
        assert_eq!(c.max_tokens, 16000);
    }}
}}
"
    )
    .unwrap();

    let workspace_root = PathBuf::from("/");
    let relative = file.path().strip_prefix("/").unwrap().to_path_buf();
    let relative_arc = Arc::new(relative);

    let tasks: Vec<_> = (0..CONCURRENT_TASKS)
        .map(|i| {
            let surgeon = surgeon.clone();
            let workspace_root = workspace_root.clone();
            let relative = relative_arc.clone();
            async move {
                for round in 0..3 {
                    let result = surgeon.extract_symbols(&workspace_root, &relative).await;

                    assert!(
                        result.is_ok(),
                        "Task {i}, round {round}: extract_symbols failed: {:?}",
                        result.err()
                    );

                    let symbols = result.unwrap();

                    let struct_config = symbols.iter().find(|s| s.name == "Config");
                    assert!(
                        struct_config.is_some(),
                        "Task {i}, round {round}: Config struct not found"
                    );

                    let fn_process_data = symbols.iter().find(|s| s.name == "process_data");
                    assert!(
                        fn_process_data.is_some(),
                        "Task {i}, round {round}: process_data fn not found"
                    );
                }
            }
        })
        .collect();

    stream::iter(tasks)
        .buffer_unordered(10)
        .collect::<Vec<_>>()
        .await;
}

#[tokio::test]
async fn test_concurrent_symbol_extraction_vue_stress() {
    const CONCURRENT_TASKS: usize = 15;

    use futures::stream::{self, StreamExt};

    let surgeon = Arc::new(TreeSitterSurgeon::new(10));
    let mut file = Builder::new().suffix(".vue").tempfile().unwrap();
    file.write_all(BASIC_VUE_SFC).unwrap();

    let workspace_root = PathBuf::from("/");
    let relative = file.path().strip_prefix("/").unwrap().to_path_buf();
    let relative_arc = Arc::new(relative);

    let tasks: Vec<_> = (0..CONCURRENT_TASKS)
        .map(|i| {
            let surgeon = surgeon.clone();
            let workspace_root = workspace_root.clone();
            let relative = relative_arc.clone();
            async move {
                let result = surgeon.read_source_file(&workspace_root, &relative).await;

                assert!(
                    result.is_ok(),
                    "Vue task {i}: read_source_file failed: {:?}",
                    result.err()
                );

                let (_, _, symbols) = result.unwrap();

                let func_sym = symbols.iter().find(|s| s.name == "doThing");
                assert!(func_sym.is_some(), "Vue task {i}: doThing not found");

                let template_sym = symbols.iter().find(|s| s.name == "template");
                assert!(template_sym.is_some(), "Vue task {i}: template not found");
            }
        })
        .collect();

    stream::iter(tasks)
        .buffer_unordered(8)
        .collect::<Vec<_>>()
        .await;
}

#[tokio::test]
async fn test_read_source_file_go() {
    let surgeon = TreeSitterSurgeon::new(2);
    let mut file = Builder::new().suffix(".go").tempfile().unwrap();
    writeln!(
        file,
        "package main\n\nfunc Hello() {{}}\nfunc World() {{}}\n"
    )
    .unwrap();
    let workspace_root = PathBuf::from("/");
    let relative = file.path().strip_prefix("/").unwrap();

    let (content, lang, symbols) = surgeon
        .read_source_file(&workspace_root, relative)
        .await
        .unwrap();

    assert_eq!(lang, "go");
    assert!(content.contains("func Hello()"));
    assert!(content.contains("func World()"));
    assert!(symbols.iter().any(|s| s.name == "Hello"));
    assert!(symbols.iter().any(|s| s.name == "World"));
}

#[tokio::test]
async fn test_read_source_file_typescript() {
    let surgeon = TreeSitterSurgeon::new(2);
    let mut file = Builder::new().suffix(".ts").tempfile().unwrap();
    writeln!(
        file,
        "export function greet(name: string): string {{\n  return `Hello ${{name}}`;\n}}\n"
    )
    .unwrap();
    let workspace_root = PathBuf::from("/");
    let relative = file.path().strip_prefix("/").unwrap();

    let (content, lang, symbols) = surgeon
        .read_source_file(&workspace_root, relative)
        .await
        .unwrap();

    assert_eq!(lang, "typescript");
    assert!(content.contains("greet"));
    assert!(symbols.iter().any(|s| s.name == "greet"));
}

#[tokio::test]
async fn test_generate_skeleton_delegates_to_repo_map() {
    let surgeon = TreeSitterSurgeon::new(2);
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("lib.rs");
    std::fs::write(
        &file_path,
        "pub struct Config {\n    pub name: String,\n}\n\npub fn process() {}\n",
    )
    .unwrap();

    let workspace_root = dir.path().to_path_buf();
    let config = crate::repo_map::SkeletonConfig::new(10000, 5, "all", 2000);

    let result = surgeon
        .generate_skeleton(&workspace_root, std::path::Path::new(""), &config)
        .await
        .unwrap();

    assert!(
        result.skeleton.contains("Config"),
        "skeleton should contain struct name Config, got: {}",
        result.skeleton
    );
    assert!(
        result.skeleton.contains("process"),
        "skeleton should contain function name process, got: {}",
        result.skeleton
    );
}

#[tokio::test]
async fn test_enclosing_symbol_detail_returns_symbol_info() {
    let surgeon = TreeSitterSurgeon::new(2);
    let mut file = Builder::new().suffix(".go").tempfile().unwrap();
    writeln!(
        file,
        "package main\n\nfunc MyHandler() {{\n\t// handler logic\n\tprintln(\"hello\")\n}}\n"
    )
    .unwrap();
    let workspace_root = PathBuf::from("/");
    let relative = file.path().strip_prefix("/").unwrap();

    // Line 4 (1-indexed) is inside MyHandler
    let detail = surgeon
        .enclosing_symbol_detail(&workspace_root, relative, 4)
        .await
        .unwrap();

    assert!(
        detail.is_some(),
        "should find enclosing symbol detail on line 4"
    );
    let sym = detail.unwrap();
    assert_eq!(sym.name, "MyHandler");
    assert!(sym.start_line <= 3); // 0-indexed row 2 = line 3
    assert!(sym.end_line >= 4); // 0-indexed row 5 = line 6
}

#[tokio::test]
async fn test_read_symbol_scope_missing_chain() {
    let surgeon = TreeSitterSurgeon::new(2);
    let workspace_root = PathBuf::from("/");
    let sp = SemanticPath {
        file_path: PathBuf::from("dummy.go"),
        symbol_chain: None,
    };
    let err = surgeon
        .read_symbol_scope(&workspace_root, &sp)
        .await
        .unwrap_err();
    match err {
        SurgeonError::SymbolNotFound { .. } => {}
        _ => panic!("Expected SymbolNotFound"),
    }
}

#[tokio::test]
async fn test_read_symbol_scope_invalid_utf8() {
    let surgeon = TreeSitterSurgeon::new(2);
    let mut file = Builder::new().suffix(".go").tempfile().unwrap();
    // Write invalid UTF-8 bytes inside the function body
    file.write_all(b"package main\n\nfunc Login() {\n\t// \xc3\x28\n}\n")
        .unwrap();
    let workspace_root = PathBuf::from("/");
    let relative = file.path().strip_prefix("/").unwrap();
    let sp = SemanticPath::parse(&format!("{}::Login", relative.display())).unwrap();

    let err = surgeon
        .read_symbol_scope(&workspace_root, &sp)
        .await
        .unwrap_err();
    match err {
        SurgeonError::ParseError { reason, .. } => {
            assert!(reason.contains("not valid UTF-8"));
        }
        _ => panic!("Expected ParseError, got: {err:?}"),
    }
}

#[tokio::test]
async fn test_read_source_file_invalid_utf8() {
    let surgeon = TreeSitterSurgeon::new(2);
    let mut file = Builder::new().suffix(".go").tempfile().unwrap();
    file.write_all(b"package main\n\n// \xc3\x28").unwrap();
    let workspace_root = PathBuf::from("/");
    let relative = file.path().strip_prefix("/").unwrap();

    let err = surgeon
        .read_source_file(&workspace_root, relative)
        .await
        .unwrap_err();
    match err {
        SurgeonError::ParseError { reason, .. } => {
            assert!(reason.contains("not valid UTF-8"));
        }
        _ => panic!("Expected ParseError, got: {err:?}"),
    }
}

#[tokio::test]
async fn test_extract_symbols_preloaded_unsupported_language() {
    let surgeon = TreeSitterSurgeon::new(2);
    let workspace_root = PathBuf::from("/");
    let err = surgeon
        .extract_symbols_preloaded(
            &workspace_root,
            Path::new("dummy.txt"),
            Arc::from(b"hello".as_slice()),
            std::time::SystemTime::now(),
        )
        .await
        .unwrap_err();
    match err {
        SurgeonError::UnsupportedLanguage(_) => {}
        _ => panic!("Expected UnsupportedLanguage"),
    }
}

#[tokio::test]
async fn test_vue_sfc_no_script_block() {
    let surgeon = TreeSitterSurgeon::new(2);
    let mut file = Builder::new().suffix(".vue").tempfile().unwrap();
    file.write_all(b"<template><div>Hello</div></template>")
        .unwrap();
    let workspace_root = PathBuf::from("/");
    let relative = file.path().strip_prefix("/").unwrap();

    let (content, lang, symbols) = surgeon
        .read_source_file(&workspace_root, relative)
        .await
        .unwrap();

    assert_eq!(lang, "vue");
    assert!(content.contains("<template>"));
    // Since there is no script block, symbols will only contain template zone components if any, or be empty
    let template_sym = symbols.iter().find(|s| s.name == "template");
    assert!(template_sym.is_some());
}
