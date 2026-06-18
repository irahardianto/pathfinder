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

    #[tokio::test]
    async fn test_generate_symbols_skeleton_respects_token_budget() {
        let surgeon = MockSurgeon::default();
        surgeon
            .extract_symbols_results
            .lock()
            .expect("lock success")
            .push(Ok(vec![
                make_sym("foo", SymbolKind::Function),
                make_sym("bar", SymbolKind::Function),
            ]));

        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("main.rs");
        std::fs::write(&file_path, "fn foo() {} fn bar() {}").expect("write file");

        let files = vec![FileEntry {
            abs_path: file_path.clone(),
            rel_path: PathBuf::from("main.rs"),
        }];

        let res = generate_symbols_skeleton(
            &surgeon,
            dir.path(),
            files.clone(),
            "all",
            true,
            1000,
            2000,
            &[crate::language::SupportedLanguage::Rust],
            1,
        )
        .await
        .expect("generate_symbols_skeleton");
        assert!(res.skeleton.contains("func foo"));
        assert!(res.skeleton.contains("func bar"));
        assert_eq!(res.files_scanned, 1);
        assert_eq!(res.files_truncated, 0);

        surgeon
            .extract_symbols_results
            .lock()
            .expect("lock success")
            .push(Ok(vec![
                make_sym("foo", SymbolKind::Function),
                make_sym("bar", SymbolKind::Function),
            ]));
        let res = generate_symbols_skeleton(
            &surgeon,
            dir.path(),
            files,
            "all",
            true,
            5,
            2000,
            &[crate::language::SupportedLanguage::Rust],
            1,
        )
        .await
        .expect("generate_symbols_skeleton");
        assert_eq!(res.files_scanned, 0);
        assert_eq!(res.files_truncated, 1);
        assert!(res.skeleton.is_empty() || res.skeleton.contains("Omitted"));
    }

    #[tokio::test]
    async fn test_generate_symbols_skeleton_visibility_filter() {
        let surgeon = MockSurgeon::default();
        let mut sym_pub = make_sym("foo_pub", SymbolKind::Function);
        sym_pub.access_level = crate::surgeon::AccessLevel::Public;
        let mut sym_priv = make_sym("foo_priv", SymbolKind::Function);
        sym_priv.access_level = crate::surgeon::AccessLevel::Private;

        surgeon
            .extract_symbols_results
            .lock()
            .expect("lock success")
            .push(Ok(vec![sym_pub, sym_priv]));

        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("main.rs");
        std::fs::write(&file_path, "pub fn foo_pub() {} fn foo_priv() {}").expect("write file");

        let files = vec![FileEntry {
            abs_path: file_path,
            rel_path: PathBuf::from("main.rs"),
        }];

        let res = generate_symbols_skeleton(
            &surgeon,
            dir.path(),
            files,
            "public",
            false,
            1000,
            2000,
            &[crate::language::SupportedLanguage::Rust],
            1,
        )
        .await
        .expect("generate_symbols_skeleton");
        assert!(res.skeleton.contains("func foo_pub"));
        assert!(!res.skeleton.contains("func foo_priv"));
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

        let (output, truncated) = render_file_skeleton(&symbols, MAX_TOKENS_PER_FILE);
        assert!(!truncated, "should not truncate simple symbol tree");
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
        let (output, truncated) = render_file_skeleton(&symbols, MAX_TOKENS_PER_FILE);
        assert!(truncated, "should truncate massive symbol tree");
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
    async fn test_generate_skeleton_text_depth_3_reaches_rust_files_at_depth_4() {
        // Previously, depth=3 missed Rust files at depth 4 (crates/my-crate/src/lib.rs).
        // With language-aware depth, the effective depth auto-expands to 5 for Rust
        // projects, so files at depth 4 are now correctly found.
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
        // Configure extract_symbols to return an empty list — the file IS reached now.
        surgeon
            .extract_symbols_results
            .lock()
            .expect("mutex")
            .push(Ok(vec![]));

        let config = SkeletonConfig::new(50_000, 3, "all", 2_000); // Requested depth=3
        let result =
            generate_skeleton_text(&*surgeon, ws_dir.path(), std::path::Path::new("."), &config)
                .await
                .expect("skeleton generation succeeds");

        // effective_depth = max(3, 5) = 5 → reaches depth-4 file
        assert_eq!(
            result.files_in_scope, 1,
            "depth=3 for Rust projects should reach files at depth 4 via language-aware depth expansion"
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
                truncated_paths: vec![],
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
                truncated_paths: vec![],
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

    #[tokio::test]
    async fn test_truncated_paths_collected() {
        use crate::mock::MockSurgeon;
        use crate::surgeon::{ExtractedSymbol, SymbolKind};
        use std::sync::Arc;

        let dir = tempfile::tempdir().expect("create tempdir");
        let ws_root = dir.path();

        let mock = MockSurgeon::new();
        for name in &["a.rs", "b.rs", "c.rs", "d.rs"] {
            let path = ws_root.join(name);
            std::fs::write(&path, "fn main() {}").expect("write test file");
            mock.extract_symbols_results
                .lock()
                .expect("mutex")
                .push(Ok(vec![ExtractedSymbol {
                    name: "main".to_string(),
                    semantic_path: "main".to_string(),
                    kind: SymbolKind::Function,
                    byte_range: 0..13,
                    start_line: 0,
                    end_line: 0,
                    name_column: 0,
                    access_level: crate::surgeon::AccessLevel::Public,
                    children: vec![],
                }]));
        }

        let surgeon = Arc::new(mock);

        let config = SkeletonConfig::new(20, 5, "all", 50).with_include_tests(true);

        let result = generate_skeleton_text(&*surgeon, ws_root, std::path::Path::new("."), &config)
            .await
            .expect("generate skeleton text");

        assert!(
            result.files_truncated > 0,
            "at least one file should be truncated with very low max_tokens"
        );
        assert_eq!(
            result.truncated_paths.len(),
            result.files_truncated,
            "truncated_paths length should match files_truncated count"
        );
        for path_str in &result.truncated_paths {
            assert!(
                std::path::Path::new(path_str)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("rs")),
                "truncated path should be an .rs file: {path_str}"
            );
        }
    }

    #[tokio::test]
    async fn test_per_file_truncated_paths_collected() {
        use crate::mock::MockSurgeon;
        use crate::surgeon::{ExtractedSymbol, SymbolKind};
        use std::sync::Arc;

        let dir = tempfile::tempdir().expect("create tempdir");
        let ws_root = dir.path();

        let mock = MockSurgeon::new();

        let mut massive_methods = Vec::default();
        for i in 0..200 {
            massive_methods.push(ExtractedSymbol {
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

        let path = ws_root.join("large.rs");
        std::fs::write(&path, "struct MyGiganticClass {}").expect("write test file");
        mock.extract_symbols_results
            .lock()
            .expect("mutex")
            .push(Ok(vec![ExtractedSymbol {
                name: "MyGiganticClass".to_string(),
                semantic_path: "MyGiganticClass".to_string(),
                kind: SymbolKind::Struct,
                byte_range: 0..100,
                start_line: 0,
                end_line: 100,
                name_column: 0,
                access_level: crate::surgeon::AccessLevel::Public,
                children: massive_methods,
            }]));

        let surgeon = Arc::new(mock);

        let config = SkeletonConfig::new(5000, 500, "all", 50).with_include_tests(true);

        let result = generate_skeleton_text(&*surgeon, ws_root, std::path::Path::new("."), &config)
            .await
            .expect("generate skeleton text");

        assert!(
            result.skeleton.contains("[TRUNCATED DUE TO SIZE]"),
            "skeleton should contain truncation marker for per-file truncation"
        );
        assert!(
            result
                .truncated_paths
                .iter()
                .any(|p| p.ends_with("large.rs")),
            "truncated_paths should contain large.rs (per-file truncated)"
        );
    }

    // ---------------------------------------------------------------
    // BATCH-03a: Configuration variant coverage
    // ---------------------------------------------------------------

    /// BATCH-03a: visibility="all" keeps private symbols (verifies the non-"public" fast path).
    #[test]
    fn test_filter_all_keeps_private_symbols() {
        let mut priv_sym = make_sym("_internal", SymbolKind::Function);
        priv_sym.access_level = crate::surgeon::AccessLevel::Private;
        let syms = vec![priv_sym, make_sym("Public", SymbolKind::Function)];
        // "all" must return all symbols without filtering
        let filtered = filter_by_visibility(syms, "all", false);
        assert_eq!(filtered.len(), 2, "visibility=all keeps everything");
    }

    /// BATCH-03a: visibility="all" with `include_tests=true` keeps everything too.
    #[test]
    fn test_filter_all_visibility_with_include_tests() {
        let syms = vec![
            make_sym("test_foo", SymbolKind::Function),
            make_sym("_hidden", SymbolKind::Function),
        ];
        let filtered = filter_by_visibility(syms, "all", true);
        assert_eq!(filtered.len(), 2);
    }

    /// BATCH-03a: deeply nested symbol hierarchy renders with correct indentation.
    #[test]
    fn test_render_deeply_nested_symbols() {
        let leaf = ExtractedSymbol {
            name: "deep_method".to_string(),
            semantic_path: "Outer.Inner.deep_method".to_string(),
            kind: SymbolKind::Method,
            byte_range: 0..1,
            start_line: 0,
            end_line: 1,
            name_column: 0,
            access_level: crate::surgeon::AccessLevel::Public,
            children: vec![],
        };
        let inner = ExtractedSymbol {
            name: "Inner".to_string(),
            semantic_path: "Outer.Inner".to_string(),
            kind: SymbolKind::Struct,
            byte_range: 0..10,
            start_line: 0,
            end_line: 10,
            name_column: 0,
            access_level: crate::surgeon::AccessLevel::Public,
            children: vec![leaf],
        };
        let outer = ExtractedSymbol {
            name: "Outer".to_string(),
            semantic_path: "Outer".to_string(),
            kind: SymbolKind::Module,
            byte_range: 0..100,
            start_line: 0,
            end_line: 100,
            name_column: 0,
            access_level: crate::surgeon::AccessLevel::Public,
            children: vec![inner],
        };

        let mut out = String::default();
        render_symbols_recursive(&[outer], 0, &mut out);

        assert!(out.contains("mod Outer // Outer\n"));
        assert!(out.contains("  struct Inner // Outer.Inner\n"));
        assert!(out.contains("    method deep_method // Outer.Inner.deep_method\n"));
    }

    /// BATCH-03a: duplicate symbol names across modules are both rendered.
    #[test]
    fn test_render_duplicate_symbol_names_across_modules() {
        let mod_a = ExtractedSymbol {
            name: "foo".to_string(),
            semantic_path: "module_a::foo".to_string(),
            kind: SymbolKind::Function,
            byte_range: 0..10,
            start_line: 0,
            end_line: 5,
            name_column: 0,
            access_level: crate::surgeon::AccessLevel::Public,
            children: vec![],
        };
        let mod_b = ExtractedSymbol {
            name: "foo".to_string(),
            semantic_path: "module_b::foo".to_string(),
            kind: SymbolKind::Function,
            byte_range: 10..20,
            start_line: 6,
            end_line: 10,
            name_column: 0,
            access_level: crate::surgeon::AccessLevel::Public,
            children: vec![],
        };
        let (out, truncated) = render_file_skeleton(&[mod_a, mod_b], MAX_TOKENS_PER_FILE);
        assert!(!truncated);
        assert!(out.contains("module_a::foo"), "first foo must appear");
        assert!(out.contains("module_b::foo"), "second foo must appear");
    }

    /// BATCH-03a: `generate_skeleton_text` with very low `max_tokens` skips the second file.
    ///
    /// The `max_tokens` budget is set to 5 — small enough that even a single-function file
    /// cannot fit after the first file is rendered. This exercises the token-budget
    /// truncation branch at line 531-543 of `generate_skeleton_text`.
    #[tokio::test]
    async fn test_generate_skeleton_token_budget_omission_comment() {
        use crate::mock::MockSurgeon;
        use std::sync::Arc;

        let dir = tempfile::tempdir().expect("create tempdir");
        let ws_root = dir.path();

        // Two files — only the first can fit in a tiny token budget.
        for name in &["a.rs", "b.rs"] {
            std::fs::write(ws_root.join(name), "fn main() {}").expect("write");
        }

        let mock = MockSurgeon::new();
        // Push symbols for both files; after the first is rendered the budget is exhausted.
        for _ in 0..2 {
            mock.extract_symbols_results
                .lock()
                .expect("mutex")
                .push(Ok(vec![ExtractedSymbol {
                    name: "main".to_string(),
                    semantic_path: "main".to_string(),
                    kind: SymbolKind::Function,
                    byte_range: 0..13,
                    start_line: 0,
                    end_line: 0,
                    name_column: 0,
                    access_level: crate::surgeon::AccessLevel::Public,
                    children: vec![],
                }]));
        }

        let surgeon = Arc::new(mock);
        // max_tokens=5 is far too small for two files; the second file will be truncated.
        // Each file's header alone is ~"\nFile: a.rs\n=========\n" ≈ 15 tokens,
        // so after a.rs is processed the budget is fully consumed and b.rs is skipped.
        let config = SkeletonConfig::new(5, 5, "all", 2_000);
        let result = generate_skeleton_text(&*surgeon, ws_root, std::path::Path::new("."), &config)
            .await
            .expect("generate skeleton");

        // With max_tokens=5, the total skeleton immediately exceeds the budget,
        // so files_truncated should be > 0 (the second file is skipped).
        assert!(
            result.files_truncated > 0 || result.files_scanned <= 1,
            "with max_tokens=5, at most 1 file should render; files_truncated={}, files_scanned={}",
            result.files_truncated,
            result.files_scanned
        );
        assert!(
            result.truncated_paths.len() == result.files_truncated,
            "truncated_paths length should match files_truncated: paths={}, truncated={}",
            result.truncated_paths.len(),
            result.files_truncated
        );
    }

    // ---------------------------------------------------------------
    // BATCH-03b: AST edge-case rendering coverage
    // ---------------------------------------------------------------

    /// BATCH-03b: Impl block is rendered with "impl" prefix.
    #[test]
    fn test_render_impl_symbol_kind() {
        let sym = make_sym("MyStruct", SymbolKind::Impl);
        let (out, truncated) = render_file_skeleton(&[sym], MAX_TOKENS_PER_FILE);
        assert!(!truncated);
        assert!(out.contains("impl MyStruct // MyStruct\n"));
    }

    /// BATCH-03b: Constant kind is rendered with "const" prefix.
    #[test]
    fn test_render_constant_symbol_kind() {
        let sym = make_sym("MAX_SIZE", SymbolKind::Constant);
        let (out, truncated) = render_file_skeleton(&[sym], MAX_TOKENS_PER_FILE);
        assert!(!truncated);
        assert!(out.contains("const MAX_SIZE // MAX_SIZE\n"));
    }

    /// BATCH-03b: Interface kind is rendered with "interface" prefix.
    #[test]
    fn test_render_interface_symbol_kind() {
        let sym = make_sym("Runnable", SymbolKind::Interface);
        let (out, truncated) = render_file_skeleton(&[sym], MAX_TOKENS_PER_FILE);
        assert!(!truncated);
        assert!(out.contains("interface Runnable // Runnable\n"));
    }

    /// BATCH-03b: Enum kind is rendered with "enum" prefix.
    #[test]
    fn test_render_enum_symbol_kind() {
        let sym = make_sym("Color", SymbolKind::Enum);
        let (out, truncated) = render_file_skeleton(&[sym], MAX_TOKENS_PER_FILE);
        assert!(!truncated);
        assert!(out.contains("enum Color // Color\n"));
    }

    /// BATCH-03b: Test kind is rendered with "test" prefix.
    #[test]
    fn test_render_test_symbol_kind() {
        let sym = make_sym("test_something", SymbolKind::Test);
        let (out, truncated) = render_file_skeleton(&[sym], MAX_TOKENS_PER_FILE);
        assert!(!truncated);
        assert!(out.contains("test test_something // test_something\n"));
    }

    /// BATCH-03b: Vue Zone kind is rendered with "zone" prefix.
    #[test]
    fn test_render_zone_symbol_kind() {
        let sym = make_sym("template", SymbolKind::Zone);
        let (out, truncated) = render_file_skeleton(&[sym], MAX_TOKENS_PER_FILE);
        assert!(!truncated);
        assert!(out.contains("zone template // template\n"));
    }

    /// BATCH-03b: Component kind is rendered with "component" prefix.
    #[test]
    fn test_render_component_symbol_kind() {
        let sym = make_sym("MyButton", SymbolKind::Component);
        let (out, truncated) = render_file_skeleton(&[sym], MAX_TOKENS_PER_FILE);
        assert!(!truncated);
        assert!(out.contains("component MyButton // MyButton\n"));
    }

    /// BATCH-03b: `HtmlElement` kind is rendered with "element" prefix.
    #[test]
    fn test_render_html_element_symbol_kind() {
        let sym = make_sym("div", SymbolKind::HtmlElement);
        let (out, truncated) = render_file_skeleton(&[sym], MAX_TOKENS_PER_FILE);
        assert!(!truncated);
        assert!(out.contains("element div // div\n"));
    }

    /// BATCH-03b: `CssSelector` kind is rendered with "selector" prefix.
    #[test]
    fn test_render_css_selector_symbol_kind() {
        let sym = make_sym(".primary", SymbolKind::CssSelector);
        let (out, truncated) = render_file_skeleton(&[sym], MAX_TOKENS_PER_FILE);
        assert!(!truncated);
        assert!(out.contains("selector .primary // .primary\n"));
    }

    /// BATCH-03b: `CssAtRule` kind is rendered with "at-rule" prefix.
    #[test]
    fn test_render_css_at_rule_symbol_kind() {
        let sym = make_sym("@media", SymbolKind::CssAtRule);
        let (out, truncated) = render_file_skeleton(&[sym], MAX_TOKENS_PER_FILE);
        assert!(!truncated);
        assert!(out.contains("at-rule @media // @media\n"));
    }

    /// BATCH-03b: Truncated skeleton for an Enum with functions reports func count.
    #[test]
    fn test_render_truncated_skeleton_with_functions_in_enum() {
        let mut fns = Vec::default();
        for i in 0..200 {
            fns.push(ExtractedSymbol {
                name: format!("fn_variant_{i}"),
                semantic_path: format!("BigEnum.fn_variant_{i}"),
                kind: SymbolKind::Function,
                byte_range: 0..0,
                start_line: 0,
                end_line: 0,
                name_column: 0,
                access_level: crate::surgeon::AccessLevel::Public,
                children: vec![],
            });
        }
        let sym = ExtractedSymbol {
            name: "BigEnum".to_string(),
            semantic_path: "BigEnum".to_string(),
            kind: SymbolKind::Enum,
            byte_range: 0..0,
            start_line: 0,
            end_line: 0,
            name_column: 0,
            access_level: crate::surgeon::AccessLevel::Public,
            children: fns,
        };
        let (out, truncated) = render_file_skeleton(&[sym], MAX_TOKENS_PER_FILE);
        assert!(truncated, "large enum with 200 functions should truncate");
        assert!(out.contains("[TRUNCATED DUE TO SIZE]"));
        assert!(out.contains("enum BigEnum"));
        assert!(out.contains("200 functions omitted"));
    }

    /// BATCH-03b: Truncated skeleton for an Impl block with constants reports const count.
    #[test]
    fn test_render_truncated_skeleton_with_constants_in_impl() {
        let mut consts = Vec::default();
        for i in 0..200 {
            consts.push(ExtractedSymbol {
                name: format!("CONST_{i}"),
                semantic_path: format!("BigImpl.CONST_{i}"),
                kind: SymbolKind::Constant,
                byte_range: 0..0,
                start_line: 0,
                end_line: 0,
                name_column: 0,
                access_level: crate::surgeon::AccessLevel::Public,
                children: vec![],
            });
        }
        let sym = ExtractedSymbol {
            name: "BigImpl".to_string(),
            semantic_path: "BigImpl".to_string(),
            kind: SymbolKind::Impl,
            byte_range: 0..0,
            start_line: 0,
            end_line: 0,
            name_column: 0,
            access_level: crate::surgeon::AccessLevel::Public,
            children: consts,
        };
        // Use a low per-file cap (100 tokens ≈ 400 chars) — well under the ~7000 chars
        // produced by 200 constants — to reliably force truncation and exercise the
        // render_truncated_file_skeleton constants-count branch.
        let (out, truncated) = render_file_skeleton(&[sym], 100);
        assert!(
            truncated,
            "impl with 200 constants should truncate with low max_tokens_per_file"
        );
        assert!(out.contains("[TRUNCATED DUE TO SIZE]"));
        assert!(out.contains("impl BigImpl"));
        assert!(out.contains("200 constants omitted"));
    }

    /// BATCH-03b: Truncated skeleton with empty symbols hits the NO SYMBOLS path.
    #[test]
    fn test_render_truncated_skeleton_no_symbols() {
        // render_truncated_file_skeleton with an empty slice returns the special marker.
        let result = render_truncated_file_skeleton(&[]);
        assert_eq!(result, "// [TRUNCATED - NO SYMBOLS EXTRACTED]");
    }

    /// BATCH-03b: `render_truncated_file_skeleton` with a non-container kind (Function)
    /// does NOT emit child-count lines (covers the "not in the matches! set" branch).
    #[test]
    fn test_render_truncated_skeleton_function_no_child_count() {
        let sym = ExtractedSymbol {
            name: "standalone_fn".to_string(),
            semantic_path: "standalone_fn".to_string(),
            kind: SymbolKind::Function,
            byte_range: 0..0,
            start_line: 0,
            end_line: 0,
            name_column: 0,
            access_level: crate::surgeon::AccessLevel::Public,
            children: vec![make_sym("nested", SymbolKind::Method)],
        };
        // Force render_truncated_file_skeleton by calling directly.
        let result = render_truncated_file_skeleton(&[sym]);
        // Functions are NOT in the container kinds set — no omission line.
        assert!(result.contains("func standalone_fn"), "should show fn name");
        assert!(
            !result.contains("omitted"),
            "functions should not emit child omission count"
        );
    }

    /// BATCH-03b: Low `max_tokens_per_file` with a single struct triggers per-file truncation.
    #[test]
    fn test_render_file_skeleton_low_max_tokens() {
        // max_tokens_per_file=1 forces truncation even for small symbols.
        let sym = make_sym("MyStruct", SymbolKind::Struct);
        let (out, truncated) = render_file_skeleton(&[sym], 1);
        assert!(
            truncated,
            "must truncate when max_tokens_per_file is very low"
        );
        assert!(
            out.contains("struct MyStruct"),
            "truncated skeleton must still show symbol name"
        );
    }

    /// BATCH-03b: `is_test_symbol` handles `SymbolKind::Test` directly.
    #[test]
    fn test_is_test_symbol_test_kind() {
        let sym = make_sym("any_name", SymbolKind::Test);
        // filter_by_visibility with include_tests=true on a private Test symbol keeps it.
        let mut private_test = sym;
        private_test.access_level = crate::surgeon::AccessLevel::Private;
        let filtered = filter_by_visibility(vec![private_test], "public", true);
        assert_eq!(
            filtered.len(),
            1,
            "SymbolKind::Test must be kept when include_tests=true"
        );
    }

    /// BATCH-03b: `is_test_symbol` handles it_ prefix functions.
    #[test]
    fn test_is_test_symbol_it_prefix() {
        let mut it_fn = make_sym("it_does_something", SymbolKind::Function);
        it_fn.access_level = crate::surgeon::AccessLevel::Private;
        let filtered = filter_by_visibility(vec![it_fn], "public", true);
        assert_eq!(
            filtered.len(),
            1,
            "it_ prefix function kept with include_tests=true"
        );
    }

    // ── Detail level tests ─────────────────────────────────────────────

    /// Symbol keywords that must NOT appear in Structure/Files mode output.
    const SYMBOL_KEYWORDS: &[&str] = &[
        "func ",
        "struct ",
        "class ",
        "method ",
        "impl ",
        "const ",
        "interface ",
        "enum ",
        "mod ",
        "test ",
        "zone ",
        "component ",
        "element ",
        "selector ",
    ];

    /// Helper: create a temp dir with a Rust source file and a Cargo.toml.
    fn setup_detail_test_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("create tempdir");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).expect("create src/");
        std::fs::write(
            src.join("main.rs"),
            "pub fn hello() { println!(\"hi\"); }\npub struct Foo;\n",
        )
        .expect("write main.rs");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"test\"\n",
        )
        .expect("write Cargo.toml");
        dir
    }

    /// Structure mode: output contains directories and manifest files,
    /// but NO symbol keywords (func, struct, class, etc.).
    #[tokio::test]
    async fn test_skeleton_detail_structure_no_symbols() {
        let dir = setup_detail_test_dir();
        let surgeon = crate::TreeSitterSurgeon::new(100);

        let config =
            SkeletonConfig::new(4_000, 3, "all", 2_000).with_detail(SkeletonDetail::Structure);

        let result = generate_skeleton_text(&surgeon, dir.path(), Path::new(""), &config)
            .await
            .expect("structure mode should succeed");

        // Must contain the directory structure
        assert!(
            result.skeleton.contains("src/"),
            "structure mode must list src/ directory: got {:?}",
            result.skeleton
        );

        // Must contain manifest files
        assert!(
            result.skeleton.contains("Cargo.toml"),
            "structure mode must list Cargo.toml manifest: got {:?}",
            result.skeleton
        );

        // Must NOT contain any symbol keywords
        for kw in SYMBOL_KEYWORDS {
            assert!(
                !result.skeleton.contains(kw),
                "structure mode must not contain symbol keyword {kw:?}: got {:?}",
                result.skeleton
            );
        }

        // files_scanned should be 0 (no source files read)
        assert_eq!(
            result.files_scanned, 0,
            "structure mode should not scan source files"
        );
    }

    /// Files mode: output contains file paths but NO symbol keywords.
    #[tokio::test]
    async fn test_skeleton_detail_files_no_symbols() {
        let dir = setup_detail_test_dir();
        let surgeon = crate::TreeSitterSurgeon::new(100);

        let config = SkeletonConfig::new(8_000, 3, "all", 2_000).with_detail(SkeletonDetail::Files);

        let result = generate_skeleton_text(&surgeon, dir.path(), Path::new(""), &config)
            .await
            .expect("files mode should succeed");

        // Must contain file paths
        assert!(
            result.skeleton.contains("src/main.rs"),
            "files mode must list source files: got {:?}",
            result.skeleton
        );

        // Must NOT contain any symbol keywords
        for kw in SYMBOL_KEYWORDS {
            assert!(
                !result.skeleton.contains(kw),
                "files mode must not contain symbol keyword {kw:?}: got {:?}",
                result.skeleton
            );
        }

        // Should have version hashes for files
        assert!(
            !result.version_hashes.is_empty(),
            "files mode should compute version hashes"
        );

        // files_scanned should be > 0
        assert!(
            result.files_scanned > 0,
            "files mode should count scanned files"
        );
    }

    /// Symbols mode: output contains symbol keywords (regression test).
    #[tokio::test]
    async fn test_skeleton_detail_symbols_has_symbols() {
        let dir = setup_detail_test_dir();
        let surgeon = crate::TreeSitterSurgeon::new(100);

        let config =
            SkeletonConfig::new(16_000, 3, "all", 2_000).with_detail(SkeletonDetail::Symbols);

        let result = generate_skeleton_text(&surgeon, dir.path(), Path::new(""), &config)
            .await
            .expect("symbols mode should succeed");

        // Must contain symbol keywords
        let has_symbols = SYMBOL_KEYWORDS
            .iter()
            .any(|kw| result.skeleton.contains(kw));
        assert!(
            has_symbols,
            "symbols mode must contain at least one symbol keyword: got {:?}",
            result.skeleton
        );

        // Must contain the file header format
        assert!(
            result.skeleton.contains("File: "),
            "symbols mode must use 'File: ' header format: got {:?}",
            result.skeleton
        );
    }

    /// Structure mode populates `tech_stack` from file extensions.
    #[tokio::test]
    async fn test_skeleton_detail_structure_populates_tech_stack() {
        let dir = setup_detail_test_dir();
        let surgeon = crate::TreeSitterSurgeon::new(100);

        let config =
            SkeletonConfig::new(4_000, 3, "all", 2_000).with_detail(SkeletonDetail::Structure);

        let result = generate_skeleton_text(&surgeon, dir.path(), Path::new(""), &config)
            .await
            .expect("structure mode should succeed");

        assert!(
            result.tech_stack.iter().any(|t| t == "rust"),
            "structure mode must detect Rust tech stack from .rs files: got {:?}",
            result.tech_stack
        );
    }

    /// Files mode's `version_hashes` contain the expected file path keys.
    #[tokio::test]
    async fn test_skeleton_detail_files_version_hash_keys() {
        let dir = setup_detail_test_dir();
        let surgeon = crate::TreeSitterSurgeon::new(100);

        let config = SkeletonConfig::new(8_000, 3, "all", 2_000).with_detail(SkeletonDetail::Files);

        let result = generate_skeleton_text(&surgeon, dir.path(), Path::new(""), &config)
            .await
            .expect("files mode should succeed");

        assert!(
            result.version_hashes.contains_key("src/main.rs"),
            "version_hashes must contain src/main.rs: got {:?}",
            result.version_hashes.keys().collect::<Vec<_>>()
        );

        // Each hash must be 7-char hex
        for (path, hash) in &result.version_hashes {
            assert_eq!(
                hash.len(),
                7,
                "version hash for {path} must be 7 chars, got {hash:?}"
            );
        }
    }

    /// `SkeletonDetail` default is Symbols.
    #[test]
    fn test_skeleton_detail_default_is_symbols() {
        assert_eq!(SkeletonDetail::default(), SkeletonDetail::Symbols);
    }

    /// `is_manifest_file` detects known manifest files and rejects non-manifests.
    #[test]
    fn test_is_manifest_file() {
        use std::path::Path;

        // Should match
        assert!(is_manifest_file(Path::new("Cargo.toml")));
        assert!(is_manifest_file(Path::new("package.json")));
        assert!(is_manifest_file(Path::new("go.mod")));
        assert!(is_manifest_file(Path::new("pyproject.toml")));
        assert!(is_manifest_file(Path::new("Dockerfile")));
        assert!(is_manifest_file(Path::new("Makefile")));
        assert!(is_manifest_file(Path::new("tsconfig.json")));

        // Should NOT match
        assert!(!is_manifest_file(Path::new("main.rs")));
        assert!(!is_manifest_file(Path::new("index.ts")));
        assert!(!is_manifest_file(Path::new("README.md")));
        assert!(!is_manifest_file(Path::new(".gitignore")));
    }

    /// Structure mode with tight token budget produces truncated but valid output.
    #[tokio::test]
    async fn test_skeleton_detail_structure_tight_budget() {
        let dir = setup_detail_test_dir();
        let surgeon = crate::TreeSitterSurgeon::new(100);

        // Very tight budget — may not fit all dirs
        let config = SkeletonConfig::new(1, 3, "all", 2_000).with_detail(SkeletonDetail::Structure);

        let result = generate_skeleton_text(&surgeon, dir.path(), Path::new(""), &config)
            .await
            .expect("structure mode with tight budget should not error");

        // Should return successfully even with empty skeleton
        assert!(
            result.skeleton.len() <= 100,
            "tight budget should produce minimal or empty skeleton"
        );
    }

    /// Structure mode with empty directory (no manifests, no source files).
    #[tokio::test]
    async fn test_skeleton_detail_structure_empty_dir() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let surgeon = crate::TreeSitterSurgeon::new(100);

        let config =
            SkeletonConfig::new(4_000, 3, "all", 2_000).with_detail(SkeletonDetail::Structure);

        let result = generate_skeleton_text(&surgeon, dir.path(), Path::new(""), &config)
            .await
            .expect("structure mode on empty dir should succeed");

        // Empty dir has no subdirs, no manifests
        assert_eq!(result.files_in_scope, 0, "empty dir has 0 manifest files");
        assert!(result.tech_stack.is_empty(), "empty dir has no tech stack");
    }

    /// Structure mode at depth=1 sees immediate subdirs and root manifests,
    /// but NOT source files at depth >= 2.
    #[tokio::test]
    async fn test_skeleton_detail_structure_depth_1() {
        let dir = setup_detail_test_dir();
        let surgeon = crate::TreeSitterSurgeon::new(100);

        let config =
            SkeletonConfig::new(4_000, 1, "all", 2_000).with_detail(SkeletonDetail::Structure);

        let result = generate_skeleton_text(&surgeon, dir.path(), Path::new(""), &config)
            .await
            .expect("structure mode at depth=1 should succeed");

        // Must see immediate subdir src/
        assert!(
            result.skeleton.contains("src/"),
            "depth=1 must list immediate subdirectory src/: got {:?}",
            result.skeleton
        );

        // Must see root manifest
        assert!(
            result.skeleton.contains("Cargo.toml"),
            "depth=1 must list root Cargo.toml: got {:?}",
            result.skeleton
        );

        // Source files at depth=2 (src/main.rs) are invisible to walker,
        // so tech_stack is empty at depth=1 when all source files are in
        // subdirectories. This is expected — agents can request higher depth
        // or use Files/Symbols mode for tech_stack detection.
        assert!(
            result.tech_stack.is_empty(),
            "depth=1 should have empty tech_stack when source files are in subdirs: got {:?}",
            result.tech_stack
        );
    }
}
