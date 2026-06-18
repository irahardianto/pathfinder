use crate::error::SurgeonError;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};

/// Level of detail for skeleton output.
///
/// Controls how much work `generate_skeleton_text` performs and what kind
/// of output it produces. Higher detail levels are more expensive (CPU,
/// I/O, tokens) because they involve tree-sitter AST parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SkeletonDetail {
    /// Directory tree + manifest/config files only.
    ///
    /// Cheapest mode. Walks the directory tree but does NOT read source
    /// files or run tree-sitter. Includes package manager manifests
    /// (Cargo.toml, package.json, go.mod, pyproject.toml, etc.) as
    /// notable files in the tree.
    Structure,
    /// File listing without symbol extraction.
    ///
    /// Walks all source files, computes version hashes and detects the
    /// tech stack, but does NOT run tree-sitter symbol extraction.
    /// Output shows `File: path` headers only, no symbol bodies.
    Files,
    /// Full AST symbol hierarchy.
    ///
    /// Current default behavior. Reads every source file, runs tree-sitter
    /// to extract symbols, and renders the full skeleton with function
    /// signatures, classes, structs, etc.
    #[default]
    Symbols,
}

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
    /// Level of detail to produce.
    pub detail: SkeletonDetail,
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
            detail: SkeletonDetail::Symbols,
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

    /// Builder-style setter for detail level.
    #[must_use]
    pub const fn with_detail(mut self, detail: SkeletonDetail) -> Self {
        self.detail = detail;
        self
    }
}

/// The result of an `explore` map generation.
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
    /// File paths that were truncated due to token budget.
    pub truncated_paths: Vec<String>,
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
pub fn render_file_skeleton(
    symbols: &[ExtractedSymbol],
    max_tokens_per_file: u32,
) -> (String, bool) {
    let mut out = String::default();
    render_symbols_recursive(symbols, 0, &mut out);

    // Check if the file is too large
    if estimate_tokens(&out) > max_tokens_per_file {
        return (render_truncated_file_skeleton(symbols), true);
    }

    (out, false)
}

/// Return the display prefix for a given `SymbolKind`.
///
/// Centralises the mapping in one place so that `render_symbols_recursive` and
/// `render_truncated_file_skeleton` stay consistent without duplication.
#[inline]
fn symbol_prefix(kind: SymbolKind) -> &'static str {
    match kind {
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
    }
}

fn render_symbols_recursive(symbols: &[ExtractedSymbol], depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    for sym in symbols {
        let prefix = symbol_prefix(sym.kind);

        let declaration = format!("{}{}", prefix, sym.name);
        let _ = writeln!(out, "{}{} // {}", indent, declaration, sym.semantic_path);

        if !sym.children.is_empty() {
            render_symbols_recursive(&sym.children, depth + 1, out);
        }
    }
}

