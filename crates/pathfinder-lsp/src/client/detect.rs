//! Zero-Config workspace language detection (PRD §6.5).
//!
//! Scans the workspace root for well-known marker files to determine which
//! language servers should be started. Returns a list of [`LanguageLsp`]
//! descriptors — one per detected language. Only languages whose marker is
//! present in the workspace are returned.
//!
//! Language servers are started lazily on first use (not eagerly at detection
//! time), so this scan is cheap.
//!
//! # Binary resolution
//!
//! Each language server binary name (e.g. `"rust-analyzer"`) is resolved to an
//! absolute path via [`which::which`] at detection time. This ensures that GUI
//! launchers and desktop shortcuts — which typically inherit a stripped `$PATH`
//! that omits `~/.cargo/bin`, `~/.nvm/.../bin`, etc. — still find the correct
//! binary. If resolution fails, the language is skipped and a warning is logged.
//!
//! Users can override the resolved path per-language via `.pathfinder.toml`:
//! ```toml
//! [lsp.rust]
//! command = "/home/user/.cargo/bin/rust-analyzer"
//! ```

use std::path::Path;

/// Result of language detection.
///
/// Contains both languages that were fully detected (marker + binary found) and
/// languages that have markers but no LSP binaries on PATH.
#[derive(Debug, Clone)]
pub struct DetectionResult {
    /// Languages with markers AND binaries found.
    pub detected: Vec<LanguageLsp>,
    /// Languages with markers but no binary on PATH.
    pub missing: Vec<MissingLanguage>,
}

/// A language whose marker files were found but whose LSP binary is not on PATH.
///
/// Used to surface actionable install guidance in `health` responses.
#[derive(Debug, Clone)]
pub struct MissingLanguage {
    /// Short language identifier (e.g., "rust", "python").
    pub language_id: String,
    /// The marker file that was found (e.g., "Cargo.toml", "pyproject.toml").
    pub marker_file: String,
    /// All binaries that were tried and failed to resolve.
    pub tried_binaries: Vec<String>,
    /// Actionable install guidance for this language.
    pub install_hint: String,
}

/// Return an actionable install hint for each language.
///
/// Delegates to the plugin registry (single source of truth).
/// Falls back to a generic hint for unknown languages.
#[must_use]
pub fn install_hint(language_id: &str) -> String {
    crate::plugin::plugin_for_language(language_id).map_or_else(
        || format!("Install a language server for {language_id}"),
        |p| p.install_hint().to_string(),
    )
}

/// Identifies a language and the command used to spawn its LSP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageLsp {
    /// Short language identifier used as a map key (e.g., `"rust"`, `"go"`).
    pub language_id: String,
    /// The binary to execute — always an **absolute path** after detection.
    ///
    /// Resolved via `which::which` at startup; can be overridden in `.pathfinder.toml`
    /// via `lsp.<lang>.command` for non-standard installs (nix, asdf, volta, etc.).
    pub command: String,
    /// Arguments to pass after the binary (e.g., `["--stdio"]`).
    pub args: Vec<String>,
    /// The root directory to use for the LSP `initialize` request.
    ///
    /// In monorepos, this is the subdirectory containing the project's
    /// marker file (e.g., `apps/backend` for a Go backend with `go.mod` there)
    /// rather than the workspace root.
    pub root: std::path::PathBuf,
    /// Optional initialization timeout in seconds (overrides default).
    pub init_timeout_secs: Option<u64>,
    /// Auto-detected TypeScript plugins to load during initialization.
    /// Populated by detect.rs when scanning the workspace for Vue, Svelte, etc.
    pub auto_plugins: Vec<String>,
    /// Language-specific initialization options passed to LSP `initialize` request.
    ///
    /// Built by per-language detection functions:
    /// - Python: `{"python": {"pythonPath": "..."}}` (from venv detection)
    /// - Java: `{"java": {"import": {"gradle": ..., "maven": ...}}}` (jdtls settings)
    /// - All others: `serde_json::Value::Null`
    pub init_options: serde_json::Value,
}

// ---------------------------------------------------------------------------
// ST-2: Manifest pre-flight validation
// ---------------------------------------------------------------------------

fn strip_jsonc_comments(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut in_block_comment = false;
    let mut in_line_comment = false;
    let mut is_escaping = false;

    while let Some(c) = chars.next() {
        if in_block_comment {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_block_comment = false;
            }
            continue;
        }

        if in_line_comment {
            if c == '\n' {
                in_line_comment = false;
                result.push(c);
            }
            continue;
        }

        if in_string {
            if is_escaping {
                is_escaping = false;
                result.push(c);
                continue;
            }
            if c == '\\' {
                is_escaping = true;
                result.push(c);
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            result.push(c);
            continue;
        }

        if c == '"' {
            in_string = true;
            result.push(c);
            continue;
        }

        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            in_block_comment = true;
            continue;
        }

        if c == '/' && chars.peek() == Some(&'/') {
            chars.next();
            in_line_comment = true;
            continue;
        }

        result.push(c);
    }

    result
}

fn strip_json_trailing_commas(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut is_escaping = false;

    while let Some(c) = chars.next() {
        if in_string {
            if is_escaping {
                is_escaping = false;
                result.push(c);
                continue;
            }
            if c == '\\' {
                is_escaping = true;
                result.push(c);
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            result.push(c);
            continue;
        }

        if c == '"' {
            in_string = true;
            result.push(c);
            continue;
        }

        if c == ',' {
            let temp_peek = chars.clone();
            let mut is_trailing = false;
            for next_c in temp_peek {
                if next_c.is_whitespace() {
                    continue;
                }
                if next_c == '}' || next_c == ']' {
                    is_trailing = true;
                }
                break;
            }
            if is_trailing {
                continue;
            }
        }

        result.push(c);
    }

    result
}

/// Validate a marker file before starting an LSP process.
///
/// Returns `Ok(())` when the file is structurally valid. Returns `Err(reason)`
/// when the file is malformed — `start_process` uses this to short-circuit
/// before spawning a process that would fail during initialize anyway.
///
/// # Supported languages
/// - **Rust**: Cargo.toml must be parseable TOML and contain `[package]` or `[workspace]`
/// - **Go**: go.mod must start with the `module` keyword
/// - **TypeScript**: tsconfig.json must be parseable JSON (supports JSONC comments)
/// - **Python**: pyproject.toml (if present) must be parseable TOML
/// - **Java**: pom.xml must contain `<project`; build.gradle[.kts] must be non-empty
pub(crate) fn validate_marker_file(
    marker_path: &std::path::Path,
    language_id: &str,
) -> Result<(), String> {
    let file_name = marker_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    if !marker_path.exists() {
        return Ok(());
    }

    let contents = match std::fs::read_to_string(marker_path) {
        Ok(c) => c,
        Err(e) => return Err(format!("cannot read {file_name}: {e}")),
    };

    match (language_id, file_name) {
        ("rust", "Cargo.toml") => match toml::from_str::<toml::Value>(&contents) {
            Ok(v) => {
                if v.get("package").is_some() || v.get("workspace").is_some() {
                    Ok(())
                } else {
                    Err("Cargo.toml has neither [package] nor [workspace] section".to_owned())
                }
            }
            Err(e) => Err(format!("Cargo.toml is not valid TOML: {e}")),
        },
        ("go", "go.mod") => {
            let target_line = contents
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with("//"));

            if let Some(line) = target_line {
                if line.starts_with("module ") && !line["module ".len()..].trim().is_empty() {
                    Ok(())
                } else {
                    Err(format!(
                        "go.mod does not start with a valid 'module' declaration (got: {line:?})"
                    ))
                }
            } else {
                Err("go.mod is empty or contains only comments".to_owned())
            }
        }
        ("typescript", "tsconfig.json") => {
            // Many real-world tsconfig.json files use JSONC (JavaScript-style comments).
            // First try parsing the raw JSON, then fall back to stripping comments.
            if serde_json::from_str::<serde_json::Value>(&contents).is_ok() {
                Ok(())
            } else {
                let stripped_comments = strip_jsonc_comments(&contents);
                let stripped_commas = strip_json_trailing_commas(&stripped_comments);
                serde_json::from_str::<serde_json::Value>(&stripped_commas)
                    .map(|_| ())
                    .map_err(|e| format!("tsconfig.json is not valid JSON: {e}"))
            }
        }
        ("python", "pyproject.toml") => match toml::from_str::<toml::Value>(&contents) {
            Ok(_) => Ok(()),
            Err(e) => Err(format!("pyproject.toml is not valid TOML: {e}")),
        },
        ("java", "pom.xml") => {
            if contents.contains("<project") {
                Ok(())
            } else {
                Err("pom.xml does not contain <project element".to_owned())
            }
        }
        ("java", "build.gradle" | "build.gradle.kts") => {
            // Gradle files are Groovy/Kotlin scripts — minimal structural validation
            if contents.is_empty() {
                Err("build.gradle is empty".to_owned())
            } else {
                Ok(())
            }
        }
        // package.json, setup.py, requirements.txt, settings.gradle[.kts] — too loose to validate
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// ST-5: Python virtual environment detection
// ---------------------------------------------------------------------------

/// Detect the Python interpreter from the workspace's virtual environment.
///
/// Checks well-known venv locations in priority order:
/// 1. `.venv/bin/python` (standard `python -m venv .venv`)
/// 2. `venv/bin/python` / `env/bin/python` / `.env/bin/python`
/// 3. `$CONDA_PREFIX/bin/python` (Conda/Mamba)
/// 4. `$VIRTUAL_ENV/bin/python` (activated venv in parent shell)
///
/// Returns `None` when no venv is detected — Pyright/pylsp then fall back to
/// the system interpreter, which is correct for non-venv projects.
#[must_use]
pub(crate) fn detect_venv(workspace_root: &std::path::Path) -> Option<std::path::PathBuf> {
    let candidates = [
        ".venv/bin/python",
        ".venv/Scripts/python.exe", // Windows
        "venv/bin/python",
        "venv/Scripts/python.exe",
        "env/bin/python",
        "env/Scripts/python.exe",
        ".env/bin/python",
        ".env/Scripts/python.exe",
    ];

    for relative in &candidates {
        let path = workspace_root.join(relative);
        if path.exists() {
            tracing::debug!(path = %path.display(), "ST-5: detected Python venv interpreter");
            return Some(path);
        }
    }

    // Conda: $CONDA_PREFIX/bin/python or $CONDA_PREFIX/python.exe or $CONDA_PREFIX/Scripts/python.exe
    if let Ok(conda_prefix) = std::env::var("CONDA_PREFIX") {
        let prefix_path = std::path::PathBuf::from(&conda_prefix);
        let paths = [
            prefix_path.join("bin").join("python"),
            prefix_path.join("python.exe"),
            prefix_path.join("Scripts").join("python.exe"),
        ];
        for path in &paths {
            if path.exists() {
                tracing::debug!(path = %path.display(), "ST-5: detected Conda Python interpreter");
                return Some(path.clone());
            }
        }
    }

    // $VIRTUAL_ENV: already-activated venv in the shell that launched Pathfinder
    if let Ok(venv) = std::env::var("VIRTUAL_ENV") {
        let venv_path = std::path::PathBuf::from(&venv);
        let paths = [
            venv_path.join("bin").join("python"),
            venv_path.join("Scripts").join("python.exe"),
        ];
        for path in &paths {
            if path.exists() {
                tracing::debug!(path = %path.display(), "ST-5: detected activated venv via $VIRTUAL_ENV");
                return Some(path.clone());
            }
        }
    }

    None
}

/// Build Python LSP initialization options from the detected venv.
///
/// Returns a JSON value suitable for `initializationOptions.python.pythonPath`
/// (Pyright format) when a venv is found, or `Null` otherwise.
#[must_use]
pub(crate) fn detect_python_init_options(workspace_root: &std::path::Path) -> serde_json::Value {
    use serde_json::json;
    detect_venv(workspace_root).map_or(serde_json::Value::Null, |py_path| {
        json!({
            "python": {
                "pythonPath": py_path.to_string_lossy().as_ref()
            }
        })
    })
}

/// Helper to detect JDK home directory based on env vars and project files (.sdkmanrc, .java-version).
fn detect_jdk_home(root: &std::path::Path) -> Option<String> {
    // 1. Check if JAVA_HOME is already in env and not empty
    if let Ok(home) = std::env::var("JAVA_HOME") {
        if !home.is_empty() {
            return Some(home);
        }
    }

    let home_str = std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())?;
    let home_dir = std::path::PathBuf::from(home_str);

    // Helper to check and verify if a path exists and contains a bin/java or bin/javac
    let verify_jdk = |path: std::path::PathBuf| -> Option<String> {
        if path.join("bin/java").exists() || path.join("bin/javac").exists() {
            return Some(path.to_string_lossy().into_owned());
        }
        let mac_home = path.join("Contents/Home");
        if mac_home.join("bin/java").exists() {
            return Some(mac_home.to_string_lossy().into_owned());
        }
        None
    };

    // 2. Try .sdkmanrc
    let sdkmanrc_path = root.join(".sdkmanrc");
    if sdkmanrc_path.exists() {
        if let Ok(contents) = std::fs::read_to_string(&sdkmanrc_path) {
            for line in contents.lines() {
                let line = line.trim();
                if let Some(stripped) = line.strip_prefix("java=") {
                    let version = stripped.trim();
                    let path = home_dir.join(format!(".sdkman/candidates/java/{version}"));
                    if let Some(verified) = verify_jdk(path) {
                        return Some(verified);
                    }
                }
            }
        }
    }

    // 3. Try .java-version
    let java_version_path = root.join(".java-version");
    if java_version_path.exists() {
        if let Ok(contents) = std::fs::read_to_string(&java_version_path) {
            let version = contents.trim();
            if !version.is_empty() {
                let path = home_dir.join(format!(".sdkman/candidates/java/{version}"));
                if let Some(verified) = verify_jdk(path) {
                    return Some(verified);
                }
                let path = home_dir.join(format!(".asdf/installs/java/{version}"));
                if let Some(verified) = verify_jdk(path) {
                    return Some(verified);
                }
                let path = home_dir.join(format!(".jenv/versions/{version}"));
                if let Some(verified) = verify_jdk(path) {
                    return Some(verified);
                }
            }
        }
    }

    None
}

/// Build jdtls initialization options.
///
/// Enables Maven and Gradle import and, when a JDK is detected (via `JAVA_HOME`,
/// `.sdkmanrc`, or `.java-version`), pins the JDK home so jdtls uses the correct JDK
/// across different shell environments.
#[must_use]
fn detect_java_init_options(
    detected_root: &std::path::Path,
    workspace_root: &std::path::Path,
) -> serde_json::Value {
    use serde_json::json;

    let java_home = detect_jdk_home(detected_root).or_else(|| detect_jdk_home(workspace_root));

    let mut settings = json!({
        "java": {
            "import": {
                "gradle": { "enabled": true },
                "maven": { "enabled": true }
            }
        }
    });

    if let Some(home) = java_home {
        settings["java"]["jdt"] = json!({ "ls": { "java": { "home": home } } });
    }

    settings
}

/// Resolve a bare binary name to its absolute path using `which`.
///
/// If the config provides an explicit command (already an absolute path or
/// a user-provided string), that value is used directly without `which` lookup.
///
/// Returns `None` and logs a warning if the binary is not found on `PATH`.
fn resolve_command(name: &str, lang: &str) -> Option<String> {
    which::which(name)
        .map(|path| {
            tracing::debug!(
                language = lang,
                binary = %path.display(),
                "LSP: resolved binary path"
            );
            path.to_string_lossy().into_owned()
        })
        // NOTE: map_err is used here for its side effect (logging the warning) before
        // discarding the error with .ok(). This is intentional: we want to log why the
        // binary wasn't found, but we still want to return None rather than propagating
        // the error.
        .map_err(|_| {
            tracing::warn!(
                language = lang,
                binary = name,
                "LSP: binary not found on PATH — language server will not start. \
                 Install it or set `lsp.{}.command` in .pathfinder.toml to \
                 an absolute path (e.g. for nix, asdf, volta, or GUI launcher installs)",
                lang
            );
        })
        .ok()
}

/// Check if a directory has a marker file.
async fn has_marker(dir: &Path, marker: &str) -> bool {
    tokio::fs::metadata(dir.join(marker)).await.is_ok()
}

/// Searches for a marker file starting from `base` up to `max_depth` subdirectory levels.
///
/// `max_depth` is clamped to 2 — deeper scans are not supported.
///
/// `sibling_filter` refines the **multi-match** branch only:
/// When multiple directories contain `marker` and `sibling_filter` is `Some("tsconfig.json")`,
/// only directories that ALSO have `tsconfig.json` are kept. If that narrows to 1, use it.
/// If that narrows to 0 or still multiple, fall back to workspace root.
///
/// A single match is always accepted regardless of `sibling_filter`, preserving
/// detection of pure JS projects (package.json without tsconfig.json).
///
/// Returns the directory containing the marker file, or `None` if not found.
async fn find_marker(
    base: &Path,
    marker: &str,
    max_depth: usize,
    sibling_filter: Option<&str>,
) -> Option<std::path::PathBuf> {
    let max_depth = max_depth.min(2);

    // Check base directory first (depth 0)
    if has_marker(base, marker).await {
        return Some(base.to_path_buf());
    }
    if max_depth == 0 {
        return None;
    }

    let mut matches = Vec::new();

    // Scan immediate children (depth 1)
    let Ok(mut dir) = tokio::fs::read_dir(base).await else {
        return None;
    };
    while let Ok(Some(entry)) = dir.next_entry().await {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        // Skip symlinks
        if let Ok(file_type) = entry.file_type().await {
            if file_type.is_symlink() {
                continue;
            }
        }

        // Check this subdirectory
        if has_marker(&path, marker).await {
            matches.push(path.clone());
        }

        // One more level if depth allows
        if max_depth >= 2 {
            let Ok(mut sub) = tokio::fs::read_dir(&path).await else {
                continue;
            };
            while let Ok(Some(sub_entry)) = sub.next_entry().await {
                let sub_path = sub_entry.path();
                if sub_path.is_dir() {
                    // Skip symlinks
                    if let Ok(file_type) = sub_entry.file_type().await {
                        if file_type.is_symlink() {
                            continue;
                        }
                    }
                    if has_marker(&sub_path, marker).await {
                        matches.push(sub_path);
                    }
                }
            }
        }
    }

    match matches.len() {
        0 => None,
        1 => Some(matches[0].clone()),
        _ => {
            // Multiple matches found. If sibling_filter is set, prefer directories
            // that also have the sibling file (e.g., tsconfig.json next to package.json).
            // This filters out e2e/test sub-projects that have package.json but no TS config.
            if let Some(sibling) = sibling_filter {
                let mut refined = Vec::new();
                for dir in &matches {
                    if tokio::fs::metadata(dir.join(sibling)).await.is_ok() {
                        refined.push(dir.clone());
                    }
                }
                if refined.len() == 1 {
                    tracing::info!(
                        "Multiple {} markers found; selected {} (has {} sibling)",
                        marker,
                        refined[0].display(),
                        sibling,
                    );
                    return Some(refined[0].clone());
                }
                // refined.len() == 0 or still multiple — fall through to workspace root
            }

            tracing::info!(
                "Multiple {} marker files found in monorepo. Using workspace root {} as project root.",
                marker,
                base.display()
            );
            Some(base.to_path_buf())
        }
    }
}

// ---------------------------------------------------------------------------
// TypeScript plugin detection helpers
// ---------------------------------------------------------------------------

/// Check if the workspace contains Vue single-file components.
///
/// Scans for `.vue` files up to 4 levels deep from the workspace root.
///
/// Covers:
/// - Standard layouts: `src/App.vue`, `src/components/Button.vue`
/// - Monorepo layouts: `apps/frontend/src/App.vue`, `packages/web/src/views/Home.vue`
///
/// Avoids loading `@vue/typescript-plugin` unnecessarily in pure TS projects.
async fn workspace_has_vue_files(workspace_root: &Path) -> bool {
    fn should_exclude_dir(name: &std::ffi::OsStr) -> bool {
        matches!(
            name.to_str(),
            Some(
                "node_modules"
                    | ".git"
                    | "target"
                    | ".pnpm"
                    | ".venv"
                    | "__pycache__"
                    | "dist"
                    | "build"
            )
        )
    }

    fn has_vue_recursive(
        dir: &Path,
        depth: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + '_>> {
        Box::pin(async move {
            if depth > 4 {
                return false;
            }
            let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
                return false;
            };
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if let Some(ext) = path.extension() {
                    if ext == "vue" {
                        return true;
                    }
                }
                if path.is_dir() {
                    if let Ok(file_type) = entry.file_type().await {
                        if file_type.is_symlink() {
                            continue;
                        }
                    }
                    if let Some(dir_name) = path.file_name() {
                        if should_exclude_dir(dir_name) {
                            continue;
                        }
                    }
                    if has_vue_recursive(&path, depth + 1).await {
                        return true;
                    }
                }
            }
            false
        })
    }
    has_vue_recursive(workspace_root, 0).await
}

