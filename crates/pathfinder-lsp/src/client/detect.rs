//! Zero-Config workspace language detection (PRD §6.5).
//!
//! Scans the workspace root for well-known marker files to determine which
//! language servers should be started. Returns a list of [`LanguageLsp`]
//! descriptors — one per detected language. Only languages whose marker is
//! present in the workspace are returned.
//!
//! Language servers are started lazily on first use (not eagerly at detection
//! time), so this scan is cheap.

use std::path::Path;

/// Identifies a language and the command used to spawn its LSP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageLsp {
    /// Short language identifier used as a map key (e.g., `"rust"`, `"go"`).
    pub language_id: String,
    /// The binary to execute (e.g., `"rust-analyzer"`).
    pub command: String,
    /// Arguments to pass after the binary (e.g., `["--stdio"]`).
    pub args: Vec<String>,
    /// The root directory to use for the LSP `initialize` request.
    ///
    /// In monorepos, this is the subdirectory containing the project's
    /// marker file (e.g., `apps/backend` for a Go backend with `go.mod` there)
    /// rather than the workspace root.
    pub root: std::path::PathBuf,
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

#[allow(clippy::missing_errors_doc)]
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

    // Rust — Cargo.toml (root only; Rust workspaces always have it at the root)
    let rust_root = match get_override!("rust") {
        Some(r) => Some(r),
        None => find_marker(workspace_root, "Cargo.toml", 0).await,
    };
    if let Some(root) = rust_root {
        detected.push(LanguageLsp {
            language_id: "rust".to_owned(),
            command: "rust-analyzer".to_owned(),
            args: vec![],
            root,
        });
    }

    // Go — go.mod (check root then up to depth 2 for monorepos like apps/backend)
    let go_root = match get_override!("go") {
        Some(r) => Some(r),
        None => find_marker(workspace_root, "go.mod", 2).await,
    };
    if let Some(root) = go_root {
        detected.push(LanguageLsp {
            language_id: "go".to_owned(),
            command: "gopls".to_owned(),
            args: vec![],
            root,
        });
    }

    // TypeScript / JavaScript — tsconfig.json or package.json (depth 2)
    let ts_root = match get_override!("typescript") {
        Some(r) => Some(r),
        None => find_marker(workspace_root, "tsconfig.json", 2)
            .await
            .or(find_marker(workspace_root, "package.json", 2).await),
    };
    if let Some(root) = ts_root {
        detected.push(LanguageLsp {
            language_id: "typescript".to_owned(),
            command: "typescript-language-server".to_owned(),
            args: vec!["--stdio".to_owned()],
            root,
        });
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
        detected.push(LanguageLsp {
            language_id: "python".to_owned(),
            command: "pyright".to_owned(),
            args: vec!["--stdio".to_owned()],
            root,
        });
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
        assert_eq!(langs.len(), 1);
        assert_eq!(langs[0].language_id, "rust");
        assert_eq!(langs[0].command, "rust-analyzer");
        assert!(langs[0].args.is_empty());
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
        assert_eq!(langs.len(), 1);
        assert_eq!(langs[0].language_id, "go");
        assert_eq!(langs[0].command, "gopls");
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
        assert_eq!(langs.len(), 1);
        assert_eq!(langs[0].language_id, "typescript");
        assert_eq!(langs[0].args, ["--stdio"]);
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
        assert_eq!(langs.len(), 1);
        assert_eq!(langs[0].language_id, "typescript");
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
        assert_eq!(langs.len(), 1);
        assert_eq!(langs[0].language_id, "python");
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
        // Rust is added first, TypeScript second
        let ids: Vec<&str> = langs.iter().map(|l| l.language_id.as_str()).collect();
        assert!(ids.contains(&"rust"));
        assert!(ids.contains(&"typescript"));
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
        let go_lang = langs
            .into_iter()
            .find(|l| l.language_id == "go")
            .expect("go found");
        assert_eq!(go_lang.root, sub_dir);
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
        let go_lang = langs
            .into_iter()
            .find(|l| l.language_id == "go")
            .expect("go found");
        assert_eq!(go_lang.root, dir.path().join("custom/backend"));
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
}
