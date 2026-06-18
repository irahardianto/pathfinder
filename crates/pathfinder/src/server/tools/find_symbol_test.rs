use super::*;

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
    assert_eq!(
        extract_name_from_line("let myVariable = 42"),
        "myVariable"
    );
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
    if outside.parent().map_or(false, |p| p.exists()) {
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

