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
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "detect_test.rs"]
mod tests;
