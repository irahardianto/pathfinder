use super::*;
use crate::server::types::SearchParams as SearchToolParams;
use pathfinder_common::config::PathfinderConfig;
use pathfinder_common::sandbox::Sandbox;
use pathfinder_common::types::WorkspaceRoot;
use pathfinder_search::{MockScout, SearchMatch, SearchResult};
use pathfinder_treesitter::mock::MockSurgeon;
use pathfinder_treesitter::surgeon::{AccessLevel, ExtractedSymbol};
use std::sync::Arc;
use tempfile::tempdir;

#[test]
fn test_is_valid_identifier_start() {
    assert!(is_valid_identifier_start('a'));
    assert!(is_valid_identifier_start('Z'));
    assert!(is_valid_identifier_start('_'));
    assert!(!is_valid_identifier_start('1'));
    assert!(!is_valid_identifier_start('-'));
    assert!(!is_valid_identifier_start(' '));
}

#[test]
fn test_is_valid_identifier_continue() {
    assert!(is_valid_identifier_continue('a'));
    assert!(is_valid_identifier_continue('5'));
    assert!(is_valid_identifier_continue('_'));
    assert!(!is_valid_identifier_continue('-'));
    assert!(!is_valid_identifier_continue(' '));
    assert!(!is_valid_identifier_continue('('));
}

#[test]
fn test_extract_identifier_prefix() {
    assert_eq!(extract_identifier_prefix("my_func("), Some("my_func"));
    assert_eq!(extract_identifier_prefix("MyStruct {"), Some("MyStruct"));
    assert_eq!(
        extract_identifier_prefix("_private_var,"),
        Some("_private_var")
    );
    assert_eq!(extract_identifier_prefix("foo::bar"), Some("foo"));
    assert_eq!(extract_identifier_prefix("123invalid"), None);
    assert_eq!(extract_identifier_prefix(""), None);
}

#[test]
fn test_truncate_preview() {
    // Empty string
    assert_eq!(truncate_preview("", 10), "");

    // Short string, no truncation
    assert_eq!(truncate_preview("hello", 10), "hello");

    // ASCII truncation
    let long_ascii = "this is a very long string that needs truncation";
    assert_eq!(truncate_preview(long_ascii, 10), "this is a ...");

    // Unicode handling
    let unicode = "こんにちは世界";
    assert_eq!(truncate_preview(unicode, 5), "こんにちは...");

    // ASCII fast path (byte count == char count)
    let ascii = "abcdefghijklmnop";
    assert_eq!(truncate_preview(ascii, 10), "abcdefghij...");
}

#[test]
fn test_extract_name_from_line_basic() {
    assert_eq!(extract_name_from_line("fn my_function() {"), "my_function");
    assert_eq!(
        extract_name_from_line("function myFunction() {"),
        "myFunction"
    );
    assert_eq!(
        extract_name_from_line("def my_definition(self):"),
        "my_definition"
    );
    assert_eq!(extract_name_from_line("struct MyStruct {"), "MyStruct");
    assert_eq!(extract_name_from_line("class MyClass {"), "MyClass");
}

#[test]
fn test_extract_name_from_line_with_suffix() {
    // Function name followed by parens
    assert_eq!(
        extract_name_from_line("fn foo_bar(a: i32) -> String"),
        "foo_bar"
    );

    // Struct followed by generic
    assert_eq!(extract_name_from_line("struct Foo<T> {"), "Foo");

    // With path separator in content
    assert_eq!(extract_name_from_line("let x = a::b::c"), "x");
}

#[test]
fn test_extract_name_from_line_fallback() {
    // No keyword match, should use first token
    assert_eq!(
        extract_name_from_line("some_random_line without_keyword"),
        "some_random_line"
    );

    // Empty line
    assert_eq!(extract_name_from_line(""), "");

    // Just whitespace
    assert_eq!(extract_name_from_line("   "), "");
}

#[test]
fn test_relevance_score() {
    assert_eq!(relevance_score("foo", "foo"), 3);
    assert_eq!(relevance_score("foo", "foobar"), 2);
    assert_eq!(relevance_score("foo", "myfoothing"), 1);
    assert_eq!(relevance_score("foo", "barbaz"), 0);
    assert_eq!(relevance_score("MyStruct", "MyStruct"), 3);
}

#[test]
fn test_extract_symbol_name_from_path() {
    assert_eq!(
        extract_symbol_name_from_path("src/auth.ts::AuthService.login"),
        "AuthService.login"
    );
    assert_eq!(
        extract_symbol_name_from_path("lib.rs::foo::bar::baz"),
        "foo::bar::baz"
    );
    assert_eq!(extract_symbol_name_from_path("single_token"), "");
}

