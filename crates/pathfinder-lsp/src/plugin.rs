//! LT-2: Language Plugin Trait — per-language behavior abstraction.
//!
//! Each supported language implements [`LanguagePlugin`], encapsulating:
//! - Language identification (ID, file extensions)
//! - LSP binary discovery (binary candidates, default args)
//! - Workspace detection (marker files, search depth)
//! - Manifest validation rules
//! - Install guidance for missing binaries
//! - LSP initialization options
//! - Readiness signal and budget
//! - Handshake timeout
//! - Symbol name normalization
//! - Capability quirks
//! - Marker sibling requirements
//! - Server plugin discovery
//! - Process spawn environment and gitignore handling
//! - Spawn argv injection hooks
//! - Spawn-time revalidation
//! - Isolation UX descriptions
//!
//! # Design Rationale
//!
//! All 11 divergence axes identified in LSP-B5 are represented on the trait,
//! completely removing per-language conditionals from `detect.rs`.
//!
//! The trait is **object-safe** so implementations can be used as
//! `Box<dyn LanguagePlugin>` or `&dyn LanguagePlugin`.

use std::any::Any;
use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

const ENV_CONDA_PREFIX: &str = "CONDA_PREFIX";
const ENV_VIRTUAL_ENV: &str = "VIRTUAL_ENV";
const ENV_JAVA_HOME: &str = "JAVA_HOME";
const ENV_HOME: &str = "HOME";
const ENV_USERPROFILE: &str = "USERPROFILE";

/// Describes a candidate LSP binary with its default arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspCandidate {
    /// Binary name to resolve via `which` (e.g., `"rust-analyzer"`, `"gopls"`).
    pub binary: &'static str,
    /// Default CLI arguments (e.g., `["--stdio"]`).
    pub default_args: &'static [&'static str],
}

/// Readiness signal expected from the language server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessSignal {
    /// Readiness is determined solely by successful behavioral probing.
    #[default]
    ProbeOnly,
    /// Server emits `$/progress` notifications during background indexing.
    /// Note: on a lossy channel, missed progress events degrade gracefully to probe-only.
    WorkDoneProgress,
}

/// Known quirks and capability flags for a language server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct CapabilityQuirks {
    /// Whether the server requires didOpen before answering queries on a file.
    #[serde(default)]
    pub requires_did_open: bool,
}

/// Server plugin representation (e.g. for TypeScript tsserver plugins like `@vue/typescript-plugin`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ServerPlugin {
    /// Name of the plugin.
    pub name: String,
    /// Optional filesystem location of the plugin.
    #[serde(default)]
    pub location: Option<String>,
}

impl ServerPlugin {
    /// Create a new server plugin by name.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            location: None,
        }
    }
}

/// Per-language LSP behaviour abstraction across all 11 divergence axes (LSP-B5).
pub trait LanguagePlugin: Send + Sync {
    // ── Pure Data ─────────────────────────────────────────────────────────────

    /// Short identifier used as a map key (e.g., `"rust"`, `"go"`, `"typescript"`, `"python"`, `"java"`).
    fn language_id(&self) -> &'static str;