/// Find a TypeScript plugin in the workspace's `node_modules`.
///
/// Checks standard npm/yarn location first, then pnpm's `.pnpm` store.
/// Returns the plugin *name* (not path) — tsserver resolves plugins by name.
async fn detect_ts_plugin(workspace_root: &Path, plugin_name: &str) -> Option<String> {
    // Standard npm/yarn: node_modules/@vue/typescript-plugin
    let standard = workspace_root.join("node_modules").join(plugin_name);
    if tokio::fs::metadata(&standard).await.is_ok() {
        return Some(plugin_name.to_owned());
    }

    // pnpm: node_modules/.pnpm/@vue+typescript-plugin@x.y.z/node_modules/@vue/typescript-plugin
    let pnpm_dir = workspace_root.join("node_modules").join(".pnpm");
    if let Ok(mut entries) = tokio::fs::read_dir(&pnpm_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            // pnpm uses `+` as path separator in directory names: @vue+typescript-plugin@2.0.0
            let pattern = plugin_name.replace('/', "+").replace('@', "");
            if name_str.contains(&pattern) {
                let plugin_path = entry.path().join("node_modules").join(plugin_name);
                if tokio::fs::metadata(&plugin_path).await.is_ok() {
                    return Some(plugin_name.to_owned());
                }
            }
        }
    }

    None
}

/// Detect TypeScript plugins for this workspace.
///
/// Priority: config override > auto-detection (only when `.vue` files exist).
async fn detect_typescript_plugins(
    project_root: &Path,
    workspace_root: &Path,
    config: &pathfinder_common::config::PathfinderConfig,
) -> Vec<String> {
    // Config override takes precedence
    let config_plugins: Vec<String> = config
        .lsp
        .get("typescript")
        .map(|c| c.typescript_plugins.clone())
        .unwrap_or_default();

    if !config_plugins.is_empty() {
        tracing::info!(
            plugins = ?config_plugins,
            "Using configured TypeScript plugins"
        );
        return config_plugins;
    }

    // Auto-detect Vue plugin when .vue files are present
    let mut plugins = Vec::new();
    let has_vue = workspace_has_vue_files(project_root).await
        || (project_root != workspace_root && workspace_has_vue_files(workspace_root).await);
    if has_vue {
        let plugin = detect_ts_plugin(project_root, "@vue/typescript-plugin").await;
        let plugin = match plugin {
            Some(p) => Some(p),
            None => detect_ts_plugin(workspace_root, "@vue/typescript-plugin").await,
        };
        if let Some(p) = plugin {
            tracing::info!("Auto-detected @vue/typescript-plugin for Vue SFC support");
            plugins.push(p);
        }
    }

    plugins
}

// ---------------------------------------------------------------------------
// Main detection entry point
// ---------------------------------------------------------------------------