#[test]
fn test_infer_kind_from_line() {
    // Rust
    assert_eq!(infer_kind_from_line("fn foo() {"), "function");
    assert_eq!(infer_kind_from_line("pub async fn bar() {"), "function");
    // JavaScript/TypeScript
    assert_eq!(infer_kind_from_line("function bar() {"), "function");
    // Python
    assert_eq!(infer_kind_from_line("def baz():"), "function");
    // Go — `func` was previously missing
    assert_eq!(infer_kind_from_line("func main() {"), "function");
    assert_eq!(
        infer_kind_from_line("func (s *Server) Handle() {"),
        "function"
    );

    assert_eq!(infer_kind_from_line("struct Foo {"), "struct");
    assert_eq!(infer_kind_from_line("class Bar {"), "class");
    assert_eq!(infer_kind_from_line("interface Baz {"), "interface");
    assert_eq!(infer_kind_from_line("trait Qux {"), "interface");

    assert_eq!(infer_kind_from_line("const X = 5;"), "constant");
    assert_eq!(infer_kind_from_line("static Y: i32 = 10;"), "constant");

    assert_eq!(infer_kind_from_line("mod utils;"), "module");
    assert_eq!(infer_kind_from_line("impl Foo {"), "impl");

    // Java methods have no fn/def/function keyword — heuristic returns "unknown".
    // This is expected; Fix 1 uses treesitter SymbolKind as the primary source.
    assert_eq!(
        infer_kind_from_line("    public void processPayment(String txId) {"),
        "unknown"
    );
    assert_eq!(infer_kind_from_line("something_unrecognized"), "unknown");
}

#[test]
fn test_symbol_kind_to_filter_string() {
    use pathfinder_treesitter::surgeon::SymbolKind;

    assert_eq!(
        symbol_kind_to_filter_string(SymbolKind::Function),
        "function"
    );
    assert_eq!(symbol_kind_to_filter_string(SymbolKind::Method), "function");
    assert_eq!(symbol_kind_to_filter_string(SymbolKind::Test), "function");
    assert_eq!(symbol_kind_to_filter_string(SymbolKind::Class), "class");
    assert_eq!(symbol_kind_to_filter_string(SymbolKind::Struct), "struct");
    assert_eq!(symbol_kind_to_filter_string(SymbolKind::Impl), "impl");
    assert_eq!(
        symbol_kind_to_filter_string(SymbolKind::Constant),
        "constant"
    );
    assert_eq!(
        symbol_kind_to_filter_string(SymbolKind::Interface),
        "interface"
    );
    assert_eq!(symbol_kind_to_filter_string(SymbolKind::Enum), "enum");
    assert_eq!(symbol_kind_to_filter_string(SymbolKind::Module), "module");
    // Vue-specific kinds fall through to "unknown"
    assert_eq!(symbol_kind_to_filter_string(SymbolKind::Zone), "unknown");
    assert_eq!(
        symbol_kind_to_filter_string(SymbolKind::Component),
        "unknown"
    );
}

#[test]
fn test_kind_matches_filter() {
    // Exact matches
    assert!(kind_matches_filter("function", "function"));
    assert!(kind_matches_filter("struct", "struct"));
    assert!(kind_matches_filter("class", "class"));

    // Cross-language mappings: filter="function" accepts method/fn kinds
    assert!(kind_matches_filter("fn", "function"));
    assert!(kind_matches_filter("method", "function"));
    assert!(kind_matches_filter("interface", "class"));
    assert!(kind_matches_filter("interface", "trait"));
    assert!(kind_matches_filter("trait", "interface"));
    assert!(kind_matches_filter("trait", "trait"));
    assert!(kind_matches_filter("const", "constant"));
    assert!(kind_matches_filter("mod", "module"));

    // Symmetric: filter="method" also accepts function/fn kinds
    // This is critical for Java: methods are extracted as SymbolKind::Function
    // (mapped to kind="function"), but agents may search with kind="method".
    assert!(kind_matches_filter("function", "method"));
    assert!(kind_matches_filter("fn", "method"));
    assert!(kind_matches_filter("method", "method"));

    // filter="fn" also works symmetrically
    assert!(kind_matches_filter("function", "fn"));
    assert!(kind_matches_filter("method", "fn"));

    // Case insensitive
    assert!(kind_matches_filter("FUNCTION", "function"));
    assert!(kind_matches_filter("struct", "STRUCT"));
    assert!(kind_matches_filter("METHOD", "function"));
    assert!(kind_matches_filter("function", "METHOD"));

    // No match
    assert!(!kind_matches_filter("class", "function"));
    assert!(!kind_matches_filter("enum", "function"));
    assert!(!kind_matches_filter("unknown", "class"));
    assert!(!kind_matches_filter("class", "method"));
    assert!(!kind_matches_filter("enum", "method"));
}

#[test]
fn test_is_workspace_file_relative_fast_path() -> Result<(), Box<dyn std::error::Error>> {
    // OPT-1: relative paths without ".." should hit the fast path
    // and return true without any syscall.
    let dir = tempfile::tempdir()?;
    let canonical = dir.path().canonicalize()?;

    // Normal source files — fast path returns true
    assert!(is_workspace_file(
        Path::new("src/main.rs"),
        dir.path(),
        &canonical
    ));
    assert!(is_workspace_file(
        Path::new("crates/pathfinder/src/lib.rs"),
        dir.path(),
        &canonical
    ));
    assert!(is_workspace_file(
        Path::new("README.md"),
        dir.path(),
        &canonical
    ));
    Ok(())
}