    /// File extensions that this language handles.
    fn file_extensions(&self) -> &'static [&'static str];

    /// Marker files that indicate this language is used in the workspace.
    fn marker_files(&self) -> &'static [&'static str];

    /// Maximum directory depth to search for marker files (0 = root only, 2 = up to 2 levels).
    fn marker_search_depth(&self) -> u32;

    /// LSP binary candidates in preference order.
    fn lsp_candidates(&self) -> &[LspCandidate];

    /// Human-readable install guidance when no LSP binary is found.
    fn install_hint(&self) -> &'static str;

    // ── AXIS 1: Initialization Options (LSP-B4) ──────────────────────────────

    /// Build LSP initialization options from workspace and project roots.
    fn initialization_options(
        &self,
        project_root: &Path,
        workspace_root: &Path,
    ) -> serde_json::Value {
        let _ = (project_root, workspace_root);
        serde_json::Value::Null
    }

    // ── AXIS 2: Readiness Signal & Budget ─────────────────────────────────────

    /// Readiness signal mechanism for this language server.
    fn readiness_signal(&self) -> ReadinessSignal {
        ReadinessSignal::ProbeOnly
    }

    /// Maximum time to wait for the language server to become ready.
    fn readiness_budget(&self) -> Duration {
        Duration::from_secs(120)
    }

    // ── AXIS 4: Handshake Timeout ─────────────────────────────────────────────

    /// Handshake timeout for the `initialize` request.
    fn handshake_timeout(&self) -> Duration {
        Duration::from_secs(60)
    }

    // ── Symbol Normalization & Quirks ─────────────────────────────────────────

    /// Normalize symbol names emitted by this server (e.g. stripping signatures or return types).
    fn normalize_symbol_name<'a>(&self, raw: &'a str) -> Cow<'a, str> {
        Cow::Borrowed(raw)
    }

    /// Capability quirks for this language server.
    fn capability_quirks(&self) -> CapabilityQuirks {
        CapabilityQuirks::default()
    }

    // ── AXIS 5: Manifest Validation (LSP-B5) ──────────────────────────────────

    /// Validate manifest / marker file contents.
    ///
    /// # Errors
    /// Returns `Err(reason)` if the manifest is malformed or invalid.
    fn validate_marker(&self, marker: &str, content: &str) -> Result<(), String> {
        let _ = (marker, content);
        Ok(())
    }

    // ── AXIS 6: Marker Sibling Requirement ───────────────────────────────────

    /// Sibling marker required when resolving multi-matches (e.g. tsconfig.json for package.json).
    fn marker_sibling_requirement(&self, marker: &str) -> Option<&'static str> {
        let _ = marker;
        None
    }

    // ── AXIS 7: Server Plugin Discovery ──────────────────────────────────────

    /// Discover server-side plugins (e.g. `@vue/typescript-plugin` in `node_modules`).
    fn discover_server_plugins(&self, root: &Path) -> Vec<ServerPlugin> {
        let _ = root;
        Vec::new()
    }

    // ── AXIS 8: Spawn Environment & Gitignore ────────────────────────────────

    /// Environment variables to set when spawning the LSP process.
    fn spawn_env(&self, root: &Path) -> Vec<(OsString, OsString)> {
        let _ = root;
        Vec::new()
    }

    /// Whether `.pathfinder` should be added to `.gitignore`.
    fn needs_gitignore_entry(&self) -> bool {
        true
    }

    // ── AXIS 9: Spawn Argv Hook & Resource Holding ───────────────────────────

    /// Hook to inject CLI arguments and return a lifetime-bound resource (e.g. advisory lock).
    fn spawn_argv_hook(&self, root: &Path, argv: &mut Vec<String>) -> Option<Box<dyn Any + Send>> {
        let _ = (root, argv);
        None
    }

    // ── AXIS 10: Spawn-time Revalidation ─────────────────────────────────────

    /// Revalidate or refresh initialization options at spawn time.
    fn revalidate_at_spawn(
        &self,
        opts: serde_json::Value,
        project_root: &Path,
    ) -> serde_json::Value {
        let _ = project_root;
        opts
    }

    // ── AXIS 11: Isolation UX Description ────────────────────────────────────

    /// Human-readable description of the process isolation directory.
    fn isolation_description(&self) -> Option<&'static str> {
        None
    }
}

// ── Concrete Built-in Implementations ─────────────────────────────────────────

/// Rust language plugin — `rust-analyzer` + `Cargo.toml`.
pub struct RustPlugin;

impl LanguagePlugin for RustPlugin {
    fn language_id(&self) -> &'static str {
        "rust"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }

    fn marker_files(&self) -> &'static [&'static str] {
        &["Cargo.toml"]
    }

    fn marker_search_depth(&self) -> u32 {
        2
    }

    fn lsp_candidates(&self) -> &[LspCandidate] {
        &[LspCandidate {
            binary: "rust-analyzer",
            default_args: &[],
        }]
    }

    fn install_hint(&self) -> &'static str {
        "Install rust-analyzer: https://rust-analyzer.github.io/"
    }

    fn readiness_budget(&self) -> Duration {
        Duration::from_secs(60)
    }

    fn validate_marker(&self, marker: &str, content: &str) -> Result<(), String> {
        if marker == "Cargo.toml" {
            match toml::from_str::<toml::Value>(content) {
                Ok(v) => {
                    if v.get("package").is_some() || v.get("workspace").is_some() {
                        Ok(())
                    } else {
                        Err("Cargo.toml has neither [package] nor [workspace] section".to_owned())
                    }
                }
                Err(e) => Err(format!("Cargo.toml is not valid TOML: {e}")),
            }
        } else {
            Ok(())
        }
    }

    fn spawn_env(&self, root: &Path) -> Vec<(OsString, OsString)> {
        vec![(
            OsString::from("CARGO_TARGET_DIR"),
            root.join("target/pathfinder-lsp").into_os_string(),
        )]
    }

    fn needs_gitignore_entry(&self) -> bool {
        false
    }

    fn isolation_description(&self) -> Option<&'static str> {
        Some("Cargo target directory")
    }
}

