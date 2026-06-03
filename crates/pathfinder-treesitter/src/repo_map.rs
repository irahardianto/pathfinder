use crate::error::SurgeonError;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};

/// Configuration for skeleton generation.
///
/// Bundles token budget, traversal depth, visibility filtering, and
/// file extension filters into a single struct to reduce parameter
/// count across the call chain.
#[derive(Debug, Clone)]
pub struct SkeletonConfig<'a> {
    /// Maximum total tokens for the entire skeleton.
    pub max_tokens: u32,
    /// Maximum directory depth to traverse (0 = unlimited).
    pub depth: u32,
    /// Visibility filter: "public" or "all".
    pub visibility: &'a str,
    /// Per-file token cap before truncation.
    pub max_tokens_per_file: u32,
    /// Optional whitelist of changed files (None = no filter).
    pub changed_files: Option<HashSet<PathBuf>>,
    /// File extensions to include (empty = all).
    pub include_extensions: Vec<String>,
    /// File extensions to exclude (empty = none).
    pub exclude_extensions: Vec<String>,
    /// Include test symbols regardless of visibility.
    pub include_tests: bool,
}

impl<'a> SkeletonConfig<'a> {
    /// Create a new skeleton config with sensible defaults.
    #[must_use]
    pub const fn new(
        max_tokens: u32,
        depth: u32,
        visibility: &'a str,
        max_tokens_per_file: u32,
    ) -> Self {
        Self {
            max_tokens,
            depth,
            visibility,
            max_tokens_per_file,
            changed_files: None,
            include_extensions: Vec::new(),
            exclude_extensions: Vec::new(),
            include_tests: true,
        }
    }

    /// Builder-style setter for `include_tests`.
    #[must_use]
    pub fn with_include_tests(mut self, include_tests: bool) -> Self {
        self.include_tests = include_tests;
        self
    }

    /// Builder-style setter for changed files filter.
    #[must_use]
    pub fn with_changed_files(mut self, changed_files: Option<HashSet<PathBuf>>) -> Self {
        self.changed_files = changed_files;
        self
    }

    /// Builder-style setter for include extensions.
    #[must_use]
    pub fn with_include_extensions(mut self, include_extensions: Vec<String>) -> Self {
        self.include_extensions = include_extensions;
        self
    }

    /// Builder-style setter for exclude extensions.
    #[must_use]
    pub fn with_exclude_extensions(mut self, exclude_extensions: Vec<String>) -> Self {
        self.exclude_extensions = exclude_extensions;
        self
    }
}

/// The result of a `get_repo_map` generation.
#[derive(Debug, Clone)]
pub struct RepoMapResult {
    /// The repository skeleton representation.
    pub skeleton: String,
    /// List of technologies used in the repository.
    pub tech_stack: Vec<String>,
    /// Number of files scanned during repository mapping.
    pub files_scanned: usize,
    /// Number of files truncated during processing.
    pub files_truncated: usize,
    /// Number of files considered in scope.
    pub files_in_scope: usize,
    /// Percentage of files covered in the mapping process.
    pub coverage_percent: u8,
    /// Mapping of version identifiers to their corresponding hashes.
    pub version_hashes: HashMap<String, String>,
}

/// Estimate the number of tokens for the given text.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
pub fn estimate_tokens(text: &str) -> u32 {
    let chars = text.chars().count();
    (chars as f32 / 4.0).ceil() as u32
}

use crate::surgeon::{AccessLevel, ExtractedSymbol, SymbolKind};

/// Default per-file token cap. Used when no per-call override is supplied.
/// At ~4 chars/token, 2 000 tokens ≈ 8 KB — covers the vast majority of
/// real source files without falling back to the truncated stub.
#[cfg(test)]
const MAX_TOKENS_PER_FILE: u32 = 2_000;

/// Returns `true` if the symbol is a test-related symbol.
///
/// Test symbols include:
/// - Modules named "tests" or "test"
/// - Functions with test-like naming conventions: `test_*`, `it_*`, `*_test`
fn is_test_symbol(sym: &ExtractedSymbol) -> bool {
    if sym.kind == SymbolKind::Test {
        return true;
    }
    if sym.kind == SymbolKind::Module && matches!(sym.name.as_str(), "tests" | "test") {
        return true;
    }
    if sym.kind == SymbolKind::Function || sym.kind == SymbolKind::Method {
        let name = sym.name.as_str();
        if name.starts_with("test_") || name.starts_with("it_") || name.ends_with("_test") {
            return true;
        }
    }
    false
}