#[test]
fn test_is_workspace_file_skip_patterns() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let canonical = dir.path().canonicalize()?;

    // Skip patterns should reject even on fast path
    assert!(!is_workspace_file(
        Path::new("src/node_modules/lodash/index.js"),
        dir.path(),
        &canonical
    ));
    assert!(!is_workspace_file(
        Path::new("some/target/debug/main"),
        dir.path(),
        &canonical
    ));
    assert!(!is_workspace_file(
        Path::new("project/.git/objects/abc"),
        dir.path(),
        &canonical
    ));
    assert!(!is_workspace_file(
        Path::new("app/vendor/github.com/pkg"),
        dir.path(),
        &canonical
    ));
    assert!(!is_workspace_file(
        Path::new("frontend/dist/bundle.js"),
        dir.path(),
        &canonical
    ));
    assert!(!is_workspace_file(
        Path::new("app/build/output.js"),
        dir.path(),
        &canonical
    ));
    Ok(())
}

#[test]
fn test_is_workspace_file_traversal_slow_path() -> Result<(), Box<dyn std::error::Error>> {
    // Paths with ".." should take the slow path (canonicalize)
    let dir = tempfile::tempdir()?;
    let canonical = dir.path().canonicalize()?;

    // ".." traversal that stays within workspace is still valid
    // but takes the slow path. The joined path may or may not resolve.
    // Create a nested dir so the traversal resolves back to workspace.
    std::fs::create_dir_all(dir.path().join("a/b"))?;
    std::fs::write(dir.path().join("test.txt"), "hello")?;
    assert!(is_workspace_file(
        Path::new("a/b/../../test.txt"),
        dir.path(),
        &canonical
    ));
    Ok(())
}

#[test]
fn test_is_workspace_file_traversal_outside_workspace() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let canonical = dir.path().canonicalize()?;

    // Traversal outside workspace should fail (canonicalized path won't
    // start with canonical root)
    assert!(!is_workspace_file(
        Path::new("../../etc/passwd"),
        dir.path(),
        &canonical
    ));
    Ok(())
}

#[test]
fn test_pre_enrichment_dedup() {
    // OPT-4: verify dedup by (file, line) retains first occurrence
    // and removes duplicates.
    let mut matches = vec![
        MatchToEnrich {
            file: "src/main.rs".to_string(),
            line: 10,
            content: "fn foo() {".to_string(),
        },
        MatchToEnrich {
            file: "src/main.rs".to_string(),
            line: 10,
            content: "fn foo() {".to_string(), // duplicate
        },
        MatchToEnrich {
            file: "src/main.rs".to_string(),
            line: 20,
            content: "fn bar() {".to_string(), // same file, different line
        },
        MatchToEnrich {
            file: "src/lib.rs".to_string(),
            line: 10,
            content: "fn baz() {".to_string(), // different file, same line
        },
    ];

    let pre_count = matches.len();
    {
        let mut seen = std::collections::HashSet::new();
        matches.retain(|m| seen.insert((m.file.clone(), m.line)));
    }

    assert_eq!(matches.len(), 3);
    assert_eq!(pre_count - matches.len(), 1);
    assert_eq!(matches[0].file, "src/main.rs");
    assert_eq!(matches[0].line, 10);
    assert_eq!(matches[1].file, "src/main.rs");
    assert_eq!(matches[1].line, 20);
    assert_eq!(matches[2].file, "src/lib.rs");
    assert_eq!(matches[2].line, 10);
}

#[test]
fn test_pre_enrichment_dedup_preserves_all_unique() {
    // OPT-4: when all entries are unique, none should be removed.
    let mut matches = vec![
        MatchToEnrich {
            file: "a.rs".to_string(),
            line: 1,
            content: "fn a() {".to_string(),
        },
        MatchToEnrich {
            file: "b.rs".to_string(),
            line: 2,
            content: "fn b() {".to_string(),
        },
        MatchToEnrich {
            file: "c.rs".to_string(),
            line: 3,
            content: "fn c() {".to_string(),
        },
    ];

    let pre_count = matches.len();
    {
        let mut seen = std::collections::HashSet::new();
        matches.retain(|m| seen.insert((m.file.clone(), m.line)));
    }

    assert_eq!(matches.len(), pre_count);
    assert_eq!(matches.len(), 3);
}

// ── Additional kind_matches_filter coverage ────────────────────────────

#[test]
fn test_kind_matches_filter_enum_exact() {
    assert!(kind_matches_filter("enum", "enum"));
    assert!(!kind_matches_filter("enum", "class"));
    assert!(!kind_matches_filter("class", "enum"));
}

#[test]
fn test_kind_matches_filter_constant_aliases() {
    // filter="constant" should accept const, static, let
    assert!(kind_matches_filter("const", "constant"));
    assert!(kind_matches_filter("static", "constant"));
    assert!(kind_matches_filter("let", "constant"));
    assert!(kind_matches_filter("constant", "constant"));
    // But not function-like things
    assert!(!kind_matches_filter("function", "constant"));
}

#[test]
fn test_kind_matches_filter_module_aliases() {
    // filter="module" should accept mod, namespace
    assert!(kind_matches_filter("mod", "module"));
    assert!(kind_matches_filter("namespace", "module"));
    assert!(kind_matches_filter("module", "module"));
    assert!(!kind_matches_filter("class", "module"));
}

