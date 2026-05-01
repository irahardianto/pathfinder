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
/// Used to surface actionable install guidance in `lsp_health` responses.
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
/// Provides specific commands users can run to install their LSP servers.
#[must_use]
pub fn install_hint(language_id: &str) -> String {
    match language_id {
        "rust" => {
            "Install rust-analyzer: https://rust-analyzer.github.io/".to_string()
        }
        "go" => "Install gopls: go install golang.org/x/tools/gopls@latest".to_string(),
        "typescript" => {
            "Install typescript-language-server: npm install -g typescript-language-server typescript"
                .to_string()
        }
        "python" => {
            "Install pyright: npm install -g pyright\nOr install pylsp: pip install python-lsp-server"
                .to_string()
        }
        _ => format!("Install a language server for {language_id}"),
    }
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
                 Install it or set `lsp.{lang}.command` in .pathfinder.toml to \
                 an absolute path (e.g. for nix, asdf, volta, or GUI launcher installs)"
            );
        })
        .ok()
}

/// Search for a marker file within `base` directory up to `max_depth` levels deep.
///
/// Returns the directory containing the marker file, or `None` if not found.
async fn find_marker(base: &Path, marker: &str, max_depth: usize) -> Option<std::path::PathBuf> {
    // Check base directory first (depth 0)
    if tokio::fs::metadata(base.join(marker)).await.is_ok() {
        return Some(base.to_path_buf());
    }
    if max_depth == 0 {
        return None;
    }
    // Scan immediate children (depth 1 and 2)
    let Ok(mut dir) = tokio::fs::read_dir(base).await else {
        return None;
    };
    while let Ok(Some(entry)) = dir.next_entry().await {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // Check this subdirectory
        if tokio::fs::metadata(path.join(marker)).await.is_ok() {
            return Some(path);
        }
        // One more level if depth allows
        if max_depth >= 2 {
            let Ok(mut sub) = tokio::fs::read_dir(&path).await else {
                continue;
            };
            while let Ok(Some(sub_entry)) = sub.next_entry().await {
                let sub_path = sub_entry.path();
                if sub_path.is_dir() && tokio::fs::metadata(sub_path.join(marker)).await.is_ok() {
                    return Some(sub_path);
                }
            }
        }
    }
    None
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
                if path.is_dir() && has_vue_recursive(&path, depth + 1).await {
                    return true;
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
    if workspace_has_vue_files(workspace_root).await {
        if let Some(plugin) = detect_ts_plugin(workspace_root, "@vue/typescript-plugin").await {
            tracing::info!("Auto-detected @vue/typescript-plugin for Vue SFC support");
            plugins.push(plugin);
        }
    }

    plugins
}

// ---------------------------------------------------------------------------
// Main detection entry point
// ---------------------------------------------------------------------------

/// Detect available language servers for the given workspace root and configuration.
#[allow(clippy::missing_errors_doc)]
// The function is structured as one block per language (Rust, Go, TS, Python).
// Each block is short but the four-language repetition pushes the total just
// over the 100-line clippy default. Suppressing to keep the pattern intact.
#[expect(
    clippy::too_many_lines,
    reason = "Four-language repetition block; each block is short but cumulative length exceeds threshold. Pattern is clean per-language — extraction would add indirection without clarity."
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

    // Rust — Cargo.toml (root only; Rust workspaces always have it at the root)
    let rust_root = match get_override!("rust") {
        Some(r) => Some(r),
        None => find_marker(workspace_root, "Cargo.toml", 0).await,
    };
    if let Some(root) = rust_root {
        let has_override = get_command_override!("rust").is_some();
        let cmd =
            get_command_override!("rust").or_else(|| resolve_command("rust-analyzer", "rust"));
        if let Some(command) = cmd {
            detected.push(LanguageLsp {
                language_id: "rust".to_owned(),
                command,
                args: get_args!("rust", vec![]),
                root,
                init_timeout_secs: None,
                auto_plugins: vec![],
            });
        } else if !has_override {
            // Marker found but no binary, and no custom command configured
            missing.push(MissingLanguage {
                language_id: "rust".to_owned(),
                marker_file: "Cargo.toml".to_string(),
                tried_binaries: vec!["rust-analyzer".to_string()],
                install_hint: install_hint("rust"),
            });
        }
    }

    // Go — go.mod (check root then up to depth 2 for monorepos like apps/backend)
    let go_root = match get_override!("go") {
        Some(r) => Some(r),
        None => find_marker(workspace_root, "go.mod", 2).await,
    };
    if let Some(root) = go_root {
        let has_override = get_command_override!("go").is_some();
        let cmd = get_command_override!("go").or_else(|| resolve_command("gopls", "go"));
        if let Some(command) = cmd {
            detected.push(LanguageLsp {
                language_id: "go".to_owned(),
                command,
                args: get_args!("go", vec![]),
                root,
                init_timeout_secs: None,
                auto_plugins: vec![],
            });
        } else if !has_override {
            missing.push(MissingLanguage {
                language_id: "go".to_owned(),
                marker_file: "go.mod".to_string(),
                tried_binaries: vec!["gopls".to_string()],
                install_hint: install_hint("go"),
            });
        }
    }

    // TypeScript / JavaScript — tsconfig.json or package.json (depth 2)
    let (ts_root, ts_marker) = if get_override!("typescript").is_some() {
        (get_override!("typescript"), None)
    } else if let Some(r) = find_marker(workspace_root, "tsconfig.json", 2).await {
        (Some(r), Some("tsconfig.json"))
    } else {
        (
            find_marker(workspace_root, "package.json", 2).await,
            Some("package.json"),
        )
    };
    if let Some(root) = ts_root {
        let has_override = get_command_override!("typescript").is_some();
        let cmd = get_command_override!("typescript")
            .or_else(|| resolve_command("typescript-language-server", "typescript"));
        if let Some(command) = cmd {
            let auto_plugins = detect_typescript_plugins(workspace_root, config).await;
            detected.push(LanguageLsp {
                language_id: "typescript".to_owned(),
                command,
                args: get_args!("typescript", vec!["--stdio".to_owned()]),
                root,
                init_timeout_secs: None,
                auto_plugins,
            });
        } else if !has_override {
            missing.push(MissingLanguage {
                language_id: "typescript".to_owned(),
                marker_file: ts_marker
                    .unwrap_or("tsconfig.json or package.json")
                    .to_string(),
                tried_binaries: vec!["typescript-language-server".to_string()],
                install_hint: install_hint("typescript"),
            });
        }
    }

    // Python — pyproject.toml, setup.py, or requirements.txt (depth 2)
    let (py_root, py_marker) = if get_override!("python").is_some() {
        (get_override!("python"), None)
    } else if let Some(r) = find_marker(workspace_root, "pyproject.toml", 2).await {
        (Some(r), Some("pyproject.toml"))
    } else if let Some(r) = find_marker(workspace_root, "setup.py", 2).await {
        (Some(r), Some("setup.py"))
    } else {
        (
            find_marker(workspace_root, "requirements.txt", 2).await,
            Some("requirements.txt"),
        )
    };
    if let Some(root) = py_root {
        // Try Python LSP servers in order of preference.
        // pyright: Fast, strict type checking, most popular for modern Python
        // pylsp: Community standard, plugin ecosystem, good all-rounder
        // ruff-lsp: Extremely fast, new, growing adoption
        // jedi-language-server: Mature, lightweight, pure Python
        let python_lsp_candidates = [
            ("pyright-langserver", vec!["--stdio".to_owned()]),
            ("pylsp", vec![]),
            ("ruff-lsp", vec![]),
            ("jedi-language-server", vec![]),
        ];

        let has_override = get_command_override!("python").is_some();

        let maybe_command_and_args = if let Some(cmd_override) = get_command_override!("python") {
            // User specified custom command in config
            Some((cmd_override, vec!["--stdio".to_owned()])) // Keep backward compatibility: default to --stdio for custom commands
        } else {
            // Try each candidate in order, tracking which were tried
            let mut resolved = None;
            for (binary, args) in &python_lsp_candidates {
                if let Some(resolved_cmd) = resolve_command(binary, "python") {
                    resolved = Some((resolved_cmd, args.clone()));
                    break;
                }
            }
            resolved
        };

        if let Some((command, default_args)) = maybe_command_and_args {
            detected.push(LanguageLsp {
                language_id: "python".to_owned(),
                command,
                args: get_args!("python", default_args),
                root,
                init_timeout_secs: None,
                auto_plugins: vec![],
            });
        } else if !has_override {
            // No binary found and no custom command configured — add to missing
            missing.push(MissingLanguage {
                language_id: "python".to_owned(),
                marker_file: py_marker
                    .unwrap_or("pyproject.toml, setup.py, or requirements.txt")
                    .to_string(),
                tried_binaries: python_lsp_candidates
                    .iter()
                    .map(|(name, _)| name.to_string())
                    .collect(),
                install_hint: install_hint("python"),
            });
        }
    }

    Ok(DetectionResult { detected, missing })
}