/// Recursively filter `symbols` keeping only those visible under `visibility`.
///
/// - `"all"` — no filtering, returns the slice unchanged in a cloned `Vec`.
/// - `"public"` — keeps symbols with `AccessLevel::Public` or `AccessLevel::Protected`
///   and recursively filters children; empty parents are dropped.
///
/// When `include_tests = true`, test symbols (modules named "tests"/"test",
/// functions with `test_` prefix etc.) are always included regardless of visibility.
#[must_use]
fn filter_by_visibility(
    symbols: Vec<ExtractedSymbol>,
    visibility: &str,
    include_tests: bool,
) -> Vec<ExtractedSymbol> {
    if visibility != "public" {
        return symbols;
    }
    symbols
        .into_iter()
        .filter(|sym| {
            if include_tests && is_test_symbol(sym) {
                return true;
            }
            matches!(
                sym.access_level,
                AccessLevel::Public | AccessLevel::Protected
            )
        })
        .map(|mut sym| {
            sym.children = filter_by_visibility(sym.children, visibility, include_tests);
            sym
        })
        .collect()
}

/// Render a single file's skeleton into an indented string.
///
/// If the rendered output exceeds `max_tokens_per_file`, the result is
/// collapsed to a truncated stub showing only class/struct names and method
/// counts. Pass [`MAX_TOKENS_PER_FILE`] as the default when no caller override
/// is available.
#[must_use]
pub fn render_file_skeleton(symbols: &[ExtractedSymbol], max_tokens_per_file: u32) -> String {
    let mut out = String::default();
    render_symbols_recursive(symbols, 0, &mut out);

    // Check if the file is too large
    if estimate_tokens(&out) > max_tokens_per_file {
        return render_truncated_file_skeleton(symbols);
    }

    out
}

fn render_symbols_recursive(symbols: &[ExtractedSymbol], depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    for sym in symbols {
        use crate::surgeon::SymbolKind;
        let prefix = match sym.kind {
            SymbolKind::Test => "test ",
            SymbolKind::Function => "func ",
            SymbolKind::Class => "class ",
            SymbolKind::Struct => "struct ",
            SymbolKind::Method => "method ",
            SymbolKind::Impl => "impl ",
            SymbolKind::Constant => "const ",
            SymbolKind::Interface => "interface ",
            SymbolKind::Enum => "enum ",
            SymbolKind::Module => "mod ",
            // Vue SFC zone symbols
            SymbolKind::Zone => "zone ",
            SymbolKind::Component => "component ",
            SymbolKind::HtmlElement => "element ",
            SymbolKind::CssSelector => "selector ",
            SymbolKind::CssAtRule => "at-rule ",
        };

        let declaration = format!("{}{}", prefix, sym.name);
        let _ = writeln!(out, "{}{} // {}", indent, declaration, sym.semantic_path);

        if !sym.children.is_empty() {
            render_symbols_recursive(&sym.children, depth + 1, out);
        }
    }
}

/// A fallback rendering that preserves top-level symbol names of all kinds with child counts.
fn render_truncated_file_skeleton(symbols: &[ExtractedSymbol]) -> String {
    use crate::surgeon::SymbolKind;
    use std::fmt::Write as _;

    let mut out = String::default();
    for sym in symbols {
        let prefix = match sym.kind {
            SymbolKind::Test => "test ",
            SymbolKind::Function => "func ",
            SymbolKind::Class => "class ",
            SymbolKind::Struct => "struct ",
            SymbolKind::Method => "method ",
            SymbolKind::Impl => "impl ",
            SymbolKind::Constant => "const ",
            SymbolKind::Interface => "interface ",
            SymbolKind::Enum => "enum ",
            SymbolKind::Module => "mod ",
            SymbolKind::Zone => "zone ",
            SymbolKind::Component => "component ",
            SymbolKind::HtmlElement => "element ",
            SymbolKind::CssSelector => "selector ",
            SymbolKind::CssAtRule => "at-rule ",
        };

        let _ = writeln!(out, "{}{} // {}", prefix, sym.name, sym.semantic_path);

        if matches!(
            sym.kind,
            SymbolKind::Class
                | SymbolKind::Struct
                | SymbolKind::Enum
                | SymbolKind::Interface
                | SymbolKind::Impl
                | SymbolKind::Module
        ) {
            let method_count = sym
                .children
                .iter()
                .filter(|c| c.kind == SymbolKind::Method)
                .count();
            let func_count = sym
                .children
                .iter()
                .filter(|c| c.kind == SymbolKind::Function)
                .count();
            let const_count = sym
                .children
                .iter()
                .filter(|c| c.kind == SymbolKind::Constant)
                .count();

            let mut omitted = Vec::new();
            if method_count > 0 {
                omitted.push(format!("{method_count} methods"));
            }
            if func_count > 0 {
                omitted.push(format!("{func_count} functions"));
            }
            if const_count > 0 {
                omitted.push(format!("{const_count} constants"));
            }
            if !omitted.is_empty() {
                let _ = writeln!(out, "  // ... {} omitted", omitted.join(", "));
            }
        }
    }

    if out.is_empty() {
        "// [TRUNCATED - NO SYMBOLS EXTRACTED]".to_string()
    } else {
        format!("// [TRUNCATED DUE TO SIZE]\n{out}")
    }
}