#[test]
fn test_kind_matches_filter_struct_exact_only() {
    // filter="struct" should only match struct, not class
    assert!(kind_matches_filter("struct", "struct"));
    assert!(!kind_matches_filter("class", "struct"));
    assert!(!kind_matches_filter("interface", "struct"));
}

#[test]
fn test_kind_matches_filter_unknown_filter_returns_false() {
    // A filter value that doesn't match any known category returns false
    // (unless kind == filter, which always returns true via exact match)
    assert!(!kind_matches_filter("function", "banana"));
    assert!(!kind_matches_filter("class", "banana"));
    // kind == filter exact match always returns true, even for unknown categories
    assert!(kind_matches_filter("banana", "banana"));
    // But mismatched unknowns return false
    assert!(!kind_matches_filter("apple", "banana"));
}

#[test]
fn test_kind_matches_filter_interface_class_cross() {
    // filter="class" accepts class, struct, interface
    assert!(kind_matches_filter("struct", "class"));
    assert!(kind_matches_filter("interface", "class"));
    assert!(kind_matches_filter("class", "class"));
    assert!(!kind_matches_filter("enum", "class"));
}

#[test]
fn test_kind_matches_filter_alias_filter_side() {
    // REGRESSION GUARD: aliases must work on the FILTER side, not just the kind side.
    // Before the fix, filter="const" would accept the string "const" as valid via is_valid_kind_filter
    // but kind_matches_filter silently returned false for (kind="constant", filter="const"),
    // producing 0 results even though the input was accepted as valid.

    // filter="const" must match tree-sitter canonical kind "constant"
    assert!(kind_matches_filter("constant", "const"));
    assert!(kind_matches_filter("constant", "static"));
    assert!(kind_matches_filter("constant", "let"));
    // alias-to-alias on kind side also works
    assert!(kind_matches_filter("const", "const"));
    assert!(kind_matches_filter("static", "let"));
    // filter="mod" must match tree-sitter canonical kind "module"
    assert!(kind_matches_filter("module", "mod"));
    assert!(kind_matches_filter("module", "namespace"));
    assert!(kind_matches_filter("mod", "namespace"));
    // Negatives: alias filters must not bleed into unrelated kinds
    assert!(!kind_matches_filter("function", "const"));
    assert!(!kind_matches_filter("class", "mod"));
    assert!(!kind_matches_filter("enum", "static"));
}

#[test]
fn test_kind_matches_filter_type_matches_struct() {
    assert!(kind_matches_filter("struct", "type"));
}

#[test]
fn test_kind_matches_filter_type_matches_enum() {
    assert!(kind_matches_filter("enum", "type"));
}

#[test]
fn test_kind_matches_filter_type_matches_class() {
    assert!(kind_matches_filter("class", "type"));
}

#[test]
fn test_kind_matches_filter_type_matches_interface() {
    assert!(kind_matches_filter("interface", "type"));
}

#[test]
fn test_kind_matches_filter_type_matches_trait() {
    assert!(kind_matches_filter("trait", "type"));
}

#[test]
fn test_kind_matches_filter_type_does_not_match_function() {
    assert!(!kind_matches_filter("function", "type"));
    assert!(!kind_matches_filter("method", "type"));
    assert!(!kind_matches_filter("fn", "type"));
}

#[test]
fn test_kind_matches_filter_type_does_not_match_constant() {
    assert!(!kind_matches_filter("constant", "type"));
    assert!(!kind_matches_filter("const", "type"));
    assert!(!kind_matches_filter("static", "type"));
    assert!(!kind_matches_filter("let", "type"));
}

#[test]
fn test_kind_matches_filter_type_does_not_match_impl() {
    // kind=type is a type-level umbrella — impl blocks are not types.
    // Guards against accidentally expanding the type arm to include impl.
    assert!(!kind_matches_filter("impl", "type"));
}

#[test]
fn test_kind_matches_filter_type_does_not_match_module() {
    // Modules are namespaces, not type-level constructs.
    assert!(!kind_matches_filter("module", "type"));
    assert!(!kind_matches_filter("mod", "type"));
    assert!(!kind_matches_filter("namespace", "type"));
}

#[test]
fn test_is_valid_kind_filter_type_accepted() {
    assert!(is_valid_kind_filter("type"));
    assert!(is_valid_kind_filter("TYPE"));
}

// ── Additional truncate_preview coverage ────────────────────────────

#[test]
fn test_truncate_preview_exact_boundary() {
    // When content.len() == max_chars, no truncation
    assert_eq!(truncate_preview("12345", 5), "12345");
}

#[test]
fn test_truncate_preview_unicode_shorter_than_max() {
    // Unicode string with fewer chars than max_chars — no truncation
    // "αβγ" has 3 chars but 6 bytes
    let unicode = "αβγ";
    assert_eq!(truncate_preview(unicode, 10), "αβγ");
}