/// A fallback rendering that preserves top-level symbol names of all kinds with child counts.
fn render_truncated_file_skeleton(symbols: &[ExtractedSymbol]) -> String {
    use std::fmt::Write as _;

    let mut out = String::default();
    for sym in symbols {
        let prefix = symbol_prefix(sym.kind);

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

/// Determine the effective directory traversal depth based on detected languages.
///
/// # For future AI agents adding new language support
///
/// Deep package-as-directory structures are inherent to JVM-family and .NET languages
/// (Java, Kotlin, C#, Scala) that encode package namespace as directory hierarchy.
/// A Java class at `com.example.corp.service.user.UserService` lives 8+ directory
/// levels deep: `src/main/java/com/example/corp/service/user/UserService.java`.
///
/// Pure functional and scripting languages (Go, Rust, Python, TypeScript) use shallow
/// module trees and work fine with depth 5.
///
/// When adding support for a new OOP language with package-to-directory mapping,
/// check if it needs depth > 5 and add it here (e.g., Kotlin, Scala, C#).
fn depth_for_detected_languages(languages: &[crate::language::SupportedLanguage]) -> u32 {
    // Languages requiring deep directory traversal due to package-as-directory conventions.
    // Java Maven/Gradle layout: src/main/java/com/company/pkg/ = 6+ levels before .java file.
    let needs_deep_traversal = languages
        .iter()
        .any(|l| matches!(l, crate::language::SupportedLanguage::Java));

    if needs_deep_traversal {
        10 // Covers com.example.corp.service.user at any reasonable nesting depth
    } else {
        5 // Sufficient for Go, Rust, Python, TypeScript, JavaScript, Vue
    }
}

/// Quick extension-based language detection using a shallow directory scan.
///
/// Unlike the full walk in `generate_skeleton_text`, this does NOT parse files with
/// tree-sitter — it only inspects file extensions. Used to determine language-aware
/// depth before the main walk is configured.
///
/// The scan runs at the requested config depth to avoid missing files at exactly
/// the depth boundary, but stops as soon as all depth-relevant languages are found.
fn detect_languages_shallow(
    abs_target: &Path,
    depth: u32,
) -> Vec<crate::language::SupportedLanguage> {
    use ignore::WalkBuilder;

    let mut builder = WalkBuilder::new(abs_target);
    builder.max_depth(Some(depth as usize));
    builder.require_git(false);
    builder.hidden(true);

    let walker = builder.build();
    let mut detected: Vec<crate::language::SupportedLanguage> = Vec::new();

    for entry in walker.flatten() {
        if entry.path().is_dir() {
            continue;
        }
        if let Some(lang) = crate::language::SupportedLanguage::detect(entry.path()) {
            if !detected.contains(&lang) {
                detected.push(lang);
            }
        }
    }

    detected
}

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub abs_path: PathBuf,
    pub rel_path: PathBuf,
}

/// Generate an AST-based skeleton of a directory.
///
/// # Errors
/// Returns `SurgeonError` if an operation on the AST fails.
#[allow(clippy::items_after_statements)]
pub async fn generate_skeleton_text(
    surgeon: &impl crate::surgeon::Surgeon,
    workspace_root: &Path,
    target_path: &Path,
    config: &SkeletonConfig<'_>,
) -> Result<RepoMapResult, SurgeonError> {
    use ignore::WalkBuilder;

    let abs_target = workspace_root.join(target_path);

    // ── Structure mode: directory tree + manifest files only ──────────
    //
    // Cheapest mode. Does NOT read source files or run tree-sitter.
    // Walks the directory tree, collects directory names and notable
    // manifest/config files, and renders a flat tree listing.
    //
    // Structure mode intentionally ignores `changed_files`,
    // `include_extensions`, and `exclude_extensions` because it operates
    // on directory-level structure, not individual source files. These
    // filters are source-file concerns that don't apply to directory
    // trees and manifests.
    //
    // Structure mode also skips the two-pass depth strategy (language-aware
    // depth expansion) since it only needs the configured depth to list
    // directories.
    if config.detail == SkeletonDetail::Structure {
        let mut builder = WalkBuilder::new(&abs_target);
        builder.max_depth(Some(config.depth as usize));
        builder.require_git(false);
        builder.hidden(true);
        builder.add_custom_ignore_filename(".pathfinderignore");
        let walker = builder.build();
        return generate_structure_skeleton(walker, workspace_root, config);
    }

    // Two-pass depth strategy:
    // 1. Quick extension-only pre-scan at the requested depth to detect languages.
    //    This is O(file count) with no tree-sitter parsing — fast and cheap.
    // 2. Compute language-aware effective depth (Java/JVM need depth 10 for
    //    package-as-directory layouts like src/main/java/com/example/pkg/).
    // 3. Build the full walker with the effective depth.
    let pre_scan_languages = detect_languages_shallow(&abs_target, config.depth);
    let language_aware_depth = depth_for_detected_languages(&pre_scan_languages);
    let effective_depth = config.depth.max(language_aware_depth);

    let mut builder = WalkBuilder::new(&abs_target);
    builder.max_depth(Some(effective_depth as usize));
    builder.require_git(false);
    builder.hidden(true);
    builder.add_custom_ignore_filename(".pathfinderignore");

    let walker = builder.build();

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
        });
    }

    let files_in_scope = file_entries.len();

    // ── Files mode: file listing without symbol extraction ───────────
    //
    // Walks all source files, computes version hashes and detects the
    // tech stack, but does NOT run tree-sitter symbol extraction.
    // Output shows file paths only, no symbol bodies.
    if config.detail == SkeletonDetail::Files {
        return generate_files_skeleton(
            file_entries
                .iter()
                .map(|e| (&e.abs_path, &e.rel_path))
                .collect(),
            &tech_stack,
            files_in_scope,
            config,
        )
        .await;
    }

    // ── Symbols mode: full AST symbol hierarchy (default) ────────────
    generate_symbols_skeleton(
        surgeon,
        workspace_root,
        file_entries,
        config.visibility,
        config.include_tests,
        config.max_tokens,
        config.max_tokens_per_file,
        &tech_stack,
        files_in_scope,
    )
    .await
}