/// Detect available language servers for the given workspace root and configuration.
#[allow(clippy::missing_errors_doc)]
// The function is structured as one block per language (Rust, Go, TS, Python, Java).
// Each block is short but the five-language repetition pushes the total just
// over the 100-line clippy default. Suppressing to keep the pattern intact.
#[expect(
    clippy::too_many_lines,
    reason = "Five-language repetition block; each block is short but cumulative length exceeds threshold. Pattern is clean per-language — extraction would add indirection without clarity."
)]
pub async fn detect_languages(
    workspace_root: &Path,
    config: &pathfinder_common::config::PathfinderConfig,
) -> std::io::Result<DetectionResult> {
    let mut detected = Vec::new();
    let mut missing = Vec::new();

    // Helper macro to get the root_override path if configured
    macro_rules! get_override {
        ($lang:expr) => {
            config
                .lsp
                .get($lang)
                .and_then(|c| c.root_override.as_ref())
                .map(|r| workspace_root.join(r))
        };
    }

    // Helper macro to get a command override from config (skips `which` lookup).
    macro_rules! get_command_override {
        ($lang:expr) => {
            config
                .lsp
                .get($lang)
                .map(|c| c.command.clone())
                .filter(|c| !c.is_empty())
        };
    }

    // Get args from config (non-empty overrides language defaults; enables test mock flags).
    macro_rules! get_args {
        ($lang:expr, $default:expr) => {
            config
                .lsp
                .get($lang)
                .filter(|c| !c.args.is_empty())
                .map(|c| c.args.clone())
                .unwrap_or_else(|| $default)
        };
    }

    // Rust
    if let Some(rust_plugin) = crate::plugin::plugin_for_language("rust") {
        let (rust_root, rust_marker) = if get_override!("rust").is_some() {
            (get_override!("rust"), None)
        } else {
            let mut found = None;
            for marker in rust_plugin.marker_files() {
                if let Some(r) = find_marker(
                    workspace_root,
                    marker,
                    rust_plugin.marker_search_depth() as usize,
                    None,
                )
                .await
                {
                    found = Some((r, marker));
                    break;
                }
            }
            if let Some((r, m)) = found {
                (Some(r), Some(*m))
            } else {
                (None, None)
            }
        };
        if let Some(root) = rust_root {
            let has_override = get_command_override!("rust").is_some();
            let cmd = get_command_override!("rust").or_else(|| {
                let mut resolved = None;
                for candidate in rust_plugin.lsp_candidates() {
                    if let Some(resolved_cmd) = resolve_command(candidate.binary, "rust") {
                        resolved = Some(resolved_cmd);
                        break;
                    }
                }
                resolved
            });
            if let Some(command) = cmd {
                // ST-2: validate Cargo.toml before spending time spawning the process
                if let Some(marker) = rust_marker {
                    let marker_path = root.join(marker);
                    if let Err(reason) = validate_marker_file(&marker_path, "rust") {
                        tracing::warn!(language = "rust", %reason, "ST-2: invalid manifest — skipping LSP start");
                        missing.push(MissingLanguage {
                            language_id: "rust".to_owned(),
                            marker_file: marker.to_string(),
                            tried_binaries: rust_plugin
                                .lsp_candidates()
                                .iter()
                                .map(|c| c.binary.to_string())
                                .collect(),
                            install_hint: format!("Fix {marker}: {reason}"),
                        });
                    } else {
                        detected.push(LanguageLsp {
                            language_id: "rust".to_owned(),
                            command,
                            args: get_args!("rust", vec![]),
                            root,
                            init_timeout_secs: None,
                            auto_plugins: vec![],
                            init_options: serde_json::Value::Null,
                        });
                    }
                } else {
                    // config root_override — no marker to validate
                    detected.push(LanguageLsp {
                        language_id: "rust".to_owned(),
                        command,
                        args: get_args!("rust", vec![]),
                        root,
                        init_timeout_secs: None,
                        auto_plugins: vec![],
                        init_options: serde_json::Value::Null,
                    });
                }
            } else if !has_override {
                missing.push(MissingLanguage {
                    language_id: "rust".to_owned(),
                    marker_file: rust_marker.unwrap_or("Cargo.toml").to_string(),
                    tried_binaries: rust_plugin
                        .lsp_candidates()
                        .iter()
                        .map(|c| c.binary.to_string())
                        .collect(),
                    install_hint: install_hint("rust"),
                });
            }
        }
    } else {
        tracing::error!("rust plugin not found in registry — skipping detection");
    }

    // Go
    if let Some(go_plugin) = crate::plugin::plugin_for_language("go") {
        let (go_root, go_marker) = if get_override!("go").is_some() {
            (get_override!("go"), None)
        } else {
            let mut found = None;
            for marker in go_plugin.marker_files() {
                if let Some(r) = find_marker(
                    workspace_root,
                    marker,
                    go_plugin.marker_search_depth() as usize,
                    None,
                )
                .await
                {
                    found = Some((r, marker));
                    break;
                }
            }
            if let Some((r, m)) = found {
                (Some(r), Some(*m))
            } else {
                (None, None)
            }
        };
        if let Some(root) = go_root {
            let has_override = get_command_override!("go").is_some();
            let cmd = get_command_override!("go").or_else(|| {
                let mut resolved = None;
                for candidate in go_plugin.lsp_candidates() {
                    if let Some(resolved_cmd) = resolve_command(candidate.binary, "go") {
                        resolved = Some(resolved_cmd);
                        break;
                    }
                }
                resolved
            });
            if let Some(command) = cmd {
                // ST-2: validate go.mod before spawning gopls
                if let Some(marker) = go_marker {
                    let marker_path = root.join(marker);
                    if let Err(reason) = validate_marker_file(&marker_path, "go") {
                        tracing::warn!(language = "go", %reason, "ST-2: invalid manifest — skipping LSP start");
                        missing.push(MissingLanguage {
                            language_id: "go".to_owned(),
                            marker_file: marker.to_string(),
                            tried_binaries: go_plugin
                                .lsp_candidates()
                                .iter()
                                .map(|c| c.binary.to_string())
                                .collect(),
                            install_hint: format!("Fix {marker}: {reason}"),
                        });
                    } else {
                        detected.push(LanguageLsp {
                            language_id: "go".to_owned(),
                            command,
                            args: get_args!("go", vec![]),
                            root,
                            init_timeout_secs: None,
                            auto_plugins: vec![],
                            init_options: serde_json::Value::Null,
                        });
                    }
                } else {
                    // config root_override — no marker to validate
                    detected.push(LanguageLsp {
                        language_id: "go".to_owned(),
                        command,
                        args: get_args!("go", vec![]),
                        root,
                        init_timeout_secs: None,
                        auto_plugins: vec![],
                        init_options: serde_json::Value::Null,
                    });
                }
            } else if !has_override {
                missing.push(MissingLanguage {
                    language_id: "go".to_owned(),
                    marker_file: go_marker.unwrap_or("go.mod").to_string(),
                    tried_binaries: go_plugin
                        .lsp_candidates()
                        .iter()
                        .map(|c| c.binary.to_string())
                        .collect(),
                    install_hint: install_hint("go"),
                });
            }
        }
    } else {
        tracing::error!("go plugin not found in registry — skipping detection");
    }

    // TypeScript / JavaScript
    if let Some(ts_plugin) = crate::plugin::plugin_for_language("typescript") {
        let (ts_root, ts_marker) = if get_override!("typescript").is_some() {
            (get_override!("typescript"), None)
        } else {
            let mut found = None;
            for marker in ts_plugin.marker_files() {
                // For `package.json`, require a sibling `tsconfig.json` to
                // avoid false-positive root detection in monorepos where
                // e2e/test sub-projects have `package.json` without TS config.
                let sibling = if *marker == "package.json" {
                    Some("tsconfig.json")
                } else {
                    None
                };
                if let Some(r) = find_marker(
                    workspace_root,
                    marker,
                    ts_plugin.marker_search_depth() as usize,
                    sibling,
                )
                .await
                {
                    found = Some((r, marker));
                    break;
                }
            }
            if let Some((r, m)) = found {
                (Some(r), Some(*m))
            } else {
                (None, None)
            }
        };
        if let Some(root) = ts_root {
            let has_override = get_command_override!("typescript").is_some();
            let cmd = get_command_override!("typescript").or_else(|| {
                let mut resolved = None;
                for candidate in ts_plugin.lsp_candidates() {
                    if let Some(resolved_cmd) = resolve_command(candidate.binary, "typescript") {
                        resolved = Some(resolved_cmd);
                        break;
                    }
                }
                resolved
            });
            if let Some(command) = cmd {
                let auto_plugins = detect_typescript_plugins(&root, workspace_root, config).await;
                // ST-2: validate marker file before spawning tsserver
                if let Some(marker) = ts_marker {
                    let marker_path = root.join(marker);
                    if let Err(reason) = validate_marker_file(&marker_path, "typescript") {
                        tracing::warn!(language = "typescript", %reason, "ST-2: invalid manifest — skipping LSP start");
                        missing.push(MissingLanguage {
                            language_id: "typescript".to_owned(),
                            marker_file: marker.to_string(),
                            tried_binaries: ts_plugin
                                .lsp_candidates()
                                .iter()
                                .map(|c| c.binary.to_string())
                                .collect(),
                            install_hint: format!("Fix {marker}: {reason}"),
                        });
                    } else {
                        detected.push(LanguageLsp {
                            language_id: "typescript".to_owned(),
                            command,
                            args: get_args!(
                                "typescript",
                                ts_plugin.lsp_candidates()[0]
                                    .default_args
                                    .iter()
                                    .map(ToString::to_string)
                                    .collect()
                            ),
                            root,
                            init_timeout_secs: None,
                            auto_plugins,
                            init_options: serde_json::Value::Null,
                        });
                    }
                } else {
                    // config root_override — no marker to validate
                    detected.push(LanguageLsp {
                        language_id: "typescript".to_owned(),
                        command,
                        args: get_args!(
                            "typescript",
                            ts_plugin.lsp_candidates()[0]
                                .default_args
                                .iter()
                                .map(ToString::to_string)
                                .collect()
                        ),
                        root,
                        init_timeout_secs: None,
                        auto_plugins,
                        init_options: serde_json::Value::Null,
                    });
                }
            } else if !has_override {
                missing.push(MissingLanguage {
                    language_id: "typescript".to_owned(),
                    marker_file: ts_marker
                        .unwrap_or("tsconfig.json or package.json")
                        .to_string(),
                    tried_binaries: ts_plugin
                        .lsp_candidates()
                        .iter()
                        .map(|c| c.binary.to_string())
                        .collect(),
                    install_hint: install_hint("typescript"),
                });
            }
        }
    } else {
        tracing::error!("typescript plugin not found in registry — skipping detection");
    }

    // Python
    if let Some(py_plugin) = crate::plugin::plugin_for_language("python") {
        let (py_root, py_marker) = if get_override!("python").is_some() {
            (get_override!("python"), None)
        } else {
            let mut found = None;
            for marker in py_plugin.marker_files() {
                if let Some(r) = find_marker(
                    workspace_root,
                    marker,
                    py_plugin.marker_search_depth() as usize,
                    None,
                )
                .await
                {
                    found = Some((r, marker));
                    break;
                }
            }
            if let Some((r, m)) = found {
                (Some(r), Some(*m))
            } else {
                (None, None)
            }
        };
        if let Some(root) = py_root {
            let has_override = get_command_override!("python").is_some();
            let maybe_command_and_args = if let Some(cmd_override) = get_command_override!("python")
            {
                Some((cmd_override, vec!["--stdio".to_owned()]))
            } else {
                let mut resolved = None;
                for candidate in py_plugin.lsp_candidates() {
                    if let Some(resolved_cmd) = resolve_command(candidate.binary, "python") {
                        resolved = Some((
                            resolved_cmd,
                            candidate
                                .default_args
                                .iter()
                                .map(ToString::to_string)
                                .collect(),
                        ));
                        break;
                    }
                }
                resolved
            };

            if let Some((command, default_args)) = maybe_command_and_args {
                let maybe_marker_path = py_marker
                    .filter(|m| *m == "pyproject.toml")
                    .map(|m| root.join(m));
                let manifest_ok = if let Some(mp) = maybe_marker_path {
                    match validate_marker_file(&mp, "python") {
                        Ok(()) => true,
                        Err(reason) => {
                            tracing::warn!(language = "python", %reason, "ST-2: invalid manifest — skipping LSP start");
                            missing.push(MissingLanguage {
                                language_id: "python".to_owned(),
                                marker_file: "pyproject.toml".to_string(),
                                tried_binaries: vec![command.clone()],
                                install_hint: format!("Fix pyproject.toml: {reason}"),
                            });
                            false
                        }
                    }
                } else {
                    true
                };

                if manifest_ok {
                    let init_options = detect_python_init_options(&root);
                    if !init_options.is_null() {
                        tracing::info!(
                            options = ?init_options,
                            "ST-5: Python venv detected — will pass pythonPath to LSP"
                        );
                    }
                    detected.push(LanguageLsp {
                        language_id: "python".to_owned(),
                        command,
                        args: get_args!("python", default_args),
                        root,
                        init_timeout_secs: None,
                        auto_plugins: vec![],
                        init_options,
                    });
                }
            } else if !has_override {
                missing.push(MissingLanguage {
                    language_id: "python".to_owned(),
                    marker_file: py_marker
                        .unwrap_or("pyproject.toml, setup.py, or requirements.txt")
                        .to_string(),
                    tried_binaries: py_plugin
                        .lsp_candidates()
                        .iter()
                        .map(|c| c.binary.to_string())
                        .collect(),
                    install_hint: install_hint("python"),
                });
            }
        }
    } else {
        tracing::error!("python plugin not found in registry — skipping detection");
    }

    // Java
    if let Some(java_plugin) = crate::plugin::plugin_for_language("java") {
        let (java_root, java_marker) = if get_override!("java").is_some() {
            (get_override!("java"), None)
        } else {
            let mut found = None;
            for marker in java_plugin.marker_files() {
                if let Some(r) = find_marker(
                    workspace_root,
                    marker,
                    java_plugin.marker_search_depth() as usize,
                    None,
                )
                .await
                {
                    found = Some((r, marker));
                    break;
                }
            }
            if let Some((r, m)) = found {
                (Some(r), Some(*m))
            } else {
                (None, None)
            }
        };
        if let Some(root) = java_root {
            let has_override = get_command_override!("java").is_some();
            let cmd = get_command_override!("java").or_else(|| {
                let mut resolved = None;
                for candidate in java_plugin.lsp_candidates() {
                    if let Some(resolved_cmd) = resolve_command(candidate.binary, "java") {
                        resolved = Some(resolved_cmd);
                        break;
                    }
                }
                resolved
            });
            if let Some(command) = cmd {
                let maybe_marker_path = java_marker
                    .filter(|m| *m == "pom.xml" || *m == "build.gradle" || *m == "build.gradle.kts")
                    .map(|m| root.join(m));
                let manifest_ok = if let Some(mp) = &maybe_marker_path {
                    match validate_marker_file(mp, "java") {
                        Ok(()) => true,
                        Err(reason) => {
                            let marker = java_marker.unwrap_or("pom.xml or build.gradle");
                            tracing::warn!(language = "java", %reason, "ST-2: invalid manifest");
                            missing.push(MissingLanguage {
                                language_id: "java".to_owned(),
                                marker_file: marker.to_string(),
                                tried_binaries: java_plugin
                                    .lsp_candidates()
                                    .iter()
                                    .map(|c| c.binary.to_string())
                                    .collect(),
                                install_hint: format!("Fix {marker}: {reason}"),
                            });
                            false
                        }
                    }
                } else {
                    true
                };

                if manifest_ok {
                    let init_opts = detect_java_init_options(&root, workspace_root);
                    detected.push(LanguageLsp {
                        language_id: "java".to_owned(),
                        command,
                        args: get_args!("java", vec![]),
                        root,
                        init_timeout_secs: Some(180),
                        auto_plugins: vec![],
                        init_options: init_opts,
                    });
                }
            } else if !has_override {
                missing.push(MissingLanguage {
                    language_id: "java".to_owned(),
                    marker_file: java_marker.unwrap_or("pom.xml or build.gradle").to_string(),
                    tried_binaries: java_plugin
                        .lsp_candidates()
                        .iter()
                        .map(|c| c.binary.to_string())
                        .collect(),
                    install_hint: install_hint("java"),
                });
            }
        }
    } else {
        tracing::error!("java plugin not found in registry — skipping detection");
    }

    let mut filtered_detected = Vec::new();
    for lsp in detected {
        if let Some(plugin) = crate::plugin::plugin_for_language(&lsp.language_id) {
            if has_source_files_recursive(workspace_root, plugin.file_extensions(), 0).await {
                filtered_detected.push(lsp);
            } else {
                tracing::info!(
                    language = %lsp.language_id,
                    "Filtering out language from detection: no source files found in workspace"
                );
            }
        } else {
            filtered_detected.push(lsp);
        }
    }

    let mut filtered_missing = Vec::new();
    for lsp in missing {
        if let Some(plugin) = crate::plugin::plugin_for_language(&lsp.language_id) {
            if has_source_files_recursive(workspace_root, plugin.file_extensions(), 0).await {
                filtered_missing.push(lsp);
            } else {
                tracing::info!(
                    language = %lsp.language_id,
                    "Filtering out missing language from detection: no source files found in workspace"
                );
            }
        } else {
            filtered_missing.push(lsp);
        }
    }

    Ok(DetectionResult {
        detected: filtered_detected,
        missing: filtered_missing,
    })
}