#[test]
fn test_truncate_preview_unicode_at_char_boundary() {
    // Unicode string where char count > max_chars but byte count also > max_chars
    // "αβγδε" has 5 chars, we want 3 → "αβγ..."
    let unicode = "αβγδε";
    assert_eq!(truncate_preview(unicode, 3), "αβγ...");
}

// ── Additional extract_name_from_line coverage ──────────────────────

#[test]
fn test_extract_name_from_line_var_let_const() {
    assert_eq!(extract_name_from_line("const MAX_SIZE = 100"), "MAX_SIZE");
    assert_eq!(extract_name_from_line("let myVariable = 42"), "myVariable");
    assert_eq!(extract_name_from_line("var count = 0"), "count");
    assert_eq!(extract_name_from_line("static INSTANCE = null"), "INSTANCE");
}

#[test]
fn test_extract_name_from_line_mod_impl() {
    assert_eq!(extract_name_from_line("mod utils;"), "utils");
    assert_eq!(extract_name_from_line("impl MyStruct {"), "MyStruct");
}

// ── Additional extract_identifier_prefix coverage ───────────────────

#[test]
fn test_extract_identifier_prefix_single_char() {
    assert_eq!(extract_identifier_prefix("x"), Some("x"));
    assert_eq!(extract_identifier_prefix("_"), Some("_"));
}

#[test]
fn test_extract_identifier_prefix_with_special_chars() {
    // Stops at first non-identifier char
    assert_eq!(extract_identifier_prefix("a.b"), Some("a"));
    assert_eq!(extract_identifier_prefix("x+y"), Some("x"));
    assert_eq!(extract_identifier_prefix("foo[0]"), Some("foo"));
}

// ── Additional is_workspace_file coverage ───────────────────────────

#[test]
fn test_is_workspace_file_absolute_path_inside_workspace() -> Result<(), Box<dyn std::error::Error>>
{
    // Absolute path that resolves to inside the workspace should return true
    let dir = tempfile::tempdir()?;
    let canonical = dir.path().canonicalize()?;
    std::fs::create_dir_all(dir.path().join("src"))?;
    std::fs::write(dir.path().join("src/main.rs"), "fn main() {}")?;

    let abs_path = dir.path().join("src/main.rs");
    assert!(is_workspace_file(&abs_path, dir.path(), &canonical));
    Ok(())
}

#[test]
fn test_is_workspace_file_absolute_path_nonexistent_fails() -> Result<(), Box<dyn std::error::Error>>
{
    // Absolute path to non-existent file should fail canonicalize and return false
    let dir = tempfile::tempdir()?;
    let canonical = dir.path().canonicalize()?;

    let abs_path = dir.path().join("nonexistent/deep/file.rs");
    assert!(!is_workspace_file(&abs_path, dir.path(), &canonical));
    Ok(())
}

#[test]
fn test_is_workspace_file_absolute_path_outside_workspace() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = tempfile::tempdir()?;
    let canonical = dir.path().canonicalize()?;

    // /tmp is outside our workspace
    let outside = std::path::Path::new("/tmp/some_file.rs");
    // Only test if /tmp exists
    if outside.parent().is_some_and(std::path::Path::exists) {
        assert!(!is_workspace_file(outside, dir.path(), &canonical));
    }
    Ok(())
}

// ── Additional relevance_score coverage ─────────────────────────────

#[test]
fn test_relevance_score_empty_strings() {
    // Empty query matches empty text exactly
    assert_eq!(relevance_score("", ""), 3);
    // Empty query is prefix of everything
    assert_eq!(relevance_score("", "anything"), 2);
}

// ── infer_kind_from_line additional coverage ────────────────────────

#[test]
fn test_infer_kind_from_line_enum() {
    assert_eq!(infer_kind_from_line("enum Color {"), "enum");
    assert_eq!(infer_kind_from_line("pub enum Direction {"), "enum");
}

#[test]
fn test_infer_kind_from_line_var_let() {
    assert_eq!(infer_kind_from_line("var x = 10"), "constant");
    assert_eq!(infer_kind_from_line("let y = 20"), "constant");
}

#[test]
fn test_infer_kind_from_line_module() {
    assert_eq!(infer_kind_from_line("module MyModule"), "module");
}

// ── extract_symbol_name_from_path edge cases ────────────────────────

#[test]
fn test_extract_symbol_name_from_path_no_separator() {
    assert_eq!(extract_symbol_name_from_path("noseparator"), "");
}

#[test]
fn test_extract_symbol_name_from_path_empty() {
    assert_eq!(extract_symbol_name_from_path(""), "");
}

#[test]
fn test_truncate_preview_unicode_char_count_less_than_max() {
    // "こんにちは" has 5 characters but 15 bytes.
    // 8 is less than 15 (bytes) but greater than 5 (chars).
    assert_eq!(truncate_preview("こんにちは", 8), "こんにちは");
}