/// Well-known manifest/config files that `Structure` mode includes in its
/// directory tree listing. These are files that help agents understand the
/// project layout without reading source code.
const MANIFEST_FILES: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "setup.py",
    "requirements.txt",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "Makefile",
    "Dockerfile",
    "docker-compose.yml",
    "docker-compose.yaml",
    "tsconfig.json",
    "jsconfig.json",
    ".env.example",
    "Gemfile",
    "Pipfile",
    "flake.nix",
    "CMakeLists.txt",
];

/// Returns `true` if the filename is a well-known manifest or config file
/// that should be included in `Structure` mode output.
fn is_manifest_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| MANIFEST_FILES.contains(&name))
}

/// Generate a directory-tree-only skeleton (Structure mode).
///
/// Walks the directory tree collecting directory names and manifest files.
/// Does NOT read source file contents or run tree-sitter AST extraction.
/// This is the cheapest explore mode.
#[allow(clippy::unnecessary_wraps)] // Must match generate_skeleton_text return type for early return
fn generate_structure_skeleton(
    walker: ignore::Walk,
    workspace_root: &Path,
    config: &SkeletonConfig<'_>,
) -> Result<RepoMapResult, SurgeonError> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut manifests: Vec<PathBuf> = Vec::new();
    let mut tech_stack: Vec<crate::language::SupportedLanguage> = Vec::default();

    for result in walker {
        let Ok(entry) = result else { continue };
        let path = entry.path();
        let rel_path = path.strip_prefix(workspace_root).unwrap_or(path);

        if path.is_dir() {
            // Skip the root entry (empty relative path)
            if !rel_path.as_os_str().is_empty() {
                dirs.push(rel_path.to_path_buf());
            }
            continue;
        }

        // Detect tech stack from file extensions (cheap — no file reads)
        if let Some(lang) = crate::language::SupportedLanguage::detect(path) {
            if !tech_stack.contains(&lang) {
                tech_stack.push(lang);
            }
        }

        // Collect manifest/config files
        if is_manifest_file(path) {
            manifests.push(rel_path.to_path_buf());
        }
    }

    dirs.sort();
    manifests.sort();

    let mut skeleton_out = String::new();
    let mut current_tokens: u32 = 0;

    // Render directories
    for dir in &dirs {
        let line = format!("{}/\n", dir.display());
        let tokens = estimate_tokens(&line);
        if current_tokens + tokens > config.max_tokens {
            break;
        }
        skeleton_out.push_str(&line);
        current_tokens += tokens;
    }

    // Render manifest files under a separator
    if !manifests.is_empty() {
        let header = "\n── Notable files ──\n";
        let header_tokens = estimate_tokens(header);
        if current_tokens + header_tokens <= config.max_tokens {
            skeleton_out.push_str(header);
            current_tokens += header_tokens;

            for manifest in &manifests {
                let line = format!("{}\n", manifest.display());
                let tokens = estimate_tokens(&line);
                if current_tokens + tokens > config.max_tokens {
                    break;
                }
                skeleton_out.push_str(&line);
                current_tokens += tokens;
            }
        }
    }

    Ok(RepoMapResult {
        skeleton: skeleton_out.trim().to_string(),
        tech_stack: tech_stack.iter().map(|l| l.as_str().to_owned()).collect(),
        files_scanned: 0,
        files_truncated: 0,
        truncated_paths: vec![],
        // Count manifests only (not dirs) for consistency with Files/Symbols
        // which count source files.
        files_in_scope: manifests.len(),
        coverage_percent: 100,
        version_hashes: HashMap::default(),
    })
}