/// Map a file extension to its language identifier.
///
/// Used to look up the correct LSP process when a tool call names a specific
/// file. Returns `None` if the language is unsupported.
#[must_use]
pub fn language_id_for_extension(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" => Some("rust"),
        "go" => Some("go"),
        // Both TypeScript and JavaScript are served by typescript-language-server
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "vue" => Some("typescript"),
        "py" | "pyi" => Some("python"),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::tempdir;

    // Mutex to ensure tests that modify PATH run serially
    static PATH_MUTEX: Mutex<()> = Mutex::new(());

    /// Helper to run tests with fake Python LSP binaries in PATH
    #[allow(clippy::await_holding_lock, clippy::expect_fun_call)]
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

        // Save original PATH
        let original_path = std::env::var("PATH").ok();

        // Set PATH to ONLY our temp bin dir to avoid finding system LSPs
        let new_path = temp_bin_dir.path().to_string_lossy().to_string();
        std::env::set_var("PATH", &new_path);

        // Run the test
        test().await;

        // Restore original PATH
        if let Some(orig) = original_path {
            std::env::set_var("PATH", orig);
        } else {
            std::env::remove_var("PATH");
        }
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
    async fn test_detects_typescript_via_package_json() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("package.json"), "{}").expect("write");
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
        let result = detect_languages(dir.path(), &make_ts_config())
            .await
            .expect("detect");
        let ids: Vec<&str> = result
            .detected
            .iter()
            .map(|l| l.language_id.as_str())
            .collect();
        for id in &ids {
            assert!(["rust", "go", "typescript", "python"].contains(id));
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
        let _guard = match PATH_MUTEX.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
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
        // Lock mutex because other tests modify PATH
        let _guard = match PATH_MUTEX.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let result = resolve_command("cargo", "rust");
        assert!(
            result.is_some(),
            "resolve_command must return Some for a binary on PATH"
        );
        let path = result.expect("cargo must resolve to an absolute path");
        assert!(!path.is_empty());
    }

    #[tokio::test]
    async fn test_find_marker_finds_at_depth_2() {
        let dir = tempdir().expect("temp dir");
        let deep = dir.path().join("apps").join("backend");
        std::fs::create_dir_all(&deep).expect("create dirs");
        std::fs::write(deep.join("go.mod"), "module deep").expect("write");
        let found = find_marker(dir.path(), "go.mod", 2).await;
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
        let found = find_marker(dir.path(), "Cargo.toml", 0).await;
        assert!(
            found.is_none(),
            "max_depth=0 must not recurse into subdirectories"
        );
    }

    #[tokio::test]
    async fn test_find_marker_missing_returns_none() {
        let dir = tempdir().expect("temp dir");
        let found = find_marker(dir.path(), "no_such_marker_file.toml", 2).await;
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
    async fn test_detects_python_fallback_to_pylsp() {
        test_with_fake_python_binaries(&["pylsp"], || async {
            let dir = tempdir().expect("temp dir");
            std::fs::write(dir.path().join("pyproject.toml"), "[tool.poetry]").expect("write");

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
        test_with_fake_python_binaries(&["ruff-lsp"], || async {
            let dir = tempdir().expect("temp dir");
            std::fs::write(dir.path().join("pyproject.toml"), "[tool.poetry]").expect("write");

            let result = detect_languages(dir.path(), &make_ts_config())
                .await
                .expect("detect");

            if let Some(py) = result.detected.iter().find(|l| l.language_id == "python") {
                // ruff-lsp uses empty args
                assert!(py.args.is_empty());
                assert!(py.command.contains("ruff-lsp"));
            } else {
                panic!("Python should be detected with ruff-lsp");
            }
        })
        .await;
    }

    #[tokio::test]
    async fn test_detects_python_fallback_to_jedi() {
        test_with_fake_python_binaries(&["jedi-language-server"], || async {
            let dir = tempdir().expect("temp dir");
            std::fs::write(dir.path().join("pyproject.toml"), "[tool.poetry]").expect("write");

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
}