#[tokio::test]
async fn test_find_symbol_impl_validation_errors() {
    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    let server = PathfinderServer::with_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(MockSurgeon::new()),
    );

    // Empty query
    let params = SearchToolParams {
        query: String::new(),
        ..Default::default()
    };
    let res = server.find_symbol_impl(params).await;
    assert!(res.is_err());
    let Err(err) = res else {
        panic!("expected error")
    };
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);

    // Query with path separators
    let params = SearchToolParams {
        query: "foo/bar".to_string(),
        ..Default::default()
    };
    let res = server.find_symbol_impl(params).await;
    assert!(res.is_err());
    let Err(err2) = res else {
        panic!("expected error")
    };
    assert_eq!(err2.code, rmcp::model::ErrorCode::INVALID_PARAMS);

    // Invalid kind filter — must return INVALID_PARAMS listing accepted values
    let params = SearchToolParams {
        query: "MyStruct".to_string(),
        kind: Some("invalid_kind".to_string()), // "invalid_kind" is not an accepted kind
        ..Default::default()
    };
    let res = server.find_symbol_impl(params).await;
    assert!(res.is_err());
    let Err(err3) = res else {
        panic!("expected error for invalid kind");
    };
    assert_eq!(err3.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(
        err3.message.contains("invalid_kind"),
        "error message should echo the invalid kind value, got: {}",
        err3.message
    );
    assert!(
        err3.message.contains("function") || err3.message.contains("Canonical"),
        "error message should list accepted values, got: {}",
        err3.message
    );
}

#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn test_find_symbol_impl_various_filters_and_fallbacks() {
    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Mock scout returning a mix of matches:
    // 1. Unsafe path (with ..)
    // 2. Denied path (denied by sandbox)
    // 3. Prefix mismatch
    // 4. Successful match with treesitter detail returning None (uses fallback)
    // 5. Successful match with treesitter detail returning empty semantic path (uses fallback)
    // 6. Match skipped due to kind filter mismatch
    // 7. Successful match with empty fallback name (skipped)
    let mock_scout = MockScout::default();
    mock_scout.set_result(Ok(SearchResult {
        matches: vec![
            SearchMatch {
                file: "../outside.rs".to_owned(),
                line: 1,
                column: 0,
                content: "fn test() {}".to_owned(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "123".to_owned(),
                known: None,
            },
            SearchMatch {
                file: ".git/denied.rs".to_owned(), // denied by sandbox
                line: 1,
                column: 0,
                content: "fn test() {}".to_owned(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "123".to_owned(),
                known: None,
            },
            SearchMatch {
                file: "src/prefix_mismatch.rs".to_owned(),
                line: 1,
                column: 0,
                content: "fn test() {}".to_owned(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "123".to_owned(),
                known: None,
            },
            SearchMatch {
                file: "target_dir/match_fallback.rs".to_owned(),
                line: 5,
                column: 0,
                content: "fn my_fallback() {}".to_owned(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "123".to_owned(),
                known: None,
            },
            SearchMatch {
                file: "target_dir/match_empty_ts.rs".to_owned(),
                line: 10,
                column: 0,
                content: "fn my_empty_ts() {}".to_owned(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "123".to_owned(),
                known: None,
            },
            SearchMatch {
                file: "target_dir/match_kind_mismatch.rs".to_owned(),
                line: 15,
                column: 0,
                content: "struct Mismatch {}".to_owned(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "123".to_owned(),
                known: None,
            },
        ],
        total_matches: 6,
        truncated: false,
        files_searched: 1,
        files_in_scope: 1,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    }));

    let mock_surgeon = Arc::new(MockSurgeon::new());
    // Match 4: returns Ok(None) -> uses fallback "my_fallback"
    mock_surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));

    // Match 5: returns Ok(Some(ExtractedSymbol)) with empty semantic_path -> uses fallback "my_empty_ts"
    mock_surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(Some(ExtractedSymbol {
            name: String::new(),
            semantic_path: String::new(),
            kind: SymbolKind::Function,
            byte_range: 0..10,
            start_line: 10,
            end_line: 10,
            name_column: 0,
            access_level: AccessLevel::Public,
            children: vec![],
        })));

    // Match 6: returns Ok(None) -> fallback kind is "struct", but filter is "function" -> filtered out
    mock_surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));

    let server = PathfinderServer::with_engines(
        ws,
        config,
        sandbox,
        Arc::new(mock_scout.clone()),
        mock_surgeon,
    );

    let params = SearchToolParams {
        query: "my".to_string(),
        path_glob: "target_dir/*".to_string(),
        kind: Some("function".to_string()),
        ..Default::default()
    };

    let Json(res) = server.find_symbol_impl(params).await.expect("success");
    // Should return 2 matches: my_fallback and my_empty_ts (sorted alphabetically by file name when relevance scores are equal)
    assert_eq!(res.symbols.len(), 2);
    assert_eq!(
        res.symbols[0].semantic_path,
        "target_dir/match_empty_ts.rs::my_empty_ts"
    );
    assert_eq!(
        res.symbols[1].semantic_path,
        "target_dir/match_fallback.rs::my_fallback"
    );
}

#[tokio::test]
async fn test_find_symbol_impl_scout_error_fallback() {
    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let mock_scout = MockScout::default();
    mock_scout.set_result(Err("search failed".to_string()));

    let server = PathfinderServer::with_engines(
        ws,
        config,
        sandbox,
        Arc::new(mock_scout),
        Arc::new(MockSurgeon::new()),
    );

    let params = SearchToolParams {
        query: "test".to_string(),
        ..Default::default()
    };

    let Json(res) = server.find_symbol_impl(params).await.expect("success");
    assert_eq!(res.symbols.len(), 0);
    assert_eq!(res.total_found, 0);
}