/// Go language plugin — `gopls` + `go.mod`.
pub struct GoPlugin;

impl LanguagePlugin for GoPlugin {
    fn language_id(&self) -> &'static str {
        "go"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["go"]
    }

    fn marker_files(&self) -> &'static [&'static str] {
        &["go.mod"]
    }

    fn marker_search_depth(&self) -> u32 {
        2
    }

    fn lsp_candidates(&self) -> &[LspCandidate] {
        &[LspCandidate {
            binary: "gopls",
            default_args: &[],
        }]
    }

    fn install_hint(&self) -> &'static str {
        "Install gopls: go install golang.org/x/tools/gopls@latest"
    }

    fn readiness_budget(&self) -> Duration {
        Duration::from_secs(30)
    }

    fn validate_marker(&self, marker: &str, content: &str) -> Result<(), String> {
        if marker == "go.mod" {
            let target_line = content
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
        } else {
            Ok(())
        }
    }

    fn spawn_env(&self, root: &Path) -> Vec<(OsString, OsString)> {
        vec![
            (
                OsString::from("GOCACHE"),
                root.join(".pathfinder/gopls-cache/build").into_os_string(),
            ),
            (
                OsString::from("GOMODCACHE"),
                root.join(".pathfinder/gopls-cache/mod").into_os_string(),
            ),
        ]
    }

    fn needs_gitignore_entry(&self) -> bool {
        true
    }

    fn isolation_description(&self) -> Option<&'static str> {
        Some("Go build cache (GOCACHE/GOMODCACHE)")
    }
}

/// Helper to strip JSONC comments for tsconfig parsing.
#[must_use]
pub fn strip_jsonc_comments(input: &str) -> String {
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

/// Helper to strip trailing commas in JSON.
#[must_use]
pub fn strip_json_trailing_commas(input: &str) -> String {
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

/// Synchronous check for Vue single-file components in a directory.
pub(crate) fn check_has_vue_files(dir: &Path, depth: usize) -> bool {
    if depth > 4 {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if path.extension().and_then(|e| e.to_str()) == Some("vue") {
                return true;
            }
        } else if path.is_dir() {
            if let Ok(ft) = entry.file_type() {
                if ft.is_symlink() {
                    continue;
                }
            }
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if matches!(
                    name,
                    "node_modules"
                        | ".git"
                        | "target"
                        | ".pnpm"
                        | ".venv"
                        | "__pycache__"
                        | "dist"
                        | "build"
                ) {
                    continue;
                }
            }
            if check_has_vue_files(&path, depth + 1) {
                return true;
            }
        }
    }
    false
}

/// Find a TypeScript plugin in `node_modules`.
fn find_ts_plugin(root: &Path, plugin_name: &str) -> Option<String> {
    let standard = root.join("node_modules").join(plugin_name);
    if standard.exists() {
        return Some(plugin_name.to_owned());
    }

    let pnpm_dir = root.join("node_modules").join(".pnpm");
    if let Ok(entries) = std::fs::read_dir(&pnpm_dir) {
        let pattern = plugin_name.replace('/', "+").replace('@', "");
        for entry in entries.flatten() {
            let name_str = entry.file_name().to_string_lossy().into_owned();
            if name_str.contains(&pattern) {
                let plugin_path = entry.path().join("node_modules").join(plugin_name);
                if plugin_path.exists() {
                    return Some(plugin_name.to_owned());
                }
            }
        }
    }

    None
}

/// TypeScript / JavaScript language plugin — `typescript-language-server` + `tsconfig.json` / `package.json`.
pub struct TypeScriptPlugin;