/// Generate an AST-based skeleton of a directory.
///
/// # Errors
/// Returns `SurgeonError` if an operation on the AST fails.
#[expect(
    clippy::too_many_lines,
    reason = "Sequential directory-walk pipeline; splitting into sub-functions would obscure the linear data flow without improving readability"
)]
#[allow(clippy::items_after_statements)]
pub async fn generate_skeleton_text(
    surgeon: &impl crate::surgeon::Surgeon,
    workspace_root: &Path,
    target_path: &Path,
    config: &SkeletonConfig<'_>,
) -> Result<RepoMapResult, SurgeonError> {
    use ignore::WalkBuilder;
    use pathfinder_common::types::VersionHash;

    let abs_target = workspace_root.join(target_path);

    let mut builder = WalkBuilder::new(&abs_target);
    builder.max_depth(Some(config.depth as usize));
    builder.require_git(false);
    builder.hidden(true);
    builder.add_custom_ignore_filename(".pathfinderignore");

    let walker = builder.build();

    struct FileEntry {
        abs_path: PathBuf,
        rel_path: PathBuf,
        lang: crate::language::SupportedLanguage,
    }

    let mut file_entries: Vec<FileEntry> = Vec::new();
    let mut tech_stack: Vec<crate::language::SupportedLanguage> = Vec::default();

    for result in walker {
        let Ok(entry) = result else { continue };

        let path = entry.path();
        if path.is_dir() {
            continue;
        }

        let rel_path = path.strip_prefix(workspace_root).unwrap_or(path);

        if let Some(changed) = &config.changed_files {
            if !changed.contains(rel_path) {
                continue;
            }
        }

        if !config.include_extensions.is_empty() || !config.exclude_extensions.is_empty() {
            let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
            if !config.include_extensions.is_empty()
                && !config.include_extensions.iter().any(|e| e == ext)
            {
                continue;
            }
            if !config.exclude_extensions.is_empty()
                && config.exclude_extensions.iter().any(|e| e == ext)
            {
                continue;
            }
        }

        let Some(lang) = crate::language::SupportedLanguage::detect(path) else {
            continue;
        };

        if !tech_stack.contains(&lang) {
            tech_stack.push(lang);
        }

        file_entries.push(FileEntry {
            abs_path: path.to_path_buf(),
            rel_path: rel_path.to_path_buf(),
            lang,
        });
    }

    let files_in_scope = file_entries.len();

    let read_futures: Vec<_> = file_entries
        .iter()
        .map(|entry| async {
            let (read_result, meta_result) = tokio::join!(
                tokio::fs::read(&entry.abs_path),
                tokio::fs::metadata(&entry.abs_path)
            );
            let mtime = meta_result
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            (entry.rel_path.clone(), entry.lang, read_result, mtime)
        })
        .collect();

    let read_results = futures::future::join_all(read_futures).await;

    struct ProcessedFile {
        rel_path: PathBuf,
        skeleton: String,
        skeleton_tokens: u32,
    }

    let mut processed: Vec<ProcessedFile> = Vec::new();
    let mut files_with_symbols = 0;
    let mut version_hashes = HashMap::default();

    for (rel_path, _lang, read_result, mtime) in read_results {
        let source = match read_result {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(
                    path = %rel_path.display(),
                    error = %e,
                    "get_repo_map: skipping file (read failed)"
                );
                continue;
            }
        };
        let hash = VersionHash::compute(&source);
        version_hashes.insert(rel_path.display().to_string(), hash.short().to_owned());

        let content_arc: std::sync::Arc<[u8]> = std::sync::Arc::from(source);

        let raw_symbols = match surgeon
            .extract_symbols_preloaded(workspace_root, &rel_path, content_arc, mtime)
            .await
        {
            Ok(syms) => syms,
            Err(e) => {
                tracing::debug!(
                    path = %rel_path.display(),
                    error = %e,
                    "get_repo_map: skipping file (symbol extraction failed)"
                );
                continue;
            }
        };

        let symbols = filter_by_visibility(raw_symbols, config.visibility, config.include_tests);

        if symbols.is_empty() {
            continue;
        }

        files_with_symbols += 1;

        let file_skeleton = render_file_skeleton(&symbols, config.max_tokens_per_file);
        let file_skeleton_tokens = estimate_tokens(&file_skeleton);

        processed.push(ProcessedFile {
            rel_path,
            skeleton: file_skeleton,
            skeleton_tokens: file_skeleton_tokens,
        });
    }

    processed.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    let mut skeleton_out = String::default();
    let mut current_tokens: u32 = 0;
    let mut files_rendered: usize = 0;
    let mut files_truncated: usize = 0;

    for pf in &processed {
        if current_tokens + pf.skeleton_tokens > config.max_tokens {
            if current_tokens + 50 <= config.max_tokens {
                use std::fmt::Write;
                let _ = writeln!(
                    skeleton_out,
                    "\n// [... Omitted {} due to token budget]",
                    pf.rel_path.display()
                );
                current_tokens += 50;
            }
            files_truncated += 1;
            continue;
        }

        let path_header = format!(
            "\nFile: {}\n{}\n",
            pf.rel_path.display(),
            "=".repeat(pf.rel_path.display().to_string().len() + 6)
        );

        let header_tokens = estimate_tokens(&path_header);
        current_tokens += header_tokens + pf.skeleton_tokens;
        files_rendered += 1;
        skeleton_out.push_str(&path_header);
        skeleton_out.push_str(&pf.skeleton);
    }

    let coverage_percent = if files_in_scope > 0 {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let percent = ((files_with_symbols as f32 / files_in_scope as f32) * 100.0) as u8;
        percent
    } else {
        100
    };

    Ok(RepoMapResult {
        skeleton: skeleton_out.trim().to_string(),
        tech_stack: tech_stack.iter().map(|l| format!("{l:?}")).collect(),
        files_scanned: files_rendered,
        files_truncated,
        files_in_scope,
        coverage_percent,
        version_hashes,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::mock::MockSurgeon;
    use crate::surgeon::{ExtractedSymbol, SymbolKind};
    use std::sync::Arc;

    fn make_sym(name: &str, kind: SymbolKind) -> ExtractedSymbol {
        ExtractedSymbol {
            name: name.to_string(),
            semantic_path: name.to_string(),
            kind,
            byte_range: 0..1,
            start_line: 0,
            end_line: 1,
            name_column: 0,
            access_level: crate::surgeon::AccessLevel::Public,
            children: vec![],
        }
    }

    #[test]
    fn test_filter_all_keeps_everything() {
        let syms = vec![
            make_sym("_private", SymbolKind::Function),
            make_sym("Public", SymbolKind::Function),
        ];
        let filtered = filter_by_visibility(syms, "all", false);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn test_filter_public_removes_underscore_prefix() {
        // Simulate what detect_access_level would set during extraction:
        // _helper → Private, compute → Public
        let mut syms = vec![
            make_sym("_helper", SymbolKind::Function),
            make_sym("compute", SymbolKind::Function),
        ];
        syms[0].access_level = crate::surgeon::AccessLevel::Private;
        let filtered = filter_by_visibility(syms, "public", false);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "compute");
    }

    #[test]
    fn test_filter_public_go_removes_lowercase_top_level_functions() {
        // With access_level-based filtering, Go public/private is determined at extraction time.
        // make_sym() creates symbols with AccessLevel::Public; we manually adjust for private.
        let mut syms = vec![
            make_sym("internal", SymbolKind::Function),
            make_sym("Export", SymbolKind::Function),
            make_sym("_hidden", SymbolKind::Struct),
            make_sym("PublicStruct", SymbolKind::Struct),
        ];
        // Simulate what extract_access_level would produce for Go:
        syms[0].access_level = crate::surgeon::AccessLevel::Package; // lowercase → Package
        syms[2].access_level = crate::surgeon::AccessLevel::Private; // _hidden → Private
        let filtered = filter_by_visibility(syms, "public", false);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].name, "Export");
        assert_eq!(filtered[1].name, "PublicStruct");
    }

    #[test]
    fn test_filter_public_recursively_prunes_children() {
        let mut parent = make_sym("Parent", SymbolKind::Class);
        parent.children = vec![
            make_sym("_private_method", SymbolKind::Method),
            make_sym("public_method", SymbolKind::Method),
        ];
        // Simulate what detect_access_level would produce:
        parent.children[0].access_level = crate::surgeon::AccessLevel::Private;
        let filtered = filter_by_visibility(vec![parent], "public", false);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].children.len(), 1);
        assert_eq!(filtered[0].children[0].name, "public_method");
    }

    #[test]
    fn test_include_tests_preserves_test_module() {
        // Private "tests" module should be visible when include_tests=true
        let mut tests_mod = make_sym("tests", SymbolKind::Module);
        tests_mod.access_level = crate::surgeon::AccessLevel::Private;
        tests_mod.children = vec![make_sym("test_something", SymbolKind::Function)];
        tests_mod.children[0].access_level = crate::surgeon::AccessLevel::Private;

        let syms = vec![tests_mod];

        // With include_tests=true: test module should be kept
        let filtered_with = filter_by_visibility(syms.clone(), "public", true);
        assert_eq!(filtered_with.len(), 1);
        assert_eq!(filtered_with[0].name, "tests");

        // With include_tests=false: private module should be filtered
        let filtered_without = filter_by_visibility(syms, "public", false);
        assert_eq!(filtered_without.len(), 0);
    }

    #[test]
    fn test_include_tests_preserves_test_prefixed_functions() {
        // Private function with test_ prefix should be visible when include_tests=true
        let mut test_fn = make_sym("test_something", SymbolKind::Function);
        test_fn.access_level = crate::surgeon::AccessLevel::Private;

        let mut normal_fn = make_sym("helper", SymbolKind::Function);
        normal_fn.access_level = crate::surgeon::AccessLevel::Private;

        let syms = vec![test_fn, normal_fn];

        // With include_tests=true: test_ function should be kept
        let filtered_with = filter_by_visibility(syms.clone(), "public", true);
        assert_eq!(filtered_with.len(), 1);
        assert_eq!(filtered_with[0].name, "test_something");

        // With include_tests=false: both private functions should be filtered
        let filtered_without = filter_by_visibility(syms, "public", false);
        assert_eq!(filtered_without.len(), 0);
    }

    #[test]
    fn test_include_tests_preserves_suffix_test_functions() {
        // Private function with _test suffix should be visible when include_tests=true
        let mut test_fn = make_sym("something_test", SymbolKind::Function);
        test_fn.access_level = crate::surgeon::AccessLevel::Private;

        let syms = vec![test_fn];

        let filtered = filter_by_visibility(syms, "public", true);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "something_test");
    }

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens("a"), 1);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
    }

    #[test]
    fn test_render_file_skeleton() {
        let symbols = vec![ExtractedSymbol {
            name: "MyClass".to_string(),
            semantic_path: "MyClass".to_string(),
            kind: SymbolKind::Class,
            byte_range: 0..10,
            start_line: 0,
            end_line: 10,
            name_column: 0,
            access_level: crate::surgeon::AccessLevel::Public,
            children: vec![ExtractedSymbol {
                name: "my_method".to_string(),
                semantic_path: "MyClass.my_method".to_string(),
                kind: SymbolKind::Method,
                byte_range: 5..8,
                start_line: 5,
                end_line: 8,
                name_column: 0,
                access_level: crate::surgeon::AccessLevel::Public,
                children: vec![],
            }],
        }];

        let output = render_file_skeleton(&symbols, MAX_TOKENS_PER_FILE);
        assert!(output.contains("class MyClass // MyClass"));
        assert!(output.contains("  method my_method // MyClass.my_method"));
    }

    #[test]
    fn test_render_truncated_file_skeleton_fallback() {
        // Construct massive nested symbol structure that exceeds token limits.
        // At the new 2_000-token threshold (~8 KB), we need 200 long method names to
        // generate ~12 000 chars (~3 000 tokens), which reliably triggers truncation.
        let mut methods = Vec::default();
        for i in 0..200 {
            methods.push(ExtractedSymbol {
                name: format!("massive_method_{i}"),
                semantic_path: format!("MyGiganticClass.massive_method_{i}"),
                kind: SymbolKind::Method,
                byte_range: 0..0,
                start_line: 0,
                end_line: 0,
                name_column: 0,
                access_level: crate::surgeon::AccessLevel::Public,
                children: vec![],
            });
        }

        // This class with 100 methods with long names easily exceeds 2_000 tokens (~8 KB)
        let symbols = vec![ExtractedSymbol {
            name: "MyGiganticClass".to_string(),
            semantic_path: "MyGiganticClass".to_string(),
            kind: SymbolKind::Class,
            byte_range: 0..0,
            start_line: 0,
            end_line: 0,
            name_column: 0,
            access_level: crate::surgeon::AccessLevel::Public,
            children: methods,
        }];

        render_symbols_recursive(&symbols, 0, &mut String::default());
        // To properly test, let's call `render_file_skeleton` which calls the truncated version internally
        let output = render_file_skeleton(&symbols, MAX_TOKENS_PER_FILE);
        assert!(output.contains("[TRUNCATED DUE TO SIZE]"));
        assert!(output.contains("class MyGiganticClass // MyGiganticClass"));
        assert!(output.contains("200 methods omitted"));
        assert!(!output.contains("massive_method_0")); // methods shouldn't be printed
    }

    #[test]
    fn test_render_symbols_recursive_directly() {
        let symbols = vec![ExtractedSymbol {
            name: "Foo".to_string(),
            semantic_path: "Foo".to_string(),
            kind: SymbolKind::Function,
            byte_range: 0..0,
            start_line: 0,
            end_line: 0,
            name_column: 0,
            access_level: crate::surgeon::AccessLevel::Public,
            children: vec![],
        }];
        let mut out = String::default();
        render_symbols_recursive(&symbols, 0, &mut out);
        assert_eq!(out, "func Foo // Foo\n");
    }

    /// Regression test: default depth of 3 was too shallow for Rust workspace layouts.
    ///
    /// The standard layout `crates/X/src/file.rs` places files at depth 4 from the repo
    /// root, which `max_depth(3)` cannot reach. This test verifies that `generate_skeleton_text`
    /// with depth=4 discovers files nested inside a `src/` subdirectory (depth 4), while
    /// depth=3 would miss them — ensuring the fix (default=5) covers real-world layouts.
    #[tokio::test]
    async fn test_generate_skeleton_text_depth_reaches_nested_src_files() {
        use crate::mock::MockSurgeon;
        use crate::surgeon::{ExtractedSymbol, SymbolKind};
        use std::sync::Arc;
        use tempfile::tempdir;

        // Create a temp workspace mimicking a Rust workspace:
        //   root/
        //     crates/
        //       my-crate/
        //         src/
        //           lib.rs   ← depth 4 from root
        let ws_dir = tempdir().expect("temp dir");
        let nested_src = ws_dir.path().join("crates").join("my-crate").join("src");
        tokio::fs::create_dir_all(&nested_src)
            .await
            .expect("create dirs");
        tokio::fs::write(nested_src.join("lib.rs"), b"pub fn answer() -> u32 { 42 }")
            .await
            .expect("write file");

        let mock = MockSurgeon::new();
        // The surgeon is called once per discovered file; return a symbol so the file
        // is included in the skeleton (files with empty symbols are skipped).
        mock.extract_symbols_results
            .lock()
            .expect("lock")
            .push(Ok(vec![ExtractedSymbol {
                name: "answer".to_string(),
                semantic_path: "answer".to_string(),
                kind: SymbolKind::Function,
                byte_range: 0..29,
                start_line: 0,
                end_line: 0,
                name_column: 0,
                access_level: crate::surgeon::AccessLevel::Public,
                children: vec![],
            }]));

        let surgeon = Arc::new(mock);
        let ws_root = ws_dir.path();
        let target = std::path::Path::new(".");

        // depth=4 must find the file at crates/my-crate/src/lib.rs
        let config = SkeletonConfig::new(50_000, 4, "all", 2_000);
        let result = generate_skeleton_text(&*surgeon, ws_root, target, &config)
            .await
            .expect("skeleton generation succeeds");

        assert_eq!(
            result.files_in_scope, 1,
            "depth=4 should discover 1 source file at crates/my-crate/src/lib.rs"
        );
        assert!(
            result.skeleton.contains("lib.rs"),
            "skeleton must reference the nested file"
        );
    }

    /// Validates that depth=3 misses files at depth 4, confirming the bug that the default
    /// of 3 caused (and that the new default of 5 fixes).
    #[tokio::test]
    async fn test_generate_skeleton_text_depth_3_misses_nested_src_files() {
        use crate::mock::MockSurgeon;
        use std::sync::Arc;
        use tempfile::tempdir;

        let ws_dir = tempdir().expect("temp dir");
        let nested_src = ws_dir.path().join("crates").join("my-crate").join("src");
        tokio::fs::create_dir_all(&nested_src)
            .await
            .expect("create dirs");
        tokio::fs::write(nested_src.join("lib.rs"), b"pub fn answer() -> u32 { 42 }")
            .await
            .expect("write file");

        let surgeon = Arc::new(MockSurgeon::new());
        // No extract_symbols_results configured — the file should never be reached.

        let config = SkeletonConfig::new(50_000, 3, "all", 2_000); // OLD default — deliberately too shallow
        let result =
            generate_skeleton_text(&*surgeon, ws_dir.path(), std::path::Path::new("."), &config)
                .await
                .expect("skeleton generation succeeds");

        assert_eq!(
            result.files_in_scope, 0,
            "depth=3 must NOT reach files at crates/my-crate/src/lib.rs (depth 4)"
        );
    }

    #[tokio::test]
    async fn test_generate_skeleton_with_filters() {
        let ws_dir = tempfile::tempdir().expect("create temp dir");
        let ws_root = ws_dir.path();

        let rs_path = ws_root.join("src").join("lib.rs");
        let txt_path = ws_root.join("src").join("notes.txt");
        let toml_path = ws_root.join("Cargo.toml");
        std::fs::create_dir_all(ws_root.join("src")).expect("create src dir");

        tokio::fs::write(&rs_path, b"fn main() {}")
            .await
            .expect("write");
        tokio::fs::write(&txt_path, b"hello").await.expect("write");
        tokio::fs::write(&toml_path, b"[package]")
            .await
            .expect("write");

        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .generate_skeleton_results
            .lock()
            .expect("mutex")
            .push(Ok(crate::repo_map::RepoMapResult {
                skeleton: "lib.rs skeleton".to_owned(),
                files_in_scope: 1,
                files_truncated: 0,
                files_scanned: 1,
                coverage_percent: 100,
                version_hashes: std::collections::HashMap::default(),
                tech_stack: vec![],
            }));

        // 1. changed_files filter
        surgeon
            .extract_symbols_results
            .lock()
            .expect("mutex")
            .push(Ok(vec![]));

        let mut changed = std::collections::HashSet::new();
        changed.insert(std::path::PathBuf::from("src/lib.rs"));
        let config_changed =
            SkeletonConfig::new(50_000, 4, "all", 2_000).with_changed_files(Some(changed));
        let _result_changed = generate_skeleton_text(
            &*surgeon,
            ws_root,
            std::path::Path::new("."),
            &config_changed,
        )
        .await
        .expect("skeleton changed");

        // 2. include_extensions filter
        surgeon
            .generate_skeleton_results
            .lock()
            .expect("mutex")
            .push(Ok(crate::repo_map::RepoMapResult {
                skeleton: "lib.rs skeleton".to_owned(),
                files_in_scope: 1,
                files_truncated: 0,
                files_scanned: 1,
                coverage_percent: 100,
                version_hashes: std::collections::HashMap::default(),
                tech_stack: vec![],
            }));
        // 2. include_extensions filter
        surgeon
            .extract_symbols_results
            .lock()
            .expect("mutex")
            .push(Ok(vec![]));

        let config_ext = SkeletonConfig::new(50_000, 4, "all", 2_000)
            .with_include_extensions(vec!["rs".to_owned()]);
        let _result_ext =
            generate_skeleton_text(&*surgeon, ws_root, std::path::Path::new("."), &config_ext)
                .await
                .expect("skeleton_ext");

        let calls = surgeon.extract_symbols_calls.lock().expect("mutex");
        assert_eq!(calls.len(), 2);

        assert_eq!(calls[0].1, std::path::PathBuf::from("src/lib.rs"));
        assert_eq!(calls[1].1, std::path::PathBuf::from("src/lib.rs"));
    }

    // ---------------------------------------------------------------
    // PATCH-005-C3: pub mod visibility filter tests
    // ---------------------------------------------------------------

    /// PATCH-005-C3: `pub mod` appears in visibility="public" repo map
    #[test]
    fn test_pub_mod_appears_in_public_visibility() {
        let module = ExtractedSymbol {
            name: "types".to_string(),
            semantic_path: "types".to_string(),
            kind: SymbolKind::Module,
            byte_range: 0..30,
            start_line: 0,
            end_line: 5,
            name_column: 0,
            access_level: crate::surgeon::AccessLevel::Public,
            children: vec![ExtractedSymbol {
                name: "foo".to_string(),
                semantic_path: "types.foo".to_string(),
                kind: SymbolKind::Function,
                byte_range: 5..25,
                start_line: 1,
                end_line: 3,
                name_column: 0,
                access_level: crate::surgeon::AccessLevel::Public,
                children: vec![],
            }],
        };
        let filtered = filter_by_visibility(vec![module], "public", false);
        assert_eq!(filtered.len(), 1, "pub mod should be visible in public map");
        assert_eq!(filtered[0].name, "types");
        assert_eq!(
            filtered[0].children.len(),
            1,
            "pub mod children should also be visible"
        );
    }

    /// PATCH-005-C3: Bare `mod` is hidden in visibility="public" repo map
    #[test]
    fn test_private_mod_hidden_in_public_visibility() {
        let module = ExtractedSymbol {
            name: "internal".to_string(),
            semantic_path: "internal".to_string(),
            kind: SymbolKind::Module,
            byte_range: 0..30,
            start_line: 0,
            end_line: 5,
            name_column: 0,
            access_level: crate::surgeon::AccessLevel::Private,
            children: vec![ExtractedSymbol {
                name: "helper".to_string(),
                semantic_path: "internal.helper".to_string(),
                kind: SymbolKind::Function,
                byte_range: 5..25,
                start_line: 1,
                end_line: 3,
                name_column: 0,
                access_level: crate::surgeon::AccessLevel::Public,
                children: vec![],
            }],
        };
        let filtered = filter_by_visibility(vec![module], "public", false);
        assert!(
            filtered.is_empty(),
            "bare mod should be hidden in public map"
        );
    }

    /// PATCH-005-C3: `mod` visible in visibility="all" (no filtering)
    #[test]
    fn test_private_mod_visible_in_all_visibility() {
        let module = ExtractedSymbol {
            name: "tests".to_string(),
            semantic_path: "tests".to_string(),
            kind: SymbolKind::Module,
            byte_range: 0..30,
            start_line: 0,
            end_line: 5,
            name_column: 0,
            access_level: crate::surgeon::AccessLevel::Private,
            children: vec![],
        };
        let filtered = filter_by_visibility(vec![module], "all", false);
        assert_eq!(filtered.len(), 1, "mod should be visible in visibility=all");
    }

    /// With `include_tests=true`, private `mod tests` should appear in visibility="public"
    #[test]
    fn test_include_tests_true_makes_test_mod_visible_in_public_visibility() {
        // This is the NEW behavior: with include_tests=true (default), "tests" module is visible
        let module = ExtractedSymbol {
            name: "tests".to_string(),
            semantic_path: "tests".to_string(),
            kind: SymbolKind::Module,
            byte_range: 0..30,
            start_line: 0,
            end_line: 5,
            name_column: 0,
            access_level: crate::surgeon::AccessLevel::Private,
            children: vec![ExtractedSymbol {
                name: "test_foo".to_string(),
                semantic_path: "tests.test_foo".to_string(),
                kind: SymbolKind::Function,
                byte_range: 5..25,
                start_line: 1,
                end_line: 3,
                name_column: 0,
                access_level: crate::surgeon::AccessLevel::Private,
                children: vec![],
            }],
        };
        // With include_tests=true (DEFAULT): private "tests" module should be visible
        let filtered = filter_by_visibility(vec![module.clone()], "public", true);
        assert_eq!(
            filtered.len(),
            1,
            "mod tests should be visible in public map when include_tests=true"
        );
        assert_eq!(filtered[0].name, "tests");
        assert_eq!(
            filtered[0].children.len(),
            1,
            "test_foo should also be visible"
        );

        // With include_tests=false: private module should be hidden
        let filtered_off = filter_by_visibility(vec![module], "public", false);
        assert!(
            filtered_off.is_empty(),
            "mod tests should be hidden in public map when include_tests=false"
        );
    }
}