/// Verify that kind alias filters work end-to-end through `find_symbol_impl`.
///
/// The alias fix (`kind_matches_filter`) normalises filter-side aliases, e.g.
/// "const" → "constant" and "trait" → "interface", so that agents aren't
/// surprised by 0 results when they use the common shorthand.
///
/// Previously only `is_valid_kind_filter` accepted the aliases; now
/// `kind_matches_filter` also handles them symmetrically.
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn test_find_symbol_impl_kind_alias_filter_end_to_end() {
    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Two matches:
    //   match_const.rs  → tree-sitter returns SymbolKind::Constant  (kind string = "constant")
    //   match_trait.rs  → tree-sitter returns SymbolKind::Interface (kind string = "interface")
    let mock_scout = MockScout::default();
    mock_scout.set_result(Ok(SearchResult {
        matches: vec![
            SearchMatch {
                file: "target_dir/match_const.rs".to_owned(),
                line: 1,
                column: 0,
                content: "const MY_CONST: u32 = 42;".to_owned(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "abc".to_owned(),
                known: None,
            },
            SearchMatch {
                file: "target_dir/match_trait.rs".to_owned(),
                line: 1,
                column: 0,
                content: "trait MyTrait {}".to_owned(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "def".to_owned(),
                known: None,
            },
        ],
        total_matches: 2,
        truncated: false,
        files_searched: 1,
        files_in_scope: 1,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    }));

    let mock_surgeon = Arc::new(MockSurgeon::new());

    // match_const.rs → SymbolKind::Constant → kind_string = "constant"
    mock_surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(Some(ExtractedSymbol {
            name: "MY_CONST".to_owned(),
            semantic_path: "MY_CONST".to_owned(),
            kind: pathfinder_treesitter::surgeon::SymbolKind::Constant,
            byte_range: 0..24,
            start_line: 0,
            end_line: 0,
            name_column: 6,
            access_level: AccessLevel::Public,
            children: vec![],
        })));

    // match_trait.rs → SymbolKind::Interface → kind_string = "interface"
    mock_surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(Some(ExtractedSymbol {
            name: "MyTrait".to_owned(),
            semantic_path: "MyTrait".to_owned(),
            kind: pathfinder_treesitter::surgeon::SymbolKind::Interface,
            byte_range: 0..16,
            start_line: 0,
            end_line: 0,
            name_column: 6,
            access_level: AccessLevel::Public,
            children: vec![],
        })));

    let server =
        PathfinderServer::with_engines(ws, config, sandbox, Arc::new(mock_scout), mock_surgeon);

    // --- Test 1: filter="const" must match SymbolKind::Constant ---
    // Before the fix, filter-side alias "const" was accepted by is_valid_kind_filter
    // but then kind_matches_filter only checked the kind side, so "constant" != "const"
    // → 0 results. Now the filter side is also normalised → 1 result expected.
    let params_const = SearchToolParams {
        query: "MY".to_string(),
        path_glob: "target_dir/*".to_string(),
        kind: Some("const".to_string()),
        ..Default::default()
    };
    let Json(res) = server
        .find_symbol_impl(params_const)
        .await
        .expect("const alias should succeed");
    assert_eq!(
        res.symbols.len(),
        1,
        "filter='const' should match SymbolKind::Constant; got {:#?}",
        res.symbols
    );
    assert!(
        res.symbols[0].semantic_path.ends_with("::MY_CONST"),
        "unexpected path: {}",
        res.symbols[0].semantic_path
    );
    assert_eq!(res.symbols[0].kind, "constant");

    // --- Test 2: filter="trait" must match SymbolKind::Interface ---
    // "trait" is the Rust keyword; tree-sitter maps it to SymbolKind::Interface.
    // The filter alias must find it.
    // We need a fresh server+mock for the second call since MockSurgeon results
    // are consumed sequentially. Reset via a new scout result + surgeon queue.
    let ws_dir2 = tempdir().expect("temp dir2");
    let ws2 = WorkspaceRoot::new(ws_dir2.path()).expect("valid root2");
    let config2 = pathfinder_common::config::PathfinderConfig::default();
    let sandbox2 = Sandbox::new(ws2.path(), &config2.sandbox);

    let mock_scout2 = MockScout::default();
    mock_scout2.set_result(Ok(SearchResult {
        matches: vec![SearchMatch {
            file: "target_dir/match_trait.rs".to_owned(),
            line: 1,
            column: 0,
            content: "trait MyTrait {}".to_owned(),
            context_before: vec![],
            context_after: vec![],
            enclosing_semantic_path: None,
            is_definition: None,
            version_hash: "def".to_owned(),
            known: None,
        }],
        total_matches: 1,
        truncated: false,
        files_searched: 1,
        files_in_scope: 1,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    }));

    let mock_surgeon2 = Arc::new(MockSurgeon::new());
    mock_surgeon2
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(Some(ExtractedSymbol {
            name: "MyTrait".to_owned(),
            semantic_path: "MyTrait".to_owned(),
            kind: pathfinder_treesitter::surgeon::SymbolKind::Interface,
            byte_range: 0..16,
            start_line: 0,
            end_line: 0,
            name_column: 6,
            access_level: AccessLevel::Public,
            children: vec![],
        })));

    let server2 = PathfinderServer::with_engines(
        ws2,
        config2,
        sandbox2,
        Arc::new(mock_scout2),
        mock_surgeon2,
    );

    let params_trait = SearchToolParams {
        query: "MyTrait".to_string(),
        path_glob: "target_dir/*".to_string(),
        kind: Some("trait".to_string()),
        ..Default::default()
    };
    let Json(res2) = server2
        .find_symbol_impl(params_trait)
        .await
        .expect("trait alias should succeed");
    assert_eq!(
        res2.symbols.len(),
        1,
        "filter='trait' should match SymbolKind::Interface; got {:#?}",
        res2.symbols
    );
    assert!(
        res2.symbols[0].semantic_path.ends_with("::MyTrait"),
        "unexpected path: {}",
        res2.symbols[0].semantic_path
    );
    assert_eq!(res2.symbols[0].kind, "interface");
}