impl LanguagePlugin for TypeScriptPlugin {
    fn language_id(&self) -> &'static str {
        "typescript"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["ts", "tsx", "js", "jsx", "mjs", "cjs", "vue", "mts", "cts"]
    }

    fn marker_files(&self) -> &'static [&'static str] {
        &["tsconfig.json", "package.json"]
    }

    fn marker_search_depth(&self) -> u32 {
        2
    }

    fn lsp_candidates(&self) -> &[LspCandidate] {
        &[LspCandidate {
            binary: "typescript-language-server",
            default_args: &["--stdio"],
        }]
    }

    fn install_hint(&self) -> &'static str {
        "Install typescript-language-server: npm install -g typescript-language-server typescript"
    }

    fn validate_marker(&self, marker: &str, content: &str) -> Result<(), String> {
        if marker == "tsconfig.json" {
            if serde_json::from_str::<serde_json::Value>(content).is_ok() {
                Ok(())
            } else {
                let stripped_comments = strip_jsonc_comments(content);
                let stripped_commas = strip_json_trailing_commas(&stripped_comments);
                serde_json::from_str::<serde_json::Value>(&stripped_commas)
                    .map(|_| ())
                    .map_err(|e| format!("tsconfig.json is not valid JSON: {e}"))
            }
        } else {
            Ok(())
        }
    }

    fn marker_sibling_requirement(&self, marker: &str) -> Option<&'static str> {
        if marker == "package.json" {
            Some("tsconfig.json")
        } else {
            None
        }
    }

    fn discover_server_plugins(&self, root: &Path) -> Vec<ServerPlugin> {
        let mut plugins = Vec::new();
        if check_has_vue_files(root, 0) {
            if let Some(plugin_name) = find_ts_plugin(root, "@vue/typescript-plugin") {
                plugins.push(ServerPlugin::new(plugin_name));
            }
        }
        plugins
    }

    fn spawn_env(&self, root: &Path) -> Vec<(OsString, OsString)> {
        vec![(
            OsString::from("TMPDIR"),
            root.join(".pathfinder/tsserver-tmp").into_os_string(),
        )]
    }

    fn needs_gitignore_entry(&self) -> bool {
        true
    }

    fn isolation_description(&self) -> Option<&'static str> {
        Some("TypeScript temp directory (TMPDIR)")
    }
}