/// Generate a files-only skeleton (Files mode).
///
/// Lists all source files with version hashes and tech stack detection,
/// but does NOT run tree-sitter symbol extraction. Output shows file paths
/// only with no symbol bodies — significantly cheaper than Symbols mode.
#[allow(clippy::items_after_statements)]
async fn generate_files_skeleton(
    file_entries: Vec<(&PathBuf, &PathBuf)>, // (abs_path, rel_path)
    tech_stack: &[crate::language::SupportedLanguage],
    files_in_scope: usize,
    config: &SkeletonConfig<'_>,
) -> Result<RepoMapResult, SurgeonError> {
    use futures::stream::{self, StreamExt};
    use pathfinder_common::types::VersionHash;

    // Sort entries for deterministic output
    let mut entries = file_entries;
    entries.sort_by(|a, b| a.1.cmp(b.1));

    // Concurrently compute version hashes (same concurrency as Symbols mode).
    // This avoids the sequential I/O bottleneck for large repos.
    const HASH_CONCURRENCY: usize = 32;

    // Collect owned paths for the concurrent hash tasks (closures need 'static).
    let hash_inputs: Vec<(PathBuf, String)> = entries
        .iter()
        .map(|(abs_path, rel_path)| ((*abs_path).clone(), rel_path.display().to_string()))
        .collect();

    let hash_stream = stream::iter(hash_inputs).map(|(abs_path, rel_str)| async move {
        let hash = match tokio::fs::read(&abs_path).await {
            Ok(source) => Some(VersionHash::compute(&source).short().to_owned()),
            Err(_) => None,
        };
        (rel_str, hash)
    });

    let hash_results: Vec<(String, Option<String>)> = hash_stream
        .buffer_unordered(HASH_CONCURRENCY)
        .collect()
        .await;

    let mut version_hashes = HashMap::default();
    for (path, hash) in &hash_results {
        if let Some(h) = hash {
            version_hashes.insert(path.clone(), h.clone());
        }
    }

    // Render file listing (sequential — deterministic order from sorted entries)
    let mut skeleton_out = String::new();
    let mut current_tokens: u32 = 0;
    let mut files_rendered: usize = 0;
    let mut files_truncated: usize = 0;
    let mut truncated_paths: Vec<String> = Vec::new();

    for (_abs_path, rel_path) in &entries {
        let line = format!("{}\n", rel_path.display());
        let tokens = estimate_tokens(&line);

        if current_tokens + tokens > config.max_tokens {
            files_truncated += 1;
            truncated_paths.push(rel_path.display().to_string());
            continue;
        }

        skeleton_out.push_str(&line);
        current_tokens += tokens;
        files_rendered += 1;
    }

    Ok(RepoMapResult {
        skeleton: skeleton_out.trim().to_string(),
        tech_stack: tech_stack.iter().map(|l| l.as_str().to_owned()).collect(),
        files_scanned: files_rendered,
        files_truncated,
        truncated_paths,
        files_in_scope,
        coverage_percent: 100,
        version_hashes,
    })
}