// ── infer_single_ext_from_glob unit tests ────────────────────────────────────

#[test]
fn test_infer_single_ext_from_glob_known_single_exts() {
    assert_eq!(infer_single_ext_from_glob("**/*.rs"), Some("rs"));
    assert_eq!(infer_single_ext_from_glob("src/**/*.rs"), Some("rs"));
    assert_eq!(infer_single_ext_from_glob("**/*.ts"), Some("ts"));
    assert_eq!(infer_single_ext_from_glob("**/*.tsx"), Some("tsx"));
    assert_eq!(infer_single_ext_from_glob("**/*.js"), Some("js"));
    assert_eq!(infer_single_ext_from_glob("**/*.jsx"), Some("jsx"));
    assert_eq!(infer_single_ext_from_glob("**/*.py"), Some("py"));
    assert_eq!(infer_single_ext_from_glob("**/*.go"), Some("go"));
    assert_eq!(infer_single_ext_from_glob("**/*.java"), Some("java"));
    assert_eq!(infer_single_ext_from_glob("**/*.vue"), Some("vue"));
}

#[test]
fn test_infer_single_ext_from_glob_no_match() {
    // Extension-agnostic globs
    assert_eq!(infer_single_ext_from_glob("**/*"), None);
    assert_eq!(infer_single_ext_from_glob("src/*"), None);
    assert_eq!(infer_single_ext_from_glob("target_dir/*"), None);
    // Unknown extension
    assert_eq!(infer_single_ext_from_glob("**/*.xyz"), None);
    assert_eq!(infer_single_ext_from_glob("**/*.toml"), None);
}

#[test]
fn test_infer_single_ext_from_glob_multi_ext_brace() {
    // Brace expansion — must return None (ambiguous)
    assert_eq!(infer_single_ext_from_glob("**/*.{ts,tsx}"), None);
    assert_eq!(infer_single_ext_from_glob("**/*.{js,jsx}"), None);
    assert_eq!(infer_single_ext_from_glob("**/*.{rs,go}"), None);
}

/// When `path_glob` = "**/*.rs", `find_symbol_impl` must use Rust-specific
/// definition patterns (fn/struct/enum/trait) NOT a bare \bname\b search.
///
/// This test verifies the fix by checking that a Rust file containing the
/// symbol only as a *call site* (not a definition) is NOT returned — the
/// language-specific fn/struct patterns require definitional syntax.
#[tokio::test]
async fn test_find_symbol_ext_filter_uses_language_specific_patterns() {
    let ws_dir = tempdir().expect("tempdir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // A Rust file that has `my_func` only as a *call site*, not a definition.
    let call_site_only = "fn other() { my_func(42); }";
    std::fs::write(ws_dir.path().join("calls.rs"), call_site_only).unwrap();

    // MockScout: we use RipgrepScout against the tempdir to get real search behaviour.
    let scout = Arc::new(pathfinder_search::RipgrepScout);
    let surgeon = Arc::new(MockSurgeon::new());
    // No surgeon mocks needed — ripgrep should produce zero results (no definition
    // pattern matches) so enrichment is never called.
    let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);

    let server = crate::server::PathfinderServer::with_all_engines(
        ws, config, sandbox, scout, surgeon, lawyer,
    );

    let params = SearchToolParams {
        query: "my_func".to_string(),
        // Explicit extension filter — triggers the infer_single_ext_from_glob path.
        path_glob: "**/*.rs".to_string(),
        max_results: 10,
        ..Default::default()
    };

    let Json(response) = server
        .find_symbol_impl(params)
        .await
        .expect("find_symbol should not error");

    // Rust definition patterns (fn my_func, struct my_func, etc.) will NOT match
    // the call site `my_func(42)` — so we expect zero symbols returned.
    assert_eq!(
        response.symbols.len(),
        0,
        "language-specific Rust patterns must not match call sites; \
         got unexpected results: {:#?}",
        response.symbols
    );
}