/// Detect the Python interpreter from the workspace's virtual environment.
#[must_use]
pub fn detect_venv(workspace_root: &Path) -> Option<PathBuf> {
    let candidates = [
        ".venv/bin/python",
        ".venv/Scripts/python.exe",
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

    if let Ok(conda_prefix) = std::env::var(ENV_CONDA_PREFIX) {
        let prefix_path = PathBuf::from(&conda_prefix);
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

    if let Ok(venv) = std::env::var(ENV_VIRTUAL_ENV) {
        let venv_path = PathBuf::from(&venv);
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
#[must_use]
pub fn detect_python_init_options(workspace_root: &Path) -> serde_json::Value {
    use serde_json::json;
    match detect_venv(workspace_root) {
        Some(py_path) => json!({
            "python": {
                "pythonPath": py_path.to_string_lossy().as_ref()
            }
        }),
        None => serde_json::Value::Null,
    }
}

/// Python language plugin — `pyright-langserver` / `pyright` / `pylsp` / `ruff` / `jedi-language-server`.
pub struct PythonPlugin;

impl LanguagePlugin for PythonPlugin {
    fn language_id(&self) -> &'static str {
        "python"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["py", "pyi"]
    }

    fn marker_files(&self) -> &'static [&'static str] {
        &["pyproject.toml", "setup.py", "requirements.txt"]
    }

    fn marker_search_depth(&self) -> u32 {
        2
    }

    fn lsp_candidates(&self) -> &[LspCandidate] {
        &[
            LspCandidate {
                binary: "pyright-langserver",
                default_args: &["--stdio"],
            },
            LspCandidate {
                binary: "basedpyright-langserver",
                default_args: &["--stdio"],
            },
            LspCandidate {
                binary: "pylsp",
                default_args: &[],
            },
            LspCandidate {
                binary: "ruff",
                default_args: &["server", "--stdio"],
            },
            LspCandidate {
                binary: "jedi-language-server",
                default_args: &[],
            },
        ]
    }

    fn install_hint(&self) -> &'static str {
        "Install pyright-langserver: npm install -g pyright\nOr install pylsp: pip install python-lsp-server"
    }

    fn validate_marker(&self, marker: &str, content: &str) -> Result<(), String> {
        if marker == "pyproject.toml" {
            match toml::from_str::<toml::Value>(content) {
                Ok(_) => Ok(()),
                Err(e) => Err(format!("pyproject.toml is not valid TOML: {e}")),
            }
        } else {
            Ok(())
        }
    }

    fn initialization_options(
        &self,
        project_root: &Path,
        _workspace_root: &Path,
    ) -> serde_json::Value {
        detect_python_init_options(project_root)
    }

    fn revalidate_at_spawn(
        &self,
        opts: serde_json::Value,
        project_root: &Path,
    ) -> serde_json::Value {
        use serde_json::json;
        let current_path = opts
            .get("python")
            .and_then(|p| p.get("pythonPath"))
            .and_then(|v| v.as_str());

        if let Some(path) = current_path {
            if Path::new(path).exists() {
                return opts;
            }
        }

        if let Some(venv_path) = detect_venv(project_root) {
            json!({
                "python": {
                    "pythonPath": venv_path.to_string_lossy().as_ref()
                }
            })
        } else {
            serde_json::Value::Null
        }
    }

    fn spawn_env(&self, root: &Path) -> Vec<(OsString, OsString)> {
        vec![(
            OsString::from("PYTHONPYCACHEPREFIX"),
            root.join(".pathfinder/python-cache/pyc").into_os_string(),
        )]
    }

    fn needs_gitignore_entry(&self) -> bool {
        true
    }

    fn isolation_description(&self) -> Option<&'static str> {
        Some("Python bytecode cache (PYTHONPYCACHEPREFIX)")
    }
}

/// Helper to detect JDK home directory based on env vars and project files (.sdkmanrc, .java-version).
#[must_use]
pub fn detect_jdk_home(root: &Path) -> Option<String> {
    if let Ok(home) = std::env::var(ENV_JAVA_HOME) {
        if !home.is_empty() {
            return Some(home);
        }
    }

    let home_str = std::env::var(ENV_HOME)
        .ok()
        .or_else(|| std::env::var(ENV_USERPROFILE).ok())?;
    let home_dir = PathBuf::from(home_str);

    let verify_jdk = |path: PathBuf| -> Option<String> {
        if path.join("bin/java").exists() || path.join("bin/javac").exists() {
            return Some(path.to_string_lossy().into_owned());
        }
        let mac_home = path.join("Contents/Home");
        if mac_home.join("bin/java").exists() {
            return Some(mac_home.to_string_lossy().into_owned());
        }
        None
    };

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
#[must_use]
pub fn detect_java_init_options(detected_root: &Path, workspace_root: &Path) -> serde_json::Value {
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

/// Java language plugin — jdtls (Eclipse JDT Language Server).
pub struct JavaPlugin;

impl LanguagePlugin for JavaPlugin {
    fn language_id(&self) -> &'static str {
        "java"
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        &["java"]
    }

    fn marker_files(&self) -> &'static [&'static str] {
        &[
            "pom.xml",
            "build.gradle",
            "build.gradle.kts",
            "settings.gradle",
            "settings.gradle.kts",
        ]
    }

    fn marker_search_depth(&self) -> u32 {
        2
    }

    fn lsp_candidates(&self) -> &[LspCandidate] {
        &[LspCandidate {
            binary: "jdtls",
            default_args: &[],
        }]
    }

    fn install_hint(&self) -> &'static str {
        "Install jdtls: https://github.com/eclipse-jdtls/eclipse.jdt.ls#installation\n\
         Requires JDK 21+ to run. Use sdkman: sdk install java 21-tem && sdk install jdtls"
    }

    fn readiness_budget(&self) -> Duration {
        Duration::from_secs(180)
    }

    fn handshake_timeout(&self) -> Duration {
        Duration::from_secs(180)
    }

    fn validate_marker(&self, marker: &str, content: &str) -> Result<(), String> {
        if marker == "pom.xml" {
            if content.contains("<project") {
                Ok(())
            } else {
                Err("pom.xml does not contain <project element".to_owned())
            }
        } else if marker == "build.gradle" || marker == "build.gradle.kts" {
            if content.is_empty() {
                Err("build.gradle is empty".to_owned())
            } else {
                Ok(())
            }
        } else {
            Ok(())
        }
    }

    fn initialization_options(
        &self,
        project_root: &Path,
        workspace_root: &Path,
    ) -> serde_json::Value {
        detect_java_init_options(project_root, workspace_root)
    }

    fn normalize_symbol_name<'a>(&self, raw: &'a str) -> Cow<'a, str> {
        if let Some((name, _)) = raw.split_once('(') {
            Cow::Borrowed(name.trim())
        } else if let Some((name, _)) = raw.split_once(':') {
            Cow::Borrowed(name.trim())
        } else {
            Cow::Borrowed(raw)
        }
    }

    fn needs_gitignore_entry(&self) -> bool {
        true
    }
}