#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "Extracted sequential skeleton generation logic"
)]
async fn generate_symbols_skeleton(
    surgeon: &impl crate::surgeon::Surgeon,
    workspace_root: &Path,
    file_entries: Vec<FileEntry>,
    visibility: &str,
    include_tests: bool,
    max_tokens: u32,
    max_tokens_per_file: u32,
    tech_stack: &[crate::language::SupportedLanguage],
    files_in_scope: usize,
) -> Result<RepoMapResult, SurgeonError> {
    use futures::stream::{self, StreamExt};
    use pathfinder_common::types::VersionHash;

    const READ_CONCURRENCY: usize = 32;

    struct ProcessedFile {
        rel_path: PathBuf,
        skeleton: String,
        skeleton_tokens: u32,
    }

    struct FileProcessOutput {
        processed: Option<ProcessedFile>,
        version_entry: Option<(String, String)>,
        has_symbols: bool,
        truncated: bool,
    }

    let visibility = visibility.to_string();
    let workspace_root = workspace_root.to_path_buf();

    let process_stream = stream::iter(file_entries).map(|entry| {
        let workspace_root = workspace_root.clone();
        let visibility = visibility.clone();

        async move {
            let (read_result, meta_result) = tokio::join!(
                tokio::fs::read(&entry.abs_path),
                tokio::fs::metadata(&entry.abs_path)
            );
            let mtime = meta_result
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

            let source = match read_result {
                Ok(bytes) => bytes,
                Err(e) => {
                    tracing::warn!(
                        path = %entry.rel_path.display(),
                        error = %e,
                        "get_repo_map: skipping file (read failed)"
                    );
                    return FileProcessOutput {
                        processed: None,
                        version_entry: None,
                        has_symbols: false,
                        truncated: false,
                    };
                }
            };

            let hash = VersionHash::compute(&source);
            let path_str = entry.rel_path.display().to_string();
            let hash_short = hash.short().to_owned();

            let content_arc: std::sync::Arc<[u8]> = std::sync::Arc::from(source);

            let raw_symbols = match surgeon
                .extract_symbols_preloaded(&workspace_root, &entry.rel_path, content_arc, mtime)
                .await
            {
                Ok(syms) => syms,
                Err(e) => {
                    tracing::debug!(
                        path = %entry.rel_path.display(),
                        error = %e,
                        "get_repo_map: skipping file (symbol extraction failed)"
                    );
                    return FileProcessOutput {
                        processed: None,
                        version_entry: Some((path_str, hash_short)),
                        has_symbols: false,
                        truncated: false,
                    };
                }
            };

            let symbols = filter_by_visibility(raw_symbols, &visibility, include_tests);

            if symbols.is_empty() {
                return FileProcessOutput {
                    processed: None,
                    version_entry: Some((path_str, hash_short)),
                    has_symbols: false,
                    truncated: false,
                };
            }

            let (file_skeleton, truncated) = render_file_skeleton(&symbols, max_tokens_per_file);
            let file_skeleton_tokens = estimate_tokens(&file_skeleton);

            FileProcessOutput {
                processed: Some(ProcessedFile {
                    rel_path: entry.rel_path,
                    skeleton: file_skeleton,
                    skeleton_tokens: file_skeleton_tokens,
                }),
                version_entry: Some((path_str, hash_short)),
                has_symbols: true,
                truncated,
            }
        }
    });

    let process_results: Vec<FileProcessOutput> = process_stream
        .buffer_unordered(READ_CONCURRENCY)
        .collect()
        .await;

    let mut processed: Vec<ProcessedFile> = Vec::new();
    let mut files_with_symbols = 0;
    let mut version_hashes = HashMap::default();
    let mut per_file_truncated_paths: Vec<String> = Vec::new();

    for output in process_results {
        if let Some((path, hash)) = output.version_entry {
            version_hashes.insert(path, hash);
        }
        if output.has_symbols {
            files_with_symbols += 1;
        }
        if output.truncated {
            if let Some(ref pf) = output.processed {
                per_file_truncated_paths.push(pf.rel_path.display().to_string());
            }
        }
        if let Some(pf) = output.processed {
            processed.push(pf);
        }
    }

    processed.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    let mut skeleton_out = String::default();
    let mut current_tokens: u32 = 0;
    let mut files_rendered: usize = 0;
    let mut files_truncated: usize = 0;
    let mut truncated_paths: Vec<String> = Vec::new();

    for pf in &processed {
        // Estimate the header cost before the budget gate so the gate accurately
        // reflects the total cost of rendering this file (header + skeleton).
        // The header format is "\nFile: {path}\n{sep}\n" — compute the real cost here
        // to avoid silently exceeding max_tokens after admission.
        let path_header = format!(
            "\nFile: {}\n{}\n",
            pf.rel_path.display(),
            "=".repeat(pf.rel_path.display().to_string().len() + 6)
        );
        let header_tokens = estimate_tokens(&path_header);
        let total_cost = pf.skeleton_tokens.saturating_add(header_tokens);

        if current_tokens + total_cost > max_tokens {
            if current_tokens + 50 <= max_tokens {
                let _ = writeln!(
                    skeleton_out,
                    "\n// [... Omitted {} due to token budget]",
                    pf.rel_path.display()
                );
                current_tokens += 50;
            }
            files_truncated += 1;
            truncated_paths.push(pf.rel_path.display().to_string());
            continue;
        }

        current_tokens += total_cost;
        files_rendered += 1;
        skeleton_out.push_str(&path_header);
        skeleton_out.push_str(&pf.skeleton);
    }

    truncated_paths.extend(per_file_truncated_paths);
    truncated_paths.sort();
    truncated_paths.dedup();

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
        tech_stack: tech_stack.iter().map(|l| l.as_str().to_owned()).collect(),
        files_scanned: files_rendered,
        files_truncated,
        truncated_paths,
        files_in_scope,
        coverage_percent,
        version_hashes,
    })
}


#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "repo_map_test.rs"]
mod tests;
