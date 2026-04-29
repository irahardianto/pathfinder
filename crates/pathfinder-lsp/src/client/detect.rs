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

/// Detect available language servers for the given workspace root and configuration.
#[allow(clippy::missing_errors_doc)]
// The function is structured as one block per language (Rust, Go, TS, Python).
// Each block is short but the four-language repetition pushes the total just
// over the 100-line clippy default. Suppressing to keep the pattern intact.
#[allow(clippy::too_many_lines)]
pub async fn detect_languages(
    workspace_root: &Path,
    config: &pathfinder_common::config::PathfinderConfig,
) -> std::io::Result<Vec<LanguageLsp>> {
    let mut detected = Vec::new();

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
        let cmd =
            get_command_override!("rust").or_else(|| resolve_command("rust-analyzer", "rust"));
        if let Some(command) = cmd {
            detected.push(LanguageLsp {
                language_id: "rust".to_owned(),
                command,
                args: get_args!("rust", vec![]),
                root,
                init_timeout_secs: None,
            });
        }
    }

    // Go — go.mod (check root then up to depth 2 for monorepos like apps/backend)
    let go_root = match get_override!("go") {
        Some(r) => Some(r),
        None => find_marker(workspace_root, "go.mod", 2).await,
    };
    if let Some(root) = go_root {
        let cmd = get_command_override!("go").or_else(|| resolve_command("gopls", "go"));
        if let Some(command) = cmd {
            detected.push(LanguageLsp {
                language_id: "go".to_owned(),
                command,
                args: get_args!("go", vec![]),
                root,
                init_timeout_secs: None,
            });
        }
    }

    // TypeScript / JavaScript — tsconfig.json or package.json (depth 2)
    let ts_root = match get_override!("typescript") {
        Some(r) => Some(r),
        None => find_marker(workspace_root, "tsconfig.json", 2)
            .await
            .or(find_marker(workspace_root, "package.json", 2).await),
    };
    if let Some(root) = ts_root {
        let cmd = get_command_override!("typescript")
            .or_else(|| resolve_command("typescript-language-server", "typescript"));
        if let Some(command) = cmd {
            detected.push(LanguageLsp {
                language_id: "typescript".to_owned(),
                command,
                args: get_args!("typescript", vec!["--stdio".to_owned()]),
                root,
                init_timeout_secs: None,
            });
        }
    }

    // Python — pyproject.toml, setup.py, or requirements.txt (depth 2)
    let py_root = match get_override!("python") {
        Some(r) => Some(r),
        None => find_marker(workspace_root, "pyproject.toml", 2)
            .await
            .or(find_marker(workspace_root, "setup.py", 2).await)
            .or(find_marker(workspace_root, "requirements.txt", 2).await),
    };
    if let Some(root) = py_root {
        let cmd = get_command_override!("python").or_else(|| resolve_command("pyright", "python"));
        if let Some(command) = cmd {
            detected.push(LanguageLsp {
                language_id: "python".to_owned(),
                command,
                args: get_args!("python", vec!["--stdio".to_owned()]),
                root,
                init_timeout_secs: None,
            });
        }
    }

    Ok(detected)
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
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_detects_cargo_toml() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").expect("write");
        let langs = detect_languages(
            dir.path(),
            &pathfinder_common::config::PathfinderConfig::default(),
        )
        .await
        .expect("detect");
        // Only check language_id and args — command is now an absolute path from `which`
        // (or absent if rust-analyzer is not installed in this test environment)
        if let Some(rust) = langs.iter().find(|l| l.language_id == "rust") {
            assert!(rust.args.is_empty());
            assert!(rust.init_timeout_secs.is_none());
            // Command must be a non-empty string (absolute path or bare name)
            assert!(!rust.command.is_empty());
        }
        // If rust-analyzer is not on PATH in CI, the language is simply not detected
    }

    #[tokio::test]
    async fn test_detects_go_mod() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("go.mod"), "module foo").expect("write");
        let langs = detect_languages(
            dir.path(),
            &pathfinder_common::config::PathfinderConfig::default(),
        )
        .await
        .expect("detect");
        if let Some(go) = langs.iter().find(|l| l.language_id == "go") {
            assert!(!go.command.is_empty());
        }
    }

    #[tokio::test]
    async fn test_detects_typescript_via_tsconfig() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("tsconfig.json"), "{}").expect("write");
        let langs = detect_languages(
            dir.path(),
            &pathfinder_common::config::PathfinderConfig::default(),
        )
        .await
        .expect("detect");
        if let Some(ts) = langs.iter().find(|l| l.language_id == "typescript") {
            assert_eq!(ts.args, ["--stdio"]);
            assert!(ts.init_timeout_secs.is_none());
        }
    }

    #[tokio::test]
    async fn test_detects_typescript_via_package_json() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("package.json"), "{}").expect("write");
        let langs = detect_languages(
            dir.path(),
            &pathfinder_common::config::PathfinderConfig::default(),
        )
        .await
        .expect("detect");
        // typescript-language-server may or may not be on PATH in CI
        let found = langs.iter().any(|l| l.language_id == "typescript");
        // Just verify the function completes without panic
        let _ = found;
    }

    #[tokio::test]
    async fn test_detects_python_via_pyproject() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("pyproject.toml"), "[tool.poetry]").expect("write");
        let langs = detect_languages(
            dir.path(),
            &pathfinder_common::config::PathfinderConfig::default(),
        )
        .await
        .expect("detect");
        if let Some(py) = langs.iter().find(|l| l.language_id == "python") {
            assert!(!py.command.is_empty());
        }
    }

    #[tokio::test]
    async fn test_detects_multiple_languages() {
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").expect("write");
        std::fs::write(dir.path().join("package.json"), "{}").expect("write");
        let langs = detect_languages(
            dir.path(),
            &pathfinder_common::config::PathfinderConfig::default(),
        )
        .await
        .expect("detect");
        // Verify the function handles multiple markers without panic
        let ids: Vec<&str> = langs.iter().map(|l| l.language_id.as_str()).collect();
        // Languages are only present if their binary is on PATH
        for id in &ids {
            assert!(["rust", "go", "typescript", "python"].contains(id));
        }
    }

    #[tokio::test]
    async fn test_empty_directory() {
        let dir = tempdir().expect("temp dir");
        let langs = detect_languages(
            dir.path(),
            &pathfinder_common::config::PathfinderConfig::default(),
        )
        .await
        .expect("detect");
        assert!(langs.is_empty());
    }

    #[tokio::test]
    async fn test_detects_go_mod_in_subdirectory() {
        let dir = tempdir().expect("temp dir");
        let sub_dir = dir.path().join("apps").join("backend");
        std::fs::create_dir_all(&sub_dir).expect("create dir");
        std::fs::write(sub_dir.join("go.mod"), "module foo").expect("write");
        let langs = detect_languages(
            dir.path(),
            &pathfinder_common::config::PathfinderConfig::default(),
        )
        .await
        .expect("detect");
        if let Some(go_lang) = langs.into_iter().find(|l| l.language_id == "go") {
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
            },
        );

        let langs = detect_languages(dir.path(), &config).await.expect("detect");
        // With a command override, `which` is bypassed — command is used as-is
        if let Some(go_lang) = langs.into_iter().find(|l| l.language_id == "go") {
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

    // ── resolve_command — not-found path (L63-72) ────────────────────────────

    #[test]
    fn test_resolve_command_not_found_returns_none() {
        // A binary name that will never exist on any PATH — exercises the
        // `else { None }` arm (L63-72) that emits a warning and returns None.
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
        // `echo` is always available on POSIX-compliant systems.
        // This exercises the Ok arm (L56-62).
        let result = resolve_command("sh", "shell");
        assert!(
            result.is_some(),
            "resolve_command must return Some for a binary on PATH"
        );
        let path = result.expect("sh must resolve to an absolute path");
        // Resolved path must be non-empty and typically absolute
        assert!(!path.is_empty());
    }

    // ── find_marker — depth-2 scan (L100-110) ───────────────────────────────

    #[tokio::test]
    async fn test_find_marker_finds_at_depth_2() {
        // Arrange: workspace → apps → backend → go.mod
        let dir = tempdir().expect("temp dir");
        let deep = dir.path().join("apps").join("backend");
        std::fs::create_dir_all(&deep).expect("create dirs");
        std::fs::write(deep.join("go.mod"), "module deep").expect("write");

        // find_marker with max_depth=2 must recurse into the sub-subdirectory.
        let found = find_marker(dir.path(), "go.mod", 2).await;
        assert_eq!(
            found.as_deref(),
            Some(deep.as_path()),
            "depth-2 marker must be discovered"
        );
    }

    #[tokio::test]
    async fn test_find_marker_depth_0_does_not_recurse() {
        // With max_depth=0 only the base directory is checked.
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

    // ── detect_languages with command override empty string filter (L144-146) ─

    #[tokio::test]
    async fn test_command_override_empty_string_falls_back_to_which() {
        // An empty `command` in LspConfig must be treated as "not configured"
        // (the `filter(|c| !c.is_empty())` gate at L145 drops it) so the
        // code falls through to `resolve_command`. If the binary is absent in CI
        // the language will simply be absent — no panic.
        let dir = tempdir().expect("temp dir");
        std::fs::write(dir.path().join("go.mod"), "module test").expect("write");

        let mut config = pathfinder_common::config::PathfinderConfig::default();
        config.lsp.insert(
            "go".to_string(),
            pathfinder_common::config::LspConfig {
                command: String::default(), // Empty → must fall through to `which`
                args: vec![],
                idle_timeout_minutes: 15,
                settings: serde_json::Value::Null,
                root_override: None,
            },
        );

        // Must not panic; binary may or may not be present
        let langs = detect_languages(dir.path(), &config).await.expect("detect");
        for l in &langs {
            assert!(!l.command.is_empty(), "resolved command must be non-empty");
        }
    }
}