// ── TOML Plugin Support ───────────────────────────────────────────────────────

/// LSP candidate specification in TOML.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TomlLspCandidate {
    /// Binary name to resolve via `which`.
    pub binary: String,
    /// Default CLI arguments.
    #[serde(default)]
    pub args: Vec<String>,
}

/// Language plugin declaration in TOML (`[[language]]`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TomlLanguageConfig {
    /// Language identifier (e.g. `"kotlin"`).
    pub id: String,
    /// Handled file extensions without leading dot (e.g. `["kt", "kts"]`).
    pub extensions: Vec<String>,
    /// Marker files indicating workspace root (e.g. `["build.gradle.kts"]`).
    pub markers: Vec<String>,
    /// Maximum directory depth to search for marker files.
    #[serde(default = "default_marker_depth")]
    pub marker_depth: u32,
    /// LSP candidate binaries in preference order.
    #[serde(default)]
    pub lsp_candidates: Vec<TomlLspCandidate>,
    /// Install guidance if binary is missing.
    #[serde(default)]
    pub install_hint: String,
    /// Readiness budget in seconds.
    #[serde(default = "default_readiness_budget_secs")]
    pub readiness_budget_secs: u64,
    /// Handshake timeout in seconds.
    #[serde(default = "default_handshake_timeout_secs")]
    pub handshake_timeout_secs: u64,
    /// Capability quirks.
    #[serde(default)]
    pub quirks: CapabilityQuirks,
}

const fn default_marker_depth() -> u32 {
    2
}

const fn default_readiness_budget_secs() -> u64 {
    120
}

const fn default_handshake_timeout_secs() -> u64 {
    60
}

/// Top-level container for `[[language]]` TOML parsing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TomlConfigFile {
    /// Declared language plugins.
    #[serde(default, rename = "language")]
    pub languages: Vec<TomlLanguageConfig>,
}

/// Dynamic language plugin instantiated from TOML configuration.
pub struct TomlLanguagePlugin {
    id: &'static str,
    extensions: &'static [&'static str],
    markers: &'static [&'static str],
    marker_depth: u32,
    candidates: &'static [LspCandidate],
    install_hint: &'static str,
    readiness_budget: Duration,
    handshake_timeout: Duration,
    quirks: CapabilityQuirks,
}

impl TomlLanguagePlugin {
    /// Create a static `TomlLanguagePlugin` from configuration by leaking strings for process lifetime.
    #[must_use]
    pub fn from_config(config: TomlLanguageConfig) -> &'static Self {
        let id: &'static str = Box::leak(config.id.into_boxed_str());
        let extensions: &'static [&'static str] = Box::leak(
            config
                .extensions
                .into_iter()
                .map(|e| -> &'static str { Box::leak(e.into_boxed_str()) })
                .collect::<Vec<&'static str>>()
                .into_boxed_slice(),
        );
        let markers: &'static [&'static str] = Box::leak(
            config
                .markers
                .into_iter()
                .map(|m| -> &'static str { Box::leak(m.into_boxed_str()) })
                .collect::<Vec<&'static str>>()
                .into_boxed_slice(),
        );

        let candidates: &'static [LspCandidate] = Box::leak(
            config
                .lsp_candidates
                .into_iter()
                .map(|c| {
                    let binary: &'static str = Box::leak(c.binary.into_boxed_str());
                    let default_args: &'static [&'static str] = Box::leak(
                        c.args
                            .into_iter()
                            .map(|a| -> &'static str { Box::leak(a.into_boxed_str()) })
                            .collect::<Vec<&'static str>>()
                            .into_boxed_slice(),
                    );
                    LspCandidate {
                        binary,
                        default_args,
                    }
                })
                .collect::<Vec<LspCandidate>>()
                .into_boxed_slice(),
        );

        let install_hint: &'static str = Box::leak(config.install_hint.into_boxed_str());

        Box::leak(Box::new(Self {
            id,
            extensions,
            markers,
            marker_depth: config.marker_depth,
            candidates,
            install_hint,
            readiness_budget: Duration::from_secs(config.readiness_budget_secs),
            handshake_timeout: Duration::from_secs(config.handshake_timeout_secs),
            quirks: config.quirks,
        }))
    }
}