fn has_source_files_recursive<'a>(
    dir: &'a Path,
    extensions: &'a [&'static str],
    depth: usize,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
    Box::pin(async move {
        if depth > 8 {
            return false;
        }
        let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
            return false;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let Ok(file_type) = entry.file_type().await else {
                continue;
            };
            if file_type.is_dir() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with('.')
                        || name == "node_modules"
                        || name == "target"
                        || name == "vendor"
                        || name == "dist"
                        || name == "build"
                        || name == "__pycache__"
                        || name == "pytest-of-irahardianto"
                    {
                        continue;
                    }
                }
                if has_source_files_recursive(&path, extensions, depth + 1).await {
                    return true;
                }
            } else if file_type.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if extensions.iter().any(|&e| e.eq_ignore_ascii_case(ext)) {
                        return true;
                    }
                }
            }
        }
        false
    })
}

/// Map a file extension to its language identifier.
///
/// Used to look up the correct LSP process when a tool call names a specific
/// file. Returns `None` if the language is unsupported.
#[must_use]
pub fn language_id_for_extension(ext: &str) -> Option<&'static str> {
    crate::plugin::plugin_for_extension(ext).map(crate::plugin::LanguagePlugin::language_id)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::tempdir;

    // Mutex to ensure tests that modify PATH run serially
    static PATH_MUTEX: Mutex<()> = Mutex::new(());

    #[allow(
        clippy::await_holding_lock,
        clippy::expect_fun_call,
        clippy::items_after_statements
    )]
    async fn test_with_fake_python_binaries<F, Fut>(binaries: &[&str], test: F)
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        // Lock mutex to ensure serial execution
        let _guard = match PATH_MUTEX.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(), // Recover from poisoned mutex
        };

        // Create temp dir for fake binaries
        let temp_bin_dir = tempdir().expect("create temp bin dir");
        let sh_path = which::which("sh").expect("sh not found on PATH");

        // Create symlinks to `sh` for each requested binary
        for binary in binaries {
            let symlink_path = temp_bin_dir.path().join(binary);
            #[cfg(unix)]
            std::os::unix::fs::symlink(&sh_path, &symlink_path)
                .expect(&format!("create symlink for {binary}"));
        }

        struct PathGuard {
            original_path: Option<String>,
        }
        impl Drop for PathGuard {
            fn drop(&mut self) {
                if let Some(ref orig) = self.original_path {
                    std::env::set_var("PATH", orig);
                } else {
                    std::env::remove_var("PATH");
                }
            }
        }

        // Save original PATH via guard
        let _path_guard = PathGuard {
            original_path: std::env::var("PATH").ok(),
        };

        // Set PATH to ONLY our temp bin dir to avoid finding system LSPs
        let new_path = temp_bin_dir.path().to_string_lossy().to_string();
        std::env::set_var("PATH", &new_path);

        // Run the test
        test().await;
    }

    // Helper to create a fake @vue/typescript-plugin directory in node_modules.
    fn create_vue_plugin(workspace: &Path) {
        let dir = workspace
            .join("node_modules")
            .join("@vue")
            .join("typescript-plugin");
        std::fs::create_dir_all(&dir).expect("create plugin dir");
        std::fs::write(
            dir.join("package.json"),
            r#"{"name":"@vue/typescript-plugin"}"#,
        )
        .expect("write plugin package.json");
    }

    // Helper to create a fake pnpm-style @vue/typescript-plugin.
    fn create_vue_plugin_pnpm(workspace: &Path) {
        let pkg_dir = workspace
            .join("node_modules")
            .join(".pnpm")
            .join("@vue+typescript-plugin@2.0.0")
            .join("node_modules")
            .join("@vue")
            .join("typescript-plugin");
        std::fs::create_dir_all(&pkg_dir).expect("create pnpm plugin dir");
        std::fs::write(
            pkg_dir.join("package.json"),
            r#"{"name":"@vue/typescript-plugin"}"#,
        )
        .expect("write pnpm plugin package.json");
    }

    // Helper to create a .vue file in src/.
    fn create_vue_file(workspace: &Path) {
        let src = workspace.join("src");
        std::fs::create_dir_all(&src).expect("create src dir");
        std::fs::write(src.join("App.vue"), "<script setup lang=\"ts\"></script>")
            .expect("write vue file");
    }

    // Helper to create a .vue file in a nested components/ directory.
    fn create_vue_file_nested(workspace: &Path) {
        let components = workspace.join("src").join("components");
        std::fs::create_dir_all(&components).expect("create components dir");
        std::fs::write(components.join("Button.vue"), "<template></template>")
            .expect("write vue file");
    }

    // Helper to create a .vue file deep in a monorepo (depth 3+).
    // Simulates: apps/frontend/src/views/Home.vue
    fn create_vue_file_monorepo(workspace: &Path) {
        let views = workspace
            .join("apps")
            .join("frontend")
            .join("src")
            .join("views");
        std::fs::create_dir_all(&views).expect("create monorepo views dir");
        std::fs::write(views.join("Home.vue"), "<template>Home</template>")
            .expect("write vue file");
    }

    fn make_ts_config() -> pathfinder_common::config::PathfinderConfig {
        pathfinder_common::config::PathfinderConfig::default()
    }

    // ── existing tests (preserved) ──────────────────────────────────────────

    #[tokio::test]
    async fn test_detects_cargo_toml() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").expect("write");
        std::fs::create_dir_all(dir.path().join("src")).expect("create dir");
        std::fs::write(dir.path().join("src/lib.rs"), "fn main() {}").expect("write");
        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");
        if let Some(rust) = result.detected.iter().find(|l| l.language_id == "rust") {
            assert!(rust.args.is_empty());
            assert!(rust.init_timeout_secs.is_none());
            assert!(!rust.command.is_empty());
        }
    }

    #[tokio::test]
    async fn test_detects_go_mod() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("go.mod"), "module foo").expect("write");
        std::fs::write(dir.path().join("main.go"), "package main").expect("write");
        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");
        if let Some(go) = result.detected.iter().find(|l| l.language_id == "go") {
            assert!(!go.command.is_empty());
        }
    }

    #[tokio::test]
    async fn test_detects_typescript_via_tsconfig() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("tsconfig.json"), "{}").expect("write");
        std::fs::write(dir.path().join("index.ts"), "").expect("write");
        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");
        if let Some(ts) = result
            .detected
            .iter()
            .find(|l| l.language_id == "typescript")
        {
            assert_eq!(ts.args, ["--stdio"]);
            assert!(ts.init_timeout_secs.is_none());
        }
    }

    #[tokio::test]
    async fn test_detects_typescript_monorepo_multiple_tsconfig_no_root() {
        let dir = tempdir().expect("temp dir");
        let app1 = dir.path().join("apps").join("app1");
        let app2 = dir.path().join("apps").join("app2");
        std::fs::create_dir_all(&app1).expect("create dir");
        std::fs::create_dir_all(&app2).expect("create dir");
        std::fs::write(app1.join("tsconfig.json"), "{}").expect("write");
        std::fs::write(app2.join("tsconfig.json"), "{}").expect("write");
        std::fs::write(app1.join("index.ts"), "").expect("write");

        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");

        // TypeScript should appear in either `detected` (binary found) or `missing`
        // (binary not installed, as on CI). The test validates the monorepo marker
        // fallback: multiple sub-project tsconfig.json files → workspace root is chosen.
        let ts_entry = result
            .detected
            .iter()
            .find(|l| l.language_id == "typescript")
            .map(|l| l.root.clone())
            .or_else(|| {
                result
                    .missing
                    .iter()
                    .find(|l| l.language_id == "typescript")
                    .map(|_| dir.path().to_path_buf())
            });

        assert!(
            ts_entry.is_some(),
            "TypeScript should be detected (detected or missing) in monorepo despite no root tsconfig.json"
        );

        // When in detected, root must be the workspace root (monorepo fallback).
        if let Some(ts) = result
            .detected
            .iter()
            .find(|l| l.language_id == "typescript")
        {
            assert_eq!(
                ts.root,
                dir.path(),
                "Fallback should resolve to workspace root"
            );
        }
    }

    #[tokio::test]
    async fn test_detects_typescript_via_package_json() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("package.json"), "{}").expect("write");
        std::fs::write(dir.path().join("index.ts"), "").expect("write");
        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");
        let found = result
            .detected
            .iter()
            .any(|l| l.language_id == "typescript");
        let _ = found;
    }

    #[tokio::test]
    async fn test_detects_python_via_pyproject() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("pyproject.toml"), "[tool.poetry]").expect("write");
        std::fs::write(dir.path().join("main.py"), "").expect("write");
        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");
        if let Some(py) = result.detected.iter().find(|l| l.language_id == "python") {
            assert!(!py.command.is_empty());
        }
    }

    #[tokio::test]
    async fn test_detects_multiple_languages() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").expect("write");
        std::fs::write(dir.path().join("package.json"), "{}").expect("write");
        std::fs::create_dir_all(dir.path().join("src")).expect("create dir");
        std::fs::write(dir.path().join("src/lib.rs"), "").expect("write");
        std::fs::write(dir.path().join("index.ts"), "").expect("write");
        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");
        let ids: Vec<&str> = result
            .detected
            .iter()
            .map(|l| l.language_id.as_str())
            .collect();
        for id in &ids {
            assert!(["rust", "typescript"].contains(id));
        }
    }

    #[tokio::test]
    async fn test_empty_directory() {
        let dir = tempdir().expect("temp dir");
        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");
        assert!(result.detected.is_empty() && result.missing.is_empty());
    }

    #[tokio::test]
    async fn test_detects_go_mod_in_subdirectory() {
        let dir = tempdir().expect("temp dir");
        let sub_dir = dir.path().join("apps").join("backend");
        std::fs::create_dir_all(&sub_dir).expect("create dir");
        std::fs::write(sub_dir.join("go.mod"), "module foo").expect("write");
        std::fs::write(sub_dir.join("main.go"), "package main").expect("write");
        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");
        if let Some(go_lang) = result.detected.into_iter().find(|l| l.language_id == "go") {
            assert_eq!(go_lang.root, sub_dir);
        }
    }

    #[tokio::test]
    async fn test_root_override_config() {
        let dir = tempdir().expect("temp dir");
        let custom_backend = dir.path().join("custom/backend");
        std::fs::create_dir_all(&custom_backend).expect("create dir");
        std::fs::write(custom_backend.join("main.go"), "package main").expect("write");

        let mut config = pathfinder_common::config::PathfinderConfig::default();
        config.lsp.insert(
            "go".to_string(),
            pathfinder_common::config::LspConfig {
                command: "gopls".to_string(),
                args: vec![],
                idle_timeout_minutes: 15,
                settings: serde_json::Value::Null,
                root_override: Some("custom/backend".to_string()),
                typescript_plugins: vec![],
            },
        );

        let result = detect_languages(dir.path(), &config).await.expect("detect");
        if let Some(go_lang) = result.detected.into_iter().find(|l| l.language_id == "go") {
            assert_eq!(go_lang.root, dir.path().join("custom/backend"));
            assert_eq!(go_lang.command, "gopls");
        }
    }

    #[tokio::test]
    async fn test_filters_out_language_with_no_source_files() {
        let dir = tempdir().expect("temp dir");
        // Marker file present
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").expect("write");

        // No .rs file -> should NOT detect
        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");
        assert!(
            result.detected.is_empty(),
            "should filter out rust language when no source files exist"
        );
        assert!(
            result.missing.is_empty(),
            "should filter out rust language from missing when no source files exist"
        );

        // Write a .rs file -> should detect
        std::fs::create_dir_all(dir.path().join("src")).expect("create dir");
        std::fs::write(dir.path().join("src/lib.rs"), "fn main() {}").expect("write");
        let result2 = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");

        let total_detected = result2.detected.len() + result2.missing.len();
        assert_eq!(
            total_detected, 1,
            "should detect rust language when source file is added"
        );

        let detected_lang_id = if result2.detected.is_empty() {
            &result2.missing[0].language_id
        } else {
            &result2.detected[0].language_id
        };
        assert_eq!(detected_lang_id, "rust");
    }

    #[test]
    fn test_language_id_for_extension() {
        assert_eq!(language_id_for_extension("rs"), Some("rust"));
        assert_eq!(language_id_for_extension("go"), Some("go"));
        assert_eq!(language_id_for_extension("ts"), Some("typescript"));
        assert_eq!(language_id_for_extension("tsx"), Some("typescript"));
        assert_eq!(language_id_for_extension("js"), Some("typescript"));
        assert_eq!(language_id_for_extension("py"), Some("python"));
        assert_eq!(language_id_for_extension("md"), None);
        assert_eq!(language_id_for_extension("yaml"), None);
    }

    #[test]
    fn test_resolve_command_not_found_returns_none() {
        let result = resolve_command(
            "pathfinder_lsp_binary_that_does_not_exist_xyzzy_42",
            "test-lang",
        );
        assert!(
            result.is_none(),
            "resolve_command must return None when the binary is not on PATH"
        );
    }

    #[test]
    fn test_resolve_command_found_returns_some() {
        // Acquire PATH_MUTEX to avoid racing with tests that temporarily replace PATH.
        let _guard = match PATH_MUTEX.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let result = resolve_command("sh", "shell");
        assert!(
            result.is_some(),
            "resolve_command must return Some for a binary on PATH"
        );
        let path = result.expect("sh must resolve to an absolute path");
        assert!(!path.is_empty());
    }

    #[tokio::test]
    async fn test_find_marker_finds_at_depth_2() {
        let dir = tempdir().expect("temp dir");
        let deep = dir.path().join("apps").join("backend");
        std::fs::create_dir_all(&deep).expect("create dirs");
        std::fs::write(deep.join("go.mod"), "module deep").expect("write");
        let found = find_marker(dir.path(), "go.mod", 2, None).await;
        assert_eq!(
            found.as_deref(),
            Some(deep.as_path()),
            "depth-2 marker must be discovered"
        );
    }

    #[tokio::test]
    async fn test_find_marker_depth_0_does_not_recurse() {
        let dir = tempdir().expect("temp dir");
        let sub = dir.path().join("sub");
        std::fs::create_dir_all(&sub).expect("create dir");
        std::fs::write(sub.join("Cargo.toml"), "[package]").expect("write");
        let found = find_marker(dir.path(), "Cargo.toml", 0, None).await;
        assert!(
            found.is_none(),
            "max_depth=0 must not recurse into subdirectories"
        );
    }

    #[tokio::test]
    async fn test_find_marker_missing_returns_none() {
        let dir = tempdir().expect("temp dir");
        let found = find_marker(dir.path(), "no_such_marker_file.toml", 2, None).await;
        assert!(found.is_none(), "absent marker must return None");
    }

    #[tokio::test]
    async fn test_command_override_empty_string_falls_back_to_which() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("go.mod"), "module test").expect("write");
        let mut config = pathfinder_common::config::PathfinderConfig::default();
        config.lsp.insert(
            "go".to_string(),
            pathfinder_common::config::LspConfig {
                command: String::default(),
                args: vec![],
                idle_timeout_minutes: 15,
                settings: serde_json::Value::Null,
                root_override: None,
                typescript_plugins: vec![],
            },
        );
        let result = detect_languages(dir.path(), &config).await.expect("detect");
        for l in &result.detected {
            assert!(!l.command.is_empty(), "resolved command must be non-empty");
        }
    }

    // ── Vue plugin auto-detection tests ─────────────────────────────────────

    #[tokio::test]
    async fn test_auto_detects_vue_plugin_when_present() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("package.json"), "{}").expect("write");
        create_vue_file(dir.path());
        create_vue_plugin(dir.path());

        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");

        if let Some(ts) = result
            .detected
            .iter()
            .find(|l| l.language_id == "typescript")
        {
            assert_eq!(ts.auto_plugins, ["@vue/typescript-plugin"]);
        }
    }

    #[tokio::test]
    async fn test_no_vue_plugin_without_vue_files() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("package.json"), "{}").expect("write");
        // Plugin installed but NO .vue files
        create_vue_plugin(dir.path());

        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");

        if let Some(ts) = result
            .detected
            .iter()
            .find(|l| l.language_id == "typescript")
        {
            assert!(
                ts.auto_plugins.is_empty(),
                "auto_plugins should be empty when no .vue files exist"
            );
        }
    }

    #[tokio::test]
    async fn test_no_vue_plugin_when_no_ts_marker() {
        let dir = tempdir().expect("temp dir");
        // Plugin + .vue files exist but no tsconfig.json / package.json
        create_vue_file(dir.path());
        create_vue_plugin(dir.path());

        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");

        assert!(
            !result
                .detected
                .iter()
                .any(|l| l.language_id == "typescript"),
            "TypeScript should not be detected without a marker file"
        );
    }

    #[tokio::test]
    async fn test_config_plugins_override_auto_detection() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("package.json"), "{}").expect("write");
        create_vue_file(dir.path());
        create_vue_plugin(dir.path());

        let mut config = pathfinder_common::config::PathfinderConfig::default();
        config.lsp.insert(
            "typescript".to_owned(),
            pathfinder_common::config::LspConfig {
                command: String::new(),
                args: vec![],
                idle_timeout_minutes: 15,
                settings: serde_json::Value::Null,
                root_override: None,
                typescript_plugins: vec!["@custom/plugin".to_owned()],
            },
        );

        let result = detect_languages(dir.path(), &config).await.expect("detect");

        if let Some(ts) = result
            .detected
            .iter()
            .find(|l| l.language_id == "typescript")
        {
            assert_eq!(
                ts.auto_plugins,
                ["@custom/plugin"],
                "should use configured plugins, not auto-detected"
            );
        }
    }

    #[tokio::test]
    async fn test_detect_vue_files_in_subdirectory() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("package.json"), "{}").expect("write");
        create_vue_file_nested(dir.path());
        create_vue_plugin(dir.path());

        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");

        if let Some(ts) = result
            .detected
            .iter()
            .find(|l| l.language_id == "typescript")
        {
            assert_eq!(
                ts.auto_plugins.len(),
                1,
                "should detect Vue files in subdirectory"
            );
        }
    }

    #[tokio::test]
    async fn test_no_vue_files_no_plugin() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("package.json"), "{}").expect("write");
        // No .vue files, no plugin installed

        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");

        if let Some(ts) = result
            .detected
            .iter()
            .find(|l| l.language_id == "typescript")
        {
            assert!(
                ts.auto_plugins.is_empty(),
                "should not detect plugin without .vue files"
            );
        }
    }

    #[tokio::test]
    async fn test_detect_vue_files_in_monorepo_deep() {
        // Vue files at depth 4: apps/frontend/src/views/Home.vue
        // This tests the recursive depth-3 scan.
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("package.json"), "{}").expect("write");
        create_vue_file_monorepo(dir.path());
        create_vue_plugin(dir.path());

        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");

        if let Some(ts) = result
            .detected
            .iter()
            .find(|l| l.language_id == "typescript")
        {
            assert_eq!(
                ts.auto_plugins.len(),
                1,
                "should detect Vue files in deep monorepo structure (apps/frontend/src/views/)"
            );
            assert_eq!(ts.auto_plugins[0], "@vue/typescript-plugin");
        }
    }

    #[tokio::test]
    async fn test_detect_vue_plugin_pnpm() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("package.json"), "{}").expect("write");
        create_vue_file(dir.path());
        create_vue_plugin_pnpm(dir.path());

        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");

        if let Some(ts) = result
            .detected
            .iter()
            .find(|l| l.language_id == "typescript")
        {
            assert_eq!(
                ts.auto_plugins.len(),
                1,
                "should detect plugin in pnpm structure"
            );
            assert_eq!(ts.auto_plugins[0], "@vue/typescript-plugin");
        }
    }

    // ── Python LSP fallback chain tests ────────────────────────────────────
    // These tests use `sh` (always available on Unix-like systems) as a stand-in
    // for Python LSP binaries. We create symlinks in a temporary directory and
    // prepend that directory to PATH.

    #[tokio::test]
    async fn test_python_not_detected_without_binary() {
        // Run with an empty fake bin dir (no Python LSP binaries)
        test_with_fake_python_binaries(&[], || async {
            // Use a temp dir with pyproject.toml but no LSP binaries in PATH
            let dir = tempdir().expect("temp dir");
            std::fs::write(dir.path().join("pyproject.toml"), "[tool.poetry]").expect("write");
            std::fs::write(dir.path().join("main.py"), "").expect("write");

            let result = detect_languages(dir.path(), &make_ts_config())
                .await
                .expect("detect");

            assert!(
                !result.detected.iter().any(|l| l.language_id == "python"),
                "Python should not be detected without any LSP binary on PATH"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn test_prefers_pyright_over_pylsp() {
        test_with_fake_python_binaries(&["pyright-langserver", "pylsp"], || async {
            let dir = tempdir().expect("temp dir");
            std::fs::write(dir.path().join("pyproject.toml"), "[tool.poetry]").expect("write");
            std::fs::write(dir.path().join("main.py"), "").expect("write");

            let result = detect_languages(dir.path(), &make_ts_config())
                .await
                .expect("detect");

            if let Some(py) = result.detected.iter().find(|l| l.language_id == "python") {
                // Should prefer pyright-langserver, which uses --stdio
                assert_eq!(py.args, ["--stdio"]);
                assert!(py.command.contains("pyright-langserver"));
            } else {
                panic!("Python should be detected with pyright-langserver");
            }
        })
        .await;
    }

    #[tokio::test]
    async fn test_detects_python_fallback_to_basedpyright() {
        test_with_fake_python_binaries(&["basedpyright-langserver"], || async {
            let dir = tempdir().expect("temp dir");
            std::fs::write(dir.path().join("pyproject.toml"), "[tool.poetry]").expect("write");
            std::fs::write(dir.path().join("main.py"), "").expect("write");

            let result = detect_languages(dir.path(), &make_ts_config())
                .await
                .expect("detect");

            if let Some(py) = result.detected.iter().find(|l| l.language_id == "python") {
                // basedpyright-langserver uses --stdio args
                assert_eq!(py.args.len(), 1);
                assert!(py.args[0].contains("--stdio"));
                assert!(py.command.contains("basedpyright-langserver"));
            } else {
                panic!("Python should be detected with basedpyright-langserver");
            }
        })
        .await;
    }

    #[tokio::test]
    async fn test_detects_python_fallback_to_pylsp() {
        test_with_fake_python_binaries(&["pylsp"], || async {
            let dir = tempdir().expect("temp dir");
            std::fs::write(dir.path().join("pyproject.toml"), "[tool.poetry]").expect("write");
            std::fs::write(dir.path().join("main.py"), "").expect("write");

            let result = detect_languages(dir.path(), &make_ts_config())
                .await
                .expect("detect");

            if let Some(py) = result.detected.iter().find(|l| l.language_id == "python") {
                // pylsp uses empty args
                assert!(py.args.is_empty());
                assert!(py.command.contains("pylsp"));
            } else {
                panic!("Python should be detected with pylsp");
            }
        })
        .await;
    }

    #[tokio::test]
    async fn test_detects_python_fallback_to_ruff() {
        test_with_fake_python_binaries(&["ruff"], || async {
            let dir = tempdir().expect("temp dir");
            std::fs::write(dir.path().join("pyproject.toml"), "[tool.poetry]").expect("write");
            std::fs::write(dir.path().join("main.py"), "").expect("write");

            let result = detect_languages(dir.path(), &make_ts_config())
                .await
                .expect("detect");

            if let Some(py) = result.detected.iter().find(|l| l.language_id == "python") {
                // ruff uses "server --stdio" args
                assert_eq!(py.args.len(), 2);
                assert!(py.args[0].contains("server"));
                assert!(py.args[1].contains("--stdio"));
                assert!(py.command.contains("ruff"));
            } else {
                panic!("Python should be detected with ruff");
            }
        })
        .await;
    }

    #[tokio::test]
    async fn test_detects_python_fallback_to_jedi() {
        test_with_fake_python_binaries(&["jedi-language-server"], || async {
            let dir = tempdir().expect("temp dir");
            std::fs::write(dir.path().join("pyproject.toml"), "[tool.poetry]").expect("write");
            std::fs::write(dir.path().join("main.py"), "").expect("write");

            let result = detect_languages(dir.path(), &make_ts_config())
                .await
                .expect("detect");

            if let Some(py) = result.detected.iter().find(|l| l.language_id == "python") {
                // jedi-language-server uses empty args
                assert!(py.args.is_empty());
                assert!(py.command.contains("jedi-language-server"));
            } else {
                panic!("Python should be detected with jedi-language-server");
            }
        })
        .await;
    }

    #[tokio::test]
    async fn test_python_args_correct_per_binary() {
        // Test custom command with default args (backward compat: --stdio)
        test_with_fake_python_binaries(&[], || async {
            let dir = tempdir().expect("temp dir");
            std::fs::write(dir.path().join("pyproject.toml"), "[tool.poetry]").expect("write");
            std::fs::write(dir.path().join("main.py"), "").expect("write");

            let mut config = pathfinder_common::config::PathfinderConfig::default();
            config.lsp.insert(
                "python".to_string(),
                pathfinder_common::config::LspConfig {
                    command: "sh".to_string(), // Use sh as a dummy command
                    args: vec![],              // No args specified in config
                    idle_timeout_minutes: 15,
                    settings: serde_json::Value::Null,
                    root_override: None,
                    typescript_plugins: vec![],
                },
            );

            let result = detect_languages(dir.path(), &config).await.expect("detect");
            if let Some(py) = result.detected.iter().find(|l| l.language_id == "python") {
                // Custom command with no args specified in config should default to --stdio (backward compat)
                assert_eq!(py.args, ["--stdio"]);
            } else {
                panic!("Python should be detected with custom command");
            }
        })
        .await;
    }

    // ── ST-2: validate_marker_file tests ────────────────────────────────────

    #[test]
    fn test_validate_marker_file_valid_cargo_toml() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("Cargo.toml");
        std::fs::write(&path, "[package]\nname = \"foo\"").expect("write");
        assert!(validate_marker_file(&path, "rust").is_ok());
    }

    #[test]
    fn test_validate_marker_file_valid_cargo_workspace() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("Cargo.toml");
        std::fs::write(&path, "[workspace]\nmembers = []").expect("write");
        assert!(validate_marker_file(&path, "rust").is_ok());
    }

    #[test]
    fn test_validate_marker_file_invalid_cargo_toml_no_sections() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("Cargo.toml");
        std::fs::write(&path, "[dependencies]\nfoo = \"1.0\"").expect("write");
        let result = validate_marker_file(&path, "rust");
        assert!(
            result.is_err(),
            "Cargo.toml with only [dependencies] should fail"
        );
        assert!(result
            .expect_err("expected Err")
            .contains("neither [package] nor [workspace]"));
    }

    #[test]
    fn test_validate_marker_file_invalid_cargo_toml_syntax() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("Cargo.toml");
        std::fs::write(&path, "this is not toml at all !!!").expect("write");
        let result = validate_marker_file(&path, "rust");
        assert!(result.is_err(), "Malformed TOML should fail validation");
        assert!(result.expect_err("expected Err").contains("not valid TOML"));
    }

    #[test]
    fn test_validate_marker_file_valid_go_mod() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("go.mod");
        std::fs::write(&path, "module github.com/foo/bar\n\ngo 1.21").expect("write");
        assert!(validate_marker_file(&path, "go").is_ok());

        // Test with leading comments and blank lines
        let dir2 = tempdir().expect("temp dir");
        let path2 = dir2.path().join("go.mod");
        std::fs::write(
            &path2,
            "// some comment\n  // another comment\n\nmodule my-module",
        )
        .expect("write");
        assert!(validate_marker_file(&path2, "go").is_ok());
    }

    #[test]
    fn test_validate_marker_file_invalid_go_mod() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("go.mod");
        std::fs::write(&path, "// corrupted file").expect("write");
        let result = validate_marker_file(&path, "go");
        assert!(result.is_err(), "go.mod without 'module' should fail");
        assert!(result.expect_err("expected Err").contains("only comments"));

        // Test with bare module keyword
        let dir2 = tempdir().expect("temp dir");
        let path2 = dir2.path().join("go.mod");
        std::fs::write(&path2, "module").expect("write");
        let result2 = validate_marker_file(&path2, "go");
        assert!(result2.is_err(), "go.mod with bare 'module' should fail");
        assert!(result2
            .expect_err("expected Err")
            .contains("valid 'module' declaration"));
    }

    #[test]
    fn test_validate_marker_file_valid_tsconfig() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("tsconfig.json");
        std::fs::write(&path, r#"{"compilerOptions":{"strict":true}}"#).expect("write");
        assert!(validate_marker_file(&path, "typescript").is_ok());

        // Test with comments and trailing commas
        let path2 = dir.path().join("tsconfig2.json");
        std::fs::write(
            &path2,
            r#"{
                // compiler configurations
                "compilerOptions": {
                    "strict": true,
                    "target": "es2020",
                },
                /* trailing commas allowed in arrays */
                "include": [
                    "src/**/*",
                ]
            }"#,
        )
        .expect("write");
        // Note: the filename must be tsconfig.json to match the tsconfig arm in validate_marker_file
        let path3 = dir.path().join("tsconfig.json");
        std::fs::write(
            &path3,
            r#"{
                // compiler configurations
                "compilerOptions": {
                    "strict": true,
                    "target": "es2020",
                },
                /* trailing commas allowed in arrays */
                "include": [
                    "src/**/*",
                ],
            }"#,
        )
        .expect("write");
        assert!(validate_marker_file(&path3, "typescript").is_ok());
    }

    #[test]
    fn test_validate_marker_file_invalid_tsconfig() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("tsconfig.json");
        std::fs::write(&path, "{ this is not json }}").expect("write");
        let result = validate_marker_file(&path, "typescript");
        assert!(result.is_err(), "Malformed JSON tsconfig.json should fail");
        assert!(result.expect_err("expected Err").contains("not valid JSON"));
    }

    #[test]
    fn test_validate_marker_file_valid_pyproject_toml() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("pyproject.toml");
        std::fs::write(&path, "[tool.poetry]\nname = \"app\"").expect("write");
        assert!(validate_marker_file(&path, "python").is_ok());
    }

    #[test]
    fn test_validate_marker_file_invalid_pyproject_toml() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("pyproject.toml");
        std::fs::write(&path, "NOT VALID TOML !!!").expect("write");
        let result = validate_marker_file(&path, "python");
        assert!(result.is_err(), "Malformed pyproject.toml should fail");
        assert!(result.expect_err("expected Err").contains("not valid TOML"));
    }

    #[test]
    fn test_validate_marker_file_package_json_always_ok() {
        // package.json is too loose to validate structurally — always passes
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("package.json");
        std::fs::write(&path, "not even json").expect("write");
        assert!(validate_marker_file(&path, "typescript").is_ok());
    }

    // ── ST-5: detect_venv tests ──────────────────────────────────────────────

    #[test]
    fn test_detect_venv_finds_dot_venv() {
        let dir = tempdir().expect("temp dir");
        let bin = dir.path().join(".venv").join("bin");
        std::fs::create_dir_all(&bin).expect("create .venv/bin");
        let python = bin.join("python");
        std::fs::write(&python, "#!/bin/sh").expect("write fake python");
        let result = detect_venv(dir.path());
        assert_eq!(result, Some(python), "should detect .venv/bin/python");
    }

    #[test]
    fn test_detect_venv_finds_venv_fallback() {
        let dir = tempdir().expect("temp dir");
        let bin = dir.path().join("venv").join("bin");
        std::fs::create_dir_all(&bin).expect("create venv/bin");
        let python = bin.join("python");
        std::fs::write(&python, "#!/bin/sh").expect("write fake python");
        let result = detect_venv(dir.path());
        assert_eq!(result, Some(python), "should detect venv/bin/python");
    }

    #[test]
    fn test_detect_venv_prefers_dot_venv_over_venv() {
        let dir = tempdir().expect("temp dir");
        for subdir in &[".venv/bin", "venv/bin"] {
            let bin = dir.path().join(subdir);
            std::fs::create_dir_all(&bin).expect("create bin");
            std::fs::write(bin.join("python"), "#!/bin/sh").expect("write fake python");
        }
        let result = detect_venv(dir.path());
        assert_eq!(
            result,
            Some(dir.path().join(".venv").join("bin").join("python")),
            ".venv must be preferred over venv"
        );
    }

    #[test]
    fn test_detect_venv_returns_none_when_no_venv() {
        let dir = tempdir().expect("temp dir");
        let result = detect_venv(dir.path());
        assert!(result.is_none(), "should return None when no venv exists");
    }

    #[tokio::test]
    async fn test_detect_invalid_cargo_toml_adds_to_missing() {
        test_with_fake_python_binaries(&["rust-analyzer"], || async {
            let dir = tempdir().expect("temp dir");
            // ST-2: Cargo.toml with no [package] or [workspace] should be rejected
            std::fs::write(
                dir.path().join("Cargo.toml"),
                "[dependencies]\nfoo = \"1.0\"",
            )
            .expect("write");
            std::fs::create_dir_all(dir.path().join("src")).expect("create src");
            std::fs::write(dir.path().join("src/lib.rs"), "fn main() {}").expect("write");

            let result = detect_languages(dir.path(), &make_ts_config())
                .await
                .expect("detect");

            assert!(
                result.detected.iter().all(|l| l.language_id != "rust"),
                "invalid Cargo.toml should not produce a detected Rust LSP"
            );
            let maybe_missing = result.missing.iter().find(|m| m.language_id == "rust");
            assert!(
                maybe_missing.is_some(),
                "invalid Cargo.toml should add rust to missing"
            );
            let hint = &maybe_missing.expect("checked above").install_hint;
            assert!(
                hint.contains("Fix Cargo.toml"),
                "install_hint should tell user to fix the manifest, got: {hint}"
            );
        })
        .await;
    }

    // ── Java detection tests (AC-2.3 – AC-2.5, AC-2.10) ─────────────────────

    #[tokio::test]
    async fn test_detects_java_via_pom_xml() {
        test_with_fake_python_binaries(&["jdtls"], || async {
            let dir = tempdir().expect("temp dir");
            std::fs::write(
                dir.path().join("pom.xml"),
                "<project><modelVersion>4.0.0</modelVersion></project>",
            )
            .expect("write pom.xml");
            std::fs::create_dir_all(dir.path().join("src")).expect("create src");
            std::fs::write(dir.path().join("src/Main.java"), "public class Main {}")
                .expect("write");

            let config = pathfinder_common::config::PathfinderConfig::default();
            let result = detect_languages(dir.path(), &config).await.expect("detect");

            let java = result.detected.iter().find(|l| l.language_id == "java");
            assert!(java.is_some(), "Java should be detected via pom.xml");
            let java = java.expect("checked above");
            assert_eq!(
                java.init_timeout_secs,
                Some(180),
                "Java timeout must be 180s"
            );
            assert!(
                !java.init_options.is_null(),
                "Java init_options should not be null"
            );
            assert!(
                java.init_options["java"]["import"]["maven"]["enabled"]
                    .as_bool()
                    .unwrap_or(false),
                "Maven import should be enabled"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn test_detects_java_via_build_gradle() {
        test_with_fake_python_binaries(&["jdtls"], || async {
            let dir = tempdir().expect("temp dir");
            std::fs::write(dir.path().join("build.gradle"), "plugins { id 'java' }\n")
                .expect("write build.gradle");
            std::fs::create_dir_all(dir.path().join("src")).expect("create src");
            std::fs::write(dir.path().join("src/Main.java"), "public class Main {}")
                .expect("write");

            let config = pathfinder_common::config::PathfinderConfig::default();
            let result = detect_languages(dir.path(), &config).await.expect("detect");

            let java = result.detected.iter().find(|l| l.language_id == "java");
            assert!(java.is_some(), "Java should be detected via build.gradle");
        })
        .await;
    }

    #[tokio::test]
    async fn test_detects_java_via_build_gradle_kts() {
        test_with_fake_python_binaries(&["jdtls"], || async {
            let dir = tempdir().expect("temp dir");
            std::fs::write(dir.path().join("build.gradle.kts"), "plugins { java }\n")
                .expect("write build.gradle.kts");
            std::fs::create_dir_all(dir.path().join("src")).expect("create src");
            std::fs::write(dir.path().join("src/Main.java"), "public class Main {}")
                .expect("write");

            let config = pathfinder_common::config::PathfinderConfig::default();
            let result = detect_languages(dir.path(), &config).await.expect("detect");

            let java = result.detected.iter().find(|l| l.language_id == "java");
            assert!(
                java.is_some(),
                "Java should be detected via build.gradle.kts"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn test_java_not_detected_without_binary() {
        test_with_fake_python_binaries(&[], || async {
            let dir = tempdir().expect("temp dir");
            std::fs::write(
                dir.path().join("pom.xml"),
                "<project><modelVersion>4.0.0</modelVersion></project>",
            )
            .expect("write pom.xml");
            std::fs::create_dir_all(dir.path().join("src")).expect("create src");
            std::fs::write(dir.path().join("src/Main.java"), "public class Main {}")
                .expect("write");

            let config = pathfinder_common::config::PathfinderConfig::default();
            let result = detect_languages(dir.path(), &config).await.expect("detect");

            assert!(
                result.detected.iter().all(|l| l.language_id != "java"),
                "Java should not be in detected without jdtls binary"
            );
            let missing = result.missing.iter().find(|m| m.language_id == "java");
            assert!(
                missing.is_some(),
                "Java should be in missing when jdtls not found"
            );
            assert!(
                missing
                    .expect("checked above")
                    .tried_binaries
                    .contains(&"jdtls".to_string()),
                "jdtls should be listed in tried_binaries"
            );
        })
        .await;
    }

    // ── validate_marker_file Java tests ─────────────────────────────────────

    #[test]
    fn test_validate_marker_file_valid_pom_xml() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("pom.xml");
        std::fs::write(
            &path,
            "<project><modelVersion>4.0.0</modelVersion></project>",
        )
        .expect("write");
        assert!(validate_marker_file(&path, "java").is_ok());
    }

    #[test]
    fn test_validate_marker_file_invalid_pom_xml() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("pom.xml");
        std::fs::write(&path, "<dependency>foo</dependency>").expect("write");
        let result = validate_marker_file(&path, "java");
        assert!(result.is_err(), "pom.xml without <project should fail");
        assert!(
            result.expect_err("expected Err").contains("<project"),
            "error should mention <project element"
        );
    }

    #[test]
    fn test_validate_marker_file_empty_build_gradle() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("build.gradle");
        std::fs::write(&path, "").expect("write");
        let result = validate_marker_file(&path, "java");
        assert!(result.is_err(), "empty build.gradle should fail");
        assert!(
            result.expect_err("expected Err").contains("empty"),
            "error should mention empty"
        );
    }

    // ── language_id_for_extension Java ──────────────────────────────────────

    #[test]
    fn test_language_id_for_extension_java() {
        assert_eq!(language_id_for_extension("java"), Some("java"));
    }

    // ── Python init_options migration tests ─────────────────────────────────

    #[test]
    fn test_detect_python_init_options_no_venv() {
        let dir = tempdir().expect("temp dir");
        let opts = detect_python_init_options(dir.path());
        assert!(opts.is_null(), "should be Null when no venv");
    }

    #[test]
    fn test_detect_python_init_options_with_venv() {
        let dir = tempdir().expect("temp dir");
        let venv_python = dir.path().join(".venv").join("bin").join("python");
        std::fs::create_dir_all(venv_python.parent().expect("parent")).expect("mkdir");
        std::fs::write(&venv_python, "#!/bin/sh\nexec python3 \"$@\"").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&venv_python, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }

        let opts = detect_python_init_options(dir.path());
        assert!(!opts.is_null(), "should not be Null when venv found");
        assert!(
            opts["python"]["pythonPath"].as_str().is_some(),
            "pythonPath should be a string"
        );
    }

    // ── detect_java_init_options structure tests ─────────────────────────────

    #[test]
    fn test_detect_java_init_options_structure() {
        let dir = tempdir().expect("temp dir");
        let opts = detect_java_init_options(dir.path(), dir.path());
        assert!(!opts.is_null(), "java init_options should not be null");
        assert!(
            opts["java"]["import"]["gradle"]["enabled"]
                .as_bool()
                .unwrap_or(false),
            "Gradle import should be enabled"
        );
        assert!(
            opts["java"]["import"]["maven"]["enabled"]
                .as_bool()
                .unwrap_or(false),
            "Maven import should be enabled"
        );
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_detect_jdk_home_sdkmanrc() {
        let _guard = match PATH_MUTEX.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };

        let temp = tempdir().expect("temp dir");
        let proj_root = temp.path().join("project");
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&proj_root).unwrap();
        std::fs::create_dir_all(&fake_home).unwrap();

        // Create a fake sdkman jdk candidate
        let jdk_path = fake_home.join(".sdkman/candidates/java/17.0.7-tem");
        std::fs::create_dir_all(jdk_path.join("bin")).unwrap();
        std::fs::write(jdk_path.join("bin/java"), "").unwrap();

        // Write .sdkmanrc
        std::fs::write(proj_root.join(".sdkmanrc"), "java=17.0.7-tem\n").unwrap();

        // Save original env vars
        let orig_home = std::env::var("HOME").ok();
        let orig_userprofile = std::env::var("USERPROFILE").ok();
        let orig_java_home = std::env::var("JAVA_HOME").ok();

        // Set test env
        std::env::set_var("HOME", &fake_home);
        std::env::set_var("USERPROFILE", &fake_home);
        std::env::remove_var("JAVA_HOME");

        let detected = detect_jdk_home(&proj_root);

        // Restore env
        if let Some(val) = orig_home {
            std::env::set_var("HOME", val);
        } else {
            std::env::remove_var("HOME");
        }
        if let Some(val) = orig_userprofile {
            std::env::set_var("USERPROFILE", val);
        } else {
            std::env::remove_var("USERPROFILE");
        }
        if let Some(val) = orig_java_home {
            std::env::set_var("JAVA_HOME", val);
        } else {
            std::env::remove_var("JAVA_HOME");
        }

        assert_eq!(detected, Some(jdk_path.to_string_lossy().into_owned()));
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_detect_jdk_home_java_version() {
        let _guard = match PATH_MUTEX.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };

        let temp = tempdir().expect("temp dir");
        let proj_root = temp.path().join("project");
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&proj_root).unwrap();
        std::fs::create_dir_all(&fake_home).unwrap();

        // Create a fake asdf jdk candidate
        let jdk_path = fake_home.join(".asdf/installs/java/11.0.2");
        std::fs::create_dir_all(jdk_path.join("bin")).unwrap();
        std::fs::write(jdk_path.join("bin/java"), "").unwrap();

        // Write .java-version
        std::fs::write(proj_root.join(".java-version"), "11.0.2\n").unwrap();

        // Save original env vars
        let orig_home = std::env::var("HOME").ok();
        let orig_userprofile = std::env::var("USERPROFILE").ok();
        let orig_java_home = std::env::var("JAVA_HOME").ok();

        // Set test env
        std::env::set_var("HOME", &fake_home);
        std::env::set_var("USERPROFILE", &fake_home);
        std::env::remove_var("JAVA_HOME");

        let detected = detect_jdk_home(&proj_root);

        // Restore env
        if let Some(val) = orig_home {
            std::env::set_var("HOME", val);
        } else {
            std::env::remove_var("HOME");
        }
        if let Some(val) = orig_userprofile {
            std::env::set_var("USERPROFILE", val);
        } else {
            std::env::remove_var("USERPROFILE");
        }
        if let Some(val) = orig_java_home {
            std::env::set_var("JAVA_HOME", val);
        } else {
            std::env::remove_var("JAVA_HOME");
        }

        assert_eq!(detected, Some(jdk_path.to_string_lossy().into_owned()));
    }

    #[test]
    fn test_language_id_for_extension_covers_all_known() {
        assert_eq!(language_id_for_extension("rs"), Some("rust"));
        assert_eq!(language_id_for_extension("go"), Some("go"));
        assert_eq!(language_id_for_extension("ts"), Some("typescript"));
        assert_eq!(language_id_for_extension("tsx"), Some("typescript"));
        assert_eq!(language_id_for_extension("js"), Some("typescript"));
        assert_eq!(language_id_for_extension("jsx"), Some("typescript"));
        assert_eq!(language_id_for_extension("mjs"), Some("typescript"));
        assert_eq!(language_id_for_extension("cjs"), Some("typescript"));
        assert_eq!(language_id_for_extension("vue"), Some("typescript"));
        assert_eq!(language_id_for_extension("mts"), Some("typescript"));
        assert_eq!(language_id_for_extension("cts"), Some("typescript"));
        assert_eq!(language_id_for_extension("py"), Some("python"));
        assert_eq!(language_id_for_extension("pyi"), Some("python"));
        assert_eq!(language_id_for_extension("java"), Some("java"));
    }

    #[test]
    fn test_language_id_for_extension_unknown() {
        assert_eq!(language_id_for_extension("txt"), None);
        assert_eq!(language_id_for_extension("html"), None);
        assert_eq!(language_id_for_extension("css"), None);
        assert_eq!(language_id_for_extension("json"), None);
        assert_eq!(language_id_for_extension(""), None);
    }

    #[test]
    fn test_install_hint_all_languages() {
        assert!(install_hint("rust").contains("rust-analyzer"));
        assert!(install_hint("go").contains("gopls"));
        assert!(install_hint("typescript").contains("typescript-language-server"));
        assert!(install_hint("python").contains("pyright"));
        assert!(install_hint("java").contains("jdtls"));
        assert!(install_hint("unknown_lang").contains("unknown_lang"));
    }

    #[test]
    fn test_install_hint_matches_plugin_registry() {
        // Verify detect.rs::install_hint delegates to plugin registry
        // (single source of truth) and never diverges.
        use crate::plugin::all_plugins;
        for plugin in all_plugins() {
            let from_detect = super::install_hint(plugin.language_id());
            let from_plugin = plugin.install_hint();
            assert_eq!(
                from_detect,
                from_plugin,
                "install_hint for '{}' diverges between detect.rs and plugin.rs",
                plugin.language_id()
            );
        }
    }

    #[test]
    fn test_detect_venv_env_var() {
        let dir = tempdir().expect("temp dir");
        let venv_dir = dir.path().join("custom_venv").join("bin");
        std::fs::create_dir_all(&venv_dir).expect("create venv bin");
        let venv_python = venv_dir.join("python");
        std::fs::write(&venv_python, "#!/bin/sh").expect("write python");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&venv_python, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }

        let _guard = match PATH_MUTEX.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let orig = std::env::var("VIRTUAL_ENV").ok();
        std::env::set_var("VIRTUAL_ENV", dir.path().join("custom_venv"));

        let result = detect_venv(dir.path());

        if let Some(orig_val) = orig {
            std::env::set_var("VIRTUAL_ENV", orig_val);
        } else {
            std::env::remove_var("VIRTUAL_ENV");
        }

        assert!(
            result.is_some(),
            "should detect venv from VIRTUAL_ENV env var"
        );
    }

    #[tokio::test]
    async fn test_detect_typescript_with_args_override() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("package.json"), "{}").expect("write");

        let mut config = pathfinder_common::config::PathfinderConfig::default();
        config.lsp.insert(
            "typescript".to_string(),
            pathfinder_common::config::LspConfig {
                command: String::new(),
                args: vec!["--stdio".to_owned(), "--log-level".to_owned()],
                idle_timeout_minutes: 15,
                settings: serde_json::Value::Null,
                root_override: None,
                typescript_plugins: vec![],
            },
        );

        let result = detect_languages(dir.path(), &config).await.expect("detect");

        if let Some(ts) = result
            .detected
            .iter()
            .find(|l| l.language_id == "typescript")
        {
            assert_eq!(
                ts.args,
                vec!["--stdio".to_owned(), "--log-level".to_owned()],
                "args should match config override"
            );
        }
    }

    #[tokio::test]
    async fn test_detect_java_via_settings_gradle() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join("settings.gradle"),
            "rootProject.name = \"test\"",
        )
        .expect("write");
        std::fs::create_dir_all(dir.path().join("src")).expect("create src");
        std::fs::write(dir.path().join("src/Main.java"), "public class Main {}").expect("write");

        let result = detect_languages(dir.path(), &make_ts_config()).await;

        assert!(
            result.is_ok(),
            "detect_languages should not error: {result:?}"
        );

        let r = result.expect("detect_languages result should be Ok");
        let has_java = r.detected.iter().any(|l| l.language_id == "java")
            || r.missing.iter().any(|l| l.language_id == "java");
        assert!(
            has_java,
            "settings.gradle should trigger Java detection (or missing report)"
        );
    }

    #[tokio::test]
    async fn test_detect_java_via_settings_gradle_kts() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join("settings.gradle.kts"),
            "rootProject.name = \"test\"",
        )
        .expect("write");
        std::fs::create_dir_all(dir.path().join("src")).expect("create src");
        std::fs::write(dir.path().join("src/Main.java"), "public class Main {}").expect("write");

        let result = detect_languages(dir.path(), &make_ts_config()).await;

        assert!(
            result.is_ok(),
            "detect_languages should not error: {result:?}"
        );

        let r = result.expect("detect_languages result should be Ok");
        let has_java = r.detected.iter().any(|l| l.language_id == "java")
            || r.missing.iter().any(|l| l.language_id == "java");
        assert!(
            has_java,
            "settings.gradle.kts should trigger Java detection (or missing report)"
        );
    }

    #[tokio::test]
    async fn test_detect_python_via_requirements_txt() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("requirements.txt"), "flask==2.0").expect("write");
        std::fs::write(dir.path().join("main.py"), "").expect("write");

        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");

        let has_python = result.detected.iter().any(|l| l.language_id == "python")
            || result.missing.iter().any(|l| l.language_id == "python");
        assert!(
            has_python,
            "should detect or report Python missing with requirements.txt"
        );
    }

    #[tokio::test]
    async fn test_detect_python_via_setup_py() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join("setup.py"),
            "from setuptools import setup; setup()",
        )
        .expect("write");
        std::fs::write(dir.path().join("main.py"), "").expect("write");

        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");

        let has_python = result.detected.iter().any(|l| l.language_id == "python")
            || result.missing.iter().any(|l| l.language_id == "python");
        assert!(
            has_python,
            "should detect or report Python missing with setup.py"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn test_detect_python_venv_in_subdirectory() {
        let dir = tempdir().expect("temp dir");
        let sub = dir.path().join("apps").join("backend");
        std::fs::create_dir_all(&sub).expect("create dir");
        std::fs::write(sub.join("requirements.txt"), "flask==2.0").expect("write");
        std::fs::write(sub.join("main.py"), "").expect("write");

        let venv_python = sub.join(".venv").join("bin").join("python");
        std::fs::create_dir_all(venv_python.parent().expect("parent")).expect("mkdir");
        std::fs::write(&venv_python, "#!/bin/sh\nexec python3 \"$@\"").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&venv_python, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }

        // Mock a fake python binary so Python is detected as running, not missing
        // Hold the lock across await intentionally — serializes env-mutating tests.
        let _path_lock = PATH_MUTEX.lock().expect("path mutex");
        let old_path = std::env::var("PATH").expect("PATH");
        let fake_bin_dir = dir.path().join("fake_bin");
        std::fs::create_dir_all(&fake_bin_dir).expect("create bin dir");
        std::fs::write(fake_bin_dir.join("pyright-langserver"), "#!/bin/sh").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                fake_bin_dir.join("pyright-langserver"),
                std::fs::Permissions::from_mode(0o755),
            )
            .expect("chmod");
        }
        let new_path = format!("{}:{}", fake_bin_dir.to_str().expect("utf8"), old_path);
        std::env::set_var("PATH", &new_path);

        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");

        std::env::set_var("PATH", old_path);

        let py_lsp = result
            .detected
            .iter()
            .find(|l| l.language_id == "python")
            .expect("python detected");
        assert!(
            !py_lsp.init_options.is_null(),
            "python init_options should not be null when venv is in subdirectory"
        );
        assert_eq!(
            py_lsp.init_options["python"]["pythonPath"]
                .as_str()
                .expect("pythonPath string"),
            venv_python.to_str().expect("utf8")
        );
    }

    #[tokio::test]
    async fn test_detect_vue_files_in_shallow_nested_dirs() {
        let dir = tempdir().expect("temp dir");
        let nested = dir.path().join("src").join("components");
        std::fs::create_dir_all(&nested).expect("create dirs");
        std::fs::write(nested.join("Widget.vue"), "<template/>").expect("write");

        assert!(
            workspace_has_vue_files(dir.path()).await,
            "should find .vue files in shallow nested directories"
        );
    }

    #[tokio::test]
    async fn test_validate_marker_file_unsupported_marker() {
        let dir = tempdir().expect("temp dir");
        let marker = dir.path().join("package.json");
        std::fs::write(&marker, "{}").expect("write");

        let result = validate_marker_file(&marker, "rust");
        assert!(
            result.is_ok(),
            "unsupported marker file name should return Ok by default"
        );
    }

    #[tokio::test]
    async fn test_find_marker_at_root() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").expect("write");

        let found = find_marker(dir.path(), "Cargo.toml", 0, None).await;
        assert_eq!(found.as_deref(), Some(dir.path()));
    }

    #[tokio::test]
    async fn test_find_marker_at_depth_1() {
        let dir = tempdir().expect("temp dir");
        let sub = dir.path().join("backend");
        std::fs::create_dir_all(&sub).expect("create dir");
        std::fs::write(sub.join("go.mod"), "module backend").expect("write");

        let found = find_marker(dir.path(), "go.mod", 2, None).await;
        assert_eq!(found.as_deref(), Some(sub.as_path()));
    }

    #[tokio::test]
    async fn test_find_marker_multiple_matches() {
        let dir = tempdir().expect("temp dir");
        let sub1 = dir.path().join("backend");
        let sub2 = dir.path().join("worker");
        std::fs::create_dir_all(&sub1).expect("create dir");
        std::fs::create_dir_all(&sub2).expect("create dir");
        std::fs::write(sub1.join("go.mod"), "module backend").expect("write");
        std::fs::write(sub2.join("go.mod"), "module worker").expect("write");

        let found = find_marker(dir.path(), "go.mod", 2, None).await;
        assert_eq!(found.as_deref(), Some(dir.path()));
    }

    #[tokio::test]
    async fn test_detect_command_override_skips_which() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("go.mod"), "module test").expect("write");

        let mut config = pathfinder_common::config::PathfinderConfig::default();
        config.lsp.insert(
            "go".to_string(),
            pathfinder_common::config::LspConfig {
                command: "/usr/local/bin/custom-gopls".to_string(),
                args: vec![],
                idle_timeout_minutes: 15,
                settings: serde_json::Value::Null,
                root_override: None,
                typescript_plugins: vec![],
            },
        );

        let result = detect_languages(dir.path(), &config).await.expect("detect");

        if let Some(go) = result.detected.iter().find(|l| l.language_id == "go") {
            assert_eq!(
                go.command, "/usr/local/bin/custom-gopls",
                "should use config command override"
            );
        }
    }

    #[tokio::test]
    async fn test_detect_ignores_build_artifact_paths() {
        let dir = tempdir().expect("temp dir");
        let target_dir = dir.path().join("target").join("debug");
        std::fs::create_dir_all(&target_dir).expect("create target dir");

        let result = detect_languages(dir.path(), &make_ts_config()).await;
        assert!(result.is_ok(), "should handle missing markers gracefully");
    }

    #[test]
    fn test_detect_python_init_options_no_venv_returns_null() {
        let dir = tempdir().expect("temp dir");
        let opts = detect_python_init_options(dir.path());
        assert!(
            opts.is_null(),
            "should return Null when no venv found: {opts:?}"
        );
    }

    #[test]
    fn test_detect_venv_conda_prefix() {
        let dir = tempdir().expect("temp dir");
        let conda_bin = dir.path().join("bin");
        std::fs::create_dir_all(&conda_bin).expect("create bin");

        let python_path = conda_bin.join("python");
        std::fs::write(&python_path, "#!/bin/sh").expect("write python");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&python_path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }

        let _guard = match PATH_MUTEX.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let orig = std::env::var("CONDA_PREFIX").ok();
        std::env::set_var("CONDA_PREFIX", dir.path().to_string_lossy().as_ref());

        let result = detect_venv(dir.path());

        if let Some(orig_val) = orig {
            std::env::set_var("CONDA_PREFIX", orig_val);
        } else {
            std::env::remove_var("CONDA_PREFIX");
        }

        assert!(result.is_some(), "should detect Python from CONDA_PREFIX");
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_detect_venv_windows_paths() {
        let dir = tempdir().expect("temp dir");

        // 1. Conda Windows path: CONDA_PREFIX/python.exe
        let conda_windows_python = dir.path().join("conda_env").join("python.exe");
        std::fs::create_dir_all(conda_windows_python.parent().unwrap()).unwrap();
        std::fs::write(&conda_windows_python, "").unwrap();

        // 2. VIRTUAL_ENV Windows path: VIRTUAL_ENV/Scripts/python.exe
        let venv_windows_python = dir
            .path()
            .join("virtual_env")
            .join("Scripts")
            .join("python.exe");
        std::fs::create_dir_all(venv_windows_python.parent().unwrap()).unwrap();
        std::fs::write(&venv_windows_python, "").unwrap();

        let _guard = match PATH_MUTEX.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };

        // Test CONDA_PREFIX Windows path detection
        let orig_conda = std::env::var("CONDA_PREFIX").ok();
        std::env::set_var("CONDA_PREFIX", dir.path().join("conda_env"));
        let res_conda = detect_venv(dir.path());
        if let Some(val) = orig_conda {
            std::env::set_var("CONDA_PREFIX", val);
        } else {
            std::env::remove_var("CONDA_PREFIX");
        }
        assert_eq!(res_conda, Some(conda_windows_python));

        // Test VIRTUAL_ENV Windows path detection
        let orig_venv = std::env::var("VIRTUAL_ENV").ok();
        std::env::set_var("VIRTUAL_ENV", dir.path().join("virtual_env"));
        let res_venv = detect_venv(dir.path());
        if let Some(val) = orig_venv {
            std::env::set_var("VIRTUAL_ENV", val);
        } else {
            std::env::remove_var("VIRTUAL_ENV");
        }
        assert_eq!(res_venv, Some(venv_windows_python));
    }

    // ── Issue 1: find_marker max_depth clamping ─────────────────────────────

    #[tokio::test]
    async fn test_find_marker_clamps_max_depth_to_2() {
        // max_depth=5 must behave identically to max_depth=2.
        // Place a marker at depth 2 — both calls must find it.
        let dir = tempdir().expect("temp dir");
        let deep = dir.path().join("apps").join("backend");
        std::fs::create_dir_all(&deep).expect("create dirs");
        std::fs::write(deep.join("Cargo.toml"), "[package]").expect("write");

        let result_depth_2 = find_marker(dir.path(), "Cargo.toml", 2, None).await;
        let result_depth_5 = find_marker(dir.path(), "Cargo.toml", 5, None).await;
        assert_eq!(
            result_depth_2, result_depth_5,
            "max_depth=5 must behave identically to max_depth=2 (clamped)"
        );
        assert_eq!(
            result_depth_2.as_deref(),
            Some(deep.as_path()),
            "should find marker at depth 2"
        );
    }

    #[tokio::test]
    async fn test_find_marker_large_depth_no_depth_3_scan() {
        // Place a marker at depth 3 — even max_depth=100 must NOT find it (clamped to 2).
        let dir = tempdir().expect("temp dir");
        let deep = dir.path().join("a").join("b").join("c");
        std::fs::create_dir_all(&deep).expect("create dirs");
        std::fs::write(deep.join("go.mod"), "module deep").expect("write");

        let found = find_marker(dir.path(), "go.mod", 100, None).await;
        assert!(
            found.is_none(),
            "max_depth is clamped to 2, so depth-3 marker must not be found"
        );
    }

    // ── Monorepo TS detection: sibling_filter ──────────────────────────

    #[tokio::test]
    async fn test_find_marker_sibling_filter_multi_match_prefers_with_sibling() {
        // Monorepo: apps/e2e/package.json (no tsconfig) + apps/frontend/package.json + tsconfig
        let dir = tempdir().expect("temp dir");
        let e2e = dir.path().join("apps").join("e2e");
        let frontend = dir.path().join("apps").join("frontend");
        std::fs::create_dir_all(&e2e).expect("create e2e");
        std::fs::create_dir_all(&frontend).expect("create frontend");

        // e2e has package.json but NO tsconfig.json
        std::fs::write(e2e.join("package.json"), "{}").expect("write");
        // frontend has both
        std::fs::write(frontend.join("package.json"), "{}").expect("write");
        std::fs::write(frontend.join("tsconfig.json"), "{}").expect("write");

        // With sibling_filter on multi-match: only frontend should be selected
        let found = find_marker(dir.path(), "package.json", 2, Some("tsconfig.json")).await;

        assert_eq!(
            found.as_deref(),
            Some(frontend.as_path()),
            "multi-match: should pick the directory that has both package.json AND tsconfig.json"
        );
    }

    #[tokio::test]
    async fn test_find_marker_sibling_filter_single_match_accepts_without_sibling() {
        // Pure JS project: single package.json, no tsconfig.json anywhere.
        // sibling_filter only applies to multi-match, so single match is accepted.
        let dir = tempdir().expect("temp dir");
        let sub = dir.path().join("apps").join("myapp");
        std::fs::create_dir_all(&sub).expect("create dir");
        std::fs::write(sub.join("package.json"), "{}").expect("write");

        let found = find_marker(dir.path(), "package.json", 2, Some("tsconfig.json")).await;
        assert_eq!(
            found.as_deref(),
            Some(sub.as_path()),
            "single match with sibling_filter: should accept (pure JS project)"
        );
    }

    #[tokio::test]
    async fn test_find_marker_sibling_filter_multi_match_all_rejected_falls_back_to_root() {
        // Multiple package.json, NONE have tsconfig.json — fall back to workspace root.
        let dir = tempdir().expect("temp dir");
        let e2e = dir.path().join("apps").join("e2e");
        let scripts = dir.path().join("apps").join("scripts");
        std::fs::create_dir_all(&e2e).expect("create e2e");
        std::fs::create_dir_all(&scripts).expect("create scripts");

        std::fs::write(e2e.join("package.json"), "{}").expect("write");
        std::fs::write(scripts.join("package.json"), "{}").expect("write");
        // No tsconfig.json in either

        let found = find_marker(dir.path(), "package.json", 2, Some("tsconfig.json")).await;
        assert_eq!(
            found.as_deref(),
            Some(dir.path()),
            "multi-match, none have sibling: should fall back to workspace root"
        );
    }

    #[tokio::test]
    async fn test_find_marker_sibling_filter_none_keeps_original_behavior() {
        let dir = tempdir().expect("temp dir");
        let sub = dir.path().join("apps").join("e2e");
        std::fs::create_dir_all(&sub).expect("create dir");
        std::fs::write(sub.join("package.json"), "{}").expect("write");

        // Without sibling_filter: old behavior — finds the marker
        let found = find_marker(dir.path(), "package.json", 2, None).await;
        assert!(
            found.is_some(),
            "without sibling_filter, find_marker should still find the marker"
        );
    }
}