impl LanguagePlugin for TomlLanguagePlugin {
    fn language_id(&self) -> &'static str {
        self.id
    }

    fn file_extensions(&self) -> &'static [&'static str] {
        self.extensions
    }

    fn marker_files(&self) -> &'static [&'static str] {
        self.markers
    }

    fn marker_search_depth(&self) -> u32 {
        self.marker_depth
    }

    fn lsp_candidates(&self) -> &[LspCandidate] {
        self.candidates
    }

    fn install_hint(&self) -> &'static str {
        self.install_hint
    }

    fn readiness_budget(&self) -> Duration {
        self.readiness_budget
    }

    fn handshake_timeout(&self) -> Duration {
        self.handshake_timeout
    }

    fn capability_quirks(&self) -> CapabilityQuirks {
        self.quirks
    }
}

// ── Registry ──────────────────────────────────────────────────────────────────

/// Errors that can occur when registering language plugins.
#[derive(Debug, thiserror::Error)]
pub enum PluginRegistryError {
    /// A plugin with this language ID is already registered.
    #[error("duplicate language ID '{0}'")]
    DuplicateLanguageId(String),
    /// A file extension is claimed by multiple plugins (LSP-D8).
    #[error("extension '.{extension}' claimed by both '{first_lang}' and '{second_lang}'")]
    DuplicateExtension {
        /// The conflicting file extension.
        extension: String,
        /// Language ID of first plugin claiming the extension.
        first_lang: String,
        /// Language ID of second plugin claiming the extension.
        second_lang: String,
    },
    /// TOML deserialization failed.
    #[error("failed to parse TOML plugin configuration: {0}")]
    ParseError(#[from] toml::de::Error),
    /// I/O error reading plugin configuration file.
    #[error("I/O error reading plugin configuration file: {0}")]
    Io(#[from] std::io::Error),
}

/// Registry of active language plugins.
pub struct PluginRegistry {
    plugins: Vec<&'static dyn LanguagePlugin>,
    ext_to_lang: HashMap<String, String>,
}

impl PluginRegistry {
    /// Create a new registry populated with the five built-in language plugins.
    #[must_use]
    pub fn new() -> Self {
        let mut registry = Self {
            plugins: Vec::new(),
            ext_to_lang: HashMap::new(),
        };

        let built_ins: [&'static dyn LanguagePlugin; 5] = [
            &RustPlugin,
            &GoPlugin,
            &TypeScriptPlugin,
            &PythonPlugin,
            &JavaPlugin,
        ];

        for plugin in built_ins {
            let _ = registry.register_plugin(plugin);
        }

        registry
    }

    /// Register a plugin with duplicate ID and duplicate extension validation (LSP-D8).
    ///
    /// # Errors
    /// Returns `PluginRegistryError::DuplicateLanguageId` if the ID is already registered.
    /// Returns `PluginRegistryError::DuplicateExtension` if any extension is claimed by another plugin.
    pub fn register_plugin(
        &mut self,
        plugin: &'static dyn LanguagePlugin,
    ) -> Result<(), PluginRegistryError> {
        let lang_id = plugin.language_id();
        if self.plugins.iter().any(|p| p.language_id() == lang_id) {
            return Err(PluginRegistryError::DuplicateLanguageId(lang_id.to_owned()));
        }

        for ext in plugin.file_extensions() {
            if let Some(existing_lang) = self.ext_to_lang.get(*ext) {
                return Err(PluginRegistryError::DuplicateExtension {
                    extension: (*ext).to_owned(),
                    first_lang: existing_lang.clone(),
                    second_lang: lang_id.to_owned(),
                });
            }
        }

        for ext in plugin.file_extensions() {
            self.ext_to_lang
                .insert((*ext).to_owned(), lang_id.to_owned());
        }

        self.plugins.push(plugin);
        Ok(())
    }

    /// Return all registered plugins.
    #[must_use]
    pub fn plugins(&self) -> Vec<&'static dyn LanguagePlugin> {
        self.plugins.clone()
    }

    /// Look up a plugin by language ID.
    #[must_use]
    pub fn plugin_for_language(&self, language_id: &str) -> Option<&'static dyn LanguagePlugin> {
        self.plugins
            .iter()
            .find(|p| p.language_id() == language_id)
            .copied()
    }

    /// Look up a plugin by file extension.
    #[must_use]
    pub fn plugin_for_extension(&self, ext: &str) -> Option<&'static dyn LanguagePlugin> {
        self.plugins
            .iter()
            .find(|p| p.file_extensions().contains(&ext))
            .copied()
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

static GLOBAL_REGISTRY: LazyLock<parking_lot::RwLock<PluginRegistry>> =
    LazyLock::new(|| parking_lot::RwLock::new(PluginRegistry::new()));

/// Return all active language plugins.
#[must_use]
pub fn all_plugins() -> Vec<&'static dyn LanguagePlugin> {
    GLOBAL_REGISTRY.read().plugins()
}

/// Look up a plugin by language ID.
#[must_use]
pub fn plugin_for_language(language_id: &str) -> Option<&'static dyn LanguagePlugin> {
    GLOBAL_REGISTRY.read().plugin_for_language(language_id)
}

/// Look up a plugin by file extension.
#[must_use]
pub fn plugin_for_extension(ext: &str) -> Option<&'static dyn LanguagePlugin> {
    GLOBAL_REGISTRY.read().plugin_for_extension(ext)
}

/// Register a new plugin into the global registry.
///
/// # Errors
/// Returns `PluginRegistryError` on duplicate language ID or duplicate extension (LSP-D8).
pub fn register_plugin(plugin: &'static dyn LanguagePlugin) -> Result<(), PluginRegistryError> {
    GLOBAL_REGISTRY.write().register_plugin(plugin)
}

/// Parse and load language plugin declarations from a TOML string.
///
/// # Errors
/// Returns `PluginRegistryError` if TOML parsing fails or duplicate claims occur.
pub fn load_toml_plugins(
    toml_str: &str,
) -> Result<Vec<&'static dyn LanguagePlugin>, PluginRegistryError> {
    let config_file: TomlConfigFile = toml::from_str(toml_str)?;
    let mut loaded = Vec::with_capacity(config_file.languages.len());

    let mut write_guard = GLOBAL_REGISTRY.write();
    for lang_cfg in config_file.languages {
        let plugin = TomlLanguagePlugin::from_config(lang_cfg);
        write_guard.register_plugin(plugin)?;
        loaded.push(plugin as &'static dyn LanguagePlugin);
    }

    Ok(loaded)
}

/// Check workspace root for `.pathfinder.toml` or `pathfinder.toml` and load declared plugins.
///
/// # Errors
/// Returns `PluginRegistryError` if reading/parsing fails.
pub async fn load_workspace_toml_plugins(
    workspace_root: &Path,
) -> Result<Vec<&'static dyn LanguagePlugin>, PluginRegistryError> {
    let candidates = [
        workspace_root.join(".pathfinder.toml"),
        workspace_root.join("pathfinder.toml"),
    ];

    for path in &candidates {
        if tokio::fs::metadata(path).await.is_ok() {
            let content = tokio::fs::read_to_string(path).await?;
            return load_toml_plugins(&content);
        }
    }

    Ok(Vec::new())
}

/// Reset global registry to the default built-in plugins (useful in tests).
pub fn reset_registry() {
    *GLOBAL_REGISTRY.write() = PluginRegistry::new();
}

#[cfg(test)]
#[path = "plugin_test.rs"]
mod tests;
